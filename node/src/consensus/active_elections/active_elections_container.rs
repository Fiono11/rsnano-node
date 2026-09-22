use std::{
    cmp::max,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    mem::size_of,
    time::Duration,
};

use strum::EnumCount;

use rsnano_ledger::RepWeights;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, Amount, Block, BlockHash, BlockPriority, ConsensusEpoch, PublicKey, QualifiedRoot,
    SavedBlock, TimePriority, VoteError, VoteKind,
};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};
use rustc_hash::FxHashSet;

use crate::{
    consensus::{
        AecSnapshot, ElectionCandidateSource,
        election::{
            AccountFrontier, AddForkResult, CertificateEvidence, Committee, Committees,
            ConfirmationType, ConfirmedElection, Election, ElectionBehavior, ElectionId,
            ElectionState, EpochSlot, EpochState, FinalStateHash, LocalSlotState, VoteType,
        },
        election_schedulers::priority::bucket_count,
        filtered_vote::FilteredVote,
        vote_generation::VoteTarget,
    },
    representatives::QuorumSnapshot,
    utils::diagnostic,
};

use super::{
    ActiveElectionsConfig, ActiveElectionsInfo, AecFact, AecInsertError, AecInsertRequest, Entry,
    RootContainer,
    apply_vote_helper::{
        ApplyVoteHelper, ApplyVoteResult, count_kudzu_election, previous_epoch_closed,
        settle_election,
    },
    cooldown_controller::{AecCooldownReason, CooldownController, CooldownResult},
    epoch_close::{AgreedContent, CloseEvent, EpochClose, EpochCloseInfo, is_late},
    epoch_committees::{CommitteeInfo, EpochCommittees, live_committees},
    epoch_states::{Delegation, EpochStates, FinalizedInstance},
    recently_confirmed_cache::RecentlyConfirmedCache,
    slot_states::SlotStates,
    stats::AecStats,
};

/// Kudzu never evicts, so a full bucket makes the scheduler hold its blocks
/// until an election in it terminates. The spam workload keeps its accounts
/// in a few buckets, and around an epoch switch the in-flight blocks have two
/// instances each: the per-bucket cap would stall the scheduler for seconds.
/// The AEC size bounds the elections as a whole.
pub(crate) fn per_bucket_cap(max_elections: usize) -> usize {
    if cfg!(feature = "rai_protocol") {
        max_elections
    } else {
        max(max_elections / bucket_count(), 1)
    }
}

/// RAI: what this node reports for one epoch: the certified block tree and
/// the votes of its own that the tree does not summarize, with the digest of
/// the committee which issued the epoch's votes
#[cfg(feature = "rai_protocol")]
pub(crate) struct EpochReport {
    pub committee: BlockHash,
    pub certified: crate::consensus::election::CertifiedState,
    pub residual: crate::consensus::election::ResidualVotes,
}

pub(crate) struct ActiveElectionsContainer {
    roots: RootContainer,
    observer: Option<Sender<AecFact>>,
    stopped: bool,
    count_by_behavior: [usize; ElectionBehavior::COUNT],
    base_latency: Duration,
    recently_confirmed: RecentlyConfirmedCache,
    cooldown: CooldownController,
    max_elections: usize,
    max_elections_per_bucket: usize,
    stats: AecStats,
    /// Kudzu: what this node voted per slot (account height) and epoch. Shared
    /// by all elections at that height and dropped once the height is finalized.
    slots: SlotStates,
    /// Kudzu: exit final votes of elections which were finalized and erased
    /// before the voter could pick them up (line 11 still applies)
    pending_kudzu_votes: Vec<VoteTarget>,
    /// RAI: the epoch new elections are started in
    current_epoch: ConsensusEpoch,
    /// RAI: the current epoch's duration has ended: no new election starts
    /// in it, its instances run to their outcome, then it is left
    draining: bool,
    /// RAI: elections of the current epoch which got a certificate so far
    decided_in_current_epoch: usize,
    /// RAI: `decided_in_current_epoch` at which the epoch advances; 0 never
    epoch_terminated_elections: usize,
    /// RAI: how long an epoch lasts from its first election; zero: no time limit
    epoch_duration: Duration,
    /// RAI: the epochs have started: until then, during the setup of a run,
    /// no epoch ends and no epoch of another replica is followed
    epochs_started: bool,
    /// RAI: when the epochs started, the origin the epoch boundaries are
    /// aligned to on every replica
    epoch_origin: Option<Timestamp>,
    /// RAI: when the current epoch ends by time: the next boundary after
    /// its first election; None while it has none
    epoch_ends_at: Option<Timestamp>,
    /// RAI: when the drain was last reported
    drain_logged: Option<Timestamp>,
    /// RAI: what each epoch finalized explicitly
    epoch_states: EpochStates,
    /// RAI: the newest epoch each representative was seen voting in
    rep_epochs: HashMap<PublicKey, ConsensusEpoch>,
    /// RAI: the representatives seen voting in each epoch: those which take
    /// part in the epoch lead the rounds of its close election
    epoch_voters: BTreeMap<ConsensusEpoch, BTreeSet<PublicKey>>,
    /// RAI: the close elections of the epochs this node has left
    closes: BTreeMap<ConsensusEpoch, EpochClose>,
    /// RAI: the content behind the value this node attests in each close
    /// election, as of the state hashed: the frontiers the epoch's committee
    /// is derived from, and the entries the value hashed, once the value is
    /// finalized and agreed
    close_content: BTreeMap<ConsensusEpoch, Vec<EpochContent>>,
    /// RAI: the content of the epochs this node agreed on: an instance of
    /// such an epoch that notarizes anything else is late, and none is
    /// started any more
    agreed: AgreedContent,
    /// RAI: the committees the instances of each epoch are counted in
    committees: EpochCommittees,
    /// RAI: Δ_timeout of a close round
    close_round_timeout: Duration,
    /// RAI: the representatives this node votes with, to know when it leads
    /// a close round
    local_reps: Vec<PublicKey>,
}

impl ActiveElectionsContainer {
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            roots: RootContainer::new(config.max_elections),
            observer: None,
            stopped: false,
            count_by_behavior: Default::default(),
            base_latency,
            recently_confirmed: RecentlyConfirmedCache::new(config.confirmation_cache),
            cooldown: CooldownController::default(),
            max_elections: config.max_elections,
            max_elections_per_bucket: per_bucket_cap(config.max_elections),
            stats: Default::default(),
            slots: SlotStates::default(),
            pending_kudzu_votes: Vec::new(),
            current_epoch: ConsensusEpoch::ZERO,
            draining: false,
            decided_in_current_epoch: 0,
            epoch_terminated_elections: config.epoch_terminated_elections,
            epoch_duration: config.epoch_duration,
            epochs_started: false,
            epoch_origin: None,
            epoch_ends_at: None,
            drain_logged: None,
            epoch_states: EpochStates::default(),
            rep_epochs: HashMap::new(),
            epoch_voters: BTreeMap::new(),
            closes: BTreeMap::new(),
            close_content: BTreeMap::new(),
            agreed: AgreedContent::new(),
            committees: EpochCommittees::default(),
            close_round_timeout: config.close_round_timeout,
            local_reps: Vec::new(),
        }
    }

    /// RAI: whether epochs end at all on this node, by count or by time
    fn epochs_end(&self) -> bool {
        self.epochs_started
            && (self.epoch_terminated_elections > 0 || !self.epoch_duration.is_zero())
    }

    /// RAI: the setup of a run is over: epoch 0 starts now, at the same time
    /// on every replica, with the voting weights as they are now
    pub fn start_epochs(&mut self, now: Timestamp) {
        if self.epochs_started {
            return;
        }
        self.epochs_started = true;
        self.epoch_origin = Some(now);
        self.epoch_ends_at = self.next_boundary(now);
        self.decided_in_current_epoch = 0;
        if cfg!(feature = "rai_protocol") {
            diagnostic!(
                "EPOCH_START epoch={} elections={}",
                self.current_epoch,
                self.roots.len()
            );
        }
    }

    /// RAI: whether the epochs have started
    pub fn epochs_started(&self) -> bool {
        self.epochs_started
    }

    /// RAI: a vote of an epoch ahead of this node. Once more than f of the
    /// weight votes in a later epoch at least one correct representative has
    /// reached the epoch's end, and this node ends its epoch at once instead
    /// of finishing its own count: the blocks in flight during the switch get
    /// their decision as soon as every replica is in the same epoch. Once a
    /// certificate quorum is ahead, the epoch's close has started on the
    /// network and this node leaves the epoch at once, drained or not: its
    /// remaining instances run on, while waiting for them would leave it
    /// behind by every block published meanwhile.
    fn observe_epoch(&mut self, args: &ApplyVoteArgs) {
        let vote = &args.vote;
        // A close vote for an epoch is cast by a replica which has left it
        let epoch = vote.epoch.required_epoch();
        if !self.epochs_end() || epoch <= self.current_epoch {
            return;
        }
        self.rep_epochs.insert(vote.voter, epoch);
        let next = self.current_epoch.next();
        // The weights of the epoch ahead, as far as known here
        let committees = self
            .committees_for(next)
            .unwrap_or_else(|| live_committees(args.rep_weights, args.quorum_snapshot));
        let committee = committees.primary();
        let ahead: Amount = self
            .rep_epochs
            .iter()
            .filter(|(_, epoch)| **epoch >= next)
            .map(|(rep, _)| committee.weight(rep))
            .sum();
        let thresholds = committee.thresholds();
        if ahead > thresholds.f && !self.draining {
            self.stats.epochs_followed += 1;
            self.end_epoch(args.now);
        }
        if ahead >= thresholds.certificate && self.draining {
            self.stats.epochs_left_behind += 1;
            self.draining = false;
            self.advance_epoch(args.now);
        }
    }

    /// RAI: the epoch whose instances are draining, if any
    pub fn draining_epoch(&self) -> Option<ConsensusEpoch> {
        self.draining.then_some(self.current_epoch)
    }

    /// RAI: the representatives this node votes with
    pub fn set_local_representatives(&mut self, reps: Vec<PublicKey>) {
        self.local_reps = reps;
    }

    /// RAI: the committees known here, the genesis one first
    pub fn epoch_committees(&self) -> Vec<CommitteeInfo> {
        self.committees.infos()
    }

    /// RAI: the close elections of the epochs this node has left
    pub fn epoch_closes(&self) -> Vec<EpochCloseInfo> {
        self.closes.values().map(|close| close.info()).collect()
    }

    /// RAI: end the current epoch now, whatever its count; it is left once
    /// its instances have drained
    pub fn leave_epoch(&mut self, now: Timestamp) {
        self.start_epochs(now);
        self.end_epoch(now);
    }

    /// RAI: the current epoch's duration has ended and its instances drain
    pub fn is_draining(&self) -> bool {
        self.draining
    }

    /// RAI: elections of the current epoch which got a certificate so far
    pub fn decided_in_current_epoch(&self) -> usize {
        self.decided_in_current_epoch
    }

    /// RAI: whether this block was finalized explicitly in the given epoch
    pub fn finalized_in_epoch(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> bool {
        self.epoch_states.finalized_in_epoch(hash, epoch)
    }

    /// RAI: whether this block was finalized explicitly in any epoch
    pub fn is_finalized(&self, hash: &BlockHash) -> bool {
        self.epoch_states.is_finalized(hash)
    }

    /// RAI: whether this node cast its final vote for the block in the epoch
    pub fn final_voted_in_epoch(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> bool {
        self.epoch_states.final_voted_in_epoch(hash, epoch)
    }

    /// RAI: the explicitly finalized state per epoch
    pub fn finalized_by_epoch(&self) -> &BTreeMap<ConsensusEpoch, FinalStateHash> {
        self.epoch_states.by_epoch()
    }

    /// RAI: the blocks finalized explicitly in the given epoch
    pub fn finalized_in(&self, epoch: ConsensusEpoch) -> Vec<(Account, u64, BlockHash)> {
        self.epoch_states.finalized_in(epoch)
    }

    /// RAI: count the elections of the current epoch which just got their
    /// first certificate and end the epoch once enough have
    fn count_decided(&mut self, decided: &[ConsensusEpoch], now: Timestamp) {
        self.decided_in_current_epoch += decided
            .iter()
            .filter(|epoch| **epoch == self.current_epoch)
            .count();
        if self.epochs_started
            && self.epoch_terminated_elections > 0
            && self.decided_in_current_epoch >= self.epoch_terminated_elections
            && !self.draining
        {
            self.end_epoch(now);
        }
    }

    /// RAI: the epoch boundaries are the multiples of the epoch duration
    /// from the start of the epochs, the same instants on every replica;
    /// the next one after `now`
    fn next_boundary(&self, now: Timestamp) -> Option<Timestamp> {
        if self.epoch_duration.is_zero() {
            return None;
        }
        let origin = self.epoch_origin?;
        let passed = origin.elapsed(now).as_nanos() / self.epoch_duration.as_nanos();
        let next = self.epoch_duration.as_nanos() * (passed + 1);
        Some(origin + Duration::from_nanos(next as u64))
    }

    /// RAI: an epoch with a time limit ends at the first boundary after its
    /// first election; an epoch without elections never ends by time
    fn end_epoch_by_time(&mut self, now: Timestamp) {
        if !self.epochs_started || self.draining {
            return;
        }
        if self.epoch_ends_at.is_some_and(|ends_at| now >= ends_at) {
            self.end_epoch(now);
        }
    }

    /// RAI: the current epoch's duration ends. No new election starts in it
    /// any more; its instances run to their outcome, and once they have all
    /// terminated and the epoch before is closed, the epoch is left: its close
    /// election starts and the next epoch starts with it. The close election
    /// proposes and votes once the instances have also settled (see
    /// `tick_closes`), which they do while the next epoch runs.
    fn end_epoch(&mut self, now: Timestamp) {
        self.draining = true;
        self.stats.epochs_ended += 1;
        if cfg!(feature = "rai_protocol") {
            diagnostic!(
                "EPOCH_ENDED epoch={} elections={} active={}",
                self.current_epoch,
                self.roots.len(),
                self.roots.active_len()
            );
        }
        self.try_advance_epoch(now);
    }

    /// RAI: leave the ended epoch once it has drained
    fn try_advance_epoch(&mut self, now: Timestamp) {
        if !self.draining
            || !self.previous_epoch_closed(self.current_epoch)
            || !self.epoch_state(self.current_epoch).is_terminated()
        {
            return;
        }
        self.draining = false;
        self.advance_epoch(now);
    }

    /// RAI: whether the close election of the epoch before this one has
    /// finalized its value; an epoch without a known predecessor counts as such
    fn previous_epoch_closed(&self, epoch: ConsensusEpoch) -> bool {
        previous_epoch_closed(&self.closes, epoch)
    }

    /// RAI: the committee the instances of an epoch are counted in, if it
    /// is known here (see `EpochCommittees`)
    fn committees_for(&self, epoch: ConsensusEpoch) -> Option<Committees> {
        self.committees.for_epoch(epoch)
    }

    /// RAI: the frontiers of every account at the end of the setup: the
    /// genesis committee the first two epochs count in, and the base the
    /// committees of the epochs closed are derived on
    pub fn set_genesis_committee(&mut self, frontiers: Vec<AccountFrontier>) {
        if self.committees.started() {
            return;
        }
        let committee = self.committees.start(frontiers);
        self.log_committee("genesis", &committee);
    }

    /// RAI: the frontiers an epoch finalized, as of the value its close
    /// agreed on: the committee the epoch two after counts in. The
    /// instances collected so far in that epoch are counted now.
    pub fn derive_committee(
        &mut self,
        epoch: ConsensusEpoch,
        frontiers: Vec<AccountFrontier>,
        now: Timestamp,
    ) {
        let derived = self.committees.derive(epoch, frontiers);
        if derived.is_empty() {
            return;
        }
        for (epoch, committee) in &derived {
            self.log_committee(&epoch.to_string(), committee);
        }
        // The epoch two after each counts in it (K_e = C(e−2)), and it is
        // the new committee of the close of the epoch before that one
        for (epoch, _) in &derived {
            self.recount_epoch(ConsensusEpoch::new(epoch.as_u64() + 2), now);
        }
        self.recount_closes(now);
    }

    /// RAI: count the open instances of an epoch again: its committee
    /// became known here, after votes of the epoch had been collected. The
    /// certificates the committee supports form on that count, on every
    /// replica alike.
    fn recount_epoch(&mut self, epoch: ConsensusEpoch, now: Timestamp) {
        let Some(committees) = self.committees_for(epoch) else {
            return;
        };
        let ids: Vec<ElectionId> = self
            .roots
            .iter()
            .map(|entry| &entry.election)
            .filter(|election| election.epoch() == epoch && !election.state().has_ended())
            .map(|election| election.id())
            .collect();
        let mut result = ApplyVoteResult::default();
        for id in ids {
            let Some(election) = self.roots.election_mut(&id) else {
                continue;
            };
            count_kudzu_election(
                election,
                &committees,
                now,
                &mut self.stats,
                &self.observer,
                &mut self.recently_confirmed,
                &self.agreed,
            );
            settle_election(&mut self.roots, &id, &self.observer, &mut result);
        }
        self.stats.recounted += result.decided.len();
        for entry in result.confirmed {
            self.cleanup_election(entry);
        }
        self.count_decided(&result.decided, now);
    }

    /// RAI: count the rounds of the open close elections again, in the
    /// committee of their epoch as known now
    fn recount_closes(&mut self, now: Timestamp) {
        let epochs: Vec<_> = self
            .closes
            .values()
            .filter(|close| !close.is_closed())
            .map(|close| close.epoch())
            .collect();
        for epoch in epochs {
            let Some(committees) = self.committees.for_close(epoch) else {
                continue;
            };
            let close = self.closes.get_mut(&epoch).unwrap();
            close.recount(&committees, now);
            let events = close.take_events();
            self.log_close_events(epoch, events, now);
        }
    }

    fn log_committee(&self, derived_by: &str, committee: &Committee) {
        if !cfg!(feature = "rai_protocol") {
            return;
        }
        let mut members: Vec<_> = committee.weights().iter().collect();
        members.sort_by(|(_, a), (_, b)| b.cmp(a));
        let shares: Vec<String> = members
            .iter()
            .map(|(rep, weight)| {
                let share = weight.number() as f64 / committee.online().number().max(1) as f64;
                format!("{}:{:.1}%", &rep.to_string()[..8], share * 100.0)
            })
            .collect();
        diagnostic!(
            "EPOCH_COMMITTEE derived_by={} members={} accounts={} n={} digest={} shares={:?}",
            derived_by,
            committee.len(),
            self.committees.counted(),
            committee.online().number(),
            committee.digest(),
            shares
        );
    }

    /// RAI: leave the current epoch and start the next one. The instances of
    /// the old epoch run on to their termination: this node proposed in
    /// them, so it keeps voting in them. Every replica which started one of
    /// them before its own switch does the same; a replica which learns of a
    /// block only after switching proposes it in the new epoch, and its votes
    /// open that instance here.
    /// The close election of the epoch left is created; it takes part only
    /// once every instance of the epoch has terminated and settled here (see
    /// `tick_closes`). Its rounds are led by the representatives which voted
    /// in the epoch or in the one after it (see `close_leaders`).
    fn advance_epoch(&mut self, now: Timestamp) {
        let left = self.current_epoch;
        self.current_epoch = self.current_epoch.next();
        self.decided_in_current_epoch = 0;
        self.epoch_ends_at = None;
        self.stats.epochs_advanced += 1;
        let leaders = self.close_leaders(left);
        self.closes.insert(
            left,
            EpochClose::new(left, leaders, self.close_round_timeout),
        );
        self.tick_closes(now);
        self.notify(AecFact::EpochAdvanced(self.current_epoch));
        if cfg!(feature = "rai_protocol") {
            diagnostic!(
                "EPOCH_ADVANCED epoch={} elections={} active={}",
                self.current_epoch,
                self.roots.len(),
                self.roots.active_len()
            );
        }
    }

    /// RAI: the final state of an epoch as it stands on this node, the value
    /// it attests in the epoch's close: what its finalized instances decided
    /// and what its running ones notarized - those opened before the close
    /// certificate was seen here, so that the value stands still once the
    /// epoch is closed and can agree. Of an agreed epoch, without its late
    /// instances: this node keeps attesting the value finalized.
    pub fn epoch_state(&self, epoch: ConsensusEpoch) -> EpochState {
        self.epoch_state_and_late(epoch, self.close_cutoff(epoch)).0
    }

    /// RAI: the epoch's state with every instance counted, whenever opened:
    /// the value a replica that lacked blocks at the close reaches once
    /// they came, and may agree with
    fn epoch_state_all(&self, epoch: ConsensusEpoch) -> EpochState {
        self.epoch_state_and_late(epoch, None).0
    }

    /// RAI: when the close certificate of the epoch was seen here, if it was
    fn close_cutoff(&self, epoch: ConsensusEpoch) -> Option<Timestamp> {
        self.closes.get(&epoch).and_then(|close| close.closed_at())
    }

    /// RAI: the epoch's state over the instances opened before the cutoff,
    /// and its late instances: those of an agreed epoch which notarized a
    /// block the agreed value does not hold
    fn epoch_state_and_late(
        &self,
        epoch: ConsensusEpoch,
        cutoff: Option<Timestamp>,
    ) -> (EpochState, Vec<ElectionId>) {
        let finalized = self.epoch_states.by_epoch().get(&epoch);
        let mut state = EpochState::with_finalized(
            finalized.unwrap_or(&FinalStateHash::default()),
            self.epoch_states.finalized_count(epoch),
        );
        let mut late = Vec::new();
        for election in self.roots.iter().map(|entry| &entry.election) {
            if election.epoch() != epoch {
                continue;
            }
            if is_late(&self.agreed, election) {
                late.push(election.id());
                continue;
            }
            if cutoff.is_some_and(|cutoff| election.start() >= cutoff) {
                continue;
            }
            state.add_election(
                &election.account(),
                election.height(),
                election.state(),
                election.certificates(),
            );
        }
        (state, late)
    }

    /// RAI: the content behind an epoch's state as it stands: every
    /// (account, height, block) the state hashes - the winners of the
    /// finalized instances and the blocks the running ones notarized - and
    /// the frontiers: for every account with a block in the state, the block
    /// at its greatest height there, if there is exactly one at that height,
    /// with what it delegates. Two replicas attesting the same value hold
    /// the same blocks, whatever certificates each has seen, so they hold
    /// the same content. The blocks are read from the instances themselves,
    /// which hold every candidate: the ledger may not hold a fork's winner
    /// yet (still being inserted).
    fn epoch_content(&self, epoch: ConsensusEpoch, cutoff: Option<Timestamp>) -> EpochContent {
        let mut entries: HashSet<(Account, u64, BlockHash)> = HashSet::new();
        let mut by_account: HashMap<Account, (u64, Vec<Delegation>)> = HashMap::new();
        let mut add = |account: Account, height: u64, delegation: Delegation| {
            entries.insert((account, height, delegation.hash));
            let entry = by_account.entry(account).or_insert((height, Vec::new()));
            if height > entry.0 {
                *entry = (height, vec![delegation]);
            } else if height == entry.0 && !entry.1.iter().any(|d| d.hash == delegation.hash) {
                entry.1.push(delegation);
            }
        };
        for block in self.epoch_states.finalized_blocks_in(epoch) {
            add(block.account, block.height, block.delegation);
        }
        for election in self.roots.iter().map(|entry| &entry.election) {
            if election.epoch() != epoch
                || is_late(&self.agreed, election)
                || cutoff.is_some_and(|cutoff| election.start() >= cutoff)
            {
                continue;
            }
            for delegation in delegations(election) {
                add(election.account(), election.height(), delegation);
            }
        }
        let mut frontiers: Vec<AccountFrontier> = by_account
            .into_iter()
            .filter_map(
                |(account, (height, delegations))| match delegations.as_slice() {
                    [delegation] => Some(AccountFrontier {
                        account,
                        height,
                        representative: delegation.representative,
                        balance: delegation.balance,
                    }),
                    _ => None,
                },
            )
            .collect();
        frontiers.sort_by_key(|frontier| frontier.account);
        EpochContent {
            value: BlockHash::ZERO,
            frontiers,
            entries,
        }
    }

    /// RAI: keep the content behind each value this node may agree with in
    /// the epoch's close, as of the state hashed: the one it attests, and
    /// the one with every instance counted
    fn keep_close_content(
        &mut self,
        epoch: ConsensusEpoch,
        values: [(BlockHash, Option<Timestamp>); 2],
    ) {
        for (value, cutoff) in values {
            if self
                .close_content
                .get(&epoch)
                .is_some_and(|kept| kept.iter().any(|content| content.value == value))
            {
                continue;
            }
            let mut content = self.epoch_content(epoch, cutoff);
            content.value = value;
            let kept = self.close_content.entry(epoch).or_default();
            // The two values as of the last tick are all that can agree
            kept.retain(|content| content.value == value || kept_value(content, &values));
            kept.push(content);
        }
    }

    /// RAI: the leaders of the rounds of an epoch's close election, in public
    /// key order: the representatives seen voting in the epoch or in the one
    /// after it. Every replica saw the same ones vote, while the ledger's
    /// weights also name representatives which never vote; and one which
    /// voted in the epoch with a weight it has since passed on (the genesis
    /// representative funding the others) leads a round that times out, not
    /// every round.
    fn close_leaders(&self, epoch: ConsensusEpoch) -> Vec<PublicKey> {
        let mut leaders: BTreeSet<PublicKey> = BTreeSet::new();
        for epoch in [epoch, epoch.next()] {
            if let Some(voters) = self.epoch_voters.get(&epoch) {
                leaders.extend(voters.iter().copied());
            }
        }
        leaders.into_iter().collect()
    }

    /// RAI: drive the close elections: each attests the state of its epoch
    /// as it stands and enters or leaves its rounds. A close election takes
    /// part only after the epoch's duration ended and this node left the
    /// epoch, every instance of the epoch settled here (the value attested
    /// counts a single notarization certificate only once no other
    /// certificate can form in its instance), and the epoch before was
    /// closed: the epochs close one after the other. The votes of the other
    /// replicas are collected all the while.
    fn tick_closes(&mut self, now: Timestamp) {
        // A closed epoch is still followed until this node agrees with the
        // value: its instances may settle only after the certificate came
        let epochs: Vec<_> = self
            .closes
            .values()
            .filter(|close| !close.is_agreed())
            .map(|close| close.epoch())
            .collect();
        for epoch in epochs {
            let cutoff = self.close_cutoff(epoch);
            let state = self.epoch_state(epoch);
            let all = self.epoch_state_all(epoch);
            let values = [
                (state.close_value(epoch), cutoff),
                (all.close_value(epoch), None),
            ];
            self.keep_close_content(epoch, values);
            let previous_closed = self.previous_epoch_closed(epoch);
            let leaders = self.close_leaders(epoch);
            let close = self.closes.get_mut(&epoch).unwrap();
            close.set_state(&state);
            close.set_alternative(values[1].0);
            close.set_leaders(leaders);
            if previous_closed {
                close.tick(now);
            }
            if close.is_closed() {
                self.epoch_voters.remove(&epoch);
            }
            let events = close.take_events();
            self.log_close_events(epoch, events, now);
        }
        self.discard_late_instances();
        self.log_drain_wait(now);
    }

    /// RAI: the only discard: an instance of an agreed epoch notarized a
    /// block the agreed value does not hold. It is erased; its blocks are
    /// in this node's ledger and not in the value, so they are rolled back
    /// (see `cleanup_election`), unless finalized in another epoch.
    fn discard_late_instances(&mut self) {
        let agreed: Vec<_> = self.agreed.keys().copied().collect();
        for epoch in agreed {
            let (_, late) = self.epoch_state_and_late(epoch, None);
            for id in late {
                self.erase_election(&id);
            }
        }
    }

    /// RAI: what an ending epoch's drain is waiting for, once a second
    fn log_drain_wait(&mut self, now: Timestamp) {
        if !cfg!(feature = "rai_protocol") || !self.draining {
            return;
        }
        if self
            .drain_logged
            .is_some_and(|last| last.elapsed(now) < Duration::from_secs(1))
        {
            return;
        }
        self.drain_logged = Some(now);
        let waiting: Vec<_> = self
            .roots
            .iter()
            .map(|entry| &entry.election)
            .filter(|e| e.epoch() == self.current_epoch && !e.state().is_terminated())
            .take(3)
            .map(|e| {
                format!(
                    "{}:{}:{}:{}",
                    e.qualified_root().root,
                    e.state().as_str(),
                    e.block_count(),
                    e.kudzu_votes().len()
                )
            })
            .collect();
        diagnostic!(
            "EPOCH_DRAIN_WAIT epoch={} unterminated={} previous_closed={} first={:?}",
            self.current_epoch,
            self.epoch_state(self.current_epoch).unterminated,
            self.previous_epoch_closed(self.current_epoch),
            waiting
        );
    }

    fn log_close_events(&mut self, epoch: ConsensusEpoch, events: Vec<CloseEvent>, now: Timestamp) {
        for event in events {
            match &event {
                CloseEvent::Ready(_) => {}
                CloseEvent::RoundEntered { .. } => self.stats.close_rounds += 1,
                CloseEvent::Closed { .. } => self.stats.epochs_closed += 1,
                CloseEvent::Agreed(value) => self.epoch_agreed(epoch, *value, now),
                CloseEvent::RoundConflict { .. } => self.stats.close_conflicts += 1,
            }
            if !cfg!(feature = "rai_protocol") {
                continue;
            }
            match event {
                CloseEvent::Ready(value) => {
                    diagnostic!("EPOCH_CLOSE_READY epoch={} value={}", epoch, value);
                }
                CloseEvent::RoundEntered { round, leader } => {
                    let leads = leader.is_some_and(|leader| self.local_reps.contains(&leader));
                    diagnostic!(
                        "EPOCH_CLOSE_ROUND epoch={} round={} leader={} leads={}",
                        epoch,
                        round,
                        leader
                            .map(|leader| leader.to_string())
                            .unwrap_or_else(|| "none".to_string()),
                        leads
                    );
                }
                CloseEvent::Closed { round, value, own } => {
                    diagnostic!(
                        "EPOCH_CLOSED epoch={} round={} value={} own={}",
                        epoch,
                        round,
                        value,
                        own
                    );
                }
                CloseEvent::Agreed(value) => {
                    diagnostic!("EPOCH_AGREED epoch={} value={}", epoch, value);
                }
                CloseEvent::RoundConflict { round } => {
                    diagnostic!("EPOCH_CLOSE_CONFLICT epoch={} round={}", epoch, round);
                }
            }
        }
    }

    /// RAI: the value this node attests in an epoch's close is the one
    /// finalized: the epoch is decided for good. Its content is what the
    /// epoch holds from now on - an instance notarizing anything else is
    /// late, none is started any more, and the states of the slots without
    /// an instance are dropped - and its frontiers derive the committee.
    fn epoch_agreed(&mut self, epoch: ConsensusEpoch, value: BlockHash, now: Timestamp) {
        let kept = self.close_content.remove(&epoch).unwrap_or_default();
        let content = match kept.into_iter().find(|content| content.value == value) {
            Some(content) => content,
            // The value was attested on an earlier tick; the state has not
            // changed since, or the close would not have agreed. Of the two
            // states, the one hashing to the value.
            None => {
                let cutoff = self.close_cutoff(epoch);
                let mut content = self.epoch_content(epoch, cutoff);
                if self.epoch_state(epoch).close_value(epoch) != value {
                    content = self.epoch_content(epoch, None);
                }
                content.value = value;
                content
            }
        };
        self.agreed.insert(epoch, content.entries);
        let live: FxHashSet<(Account, u64)> = self
            .roots
            .iter()
            .map(|entry| &entry.election)
            .filter(|election| election.epoch() == epoch)
            .map(|election| (election.account(), election.height()))
            .collect();
        self.slots.remove_epoch_except(epoch, &live);
        self.derive_committee(epoch, content.frontiers, now);
    }

    /// RAI: a vote in a round of an epoch's close election
    fn apply_close_vote(
        &mut self,
        args: &ApplyVoteArgs,
        epoch: ConsensusEpoch,
        round: u32,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        let vote = &args.vote;
        // The close counts in the committee of its epoch; before the epochs
        // of a run started, in the ledger's live weights. A close vote of an
        // epoch whose committee is not known here yet waits in the vote cache.
        let committees = if self.committees.started() {
            self.committees.for_close(epoch)
        } else {
            Some(live_committees(args.rep_weights, args.quorum_snapshot))
        };
        let mut per_block = HashMap::new();
        for hash in vote.filtered_blocks() {
            // Not left yet: the vote waits in the vote cache until this node
            // gets there. Left before this node ran: nothing to close here.
            let result = match (self.closes.get_mut(&epoch), &committees) {
                (Some(close), Some(committees)) => {
                    close.apply_vote(vote.voter, *hash, vote.kind(), round, committees, args.now)
                }
                (Some(_), None) => Err(VoteError::Indeterminate),
                (None, _) if epoch >= self.current_epoch => Err(VoteError::Indeterminate),
                (None, _) => Err(VoteError::Late),
            };
            per_block.insert(*hash, result);
        }
        if let Some(close) = self.closes.get_mut(&epoch) {
            let events = close.take_events();
            self.log_close_events(epoch, events, args.now);
        }
        per_block
    }

    /// RAI: the close rounds whose evidence is to be solicited now, each with
    /// the value the request is routed through
    pub fn close_solicitations(&mut self, now: Timestamp) -> Vec<(ElectionId, BlockHash)> {
        let interval = self.base_latency;
        self.closes
            .values_mut()
            .flat_map(|close| {
                close
                    .solicitations(now, interval)
                    .into_iter()
                    .map(|(round, value)| (close.id(round), value))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// RAI: an instance started for a vote of its epoch, for a block this node
    /// holds. Every instance that exists on some replica must reach the same
    /// outcome on every replica, so it is started here too, even if the block
    /// is already cemented. In the current epoch this node proposes the block
    /// as usual; in an epoch it has left it casts only its timeout vote and
    /// collects the certificates the others produce.
    pub fn insert_for_vote(&mut self, block: SavedBlock, epoch: ConsensusEpoch, now: Timestamp) {
        debug_assert!(epoch <= self.current_epoch);
        if self.stopped {
            return;
        }
        let id = ElectionId::new(block.qualified_root(), epoch);
        if self.roots.get(&id).is_some() {
            return;
        }
        // An agreed epoch is decided for good: the instance could only be late
        if self.agreed.contains_key(&epoch) {
            self.stats.agreed_epoch_refused += 1;
            return;
        }
        // No new election starts in an ended epoch: in an instance of an
        // epoch this node has left, or of the one it is draining, it does
        // not propose, it abstains, and the block is decided in the next
        // epoch. A faulty representative could otherwise extend the drain
        // without end, a fresh block at a time, and with the votes of the
        // replicas still draining notarize each one: the epoch's value would
        // never stand still for its close
        if epoch < self.current_epoch || self.draining {
            let slot = EpochSlot {
                account: block.account(),
                height: block.height(),
                epoch,
            };
            if self.slots.get(&slot).is_none() {
                *self.slots.get_or_default(&slot) = LocalSlotState::stale();
            }
            self.stats.stale_started += 1;
        } else {
            self.stats.started_for_vote += 1;
        }
        let request = AecInsertRequest::new_priority(block, BlockPriority::default());
        self.insert_new_election_in_epoch(request, epoch, now);
        if let Some(election) = self.roots.election_mut(&id) {
            election.transition_active();
        }
    }

    /// RAI: the certified block tree of one epoch as it stands here: every
    /// complete notarized block with the finalization status this node has
    /// been able to construct for it. It grows as gossip delivers the votes
    /// behind a certificate, which is what lets a later state bridge to a
    /// root a report froze earlier.
    #[cfg(feature = "rai_protocol")]
    pub fn epoch_certified(
        &self,
        epoch: ConsensusEpoch,
    ) -> crate::consensus::election::CertifiedState {
        use crate::consensus::election::{CertifiedBlock, CertifiedState, CertifiedStatus};
        let mut certified = CertifiedState::new();
        // What finalized in the epoch and left the AEC is certified by the
        // certificate that finalized it
        for instance in self.epoch_states.instances_of(epoch) {
            certified.certify(
                CertifiedBlock::new(instance.account, instance.height, instance.winner),
                CertifiedStatus::Finalized,
            );
        }
        // What the instances still in the AEC have certified
        for election in self.roots.iter().map(|entry| &entry.election) {
            if election.epoch() != epoch {
                continue;
            }
            let at = |hash| CertifiedBlock::new(election.account(), election.height(), hash);
            let certificates = election.certificates();
            for hash in &certificates.notar {
                certified.certify(at(*hash), CertifiedStatus::Notarized);
            }
            if let Some(hash) = certificates.final_ {
                certified.certify(at(hash), CertifiedStatus::Finalized);
            }
            if let Some(hash) = certificates.fast {
                certified.certify(at(hash), CertifiedStatus::FastFinalized);
            }
        }
        certified
    }

    /// RAI, "Certified-state reports and reconciliation": what this node
    /// reports for one epoch. The certified state holds every complete
    /// notarized block of the epoch with the finalization status this node
    /// has been able to construct for it; the residual votes hold this
    /// node's own votes that the certified state does not summarize - its
    /// support for a block with no notarization certificate, and its final
    /// vote for a block that is notarized but not finalized. Lemma 3.7 is
    /// what the second one is for: a certificate constructible only from
    /// votes issued before the boundary stays report-visible.
    ///
    /// The certified state is read from the instances themselves, which keep
    /// their certificates, and from the instances that finalized and left
    /// the AEC; the residual votes from the slot states, which hold this
    /// node's one-shot votes per slot and epoch.
    #[cfg(feature = "rai_protocol")]
    pub fn epoch_report(&self, epoch: ConsensusEpoch) -> Option<EpochReport> {
        use crate::consensus::election::{
            CertifiedBlock, ResidualKind, ResidualVotes, TIMEOUT_BLOCK,
        };
        let committee = self.committees.committee(epoch)?;
        let certified = self.epoch_certified(epoch);
        let mut residual = ResidualVotes::new();

        // This node's own votes which the certified state does not summarize
        let mut record = |account: Account, height: u64, slot: &LocalSlotState| {
            let mut support = |hash: BlockHash| {
                let block = CertifiedBlock::new(account, height, hash);
                if certified.status(&block).is_none() {
                    residual.record(block, ResidualKind::Support);
                }
            };
            if let Some(hash) = slot.first_voted.filter(|hash| *hash != TIMEOUT_BLOCK) {
                support(hash);
            }
            for hash in &slot.notar_voted {
                support(*hash);
            }
            if let Some(hash) = slot.final_voted {
                let block = CertifiedBlock::new(account, height, hash);
                if certified
                    .status(&block)
                    .is_none_or(|status| !status.is_finalized())
                {
                    residual.record(block, ResidualKind::Final);
                }
            }
        };
        for (account, height, slot) in self.slots.iter_epoch(epoch) {
            record(account, height, slot);
        }
        for (account, height, slot) in self.epoch_states.slots_of(epoch) {
            record(account, height, slot);
        }

        Some(EpochReport {
            committee: committee.digest(),
            certified,
            residual,
        })
    }

    /// RAI: another replica named a candidate of one of this node's
    /// instances which this node does not hold. Recorded on the instance so
    /// that, if the block never arrives and it has many first votes, the
    /// timeout block is notarized in its place.
    ///
    /// RAI.tex has no account-slot timer and no account timeout votes at
    /// all: an unresolved slot is resolved by the epoch decision instead.
    /// The rule can not go until that decision exists, because the close
    /// this node still runs waits for every instance of the epoch to
    /// terminate before it proposes - removing it measured as close rounds
    /// timing out and the epoch states diverging (see the tex-step1 record).
    pub fn note_missing_candidate(
        &mut self,
        id: &ElectionId,
        hash: BlockHash,
        tally: Amount,
        now: Timestamp,
    ) {
        if let Some(election) = self.roots.election_mut(id) {
            election.note_missing_candidate(hash, tally, now);
        }
    }

    /// RAI: the epoch new elections are started in
    pub fn current_epoch(&self) -> ConsensusEpoch {
        self.current_epoch
    }

    pub fn set_current_epoch(&mut self, epoch: ConsensusEpoch) {
        self.current_epoch = epoch;
    }

    /// Kudzu: the votes to broadcast now for all elections, in round robin
    /// order. `proposal_valid` tells whether a block may be first voted
    /// (its dependencies are finalized).
    pub fn kudzu_votes_due(&self, proposal_valid: impl Fn(&BlockHash) -> bool) -> Vec<VoteTarget> {
        let mut targets = self.pending_kudzu_votes.clone();
        let empty = LocalSlotState::default();
        for election in self.roots.round_robin().map(|e| &e.election) {
            let slot = self.slots.get(&election.epoch_slot()).unwrap_or(&empty);
            // A settled instance has nothing left to say but its exit final
            // vote: the settled instances of the forks pile up over time and
            // this loop runs every tick under the AEC lock
            let due = if election.state() == ElectionState::Settled {
                election.kudzu_final_vote_due(slot).into_iter().collect()
            } else {
                election.kudzu_votes_due(slot, &proposal_valid)
            };
            targets.extend(due.into_iter().map(|(hash, kind)| VoteTarget {
                election: election.id(),
                winner: hash,
                vote_type: VoteType::from(kind),
            }));
        }
        for close in self.closes.values() {
            targets.extend(close.votes_due(&self.local_reps).into_iter().map(
                |(round, value, kind)| VoteTarget {
                    election: close.id(round),
                    winner: value,
                    vote_type: VoteType::from(kind),
                },
            ));
        }
        targets
    }

    /// Kudzu: record the votes that are about to be handed to the vote
    /// generators and return those to generate. RAI: a first vote decided on
    /// before this node left the instance's epoch is dropped, the node
    /// abstains there instead.
    pub fn mark_kudzu_voted(&mut self, targets: Vec<VoteTarget>) -> Vec<VoteTarget> {
        let mut accepted = Vec::with_capacity(targets.len());
        for target in targets {
            self.pending_kudzu_votes
                .retain(|pending| *pending != target);
            if let Some((epoch, round)) = target.election.epoch.as_close_round() {
                if let Some(close) = self.closes.get_mut(&epoch) {
                    close.mark_voted(round, target.winner, VoteKind::from(target.vote_type));
                    accepted.push(target);
                }
                continue;
            }
            let Some(election) = self.roots.election(&target.election) else {
                // The exit final vote of an election erased already
                accepted.push(target);
                continue;
            };
            let slot = self.slots.get_or_default(&election.epoch_slot());
            let kind = VoteKind::from(target.vote_type);
            // A first vote not cast before this node left the epoch is not cast
            // any more (it abstains instead); one cast before is re-broadcast
            if kind == VoteKind::First && slot.stale && slot.first_voted.is_none() {
                continue;
            }
            slot.mark_voted(target.winner, kind);
            accepted.push(target);
        }
        accepted
    }

    /// Kudzu: an election is erased as soon as it is finalized. Its exit final
    /// vote is kept so that the voter still broadcasts it.
    fn keep_exit_final_vote(&mut self, election: &Election) {
        // Only explicitly finalized elections: an implicitly finalized block is
        // already cemented, and its slot state has been dropped.
        if !cfg!(feature = "rai_protocol") || !election.certificates().is_finalized() {
            return;
        }
        let slot = self.slots.get_or_default(&election.epoch_slot());
        if let Some((hash, kind)) = election.kudzu_final_vote_due(slot) {
            slot.mark_voted(hash, kind);
            self.pending_kudzu_votes.push(VoteTarget {
                election: election.id(),
                winner: hash,
                vote_type: VoteType::from(kind),
            });
        }
    }

    /// Kudzu: the signed votes behind the certificates of the election of this
    /// block in the given epoch, if the election is terminated. RAI: the
    /// node's statements in an instance it finalized stay available after
    /// the election is gone.
    pub fn certificate_evidence(
        &self,
        hash: &BlockHash,
        epoch: ConsensusEpoch,
    ) -> Option<(ElectionId, CertificateEvidence)> {
        if let Some((epoch, round)) = epoch.as_close_round() {
            let close = self.closes.get(&epoch)?;
            return Some((close.id(round), close.certificate_evidence(round)?));
        }
        if let Some(election) = self.roots.election_for_block_in_epoch(hash, epoch) {
            let empty = LocalSlotState::default();
            let slot = self.slots.get(&election.epoch_slot()).unwrap_or(&empty);
            let evidence = election.certificate_evidence(slot)?;
            return Some((election.id(), evidence));
        }
        let instance = self.epoch_states.instance(hash, epoch)?;
        let statements = instance.statements();
        if statements.is_empty() {
            return None;
        }
        Some((
            ElectionId::new(instance.root.clone(), epoch),
            CertificateEvidence {
                statements,
                blocks: Vec::new(),
            },
        ))
    }

    /// Kudzu: is the election terminated and out of the priority buckets
    pub fn is_terminated(&self, id: &ElectionId) -> bool {
        self.roots.is_terminated(id)
    }

    pub fn slot_state(&self, slot: &EpochSlot) -> Option<&LocalSlotState> {
        self.slots.get(slot)
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    pub fn set_observer(&mut self, observer: Sender<AecFact>) {
        self.observer = Some(observer);
    }

    pub fn max_len(&self) -> usize {
        self.max_elections
    }

    pub fn count_by_behavior(&self, behavior: ElectionBehavior) -> usize {
        self.count_by_behavior[behavior as usize]
    }

    fn count_by_behavior_mut(&mut self, behavior: ElectionBehavior) -> &mut usize {
        &mut self.count_by_behavior[behavior as usize]
    }

    pub fn bucket_len(&self, bucket_id: usize) -> usize {
        self.roots.bucket_len(bucket_id)
    }

    pub fn find_bucket(&self, id: &ElectionId) -> Option<usize> {
        self.roots.find_bucket(id)
    }

    pub fn lowest_priority(&self, bucket_id: usize) -> Option<(ElectionId, TimePriority)> {
        self.roots.lowest_priority(bucket_id)
    }

    /// Iterates over all elections in round robin fashion starting at the highest bucket
    pub fn iter_round_robin(&self) -> impl Iterator<Item = &Election> {
        self.roots.round_robin().map(|i| &i.election)
    }

    /// Whether the source has a candidate `refill` would take. False while
    /// the container cools down, while the ended epoch drains and while the
    /// container is at its cap: `refill` would insert nothing then, and the
    /// scheduler would call it again at once, without end. The scheduler is
    /// woken when the cooldown is over (`AecFact::Recovered`) and on every
    /// block enqueued
    pub fn check_vacancy<T>(&self, source: &T) -> bool
    where
        T: ElectionCandidateSource,
    {
        if self.cooldown.is_cooling_down()
            || self.draining
            || self.roots.active_len() >= self.max_elections
        {
            return false;
        }
        let bucket_infos = self.roots.bucket_infos();
        source.should_schedule(&bucket_infos)
    }

    pub fn insert(
        &mut self,
        request: AecInsertRequest,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.ensure_not_stopped()?;
        self.ensure_not_recently_confirmed(&request)?;

        if self.try_upgrade_priority_election(&request)? {
            return Ok(());
        }
        if self.has_earlier_instance(&request.block.hash()) {
            return Err(AecInsertError::Duplicate);
        }
        // RAI: the ended epoch drains, the block waits for the next epoch
        if self.draining {
            return Err(AecInsertError::Draining);
        }

        self.insert_new_election(request, now);
        Ok(())
    }

    fn ensure_not_stopped(&self) -> Result<(), AecInsertError> {
        if self.stopped {
            Err(AecInsertError::Stopped)
        } else {
            Ok(())
        }
    }

    fn ensure_not_recently_confirmed(
        &self,
        request: &AecInsertRequest,
    ) -> Result<(), AecInsertError> {
        let root = request.block.qualified_root();

        if self.recently_confirmed.root_exists(&root) {
            return Err(AecInsertError::RecentlyConfirmed);
        }
        Ok(())
    }

    fn try_upgrade_priority_election(
        &mut self,
        request: &AecInsertRequest,
    ) -> Result<bool, AecInsertError> {
        let (upgraded, previous_behavior) = self
            .roots
            .try_upgrade_to_priority_election(request, self.current_epoch);

        if upgraded {
            *self.count_by_behavior_mut(previous_behavior.unwrap()) -= 1;
            *self.count_by_behavior_mut(request.behavior) += 1;
            Ok(true)
        } else if previous_behavior.is_some() {
            Err(AecInsertError::Duplicate)
        } else {
            Ok(false)
        }
    }

    /// A new block always starts an election in the current epoch
    fn insert_new_election(&mut self, request: AecInsertRequest, now: Timestamp) {
        self.insert_new_election_in_epoch(request, self.current_epoch, now);
    }

    fn insert_new_election_in_epoch(
        &mut self,
        request: AecInsertRequest,
        epoch: ConsensusEpoch,
        now: Timestamp,
    ) {
        let root = request.block.qualified_root();
        let hash = request.block.hash();
        if epoch == self.current_epoch && self.epoch_ends_at.is_none() {
            self.epoch_ends_at = self.next_boundary(now);
        }
        let election = Election::new(
            request.block,
            epoch,
            request.behavior,
            self.base_latency,
            now,
        );

        self.roots.insert(Entry {
            id: election.id(),
            election,
            priority: request.priority,
        });

        *self.count_by_behavior_mut(request.behavior) += 1;
        self.stats.started(request.behavior);
        self.notify(AecFact::ElectionStarted(hash, root));
    }

    /// A fork block joins the elections of its root in every epoch: each
    /// instance decides between the same candidates
    pub fn try_add_fork(&mut self, fork: &Block, fork_tally: Amount) -> bool {
        let root = fork.qualified_root();
        let ids: Vec<_> = self
            .roots
            .elections_for_root(&root)
            .map(|e| e.id())
            .collect();
        let mut any_added = false;
        for id in ids {
            any_added |= self.try_add_fork_to(&id, fork, fork_tally);
        }
        any_added
    }

    fn try_add_fork_to(&mut self, id: &ElectionId, fork: &Block, fork_tally: Amount) -> bool {
        let Some(entry) = self.roots.get_mut(id) else {
            return false;
        };

        let result = entry.election.try_add_fork(fork, fork_tally);
        let added = match result {
            AddForkResult::Added => {
                self.notify(AecFact::BlockAddedToElection(fork.hash()));
                true
            }
            AddForkResult::Replaced(removed) => {
                self.roots.vote_router.disconnect(&removed.hash(), id.epoch);
                self.notify(AecFact::BlockDiscarded(removed.into()));
                self.notify(AecFact::BlockAddedToElection(fork.hash()));
                true
            }
            AddForkResult::TallyTooLow => {
                self.notify(AecFact::BlockDiscarded(fork.clone()));
                false
            }
            AddForkResult::Duplicate | AddForkResult::ElectionEnded => false,
        };

        if added {
            self.roots.vote_router.connect(fork.hash(), id.clone());
            self.stats.conflicts += 1;
        }

        added
    }

    /// How many election slots are available
    /// This is a soft limit and can be negative!
    pub fn vacancy(&self) -> i64 {
        if self.cooldown.is_cooling_down() || self.draining {
            return 0;
        }
        let current_size = self.roots.active_len() as i64;
        self.max_elections as i64 - current_size
    }

    pub fn set_cooldown(&mut self, cool_down: bool, reason: AecCooldownReason) {
        let result = self.cooldown.set_cooldown(cool_down, reason);
        if result == CooldownResult::Recovered {
            self.notify(AecFact::Recovered);
        }
    }

    pub fn stop(&mut self) {
        // destroy send queue so that the receiver thread will be stopped too
        drop(self.observer.take());
        self.stopped = true;
        self.roots.clear();
    }

    /// Whether the root has an election in any epoch
    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.roots.elections_for_root(root).next().is_some()
    }

    /// Whether the block is a candidate of an election of the current epoch.
    /// RAI: or of an instance of an earlier epoch, which keeps the schedulers
    /// from proposing the block again.
    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.roots
            .vote_router
            .election_id(block_hash, self.current_epoch)
            .is_some()
            || self.has_earlier_instance(block_hash)
    }

    /// RAI: whether the block has an instance of an earlier epoch. A block is
    /// proposed once: an instance that still runs decides it within the epoch
    /// it was proposed in, and one that ended undecided leaves it undecided.
    /// Only a vote of another replica opens an instance of a later epoch.
    pub fn has_earlier_instance(&self, block_hash: &BlockHash) -> bool {
        self.roots
            .vote_router
            .elections_of(block_hash)
            .any(|id| id.epoch < self.current_epoch)
    }

    /// Whether the block has an election that a priority activation could not
    /// upgrade any further, see `Election::maybe_upgrade_to`
    pub fn is_priority_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.roots
            .election_for_block_in_epoch(block_hash, self.current_epoch)
            .is_some_and(|e| {
                matches!(
                    e.behavior(),
                    ElectionBehavior::Priority | ElectionBehavior::Manual
                )
            })
            || self.has_earlier_instance(block_hash)
    }

    pub fn was_recently_confirmed(&self, block_hash: &BlockHash) -> bool {
        self.recently_confirmed.hash_exists(block_hash)
    }

    pub fn clear_recently_confirmed(&mut self) {
        self.recently_confirmed.clear();
    }

    /// Returns the current active elections after transitioning
    pub fn transition_time(&mut self, now: Timestamp) {
        self.stats.ticked += 1;
        for entry in self.roots.iter_mut() {
            entry.election.transition_time(now);
        }
        self.erase_ended_elections();
        self.end_epoch_by_time(now);
        self.try_advance_epoch(now);
        self.tick_closes(now);
    }

    pub fn election(&self, id: &ElectionId) -> Option<&Election> {
        self.roots.election(id)
    }

    /// The election of the newest epoch for this root
    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<&Election> {
        self.roots.latest_election_for_root(root)
    }

    /// The elections of all epochs for this root, ascending by epoch
    pub fn elections_for_root(&self, root: &QualifiedRoot) -> impl Iterator<Item = &Election> {
        self.roots.elections_for_root(root)
    }

    /// The election of the newest epoch this block is a candidate in
    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<&Election> {
        self.roots.election_for_block(block_hash)
    }

    /// Activates the elections of every epoch this block is a candidate in
    pub fn transition_active(&mut self, block_hash: &BlockHash) -> bool {
        let Some(root) = self
            .roots
            .election_for_block(block_hash)
            .map(|e| e.qualified_root().clone())
        else {
            return false;
        };
        for election in self.roots.elections_for_root_mut(&root) {
            if election.candidate_blocks().contains_key(block_hash) {
                election.transition_active();
            }
        }
        true
    }

    pub fn refill<T>(&mut self, source: &mut T, now: Timestamp)
    where
        T: ElectionCandidateSource,
    {
        if self.cooldown.is_cooling_down() || self.draining {
            return;
        }

        let mut any_inserted = true;
        while any_inserted {
            any_inserted = false;
            for bucket_index in (0..self.roots.bucket_count()).rev() {
                let bucket = &self.roots.bucket_infos()[bucket_index];
                let bucket_vacancy = if self.roots.active_len() >= self.max_elections {
                    0
                } else {
                    self.max_elections_per_bucket as isize - bucket.election_count as isize
                };

                let Some(candidate) = source.next_candidate(
                    bucket_index,
                    bucket_vacancy,
                    bucket.lowest_priority.time,
                ) else {
                    continue;
                };

                any_inserted = true;
                let id = ElectionId::new(candidate.block.qualified_root(), self.current_epoch);
                if self.find_bucket(&id) == Some(candidate.bucket_id) {
                    self.stats.activate_failed_duplicate += 1;
                    continue;
                }

                if self.bucket_len(candidate.bucket_id) >= self.max_elections_per_bucket {
                    if self.erase_lowest_prio_election(candidate.bucket_id) {
                        self.stats.replaced += 1;
                    } else {
                        self.stats.over_capacity += 1;
                    }
                }

                // TODO: Don't hard code priority election!
                match self.insert(
                    AecInsertRequest::new_priority(candidate.block, candidate.priority),
                    now,
                ) {
                    Ok(_) => {
                        self.stats.activate_success += 1;
                    }
                    Err(AecInsertError::RecentlyConfirmed) => {
                        self.stats.activate_failed_confirmed += 1;
                    }
                    Err(AecInsertError::Duplicate) => {
                        self.stats.activate_failed_duplicate += 1;
                    }
                    Err(AecInsertError::Stopped | AecInsertError::Draining) => {}
                }
            }
        }
    }

    pub fn remove_votes<'a>(
        &mut self,
        root: &QualifiedRoot,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        let Some(election) = self.roots.latest_election_for_root_mut(root) else {
            return;
        };
        for voter in voters {
            election.remove_vote(voter);
        }
    }

    pub fn erase_ended_elections(&mut self) {
        let removed = self.roots.drain_filter(|i| i.election.state().has_ended());

        for entry in removed {
            self.cleanup_election(entry);
        }
    }

    /// Erase the elections of all epochs of this root
    pub fn erase(&mut self, root: &QualifiedRoot) -> bool {
        let erased = self.roots.erase_root(root);
        let any = !erased.is_empty();
        for entry in erased {
            self.cleanup_election(entry);
        }
        any
    }

    pub fn erase_election(&mut self, id: &ElectionId) -> bool {
        let Some(entry) = self.roots.erase(id) else {
            return false;
        };
        self.cleanup_election(entry);
        true
    }

    /// Returns false if nothing could be evicted
    pub fn erase_lowest_prio_election(&mut self, bucket_id: usize) -> bool {
        // Kudzu: an election leaves the AEC only when it is finalized. Votes are
        // one-shot, so an evicted election would lose evidence, and the backlog scan
        // brings an evicted block back only seconds later. Candidates wait in the
        // scheduler instead, see `Bucket::available`.
        if cfg!(feature = "rai_protocol") {
            return false;
        }
        let Some((id, _)) = self.lowest_priority(bucket_id) else {
            return false;
        };
        if let Some(election) = self.roots.election(&id) {
            self.stats.evicted(election);
        }
        self.erase_election(&id)
    }

    fn cleanup_election(&mut self, entry: Entry) {
        let election = &entry.election;
        self.keep_exit_final_vote(election);
        // A late instance's blocks are discarded - never one finalized in
        // another epoch: what an epoch finalized stays finalized
        let discarded: Vec<BlockHash> = if is_late(&self.agreed, election) {
            election
                .certificates()
                .notar
                .iter()
                .filter(|block| !self.epoch_states.is_finalized(block))
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        if is_late(&self.agreed, election) && discarded.len() < election.certificates().notar.len()
        {
            self.stats.finalized_kept +=
                (election.certificates().notar.len() - discarded.len()) as u64;
        }
        if !discarded.is_empty() {
            self.stats.discarded_instances += 1;
            self.notify(AecFact::LateBlocksDiscarded {
                epoch: election.epoch(),
                hashes: discarded,
            });
            // The blocks are rolled back: the root's instances of the other
            // epochs (the scheduler proposes a block in the current epoch)
            // would keep candidates no replica holds
            self.erase(election.qualified_root());
        } else if let Some(finalized) = election.certificates().finalized() {
            let slot = self
                .slots
                .get(&election.epoch_slot())
                .cloned()
                .unwrap_or_default();
            self.epoch_states.record_finalized(FinalizedInstance {
                root: election.qualified_root().clone(),
                account: election.account(),
                height: election.height(),
                epoch: election.epoch(),
                winner: finalized,
                candidates: election.candidate_blocks().keys().copied().collect(),
                delegation: delegations(election)
                    .into_iter()
                    .find(|delegation| delegation.hash == finalized),
                slot,
            });
        }
        // RAI: the slot state outlives the election. This node's statements
        // in the epoch's instance are one-shot; an instance for the slot may
        // be started again in that epoch (a vote for a block republished after
        // a discard, a rolled back dependent), and it must find them. Once the
        // epoch is agreed no instance of it is started again, and the state
        // goes with the election.
        if !cfg!(feature = "rai_protocol") || self.agreed.contains_key(&election.epoch()) {
            self.slots.remove(&election.epoch_slot());
        }

        // Keep track of election count by election type
        *self.count_by_behavior_mut(election.behavior()) -= 1;

        self.stats.stopped(&entry.election);
        self.notify(AecFact::ElectionEnded(entry.election));
    }

    /// Dependent elections are implicitly confirmed when their block is confirmed
    pub fn confirm_dependent_elections(
        &mut self,
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>)>,
        now: Timestamp,
    ) {
        for (confirmed_block, source_election) in confirmed {
            let confirmed_election =
                self.confirm_dependent_election(&confirmed_block, source_election, now);

            self.block_confirmed(confirmed_block, confirmed_election);
        }
    }

    fn confirm_dependent_election(
        &mut self,
        confirmed_block: &SavedBlock,
        source_election: Option<ConfirmedElection>,
        now: Timestamp,
    ) -> ConfirmedElection {
        // Check if the currently confirmed block was part of an election that triggered
        // the block confirmation
        if let Some(source) = source_election
            && confirmed_block.hash() == source.winner.hash()
        {
            // This is the block that was directly confirmed by the source election.
            // The election is already confirmed, so there is nothing to do.
            return source;
        }

        let root = confirmed_block.qualified_root();
        let Some(corresponding) = self.roots.latest_election_for_root_mut(&root) else {
            return ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::InactiveConfirmationHeight,
            );
        };

        // RAI: the block is cemented, but this replica's instances have not
        // reached their outcome yet. They keep running and collect the
        // certificates of the other replicas, so that every replica records the
        // same outcome per epoch.
        if cfg!(feature = "rai_protocol") {
            return ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::ActiveConfirmationHeight,
            );
        }

        let result = if corresponding.winner().hash() == confirmed_block.hash() {
            corresponding.force_confirm();
            corresponding.into_confirmed_election(now, ConfirmationType::ActiveConfirmationHeight)
        } else {
            corresponding.cancel();
            ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::ActiveConfirmationHeight,
            )
        };
        // The height is decided, the elections of the other epochs are over too
        let latest = corresponding.epoch();
        for election in self.roots.elections_for_root_mut(&root) {
            if election.epoch() != latest {
                election.cancel();
            }
        }
        result
    }

    fn block_confirmed(&mut self, block: SavedBlock, election: ConfirmedElection) {
        self.stats.block_confirmations[election.confirmation_type as usize] += 1;
        // The height is finalized, nothing will be voted for it any more.
        // RAI: the instances of the height keep running until they reach their
        // outcome, their slot states go with them (`cleanup_election`).
        if !cfg!(feature = "rai_protocol") {
            self.slots.remove_slot(block.account(), block.height());
        }
        self.notify(AecFact::BlockConfirmed(block, election));
    }

    pub fn remove_recently_confirmed(&mut self, block_hash: &BlockHash) {
        self.recently_confirmed.erase(block_hash);
    }

    pub fn apply_vote<'a>(
        &mut self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        // The epoch's end is due at the same instant on every replica, and
        // checked on every vote, not only on the ticks
        self.end_epoch_by_time(args.now);
        if let Some((epoch, round)) = args.vote.epoch.as_close_round() {
            // Following the voter into its epoch first opens the close here
            self.observe_epoch(&args);
            return self.apply_close_vote(&args, epoch, round);
        }
        if args.vote.epoch >= self.current_epoch {
            self.epoch_voters
                .entry(args.vote.epoch)
                .or_default()
                .insert(args.vote.voter);
        }
        let mut apply_helper = ApplyVoteHelper {
            args: &args,
            recently_confirmed: &mut self.recently_confirmed,
            stats: &mut self.stats,
            observer: &self.observer,
            roots: &mut self.roots,
            committees: &self.committees,
            agreed: &self.agreed,
        };
        let result = apply_helper.apply_vote();
        for entry in result.confirmed {
            self.cleanup_election(entry);
        }
        self.count_decided(&result.decided, args.now);
        self.observe_epoch(&args);
        let mut per_block = result.per_block;
        // RAI: a vote of an epoch this node has not reached yet is not late, it
        // waits in the vote cache until this node gets there, also for a block
        // which was decided here in an earlier epoch
        if args.vote.epoch > self.current_epoch {
            for result in per_block.values_mut() {
                if matches!(result, Err(VoteError::Late)) {
                    *result = Err(VoteError::Indeterminate);
                }
            }
        }
        // RAI: a vote of an agreed epoch for a block without an instance
        // there is late: the epoch is decided, no instance of it is started
        if self.agreed.contains_key(&args.vote.epoch) {
            for result in per_block.values_mut() {
                if matches!(result, Err(VoteError::Indeterminate)) {
                    *result = Err(VoteError::Late);
                }
            }
        }
        per_block
    }

    pub fn force_confirm(&mut self, block_hash: &BlockHash, now: Timestamp) {
        let Some(election) = self.roots.election_for_block_mut(block_hash) else {
            panic!("Force confirm failed, because no active election was found");
        };
        if election.force_confirm() {
            let confirmed_election =
                election.into_confirmed_election(now, ConfirmationType::ActiveConfirmedQuorum);
            self.notify(AecFact::ElectionConfirmed(confirmed_election));
        }
    }

    pub fn cancel(&mut self, root: &QualifiedRoot) {
        for election in self.roots.elections_for_root_mut(root) {
            election.cancel();
        }
    }

    pub fn cancel_all(&mut self) {
        for entry in self.roots.iter_mut() {
            entry.election.cancel();
        }
    }

    pub fn len(&self) -> usize {
        self.roots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn info(&self, now: Timestamp) -> ActiveElectionsInfo {
        ActiveElectionsInfo {
            max_elections: self.max_elections,
            total: self.roots.len(),
            stale: self
                .roots
                .iter()
                .filter(|i| i.election.start().elapsed(now) >= Duration::from_secs(60))
                .count(),
            priority: self.count_by_behavior(ElectionBehavior::Priority),
            hinted: self.count_by_behavior(ElectionBehavior::Hinted),
            optimistic: self.count_by_behavior(ElectionBehavior::Optimistic),
        }
    }

    pub fn simulate_event(&self, event: AecFact) {
        self.notify(event);
    }

    pub fn snapshot(&self, now: Timestamp) -> AecSnapshot {
        self.roots.snapshot(now)
    }

    fn notify(&self, event: AecFact) {
        if let Some(sender) = &self.observer {
            sender.send(event).unwrap()
        }
    }
}

impl Default for ActiveElectionsContainer {
    fn default() -> Self {
        Self::new(ActiveElectionsConfig::default(), Duration::from_secs(1))
    }
}

impl StatsSource for ActiveElectionsContainer {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.cooldown.collect_stats(result);
        self.stats.collect_stats(result);
    }
}

impl ContainerInfoProvider for ActiveElectionsContainer {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .leaf("roots", self.roots.len(), RootContainer::ELEMENT_SIZE)
            .leaf("terminated", self.roots.terminated_len(), 0)
            .leaf(
                "slots",
                self.slot_count(),
                size_of::<(EpochSlot, LocalSlotState)>(),
            )
            .leaf(
                "normal",
                self.count_by_behavior(ElectionBehavior::Priority),
                0,
            )
            .leaf(
                "hinted".to_string(),
                self.count_by_behavior(ElectionBehavior::Hinted),
                0,
            )
            .leaf(
                "optimistic".to_string(),
                self.count_by_behavior(ElectionBehavior::Optimistic),
                0,
            )
            .node(
                "recently_confirmed",
                self.recently_confirmed.container_info(),
            )
            .leaf(
                "finalized_by_epoch",
                self.epoch_states.len(),
                size_of::<(BlockHash, ConsensusEpoch)>(),
            )
            .leaf("epoch_closes", self.closes.len(), size_of::<EpochClose>())
            .node("vote_router", self.roots.vote_router.container_info())
            .node("buckets", self.roots.container_info())
            .finish()
    }
}

pub struct ApplyVoteArgs<'a> {
    pub vote: &'a FilteredVote,
    /// The ledger's live weights: the vote cooldown, and the committee of
    /// a run without epochs
    pub rep_weights: &'a RepWeights,
    pub quorum_snapshot: &'a QuorumSnapshot,
    pub now: Timestamp,
}

/// RAI: the content behind an epoch's value as this node attests it
struct EpochContent {
    value: BlockHash,
    /// Every account at its greatest height with a single block in the state
    frontiers: Vec<AccountFrontier>,
    /// Every (account, height, block) the value hashes
    entries: HashSet<(Account, u64, BlockHash)>,
}

/// Whether a kept content is one of the values of this tick
fn kept_value(content: &EpochContent, values: &[(BlockHash, Option<Timestamp>); 2]) -> bool {
    values.iter().any(|(value, _)| *value == content.value)
}

/// RAI: what each notarized block of an election delegates, read from the
/// candidate the election holds. A legacy block without a balance or
/// representative field delegates nothing.
fn delegations(election: &Election) -> Vec<Delegation> {
    election
        .certificates()
        .notar
        .iter()
        .filter_map(|hash| {
            let block = election.candidate_blocks().get(hash)?;
            Some(Delegation {
                hash: *hash,
                representative: block.representative_field()?,
                balance: block.balance_field()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{
        ReceivedVote,
        active_elections::{BucketInfo, ElectionCandidate},
    };
    use rsnano_types::{PrivateKey, TimePriority, Vote, VoteDelivery};
    use std::sync::Arc;

    #[test]
    fn empty() {
        let container = ActiveElectionsContainer::default();
        assert_eq!(container.len(), 0);
        assert!(!container.is_active_root(&QualifiedRoot::new_test_instance()));
        assert!(!container.is_active_hash(&BlockHash::from(1)));
    }

    #[test]
    fn insert_election() {
        let mut container = ActiveElectionsContainer::default();
        let request = AecInsertRequest {
            block: SavedBlock::new_test_instance(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        container
            .insert(request, Timestamp::new_test_instance())
            .unwrap();

        assert_eq!(container.len(), 1);
    }

    /// The scheduler asks before every refill: at the cap there is no
    /// vacancy, whatever the source holds
    #[test]
    fn no_vacancy_at_the_cap() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                max_elections: 2,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let now = Timestamp::new_test_instance();
        assert!(container.check_vacancy(&AlwaysAvailable));
        for key in 1..=2 {
            container
                .insert(
                    AecInsertRequest::new_priority(
                        SavedBlock::new_test_instance_with_key(key),
                        BlockPriority::new_test_instance(),
                    ),
                    now,
                )
                .unwrap();
        }
        assert!(!container.check_vacancy(&AlwaysAvailable));
    }

    /// Nor while the container cools down: `refill` inserts nothing then
    #[test]
    fn no_vacancy_while_cooling_down() {
        let mut container = ActiveElectionsContainer::default();
        assert!(container.check_vacancy(&AlwaysAvailable));
        container.set_cooldown(true, AecCooldownReason::AecFactQueueFull);
        assert!(!container.check_vacancy(&AlwaysAvailable));
        container.set_cooldown(false, AecCooldownReason::AecFactQueueFull);
        assert!(container.check_vacancy(&AlwaysAvailable));
    }

    #[test]
    fn confirm_election() {
        let mut container = ActiveElectionsContainer::default();

        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();

        let request = AecInsertRequest {
            block,
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        let now = Timestamp::new_test_instance();
        container.insert(request, now).unwrap();

        let rep_key = PrivateKey::from(1);
        let received_vote = test_final_vote(&rep_key, block_hash);

        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);

        let result = container.apply_vote(ApplyVoteArgs {
            vote: &received_vote.into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        assert_eq!(result.get(&block_hash), Some(&Ok(())));

        assert!(container.election_for_block(&block_hash).is_none());
    }

    /// RAI: the same root is contested once per epoch. A block starts its
    /// election in the current epoch, and a vote only counts in the election
    /// of its own epoch. This node proposes a block once: with an instance of
    /// an earlier epoch, running or ended undecided, the block is not proposed
    /// again; only a vote of another replica opens the instance of a later epoch.
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn one_election_per_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();
        let root = block.qualified_root();
        let now = Timestamp::new_test_instance();
        let epoch1 = ConsensusEpoch::new(1);
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);

        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        container.set_current_epoch(epoch1);
        assert_eq!(
            container.insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            ),
            Err(AecInsertError::Duplicate)
        );
        assert!(container.is_active_hash(&block_hash));
        assert!(container.is_priority_active_hash(&block_hash));
        assert_eq!(container.len(), 1);

        // The epoch 0 instance times out: undecided, and still not proposed again
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::Abstain,
            ConsensusEpoch::ZERO,
            vec![block_hash],
        ));
        container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert!(container.is_active_hash(&block_hash));
        assert_eq!(
            container.insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            ),
            Err(AecInsertError::Duplicate)
        );
        assert_eq!(container.len(), 1);
        // A vote of epoch 1 opens that instance
        container.insert_for_vote(block, epoch1, now);

        assert_eq!(container.len(), 2);
        assert!(container.is_active_root(&root));
        let epochs: Vec<_> = container
            .elections_for_root(&root)
            .map(|e| e.epoch())
            .collect();
        assert_eq!(epochs, vec![ConsensusEpoch::ZERO, epoch1]);
        assert_eq!(
            container.election_for_block(&block_hash).unwrap().epoch(),
            epoch1
        );
        assert_eq!(
            container
                .election(&ElectionId::legacy(root.clone()))
                .unwrap()
                .epoch(),
            ConsensusEpoch::ZERO
        );

        // A vote of an epoch without an election counts nowhere
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::Final,
            ConsensusEpoch::new(2),
            vec![block_hash],
        ));
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(
            result.get(&block_hash),
            Some(&Err(VoteError::Indeterminate))
        );
        assert_eq!(container.len(), 2);

        // A final vote in epoch 1 confirms the epoch 1 election only
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::Final,
            epoch1,
            vec![block_hash],
        ));
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(result.get(&block_hash), Some(&Ok(())));
        assert_eq!(container.len(), 1);
        assert_eq!(
            container.election_for_block(&block_hash).unwrap().epoch(),
            ConsensusEpoch::ZERO
        );

        // Erasing by root erases every epoch
        assert!(container.erase(&root));
        assert_eq!(container.len(), 0);
        assert!(!container.is_active_hash(&block_hash));
    }

    /// The ended epoch drains: a vote of the epoch for a block without an
    /// instance opens one, but this node does not propose in it any more, it
    /// abstains - the block is for the next epoch
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn does_not_propose_in_an_instance_opened_for_a_vote_while_draining() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let decided = SavedBlock::new_test_instance_with_key(1);
        let undecided = SavedBlock::new_test_instance_with_key(2);
        let fresh = SavedBlock::new_test_instance_with_key(3);
        let now = Timestamp::new_test_instance();
        container.start_epochs(now);
        for block in [&decided, &undecided] {
            container
                .insert(
                    AecInsertRequest::new_priority(
                        block.clone(),
                        BlockPriority::new_test_instance(),
                    ),
                    now,
                )
                .unwrap();
        }
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        container.apply_vote(ApplyVoteArgs {
            vote: &test_final_vote(&rep_key, decided.hash()).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        container.transition_time(now);
        assert!(container.is_draining());
        assert_eq!(container.current_epoch(), ConsensusEpoch::ZERO);

        container.insert_for_vote(fresh.clone(), ConsensusEpoch::ZERO, now);
        assert_eq!(container.stats.stale_started, 1);
        assert_eq!(container.stats.started_for_vote, 0);
        let due = container.kudzu_votes_due(|_| true);
        assert!(due.contains(&VoteTarget {
            election: ElectionId::legacy(fresh.qualified_root()),
            winner: fresh.hash(),
            vote_type: VoteType::Abstain,
        }));
        assert!(!due.contains(&VoteTarget {
            election: ElectionId::legacy(fresh.qualified_root()),
            winner: fresh.hash(),
            vote_type: VoteType::NonFinal,
        }));
    }

    /// left undecided
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn keeps_an_undecided_block_undecided_without_reproposing() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();
        let now = Timestamp::new_test_instance();
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);

        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::Abstain,
            ConsensusEpoch::ZERO,
            vec![block_hash],
        ));
        container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        container.set_current_epoch(ConsensusEpoch::new(1));

        assert!(container.is_active_hash(&block_hash));
        assert_eq!(
            container.insert(
                AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                now,
            ),
            Err(AecInsertError::Duplicate)
        );
        assert_eq!(container.len(), 1);
    }
    /// RAI: after enough decided elections the epoch ends: no new election
    /// starts in it, and it is left once its instances have settled. The
    /// finalized blocks are recorded per epoch.
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epoch_ends_after_enough_decided_elections_and_is_left_once_drained() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let decided = SavedBlock::new_test_instance_with_key(1);
        let undecided = SavedBlock::new_test_instance_with_key(2);
        let now = Timestamp::new_test_instance();
        container.start_epochs(now);
        for block in [&decided, &undecided] {
            container
                .insert(
                    AecInsertRequest::new_priority(
                        block.clone(),
                        BlockPriority::new_test_instance(),
                    ),
                    now,
                )
                .unwrap();
        }
        assert_eq!(container.current_epoch(), ConsensusEpoch::ZERO);
        assert!(!container.is_draining());

        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        let vote = test_final_vote(&rep_key, decided.hash());
        container.apply_vote(ApplyVoteArgs {
            vote: &vote.into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        // The epoch has ended but the undecided election keeps it from being left
        assert!(container.is_draining());
        assert_eq!(container.current_epoch(), ConsensusEpoch::ZERO);
        assert_eq!(container.vacancy(), 0);
        assert_eq!(
            container.insert(
                AecInsertRequest::new_priority(
                    SavedBlock::new_test_instance_with_key(3),
                    BlockPriority::new_test_instance()
                ),
                now,
            ),
            Err(AecInsertError::Draining)
        );
        assert!(container.finalized_in_epoch(&decided.hash(), ConsensusEpoch::ZERO));
        assert_eq!(
            container.finalized_by_epoch()[&ConsensusEpoch::ZERO].entries(),
            1
        );
        assert!(container.election_for_block(&decided.hash()).is_none());
        // This node keeps voting in the running instance
        let due = container.kudzu_votes_due(|_| true);
        assert!(due.contains(&VoteTarget {
            election: ElectionId::legacy(undecided.qualified_root()),
            winner: undecided.hash(),
            vote_type: VoteType::NonFinal,
        }));
        container.transition_time(now);
        assert_eq!(container.current_epoch(), ConsensusEpoch::ZERO);

        // Settled: the epoch is left, new elections start in the next one
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::Abstain,
            ConsensusEpoch::ZERO,
            vec![undecided.hash()],
        ));
        container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        container.transition_time(now);
        let epoch1 = ConsensusEpoch::new(1);
        assert_eq!(container.current_epoch(), epoch1);
        assert!(!container.is_draining());
        assert_eq!(container.decided_in_current_epoch(), 0);
        assert!(!container.finalized_in_epoch(&decided.hash(), epoch1));
        assert!(container.vacancy() > 0);
        container
            .insert(
                AecInsertRequest::new_priority(
                    SavedBlock::new_test_instance_with_key(3),
                    BlockPriority::new_test_instance(),
                ),
                now,
            )
            .unwrap();
        assert_eq!(container.len(), 2);
    }

    /// RAI: epoch 0 starts when the setup is over and the epochs end at the
    /// multiples of the duration from then on, the same instants on every
    /// replica; an epoch without elections does not end by time, one with
    /// elections ends at the first boundary after its first election
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epoch_ends_by_time() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_duration: Duration::from_secs(8),
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let start = Timestamp::new_test_instance();
        let at = |secs: u64| start + Duration::from_secs(secs);
        // The setup: nothing ends before the epochs start
        container.transition_time(at(100));
        assert!(!container.is_draining());
        assert!(!container.epochs_started());

        container.start_epochs(at(100));
        container.transition_time(at(107));
        assert!(!container.is_draining());
        let block = SavedBlock::new_test_instance();
        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                at(107),
            )
            .unwrap();
        container.transition_time(at(108));
        assert!(container.is_draining());
        assert_eq!(container.current_epoch(), ConsensusEpoch::ZERO);

        // Terminated: left; the next epoch ends at the boundary after its
        // first election (the boundaries are at 100 + 8k)
        container.erase(&block.qualified_root());
        container.transition_time(at(108));
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));
        container.transition_time(at(200));
        assert!(!container.is_draining());
        container
            .insert(
                AecInsertRequest::new_priority(
                    SavedBlock::new_test_instance_with_key(2),
                    BlockPriority::new_test_instance(),
                ),
                at(201),
            )
            .unwrap();
        container.transition_time(at(203));
        assert!(!container.is_draining());
        container.transition_time(at(204));
        assert!(container.is_draining());
    }

    /// RAI: once more than f of the weight votes in a later epoch this node
    /// ends its epoch without finishing its own count; once a certificate
    /// quorum is ahead it leaves the epoch at once, its open instances run on
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn follows_the_representatives_into_the_next_epoch() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1000,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        container.start_epochs(now);
        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();

        // The test quorum is 100M: f = 19M, a certificate 62M
        let small_rep = PrivateKey::from(1);
        let big_rep = PrivateKey::from(2);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(small_rep.public_key(), Amount::nano(30_000_000));
        rep_weights.put(big_rep.public_key(), Amount::nano(70_000_000));
        let epoch1 = ConsensusEpoch::new(1);
        let vote_ahead = |rep: &PrivateKey| {
            let vote = Arc::new(Vote::new_in_epoch(
                rep,
                VoteKind::First,
                epoch1,
                vec![block.hash()],
            ));
            FilteredVote::from(ReceivedVote::new(vote, VoteDelivery::Direct, None))
        };
        container.apply_vote(ApplyVoteArgs {
            vote: &vote_ahead(&small_rep),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        // More than f ahead: the epoch ends, the open instance keeps it from
        // being left until it terminates
        assert!(container.is_draining());
        assert_eq!(container.draining_epoch(), Some(ConsensusEpoch::ZERO));
        assert_eq!(container.current_epoch(), ConsensusEpoch::ZERO);
        container.transition_time(now);
        assert_eq!(container.current_epoch(), ConsensusEpoch::ZERO);

        // A certificate quorum ahead: left at once, the instance runs on
        container.apply_vote(ApplyVoteArgs {
            vote: &vote_ahead(&big_rep),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(container.current_epoch(), epoch1);
        assert!(!container.is_draining());
        assert_eq!(container.draining_epoch(), None);
        assert!(
            container
                .election(&ElectionId::legacy(block.qualified_root()))
                .is_some()
        );
        assert_eq!(container.epoch_closes().len(), 1);
    }

    /// RAI: once this node agreed on an epoch's value, an instance of the
    /// epoch which notarizes a block the value does not hold is late: it
    /// does not count in the epoch's state, its blocks are discarded and it
    /// is erased. No instance of an agreed epoch is started any more.
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn discards_the_late_notarized_instances_of_an_agreed_epoch() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        let now = Timestamp::new_test_instance();
        container.start_epochs(now);
        let decided = SavedBlock::new_test_instance_with_key(1);
        container
            .insert(
                AecInsertRequest::new_priority(decided.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        let apply = |container: &mut ActiveElectionsContainer, vote: Vote, at: Timestamp| {
            container.apply_vote(ApplyVoteArgs {
                vote: &ReceivedVote::new(Arc::new(vote), VoteDelivery::Direct, None).into(),
                rep_weights: &rep_weights,
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now: at,
            })
        };
        apply(
            &mut container,
            Vote::new_final(&rep_key, vec![decided.hash()]),
            now,
        );
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));

        // Before the close: the scheduler proposes a block in the current
        // epoch, and a peer's epoch-0 vote opens the instance of epoch 0
        // next to it. Nothing is late yet: that instance counts (with
        // nothing notarized) and may be part of the value finalized.
        let late = SavedBlock::new_test_instance_with_key(2);
        container
            .insert(
                AecInsertRequest::new_priority(late.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        container.insert_for_vote(late.clone(), ConsensusEpoch::ZERO, now);
        assert_eq!(container.len(), 2);
        let value = container.epoch_closes()[0].value.unwrap();
        let closed_at = now + Duration::from_secs(1);
        apply(
            &mut container,
            Vote::new_in_epoch(
                &rep_key,
                VoteKind::First,
                ConsensusEpoch::close_round(ConsensusEpoch::ZERO, 0),
                vec![value],
            ),
            closed_at,
        );
        assert_eq!(container.epoch_closes()[0].closed, Some((0, value)));
        // Agreed: the epoch holds `decided` and nothing else from now on, and
        // no instance of it is started any more
        let another = SavedBlock::new_test_instance_with_key(3);
        container.insert_for_vote(another.clone(), ConsensusEpoch::ZERO, closed_at);
        assert_eq!(container.len(), 2);
        assert_eq!(container.stats.agreed_epoch_refused, 1);
        let later = closed_at + Duration::from_secs(1);

        // Notarized in the agreed epoch, not in its value: discarded, both
        // instances of the root are gone, the value stands
        apply(
            &mut container,
            Vote::new_in_epoch(
                &rep_key,
                VoteKind::Notar,
                ConsensusEpoch::ZERO,
                vec![late.hash()],
            ),
            later,
        );
        container.transition_time(later);
        assert!(container.election_for_block(&late.hash()).is_none());
        assert_eq!(container.len(), 0);
        assert_eq!(container.stats.discarded_instances, 1);
        assert_eq!(
            container
                .epoch_state(ConsensusEpoch::ZERO)
                .close_value(ConsensusEpoch::ZERO),
            value
        );

        // A vote of the agreed epoch for a block without an instance is late,
        // not cached: the epoch is decided for good
        let result = apply(
            &mut container,
            Vote::new_in_epoch(
                &rep_key,
                VoteKind::First,
                ConsensusEpoch::ZERO,
                vec![another.hash()],
            ),
            later,
        );
        assert_eq!(result.get(&another.hash()), Some(&Err(VoteError::Late)));
    }

    /// RAI: what an epoch finalized stays finalized: a late instance of an
    /// agreed epoch which notarized a block finalized in another epoch is
    /// erased, but the block is not discarded
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn a_late_instance_never_discards_a_block_finalized_in_another_epoch() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        let now = Timestamp::new_test_instance();
        container.start_epochs(now);
        let apply = |container: &mut ActiveElectionsContainer, vote: Vote| {
            container.apply_vote(ApplyVoteArgs {
                vote: &ReceivedVote::new(Arc::new(vote), VoteDelivery::Direct, None).into(),
                rep_weights: &rep_weights,
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            })
        };
        // Epoch 0 finalizes one block and is left
        let decided = SavedBlock::new_test_instance_with_key(1);
        container
            .insert(
                AecInsertRequest::new_priority(decided.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        apply(
            &mut container,
            Vote::new_final(&rep_key, vec![decided.hash()]),
        );
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));

        // A block finalized in epoch 1, then a stale instance of epoch 0
        // opened for it by a peer's vote, before epoch 0 is agreed
        let block = SavedBlock::new_test_instance_with_key(2);
        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        apply(
            &mut container,
            Vote::new_in_epoch(
                &rep_key,
                VoteKind::Final,
                ConsensusEpoch::new(1),
                vec![block.hash()],
            ),
        );
        assert!(container.finalized_in_epoch(&block.hash(), ConsensusEpoch::new(1)));
        container.insert_for_vote(block.clone(), ConsensusEpoch::ZERO, now);
        assert_eq!(container.len(), 1);

        // Epoch 0 closes and is agreed without the block
        let value = container.epoch_closes()[0].value.unwrap();
        apply(
            &mut container,
            Vote::new_in_epoch(
                &rep_key,
                VoteKind::First,
                ConsensusEpoch::close_round(ConsensusEpoch::ZERO, 0),
                vec![value],
            ),
        );
        assert_eq!(container.epoch_closes()[0].closed, Some((0, value)));

        // The epoch-0 instance notarizes the block late: erased, nothing discarded
        apply(
            &mut container,
            Vote::new_in_epoch(
                &rep_key,
                VoteKind::Notar,
                ConsensusEpoch::ZERO,
                vec![block.hash()],
            ),
        );
        container.transition_time(now);
        assert_eq!(container.len(), 0);
        assert_eq!(container.stats.discarded_instances, 0);
        assert_eq!(container.stats.finalized_kept, 1);
        assert!(container.is_finalized(&block.hash()));
    }

    /// RAI: a vote for a block without an instance in the vote's epoch starts
    /// that instance. In an epoch this node has left it casts only a timeout
    /// vote there, in the current epoch it proposes the block
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn instances_are_started_for_votes() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        let epoch1 = ConsensusEpoch::new(1);
        container.set_current_epoch(epoch1);

        container.insert_for_vote(block.clone(), ConsensusEpoch::ZERO, now);
        // A second start of the same instance changes nothing
        container.insert_for_vote(block.clone(), ConsensusEpoch::ZERO, now);
        container.insert_for_vote(block.clone(), epoch1, now);

        let stale = ElectionId::legacy(block.qualified_root());
        let current = ElectionId::new(block.qualified_root(), epoch1);
        assert_eq!(
            container.election(&stale).unwrap().state(),
            ElectionState::Active
        );
        assert_eq!(container.len(), 2);
        let due = container.kudzu_votes_due(|_| true);
        assert!(due.contains(&VoteTarget {
            election: stale,
            winner: block.hash(),
            vote_type: VoteType::Abstain,
        }));
        assert!(due.contains(&VoteTarget {
            election: current,
            winner: block.hash(),
            vote_type: VoteType::NonFinal,
        }));
        assert_eq!(due.len(), 2);
    }

    /// RAI: leaving an epoch starts its close election. Once every instance
    /// of the epoch has settled, this node attests the epoch's state: as the
    /// leader of round 0 it proposes the value with its first vote, and the
    /// first votes of the representatives finalize it.
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epoch_close_election_runs_after_the_epoch_is_left() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        container.set_local_representatives(vec![rep_key.public_key()]);
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        container.start_epochs(now);
        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        assert!(container.epoch_closes().is_empty());

        // Finalizing the election advances the epoch, epoch 0 is closed next
        let vote = test_final_vote(&rep_key, block.hash());
        container.apply_vote(ApplyVoteArgs {
            vote: &vote.into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        let closes = container.epoch_closes();
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0].epoch, ConsensusEpoch::ZERO);
        assert!(closes[0].ready);
        let value = closes[0].value.unwrap();
        assert_eq!(
            value,
            container
                .epoch_state(ConsensusEpoch::ZERO)
                .close_value(ConsensusEpoch::ZERO)
        );
        assert_eq!(container.epoch_state(ConsensusEpoch::ZERO).finalized, 1);

        // This node leads round 0: its first vote is the proposal
        let round0 = ConsensusEpoch::close_round(ConsensusEpoch::ZERO, 0);
        let close_id = ElectionId::new(EpochClose::root_of(ConsensusEpoch::ZERO), round0);
        let proposal = VoteTarget {
            election: close_id.clone(),
            winner: value,
            vote_type: VoteType::NonFinal,
        };
        let due = container.kudzu_votes_due(|_| true);
        assert!(due.contains(&proposal));
        assert_eq!(
            container.mark_kudzu_voted(vec![proposal.clone()]),
            vec![proposal]
        );
        let (id, evidence) = container.certificate_evidence(&value, round0).unwrap();
        assert_eq!(id, close_id);
        assert_eq!(evidence.statements, vec![(VoteKind::First, vec![value])]);

        // The representative's first vote for the value finalizes the close
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::First,
            round0,
            vec![value],
        ));
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(result.get(&value), Some(&Ok(())));
        let closes = container.epoch_closes();
        assert_eq!(closes[0].closed, Some((0, value)));
        // The exit final vote is cast, then nothing more
        let close_votes: Vec<_> = container
            .kudzu_votes_due(|_| true)
            .into_iter()
            .filter(|target| target.election.epoch.is_close_round())
            .collect();
        assert_eq!(
            close_votes,
            vec![VoteTarget {
                election: close_id,
                winner: value,
                vote_type: VoteType::Final,
            }]
        );
        assert!(container.close_solicitations(now).is_empty());
    }

    /// RAI: the committees of the epochs. The first two count in the genesis
    /// committee; epoch 0 derives from the blocks it finalized the committee
    /// epoch 2 counts in, alone (Section 5.2). Votes of an epoch whose
    /// committee is not known here yet wait, and are counted once it is.
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epochs_count_in_their_committees_and_are_counted_again_on_a_change() {
        use rsnano_types::{Link, StateBlockArgs, WorkNonce};

        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let rep1 = PrivateKey::from(1);
        let rep2 = PrivateKey::from(2);
        // The live weights are not used once the genesis committee is set
        let rep_weights = RepWeights::default();
        let quorum = QuorumSnapshot::new_test_instance();
        let now = Timestamp::new_test_instance();
        container.start_epochs(now);
        // The genesis committee: representative 1 alone, delegated to by
        // one account which moves to representative 2 in epoch 0
        let mover = PrivateKey::from(10);
        container.set_genesis_committee(vec![AccountFrontier {
            account: mover.account(),
            height: 1,
            representative: rep1.public_key(),
            balance: Amount::raw(100),
        }]);
        let block0 = SavedBlock::new_test_instance_with(
            StateBlockArgs {
                key: &mover,
                previous: BlockHash::from(1),
                representative: rep2.public_key(),
                balance: Amount::raw(100),
                link: Link::ZERO,
                work: WorkNonce::new(0),
            }
            .into(),
        );

        let vote = |container: &mut ActiveElectionsContainer,
                    key: &PrivateKey,
                    kind: VoteKind,
                    epoch: ConsensusEpoch,
                    hash: BlockHash| {
            let vote = Arc::new(Vote::new_in_epoch(key, kind, epoch, vec![hash]));
            container.apply_vote(ApplyVoteArgs {
                vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
                rep_weights: &rep_weights,
                quorum_snapshot: &quorum,
                now,
            })
        };
        let insert = |container: &mut ActiveElectionsContainer, block: &SavedBlock| {
            container
                .insert(
                    AecInsertRequest::new_priority(
                        block.clone(),
                        BlockPriority::new_test_instance(),
                    ),
                    now,
                )
                .unwrap();
        };
        let close_value = |container: &ActiveElectionsContainer, epoch: u64| {
            container
                .epoch_closes()
                .into_iter()
                .find(|close| close.epoch == ConsensusEpoch::new(epoch))
                .unwrap()
                .value
                .unwrap()
        };

        // Epoch 0: representative 2 has no weight, representative 1
        // finalizes the block and closes the epoch, which derives the
        // committee of epoch 2: representative 2 alone
        insert(&mut container, &block0);
        vote(
            &mut container,
            &rep2,
            VoteKind::Final,
            ConsensusEpoch::ZERO,
            block0.hash(),
        );
        assert!(!container.is_finalized(&block0.hash()));
        vote(
            &mut container,
            &rep1,
            VoteKind::Final,
            ConsensusEpoch::ZERO,
            block0.hash(),
        );
        assert!(container.is_finalized(&block0.hash()));
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));
        assert_eq!(container.epoch_committees().len(), 1);
        let value0 = close_value(&container, 0);
        vote(
            &mut container,
            &rep1,
            VoteKind::First,
            ConsensusEpoch::close_round(ConsensusEpoch::ZERO, 0),
            value0,
        );
        assert_eq!(container.epoch_closes()[0].closed, Some((0, value0)));
        let committees = container.epoch_committees();
        assert_eq!(committees.len(), 2);
        assert_eq!(committees[1].derived_by, Some(ConsensusEpoch::ZERO));
        assert_eq!(
            committees[1].weights,
            vec![(rep2.public_key(), Amount::raw(100))]
        );

        // Epoch 1 still counts in the genesis committee
        let block1 = SavedBlock::new_test_instance_with_key(11);
        insert(&mut container, &block1);
        vote(
            &mut container,
            &rep1,
            VoteKind::Final,
            ConsensusEpoch::new(1),
            block1.hash(),
        );
        assert!(container.is_finalized(&block1.hash()));
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(2));

        // Epoch 2 counts in C(0) alone: representative 2 holds all of its
        // weight and finalizes there, where representative 1 has none
        let block2 = SavedBlock::new_test_instance_with_key(12);
        insert(&mut container, &block2);
        let epoch2 = ConsensusEpoch::new(2);
        let result = vote(
            &mut container,
            &rep1,
            VoteKind::Final,
            epoch2,
            block2.hash(),
        );
        assert_eq!(result.get(&block2.hash()), Some(&Ok(())));
        let election = container.election_for_block(&block2.hash()).unwrap();
        let committees = election.committees().unwrap();
        assert!(!committees.is_joint());
        assert_eq!(
            committees.primary().weight(&rep2.public_key()),
            Amount::raw(100)
        );
        assert_eq!(
            committees.primary().weight(&rep1.public_key()),
            Amount::ZERO
        );
        assert!(!election.is_confirmed());
        assert!(election.certificates().notar.is_empty());

        vote(
            &mut container,
            &rep2,
            VoteKind::Final,
            epoch2,
            block2.hash(),
        );
        assert!(container.election_for_block(&block2.hash()).is_none());
        assert!(container.finalized_in_epoch(&block2.hash(), epoch2));

        // The close of epoch 1 counts in the genesis committee, which voted
        // in epoch 1, and in C(0), which runs epoch 2: representative 1
        // alone does not close it any more
        let value1 = close_value(&container, 1);
        vote(
            &mut container,
            &rep1,
            VoteKind::First,
            ConsensusEpoch::close_round(ConsensusEpoch::new(1), 0),
            value1,
        );
        assert_eq!(container.epoch_closes()[1].closed, None);
        vote(
            &mut container,
            &rep2,
            VoteKind::First,
            ConsensusEpoch::close_round(ConsensusEpoch::new(1), 0),
            value1,
        );
        assert_eq!(container.epoch_closes()[1].closed, Some((0, value1)));
    }

    /// RAI: the epochs close one after the other: the close election of an
    /// epoch takes part only once the epoch before is closed
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epoch_close_election_waits_for_the_previous_epoch_to_close() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        container.set_local_representatives(vec![rep_key.public_key()]);
        let now = Timestamp::new_test_instance();
        container.start_epochs(now);
        // One decided election per epoch: epoch 0 is left, epoch 1 ends
        for (key, epoch) in [(1, ConsensusEpoch::ZERO), (2, ConsensusEpoch::new(1))] {
            let block = SavedBlock::new_test_instance_with_key(key);
            container
                .insert(
                    AecInsertRequest::new_priority(
                        block.clone(),
                        BlockPriority::new_test_instance(),
                    ),
                    now,
                )
                .unwrap();
            let vote = Arc::new(Vote::new_in_epoch(
                &rep_key,
                VoteKind::Final,
                epoch,
                vec![block.hash()],
            ));
            container.apply_vote(ApplyVoteArgs {
                vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
                rep_weights: &rep_weights,
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            });
        }
        // Epoch 1 has drained but is not left while epoch 0 is not closed
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));
        assert!(container.is_draining());
        let closes = container.epoch_closes();
        assert_eq!(closes.len(), 1);
        assert!(closes[0].started);
        let close_votes = |container: &ActiveElectionsContainer| -> Vec<VoteTarget> {
            container
                .kudzu_votes_due(|_| true)
                .into_iter()
                .filter(|target| target.election.epoch.is_close_round())
                .collect()
        };
        assert_eq!(close_votes(&container).len(), 1);

        // Closing epoch 0 lets epoch 1 be left and its close start
        let value = closes[0].value.unwrap();
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::First,
            ConsensusEpoch::close_round(ConsensusEpoch::ZERO, 0),
            vec![value],
        ));
        container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert!(container.epoch_closes()[0].closed.is_some());
        assert_eq!(container.epoch_closes().len(), 1);
        container.transition_time(now);
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(2));
        assert!(!container.is_draining());
        assert!(container.epoch_closes()[1].started);
        let proposal = close_votes(&container);
        assert!(proposal.iter().any(|target| {
            target.election.epoch == ConsensusEpoch::close_round(ConsensusEpoch::new(1), 0)
                && target.vote_type == VoteType::NonFinal
        }));
    }

    /// RAI: a close vote for an epoch this node has not left waits in the
    /// vote cache; it also tells that the voter left the epoch, which this
    /// node follows like any vote of a later epoch
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_vote_of_an_epoch_not_left_is_indeterminate_and_followed() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1000,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        let now = Timestamp::new_test_instance();
        container.start_epochs(now);
        let value = BlockHash::from(7);
        let close_vote = |round: ConsensusEpoch| {
            let vote = Arc::new(Vote::new_in_epoch(
                &rep_key,
                VoteKind::First,
                round,
                vec![value],
            ));
            FilteredVote::from(ReceivedVote::new(vote, VoteDelivery::Direct, None))
        };

        // A close of epoch 1 is two epochs ahead: cached, and this node follows one epoch
        let round = ConsensusEpoch::close_round(ConsensusEpoch::new(1), 0);
        let vote = close_vote(round);
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(result.get(&value), Some(&Err(VoteError::Indeterminate)));
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));
        assert_eq!(container.epoch_closes().len(), 1);

        // A close of the epoch just left is applied
        let round = ConsensusEpoch::close_round(ConsensusEpoch::ZERO, 0);
        let vote = close_vote(round);
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(result.get(&value), Some(&Ok(())));
        // The empty epoch 0 is closed by the only representative, with a
        // value this node does not attest
        assert_eq!(container.epoch_closes()[0].closed, Some((0, value)));
        assert_ne!(container.epoch_closes()[0].value, Some(value));
        // Nothing is voted or solicited in a closed epoch
        assert!(container.kudzu_votes_due(|_| true).is_empty());
        assert!(container.close_solicitations(now).is_empty());
    }

    #[test]
    fn iter_round_robin() {
        let block_a = SavedBlock::new_test_instance_with_key(1);
        let block_b = SavedBlock::new_test_instance_with_key(2);
        let block_c = SavedBlock::new_test_instance_with_key(3);
        let block_d = SavedBlock::new_test_instance_with_key(4);

        let prio_a = BlockPriority::new(Amount::nano(1), TimePriority::new(100));
        let prio_b = BlockPriority::new(Amount::nano(100), TimePriority::new(100));
        let prio_c = BlockPriority::new(Amount::nano(100), TimePriority::new(99));
        let prio_d = BlockPriority::new(Amount::nano(1_000_000), TimePriority::new(100));

        test_iter(&[], &[]);

        test_iter(&[(&block_a, prio_a)], &[&block_a]);

        test_iter(
            &[
                (&block_d, prio_d),
                (&block_a, prio_a),
                (&block_c, prio_c),
                (&block_b, prio_b),
            ],
            &[&block_d, &block_c, &block_a, &block_b],
        )
    }

    #[test]
    fn reports_stale_election_count() {
        let mut container = ActiveElectionsContainer::default();
        let request = AecInsertRequest {
            block: SavedBlock::new_test_instance(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        let start = Timestamp::new_test_instance();

        container.insert(request, start).unwrap();

        assert_eq!(container.info(start).stale, 0);
        assert_eq!(container.info(start + Duration::from_secs(60)).stale, 1);
    }

    #[test]
    fn kudzu_first_vote_is_due_for_a_new_election_and_recorded_per_slot() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let request =
            AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance());
        container
            .insert(request, Timestamp::new_test_instance())
            .unwrap();

        let due = container.kudzu_votes_due(|_| true);
        assert_eq!(
            due,
            vec![VoteTarget {
                election: ElectionId::legacy(block.qualified_root()),
                winner: block.hash(),
                vote_type: VoteType::NonFinal,
            }]
        );

        container.mark_kudzu_voted(due.clone());
        let slot = container
            .slot_state(&EpochSlot {
                account: block.account(),
                height: block.height(),
                epoch: ConsensusEpoch::ZERO,
            })
            .unwrap();
        assert_eq!(slot.first_voted, Some(block.hash()));
        assert_eq!(container.slot_count(), 1);

        // Nothing new is decided, the first vote is only re-broadcast
        assert_eq!(container.kudzu_votes_due(|_| true), due);
    }

    #[test]
    fn kudzu_slot_state_is_dropped_when_the_height_is_confirmed() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let request =
            AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance());
        let now = Timestamp::new_test_instance();
        container.insert(request, now).unwrap();
        let due = container.kudzu_votes_due(|_| true);
        container.mark_kudzu_voted(due.clone());
        assert_eq!(container.slot_count(), 1);

        container.confirm_dependent_elections(vec![(block.clone(), None)], now);

        // RAI: the instance keeps running until it reaches its outcome, and
        // its slot state outlives it: it goes once the next epoch is decided
        if cfg!(feature = "rai_protocol") {
            assert_eq!(container.slot_count(), 1);
            container.erase(&block.qualified_root());
            assert_eq!(container.slot_count(), 1);
        } else {
            assert_eq!(container.slot_count(), 0);
        }
    }

    /// Legacy never confirms on a non-final vote, so this only applies to Kudzu
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn kudzu_exit_final_vote_of_a_fast_finalized_election_is_kept() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();
        let root = block.qualified_root();
        let request = AecInsertRequest::new_priority(block, BlockPriority::new_test_instance());
        let now = Timestamp::new_test_instance();
        container.insert(request, now).unwrap();
        let first = container.kudzu_votes_due(|_| true);
        container.mark_kudzu_voted(first.clone());

        // A single first vote with all the weight fast finalizes and erases the election
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        let vote = Arc::new(Vote::new(
            &rep_key,
            rsnano_types::UnixMillisTimestamp::new(1000),
            0,
            vec![block_hash],
        ));
        let received = ReceivedVote::new(vote, VoteDelivery::Direct, None);
        container.apply_vote(ApplyVoteArgs {
            vote: &received.into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert!(container.election_for_block(&block_hash).is_none());

        let expected = VoteTarget {
            election: ElectionId::legacy(root),
            winner: block_hash,
            vote_type: VoteType::Final,
        };
        assert_eq!(container.kudzu_votes_due(|_| true), vec![expected.clone()]);
        container.mark_kudzu_voted(vec![expected]);
        assert!(container.kudzu_votes_due(|_| true).is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn terminated_election_leaves_the_buckets_and_hands_out_its_votes() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();
        let root = block.qualified_root();
        let now = Timestamp::new_test_instance();
        container
            .insert(
                AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        let vacancy_before = container.vacancy();
        let id = ElectionId::legacy(root.clone());
        assert!(!container.is_terminated(&id));
        assert!(
            container
                .certificate_evidence(&block_hash, ConsensusEpoch::ZERO)
                .is_none()
        );

        // 70%: notarization certificate, no fast finalization
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::nano(70_000_000));
        let vote = Arc::new(Vote::new(
            &rep_key,
            rsnano_types::UnixMillisTimestamp::new(1000),
            0,
            vec![block_hash],
        ));
        container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote.clone(), VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        assert!(container.is_terminated(&id));
        // It no longer takes capacity but is still there and iterated
        assert_eq!(container.vacancy(), vacancy_before + 1);
        assert_eq!(container.len(), 1);
        assert_eq!(container.iter_round_robin().count(), 1);
        assert!(container.election_for_block(&block_hash).is_some());

        let (served, evidence) = container
            .certificate_evidence(&block_hash, ConsensusEpoch::ZERO)
            .unwrap();
        assert_eq!(served, id);
        // The election of another epoch has no evidence
        assert!(
            container
                .certificate_evidence(&block_hash, ConsensusEpoch::new(1))
                .is_none()
        );
        // This node never voted here, so it only hands out the candidate
        assert!(evidence.statements.is_empty());
        assert_eq!(evidence.blocks.len(), 1);
        assert_eq!(evidence.blocks[0].hash(), block_hash);

        container.mark_kudzu_voted(vec![VoteTarget {
            election: id.clone(),
            winner: block_hash,
            vote_type: VoteType::NonFinal,
        }]);
        let (_, evidence) = container
            .certificate_evidence(&block_hash, ConsensusEpoch::ZERO)
            .unwrap();
        assert_eq!(
            evidence.statements,
            vec![(VoteKind::First, vec![block_hash])]
        );

        // Erasing it keeps the accounting consistent
        assert!(container.erase(&root));
        assert_eq!(container.len(), 0);
        assert_eq!(container.vacancy(), vacancy_before + 1);
    }

    #[test]
    fn kudzu_votes_for_unknown_roots_are_not_recorded() {
        let mut container = ActiveElectionsContainer::default();
        container.mark_kudzu_voted(vec![VoteTarget {
            election: ElectionId::new_test_instance(),
            winner: BlockHash::from(1),
            vote_type: VoteType::NonFinal,
        }]);
        assert_eq!(container.slot_count(), 0);
    }

    /// Kudzu: a running election is never evicted for a higher priority block
    #[test]
    fn evict_lowest_priority_election() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let request =
            AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance());
        container
            .insert(request, Timestamp::new_test_instance())
            .unwrap();
        let bucket = container
            .find_bucket(&ElectionId::legacy(block.qualified_root()))
            .unwrap();

        let evicted = container.erase_lowest_prio_election(bucket);

        assert_eq!(evicted, !cfg!(feature = "rai_protocol"));
        assert_eq!(container.len(), if evicted { 0 } else { 1 });
    }

    #[test]
    fn priority_active_hash_only_for_elections_that_cannot_be_upgraded() {
        let mut container = ActiveElectionsContainer::default();
        let now = Timestamp::new_test_instance();
        let priority = SavedBlock::new_test_instance_with_key(1);
        let hinted = SavedBlock::new_test_instance_with_key(2);
        container
            .insert(
                AecInsertRequest::new_priority(
                    priority.clone(),
                    BlockPriority::new_test_instance(),
                ),
                now,
            )
            .unwrap();
        container
            .insert(
                AecInsertRequest::new_hinted(hinted.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();

        assert!(container.is_priority_active_hash(&priority.hash()));
        assert!(!container.is_priority_active_hash(&hinted.hash()));
        assert!(!container.is_priority_active_hash(&BlockHash::from(3)));
    }

    /// A candidate source which always has a block to schedule
    struct AlwaysAvailable;

    impl ElectionCandidateSource for AlwaysAvailable {
        fn should_schedule(&self, _buckets: &[BucketInfo]) -> bool {
            true
        }

        fn next_candidate(
            &mut self,
            _bucket_id: usize,
            _vacancy: isize,
            _lowest_priority: TimePriority,
        ) -> Option<ElectionCandidate> {
            None
        }
    }

    fn test_final_vote(rep_key: &PrivateKey, block_hash: BlockHash) -> ReceivedVote {
        let vote = Arc::new(Vote::new_final(rep_key, vec![block_hash]));
        ReceivedVote::new(vote, VoteDelivery::Direct, None)
    }

    fn test_iter(blocks: &[(&SavedBlock, BlockPriority)], expected: &[&SavedBlock]) {
        let mut container = ActiveElectionsContainer::default();

        for (block, prio) in blocks {
            let request = AecInsertRequest::new_priority((**block).clone(), *prio);

            container
                .insert(request, Timestamp::new_test_instance())
                .unwrap();
        }

        let result: Vec<_> = container
            .iter_round_robin()
            .map(|i| i.winner().hash())
            .collect();
        let expected: Vec<_> = expected.iter().map(|i| i.hash()).collect();
        assert_eq!(result, expected);
    }
}
