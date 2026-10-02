use std::{
    cmp::max,
    collections::{BTreeMap, BTreeSet, HashMap},
    mem::size_of,
    ops::Deref,
    sync::Arc,
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

use crate::consensus::election::{AccountSlot, CertifiedBlock, EpochLedger, ResidualKind};

use super::{
    ActiveElectionsConfig, ActiveElectionsInfo, AecFact, AecInsertError, AecInsertRequest, Entry,
    RootContainer,
    apply_vote_helper::{
        ApplyVoteHelper, ApplyVoteResult, count_kudzu_election, election_got_confirmed,
        settle_election,
    },
    checkpoint_state::{DecidedStates, is_late},
    cooldown_controller::{AecCooldownReason, CooldownController, CooldownResult},
    epoch_committees::{CommitteeInfo, EpochCommittees},
    epoch_states::{Delegation, EpochStates, FinalizedInstance},
    recently_confirmed_cache::RecentlyConfirmedCache,
    slot_states::SlotStates,
    stats::AecStats,
    vote_records::VoteRecords,
};

/// Kudzu never evicts, so a full bucket makes the scheduler hold its blocks
/// until an election in it terminates. The spam workload keeps its accounts
/// in a few buckets, and around an epoch switch the in-flight blocks have two
/// instances each: the per-bucket cap would stall the scheduler for seconds.
/// The AEC size bounds the elections as a whole.
pub(crate) fn per_bucket_cap(max_elections: usize) -> usize {
    {
        #[cfg(feature = "rai_protocol")]
        {
            max_elections
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            max(max_elections / bucket_count(), 1)
        }
    }
}

/// RAI: what this node reports for one epoch: the certified block tree and
/// the votes of its own that the tree does not summarize, with the digest of
/// the committee which issued the epoch's votes
pub struct EpochReport {
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
    /// RAI: whether this node casts account votes at all (see
    /// `ActiveElectionsConfig::account_voting`)
    account_voting: bool,
    /// RAI, Algorithm 1 line 8: the epochs this node stopped signing in, at
    /// their boundary. Their instances still collect the votes of the other
    /// replicas and construct certificates; this node issues no vote in them.
    frozen: BTreeSet<ConsensusEpoch>,
    /// RAI: the votes received, by epoch and voter, from which another
    /// reporter's residual object is derived
    vote_records: VoteRecords,
    /// RAI: what the blocks a checkpoint finalized delegate, by epoch: read
    /// from the block held in the instance the checkpoint confirmed, which
    /// is erased on confirmation. `Sigma_e` derives the committee from them
    /// like from any finalized block.
    checkpoint_delegations: BTreeMap<ConsensusEpoch, HashMap<BlockHash, Delegation>>,
    /// RAI: elections of the current epoch which got a certificate so far
    decided_in_current_epoch: usize,
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
    /// RAI: `S_e` for every epoch whose joint election decided, as this
    /// node derived it from the selected reports. An instance of a decided
    /// epoch that notarizes a block the state does not hold is late, and
    /// none is started any more.
    decided: DecidedStates,
    /// RAI: `S_{-1}`, the closed genesis state: the ledger as it stood when
    /// the epochs started. The first epoch's derivation builds on it.
    genesis_state: Arc<EpochLedger>,
    /// RAI: the committees the instances of each epoch are counted in
    committees: EpochCommittees,
}

impl ActiveElectionsContainer {
    /// RAI: how many left epochs stay frozen; older ones have no instance
    /// left to sign in
    const FROZEN_EPOCHS_KEPT: u64 = 8;
    /// RAI: how many left epochs keep their received votes, for the
    /// residual objects of their reports; as many as the reports are kept
    const VOTE_RECORD_EPOCHS_KEPT: u64 = 4;

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
            account_voting: config.account_voting,
            frozen: BTreeSet::new(),
            vote_records: VoteRecords::default(),
            checkpoint_delegations: BTreeMap::new(),
            decided_in_current_epoch: 0,
            epoch_duration: config.epoch_duration,
            epochs_started: false,
            epoch_origin: None,
            epoch_ends_at: None,
            drain_logged: None,
            epoch_states: EpochStates::default(),
            decided: DecidedStates::new(),
            genesis_state: Arc::new(EpochLedger::new()),
            committees: EpochCommittees::default(),
        }
    }

    /// RAI: the setup of a run is over: epoch 0 starts now, at the same time
    /// on every replica, with the voting weights as they are now
    pub fn start_epochs(&mut self, now: Timestamp) {
        #[cfg(feature = "rai_protocol")]
        {
            if self.epochs_started {
                return;
            }
            self.epochs_started = true;
            self.epoch_origin = Some(now);
            self.epoch_ends_at = self.next_boundary(now);
            self.decided_in_current_epoch = 0;
            #[cfg(feature = "rai_protocol")]
            {
                diagnostic!(
                    "EPOCH_START epoch={} elections={}",
                    self.current_epoch,
                    self.roots.len()
                );
            }
        }
    }

    /// RAI: whether the epochs have started
    pub fn epochs_started(&self) -> bool {
        self.epochs_started
    }

    /// RAI: the epoch whose instances are draining, if any
    pub fn draining_epoch(&self) -> Option<ConsensusEpoch> {
        self.draining.then_some(self.current_epoch)
    }

    /// RAI: the committees known here, the genesis one first
    pub fn epoch_committees(&self) -> Vec<CommitteeInfo> {
        self.committees.infos()
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
    fn count_decided(&mut self, decided: &[ConsensusEpoch], _now: Timestamp) {
        self.decided_in_current_epoch += decided
            .iter()
            .filter(|epoch| **epoch == self.current_epoch)
            .count();
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
        #[cfg(feature = "rai_protocol")]
        {
            diagnostic!(
                "EPOCH_ENDED epoch={} elections={} active={}",
                self.current_epoch,
                self.roots.len(),
                self.roots.active_len()
            );
        }
        self.try_advance_epoch(now);
    }

    /// RAI, "One open epoch and one closing epoch": the ended epoch is left
    /// at once, unless an older epoch is still closing - then "the boundary
    /// of open epoch e+1 waits; account voting there continues". Nothing
    /// waits for the epoch's instances: what they leave unresolved is
    /// carried to the epoch decision by the report.
    fn try_advance_epoch(&mut self, now: Timestamp) {
        if !self.draining || !self.previous_epoch_closed(self.current_epoch) {
            return;
        }
        self.draining = false;
        self.advance_epoch(now);
    }

    /// RAI: whether the close election of the epoch before this one has
    /// finalized its value; an epoch without a known predecessor counts as such
    fn previous_epoch_closed(&self, epoch: ConsensusEpoch) -> bool {
        epoch
            .as_u64()
            .checked_sub(1)
            .is_none_or(|previous| self.decided.contains_key(&ConsensusEpoch::new(previous)))
    }

    /// RAI: the committee the instances of an epoch are counted in, if it
    /// is known here (see `EpochCommittees`)
    fn committees_for(&self, epoch: ConsensusEpoch) -> Option<Committees> {
        self.committees.for_epoch(epoch)
    }

    /// RAI: `O_e = C_{e-2}`, the committee that issued an epoch's account
    /// votes and whose reports its close selects. Its thresholds are the
    /// ones a selection and a recovery count against.
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn epoch_committee(&self, epoch: ConsensusEpoch) -> Option<Arc<Committee>> {
        self.committees.committee(epoch)
    }

    /// RAI: the frontiers of every account at the end of the setup: the
    /// genesis committee the first two epochs count in, and the base the
    /// committees of the epochs closed are derived on
    pub fn set_genesis_committee(&mut self, frontiers: Vec<AccountFrontier>) {
        if self.committees.started() {
            return;
        }
        // RAI: `S_{-1} = S_G`, the closed genesis state the first epoch's
        // derivation builds on. Every account sits at its frontier, and
        // those positions are finalized: rule 1 never rolls them back.
        let mut genesis = EpochLedger::new();
        for frontier in &frontiers {
            genesis.finalize_genesis(
                AccountSlot::new(frontier.account, frontier.height),
                frontier.hash,
            );
        }
        self.genesis_state = Arc::new(genesis);
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
                &self.decided,
            );
            settle_election(&mut self.roots, &id, &self.observer, &mut result);
        }
        self.stats.recounted += result.decided.len();
        for entry in result.confirmed {
            self.cleanup_election(entry);
        }
        self.count_decided(&result.decided, now);
    }

    fn log_committee(&self, derived_by: &str, committee: &Committee) {
        #[cfg(feature = "rai_protocol")]
        {
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
    fn advance_epoch(&mut self, _now: Timestamp) {
        let left = self.current_epoch;
        // Algorithm 1 line 8: "stop old-epoch signing before producing the
        // immutable report". Both under the one lock: no vote of the epoch
        // is signed after the report, so every final vote this node issued
        // is in it (Lemma 6.4 rests on that)
        self.frozen.insert(left);
        self.frozen
            .retain(|epoch| epoch.as_u64() + Self::FROZEN_EPOCHS_KEPT > left.as_u64());
        if let Some(kept_from) = left.as_u64().checked_sub(Self::VOTE_RECORD_EPOCHS_KEPT) {
            self.vote_records
                .trim_before(ConsensusEpoch::new(kept_from));
            self.checkpoint_delegations
                .retain(|epoch, _| epoch.as_u64() >= kept_from);
        }
        self.pending_kudzu_votes
            .retain(|target| target.election.epoch != left);
        let report = self.epoch_report(left).map(Arc::new);
        self.current_epoch = self.current_epoch.next();
        self.decided_in_current_epoch = 0;
        self.epoch_ends_at = None;
        self.stats.epochs_advanced += 1;
        self.notify(AecFact::EpochAdvanced(self.current_epoch, report));
        #[cfg(feature = "rai_protocol")]
        {
            diagnostic!(
                "EPOCH_ADVANCED epoch={} elections={} active={} vote_records={}",
                self.current_epoch,
                self.roots.len(),
                self.roots.active_len(),
                self.vote_records.len()
            );
        }
    }

    /// RAI: how one epoch stands on this node: what its finalized instances
    /// decided and what its running ones notarized. It is no longer what the
    /// close decides on - the epoch's state comes from the selected reports
    /// now - but it is what the run's diagnostics and the RPC report, and
    /// what the drain of the boundary reads.
    pub fn epoch_state(&self, epoch: ConsensusEpoch) -> EpochState {
        self.epoch_state_and_late(epoch).0
    }

    /// RAI: the epoch's state, and its late instances: those of a decided
    /// epoch which notarized a block the decided state does not hold
    fn epoch_state_and_late(&self, epoch: ConsensusEpoch) -> (EpochState, Vec<ElectionId>) {
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
            if is_late(&self.decided, election) {
                late.push(election.id());
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

    /// RAI: `Sigma_e` as account frontiers: every account at the block the
    /// decided state finalized for it, with what that block delegates. The
    /// weights are cumulative, so an account whose frontier did not move in
    /// this epoch is already counted and needs no entry here.
    ///
    /// The delegation is read from the block itself, which this node holds
    /// in the instance the block was a candidate in. A frontier block it
    /// does not hold is reported and left out rather than guessed at.
    fn decided_frontiers(
        &self,
        epoch: ConsensusEpoch,
        state: &EpochLedger,
    ) -> Vec<AccountFrontier> {
        let mut delegations_by_hash: HashMap<BlockHash, Delegation> = HashMap::new();
        for block in self.epoch_states.finalized_blocks_in(epoch) {
            delegations_by_hash.insert(block.delegation.hash, block.delegation);
        }
        if let Some(installed) = self.checkpoint_delegations.get(&epoch) {
            delegations_by_hash.extend(installed.iter().map(|(hash, d)| (*hash, *d)));
        }
        for election in self.roots.iter().map(|entry| &entry.election) {
            if election.epoch() != epoch {
                continue;
            }
            for delegation in delegations(election) {
                delegations_by_hash.insert(delegation.hash, delegation);
            }
        }
        let mut frontiers = Vec::new();
        let mut missing = 0;
        for (account, (height, hash)) in state.frontiers() {
            if self
                .committees
                .counted_height(&account)
                .is_some_and(|counted| counted >= height)
            {
                continue;
            }
            match delegations_by_hash.get(&hash) {
                Some(delegation) => frontiers.push(AccountFrontier {
                    account,
                    height,
                    hash,
                    representative: delegation.representative,
                    balance: delegation.balance,
                }),
                None => missing += 1,
            }
        }
        if missing > 0 {
            diagnostic!(
                "EPOCH_FRONTIER_MISSING epoch={} accounts={} : blocks this node does not hold",
                epoch,
                missing
            );
        }
        frontiers.sort_by_key(|frontier| frontier.account);
        frontiers
    }

    /// RAI: `S_{e-1}` for the close of an epoch: the state the epoch before
    /// it decided, and the closed genesis state for the first epoch. None
    /// while the epoch before has not been decided here, which is when this
    /// node can neither propose nor check a value.
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn epoch_previous_state(&self, epoch: ConsensusEpoch) -> Option<Arc<EpochLedger>> {
        match epoch.as_u64().checked_sub(1) {
            Some(before) => self.decided.get(&ConsensusEpoch::new(before)).cloned(),
            None => Some(self.genesis_state.clone()),
        }
    }

    /// RAI: `S_e` once the epoch's joint election decided and this node
    /// derived the state the finalized value names
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn epoch_decided_state(&self, epoch: ConsensusEpoch) -> Option<Arc<EpochLedger>> {
        self.decided.get(&epoch).cloned()
    }

    /// RAI: the only discard: an instance of an agreed epoch notarized a
    /// block the agreed value does not hold. It is erased; its blocks are
    /// in this node's ledger and not in the value, so they are rolled back
    /// (see `cleanup_election`), unless finalized in another epoch.
    fn discard_late_instances(&mut self) {
        let decided: Vec<_> = self.decided.keys().copied().collect();
        for epoch in decided {
            let (_, late) = self.epoch_state_and_late(epoch);
            for id in late {
                self.erase_election(&id);
            }
        }
    }

    /// RAI: what an ending epoch's drain is waiting for, once a second
    fn log_drain_wait(&mut self, now: Timestamp) {
        #[cfg(feature = "rai_protocol")]
        {
            if !self.draining {
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
    }

    /// RAI: the epoch's joint election finalized a value this node derived
    /// the state of: `S_e` is decided for good. That state is what the epoch
    /// holds from now on - an instance notarizing anything else is late,
    /// none is started any more, and the states of the slots without an
    /// instance are dropped - and its finalized projection derives the
    /// committee the epoch two after counts in.
    pub(crate) fn install_decided_checkpoint(
        &mut self,
        epoch: ConsensusEpoch,
        state: Arc<EpochLedger>,
        now: Timestamp,
    ) {
        if self.decided.contains_key(&epoch) {
            return;
        }
        if epoch >= self.current_epoch || self.epoch_previous_state(epoch).is_none() {
            return;
        }
        self.decided.insert(epoch, state.clone());
        self.stats.epochs_closed += 1;
        // Installed before the committee is derived: a block the checkpoint
        // finalized delegates like any other, and its instance goes with
        // the installation
        self.install_checkpoint(epoch, &state, now);
        let frontiers = self.decided_frontiers(epoch, &state);
        let live: FxHashSet<(Account, u64)> = self
            .roots
            .iter()
            .map(|entry| &entry.election)
            .filter(|election| election.epoch() == epoch)
            .map(|election| (election.account(), election.height()))
            .collect();
        self.slots.remove_epoch_except(epoch, &live);
        self.derive_committee(epoch, frontiers, now);
        self.release_undecided_instances(epoch);
        self.release_predecessor_gate(epoch.next(), now);
    }

    /// RAI: the instances of a decided epoch that the checkpoint did not
    /// finalize are over. One whose position the checkpoint neither
    /// finalized nor retained is omitted: "an omitted candidate can be
    /// retried with fresh epoch votes", so it is erased and its block, still
    /// in the ledger, is proposed again in the open epoch. One whose
    /// position the checkpoint retained as a fork is closed: "a checkpoint
    /// never reopens a retained position", the owner resolves it with a
    /// child, and the instance is erased with nothing rolled back. A late
    /// one is discarded instead.
    fn release_undecided_instances(&mut self, epoch: ConsensusEpoch) {
        let Some(state) = self.decided.get(&epoch).cloned() else {
            return;
        };
        let mut omitted = Vec::new();
        let mut retained = Vec::new();
        for election in self.roots.iter().map(|entry| &entry.election) {
            if election.epoch() != epoch
                || election.is_confirmed()
                || is_late(&self.decided, election)
            {
                continue;
            }
            let slot = AccountSlot::new(election.account(), election.height());
            if state.finalized(&slot).is_some() {
                continue;
            }
            if state.notarized(&slot).is_empty() {
                omitted.push(election.id());
            } else {
                retained.push(election.id());
            }
        }
        for id in omitted.iter().chain(&retained) {
            self.erase_election(id);
        }
        #[cfg(feature = "rai_protocol")]
        if !(omitted.is_empty() && retained.is_empty()) {
            diagnostic!(
                "EPOCH_OMITTED epoch={} instances={} retained={}",
                epoch,
                omitted.len(),
                retained.len()
            );
        }
    }

    /// RAI: whether the latest decided checkpoint holds this position as a
    /// retained fork, unresolved: no instance is started there again
    fn position_retained(&self, account: Account, height: u64) -> bool {
        let Some(state) = self.decided.values().next_back() else {
            return false;
        };
        let slot = AccountSlot::new(account, height);
        state.finalized(&slot).is_none() && !state.notarized(&slot).is_empty()
    }

    /// RAI, "Where a block may be voted on": the checkpoint the instances of
    /// the next epoch were waiting for is decided. Their finality comes out
    /// of the votes already held: a fast certificate is applied, the final
    /// vote comes due.
    fn release_predecessor_gate(&mut self, epoch: ConsensusEpoch, now: Timestamp) {
        let ids: Vec<ElectionId> = self
            .roots
            .iter()
            .map(|entry| &entry.election)
            .filter(|election| election.epoch() == epoch && !election.predecessor_decided())
            .map(|election| election.id())
            .collect();
        let mut result = ApplyVoteResult::default();
        for id in &ids {
            let Some(election) = self.roots.election_mut(id) else {
                continue;
            };
            election.set_predecessor_decided(true);
            if election.is_confirmed() {
                election_got_confirmed(
                    election,
                    now,
                    &self.observer,
                    &mut self.recently_confirmed,
                    &self.decided,
                );
            }
            settle_election(&mut self.roots, id, &self.observer, &mut result);
        }
        let confirmed = result.confirmed.len();
        for entry in result.confirmed {
            self.cleanup_election(entry);
        }
        self.count_decided(&result.decided, now);
        #[cfg(feature = "rai_protocol")]
        if !ids.is_empty() {
            diagnostic!(
                "EPOCH_GATE epoch={} released={} finalized={}",
                epoch,
                ids.len(),
                confirmed
            );
        }
    }

    /// RAI: whether the checkpoint before an epoch is decided here, which
    /// is what the epoch's instances need before any of them is final.
    /// Before the epochs of a run start there is nothing to wait for.
    fn predecessor_decided_for(&self, epoch: ConsensusEpoch) -> bool {
        if !self.epochs_started {
            return true;
        }
        match epoch.as_u64().checked_sub(1) {
            None => true,
            Some(before) => self.decided.contains_key(&ConsensusEpoch::new(before)),
        }
    }

    /// RAI, "Only the joint decision installs its checkpoint": what `S_e`
    /// finalized beyond `S_{e-1}` becomes final here. An instance holding
    /// such a block is confirmed with it as the winner, whatever its own
    /// certificates say - the checkpoint recovered a finality the votes this
    /// node saw did not show, or chose the sole survivor of a position - and
    /// every such block is cemented by the ledger. "Installation reconciles
    /// already-finalized live operations without applying their effects
    /// twice": a block already cemented is left as it is.
    fn install_checkpoint(&mut self, epoch: ConsensusEpoch, state: &EpochLedger, now: Timestamp) {
        let previous = self.epoch_previous_state(epoch);
        let hashes: Vec<BlockHash> = state
            .finalized_slots()
            .filter(|(slot, block)| {
                !previous
                    .as_ref()
                    .is_some_and(|previous| previous.is_finalized(slot, &block.hash))
            })
            .map(|(_, block)| block.hash)
            .collect();
        let mut confirmed = 0;
        for hash in &hashes {
            let Some(election) = self.roots.election_for_block_mut(hash) else {
                continue;
            };
            if let Some(delegation) = delegation_of(election, hash) {
                self.checkpoint_delegations
                    .entry(epoch)
                    .or_default()
                    .insert(*hash, delegation);
            }
            let Some(replaced) = election.finalize_by_checkpoint(hash) else {
                continue;
            };
            let id = election.id();
            let root = election.qualified_root().clone();
            let winner = election.winner().deref().clone();
            let confirmed_election =
                election.into_confirmed_election(now, ConfirmationType::ActiveConfirmedQuorum);
            if replaced != *hash {
                // The ledger holds the other branch: it is rolled back and
                // the finalized block inserted in its place
                self.notify(AecFact::WinnerChanged(replaced, winner));
            }
            self.recently_confirmed.put(root.clone(), *hash);
            self.notify(AecFact::ElectionConfirmed(confirmed_election));
            // Every instance of the root goes: the position is final
            let _ = id;
            self.erase(&root);
            confirmed += 1;
        }
        #[cfg(feature = "rai_protocol")]
        {
            diagnostic!(
                "EPOCH_INSTALL epoch={} finalized={} instances_confirmed={}",
                epoch,
                hashes.len(),
                confirmed
            );
        }
        if !hashes.is_empty() {
            self.notify(AecFact::CheckpointFinalized { epoch, hashes });
        }
    }

    /// RAI: whether a decided checkpoint finalized this block of the
    /// election's position
    fn finalized_by_checkpoint(&self, election: &Election, hash: &BlockHash) -> bool {
        let slot = AccountSlot::new(election.account(), election.height());
        self.decided
            .values()
            .any(|state| state.is_finalized(&slot, hash))
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
        // A decided epoch is settled for good: the instance could only be late
        if self.decided.contains_key(&epoch) {
            self.stats.agreed_epoch_refused += 1;
            return;
        }
        // A retained position is never reopened
        if self.position_retained(block.account(), block.height()) {
            self.stats.agreed_epoch_refused += 1;
            return;
        }
        // In an instance of an epoch this node has left it does not propose:
        // the block is decided in the open epoch. The instance is opened all
        // the same, to collect the certificates of the others.
        if epoch < self.current_epoch {
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
    pub fn epoch_certified(
        &self,
        epoch: ConsensusEpoch,
    ) -> crate::consensus::election::CertifiedState {
        use crate::consensus::election::{CertifiedBlock, CertifiedState, CertifiedStatus};
        let mut certified = CertifiedState::new();
        // What finalized in the epoch and left the AEC is certified by the
        // certificate that finalized it. The parent goes with it: an epoch
        // derivation places a candidate by the branch it continues, and two
        // blocks at one slot are told apart by nothing else.
        for instance in self.epoch_states.instances_of(epoch) {
            certified.certify(
                CertifiedBlock::new(instance.account, instance.height, instance.winner),
                instance.root.previous,
                CertifiedStatus::Finalized,
            );
        }
        // What the instances still in the AEC have certified
        for election in self.roots.iter().map(|entry| &entry.election) {
            if election.epoch() != epoch {
                continue;
            }
            let previous = election.qualified_root().previous;
            let at = |hash| CertifiedBlock::new(election.account(), election.height(), hash);
            let certificates = election.certificates();
            for hash in &certificates.notar {
                certified.certify(at(*hash), previous, CertifiedStatus::Notarized);
            }
            if let Some(hash) = certificates.finalized() {
                certified.certify(at(hash), previous, CertifiedStatus::Finalized);
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
    pub fn epoch_report(&self, epoch: ConsensusEpoch) -> Option<EpochReport> {
        use crate::consensus::election::ResidualVotes;
        let committee = self.committees.committee(epoch)?;
        let certified = self.epoch_certified(epoch);

        // This node's own votes in the epoch, each with the parent its block
        // names: the parent belongs to the block, because two conflicting
        // parents put their children in one voting domain on different
        // branches. The first vote is kept apart from the second-look
        // notarization support: only first votes can witness a hidden fast
        // finalization certificate, which is what `A_Q` recovers.
        let mut votes = Vec::new();
        let mut own = |account: Account,
                       height: u64,
                       parent: &dyn Fn(&BlockHash) -> BlockHash,
                       slot: &LocalSlotState| {
            let mut voted = |hash: BlockHash, kind: ResidualKind| {
                votes.push((
                    CertifiedBlock::new(account, height, hash),
                    kind,
                    parent(&hash),
                ));
            };
            if let Some(hash) = slot.first_voted {
                voted(hash, ResidualKind::First);
            }

            if let Some(hash) = slot.final_voted {
                voted(hash, ResidualKind::Final);
            }
        };
        // A live slot's votes: the parent of each block this node voted for
        for (account, height, slot) in self.slots.iter_epoch(epoch) {
            own(account, height, &|hash| self.slots.parent(hash), slot);
        }
        // An instance that finalized and left the AEC: every candidate of it
        // continues the branch its election was rooted at
        for instance in self.epoch_states.instances_of(epoch) {
            let previous = instance.root.previous;
            own(
                instance.account,
                instance.height,
                &|_| previous,
                &instance.slot,
            );
        }
        // What the certified state does not summarize: the same rule the
        // other validators derive this object by
        let residual = ResidualVotes::derive(&certified, votes);

        Some(EpochReport {
            committee: committee.digest(),
            certified,
            residual,
        })
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
        let mut targets: Vec<VoteTarget> = self
            .pending_kudzu_votes
            .iter()
            .filter(|target| self.account_voting && !self.frozen.contains(&target.election.epoch))
            .cloned()
            .collect();
        let empty = LocalSlotState::default();
        // One first vote and one final vote per domain: two instances of one
        // domain (conflicting parents put their children in one) are read
        // against the same slot state before either is recorded
        let mut one_shot: FxHashSet<(EpochSlot, VoteKind)> = FxHashSet::default();
        for election in self.roots.round_robin().map(|e| &e.election) {
            if !self.account_voting || self.frozen.contains(&election.epoch()) {
                continue;
            }
            let slot = self.slots.get(&election.epoch_slot()).unwrap_or(&empty);
            // A settled instance has nothing left to say but its exit final
            // vote: the settled instances of the forks pile up over time and
            // this loop runs every tick under the AEC lock
            let due = if election.state() == ElectionState::Settled {
                election.kudzu_final_vote_due(slot).into_iter().collect()
            } else {
                election.kudzu_votes_due(slot, &proposal_valid)
            };
            for (hash, kind) in due {
                if matches!(kind, VoteKind::First | VoteKind::Final)
                    && !one_shot.insert((election.epoch_slot(), kind))
                {
                    continue;
                }
                targets.push(VoteTarget {
                    election: election.id(),
                    winner: hash,
                    vote_type: VoteType::from(kind),
                });
            }
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

            let Some(election) = self.roots.election(&target.election) else {
                // The exit final vote of an election erased already
                accepted.push(target);
                continue;
            };
            if !self.account_voting || self.frozen.contains(&election.epoch()) {
                continue;
            }
            let previous = election.qualified_root().previous;
            let slot = self.slots.get_or_default(&election.epoch_slot());
            let kind = VoteKind::from(target.vote_type);
            // A first vote not cast before this node left the epoch is not cast
            // any more; one cast before is re-broadcast
            if kind == VoteKind::First && slot.stale && slot.first_voted.is_none() {
                continue;
            }
            // One first vote and one final vote per domain, whatever was read
            if (kind == VoteKind::First && slot.first_voted.is_some_and(|h| h != target.winner))
                || (kind == VoteKind::Final && slot.final_voted.is_some_and(|h| h != target.winner))
            {
                continue;
            }
            slot.mark_voted(target.winner, kind);
            self.slots.record_parent(target.winner, previous);
            accepted.push(target);
        }
        accepted
    }

    /// Kudzu: an election is erased as soon as it is finalized. Its exit final
    /// vote is kept so that the voter still broadcasts it.
    fn keep_exit_final_vote(&mut self, election: &Election) {
        #[cfg(feature = "rai_protocol")]
        {
            // Only explicitly finalized elections: an implicitly finalized block is
            // already cemented, and its slot state has been dropped.
            if !self.account_voting
                || !election.certificates().is_finalized()
                || self.frozen.contains(&election.epoch())
            {
                return;
            }
            let previous = election.qualified_root().previous;
            let slot = self.slots.get_or_default(&election.epoch_slot());
            if let Some((hash, kind)) = election.kudzu_final_vote_due(slot) {
                slot.mark_voted(hash, kind);
                self.slots.record_parent(hash, previous);
                self.pending_kudzu_votes.push(VoteTarget {
                    election: election.id(),
                    winner: hash,
                    vote_type: VoteType::from(kind),
                });
            }
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
        #[cfg(feature = "rai_protocol")]
        if self.cooldown.is_cooling_down() {
            return false;
        }
        #[cfg(feature = "rai_protocol")]
        if self.draining || self.roots.active_len() >= self.max_elections {
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
        if self.position_retained(request.block.account(), request.block.height()) {
            return Err(AecInsertError::Retained);
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
        let mut election = Election::new(
            request.block,
            epoch,
            request.behavior,
            self.base_latency,
            now,
        );
        election.set_predecessor_decided(self.predecessor_decided_for(epoch));

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
        if self.cooldown.is_cooling_down() {
            return 0;
        }
        #[cfg(feature = "rai_protocol")]
        if self.draining {
            return 0;
        }
        #[cfg(feature = "rai_protocol")]
        let current_size = self.roots.active_len() as i64;
        #[cfg(not(feature = "rai_protocol"))]
        let current_size = self.roots.len() as i64;
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
        if self.cooldown.is_cooling_down() {
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
                    Err(
                        AecInsertError::Stopped
                        | AecInsertError::Draining
                        | AecInsertError::Retained,
                    ) => {}
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
        #[cfg(feature = "rai_protocol")]
        {
            return false;
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            let Some((id, _)) = self.lowest_priority(bucket_id) else {
                return false;
            };
            if let Some(election) = self.roots.election(&id) {
                self.stats.evicted(election);
            }
            self.erase_election(&id)
        }
    }

    fn cleanup_election(&mut self, entry: Entry) {
        let election = &entry.election;
        self.keep_exit_final_vote(election);
        // A late instance's blocks are discarded - never one finalized in
        // another epoch: what an epoch finalized stays finalized
        let discarded: Vec<BlockHash> = if is_late(&self.decided, election) {
            election
                .certificates()
                .notar
                .iter()
                .filter(|block| {
                    !self.epoch_states.is_finalized(block)
                        && !self.finalized_by_checkpoint(election, block)
                })
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        if is_late(&self.decided, election) && discarded.len() < election.certificates().notar.len()
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
        #[cfg(not(feature = "rai_protocol"))]
        self.slots.remove(&election.epoch_slot());
        #[cfg(feature = "rai_protocol")]
        if self.decided.contains_key(&election.epoch()) {
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
        #[cfg(feature = "rai_protocol")]
        {
            return ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::ActiveConfirmationHeight,
            );
        }

        #[cfg(not(feature = "rai_protocol"))]
        {
            let result = if corresponding.winner().hash() == confirmed_block.hash() {
                corresponding.force_confirm();
                corresponding
                    .into_confirmed_election(now, ConfirmationType::ActiveConfirmationHeight)
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
    }

    fn block_confirmed(&mut self, block: SavedBlock, election: ConfirmedElection) {
        self.stats.block_confirmations[election.confirmation_type as usize] += 1;
        // The height is finalized, nothing will be voted for it any more.
        // RAI: the instances of the height keep running until they reach their
        // outcome, their slot states go with them (`cleanup_election`).
        #[cfg(not(feature = "rai_protocol"))]
        {
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

        #[cfg(feature = "rai_protocol")]
        self.record_votes(&args);
        let mut apply_helper = ApplyVoteHelper {
            args: &args,
            recently_confirmed: &mut self.recently_confirmed,
            stats: &mut self.stats,
            observer: &self.observer,
            roots: &mut self.roots,
            committees: &self.committees,
            decided: &self.decided,
        };
        let result = apply_helper.apply_vote();
        for entry in result.confirmed {
            self.cleanup_election(entry);
        }
        self.count_decided(&result.decided, args.now);

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
        if self.decided.contains_key(&args.vote.epoch) {
            for result in per_block.values_mut() {
                if matches!(result, Err(VoteError::Indeterminate)) {
                    *result = Err(VoteError::Late);
                }
            }
        }
        per_block
    }

    /// RAI: keeps the account votes received, by epoch and voter, for the
    /// derivation of the voters' residual objects. A vote is placed by the
    /// block's parent, read from whatever instance holds the block here,
    /// this epoch's or another's: two replicas that switched epochs at
    /// different moments hold the same block in different epochs' instances.
    /// A vote for a block this node does not hold at all waits in the vote
    /// cache and is recorded when it is replayed.
    fn record_votes(&mut self, args: &ApplyVoteArgs) {
        let vote = &args.vote;
        let kind = match vote.kind() {
            VoteKind::First => ResidualKind::First,
            VoteKind::Final => ResidualKind::Final,
        };
        for hash in vote.filtered_blocks() {
            let placed = self
                .roots
                .election_for_block(hash)
                .map(|election| {
                    (
                        election.account(),
                        election.height(),
                        election.qualified_root().previous,
                    )
                })
                .or_else(|| {
                    self.epoch_states
                        .any_instance(hash)
                        .map(|instance| (instance.account, instance.height, instance.root.previous))
                });
            let Some((account, height, previous)) = placed else {
                continue;
            };
            self.vote_records.record(
                vote.epoch,
                vote.voter,
                CertifiedBlock::new(account, height, *hash),
                kind,
                previous,
            );
        }
    }

    /// RAI: the votes of one voter received for one epoch, with the parent
    /// each voted block names
    pub fn vote_records_of(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
    ) -> Vec<(CertifiedBlock, ResidualKind, BlockHash)> {
        self.vote_records.votes_of(epoch, voter)
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

/// RAI: what one candidate of an election delegates, if it is a state block
fn delegation_of(election: &Election, hash: &BlockHash) -> Option<Delegation> {
    let block = election.candidate_blocks().get(hash)?;
    Some(Delegation {
        hash: *hash,
        representative: block.representative_field()?,
        balance: block.balance_field()?,
    })
}

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
    #[cfg(feature = "rai_protocol")]
    fn timed_boundary_freezes_and_waits_for_a_checkpoint_decision() {
        let now = Timestamp::new_test_instance();
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_duration: Duration::from_secs(1),
                ..Default::default()
            },
            Duration::from_millis(10),
        );
        container.set_genesis_committee(Vec::new());
        container.start_epochs(now);
        container.transition_time(now + Duration::from_secs(1));
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));
        assert!(container.frozen.contains(&ConsensusEpoch::ZERO));
        assert!(
            container
                .epoch_decided_state(ConsensusEpoch::ZERO)
                .is_none()
        );
        container.transition_time(now + Duration::from_secs(100));
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));
        assert!(
            container
                .epoch_decided_state(ConsensusEpoch::ZERO)
                .is_none()
        );
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn a_decided_checkpoint_installs_and_cannot_be_replaced() {
        let now = Timestamp::new_test_instance();
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_duration: Duration::from_secs(1),
                ..Default::default()
            },
            Duration::from_millis(10),
        );
        let block = SavedBlock::new_test_instance_with_key(1);
        container.set_genesis_committee(Vec::new());
        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::default()),
                now,
            )
            .unwrap();
        container.start_epochs(now);
        container.transition_time(now + Duration::from_secs(1));
        let mut state = EpochLedger::new();
        let slot = AccountSlot::new(block.account(), block.height());
        state.finalize_genesis(slot, block.hash());
        let expected = state.state_hash();
        container.install_decided_checkpoint(ConsensusEpoch::ZERO, Arc::new(state), now);
        assert_eq!(
            container
                .epoch_decided_state(ConsensusEpoch::ZERO)
                .unwrap()
                .state_hash(),
            expected
        );
        assert!(container.previous_epoch_closed(ConsensusEpoch::new(1)));
        assert!(
            container
                .election(&ElectionId::legacy(block.qualified_root()))
                .is_none()
        );
        container.install_decided_checkpoint(
            ConsensusEpoch::ZERO,
            Arc::new(EpochLedger::new()),
            now,
        );
        assert_eq!(
            container
                .epoch_decided_state(ConsensusEpoch::ZERO)
                .unwrap()
                .state_hash(),
            expected
        );
        container.install_decided_checkpoint(
            ConsensusEpoch::new(2),
            Arc::new(EpochLedger::new()),
            now,
        );
        assert!(
            container
                .epoch_decided_state(ConsensusEpoch::new(2))
                .is_none()
        );
    }

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
    #[cfg(feature = "rai_protocol")]
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
    #[cfg(feature = "rai_protocol")]
    fn no_vacancy_while_cooling_down() {
        let mut container = ActiveElectionsContainer::default();
        assert!(container.check_vacancy(&AlwaysAvailable));
        container.set_cooldown(true, AecCooldownReason::AecFactQueueFull);
        assert!(!container.check_vacancy(&AlwaysAvailable));
        container.set_cooldown(false, AecCooldownReason::AecFactQueueFull);
        assert!(container.check_vacancy(&AlwaysAvailable));
    }

    #[test]
    #[cfg(not(feature = "rai_protocol"))]
    fn legacy_vacancy_check_defers_to_the_source_during_cooldown() {
        let mut container = ActiveElectionsContainer::default();
        container.set_cooldown(true, AecCooldownReason::AecFactQueueFull);
        assert!(container.check_vacancy(&AlwaysAvailable));
        assert_eq!(container.vacancy(), 0);
    }

    #[test]
    #[cfg(not(feature = "rai_protocol"))]
    fn legacy_votes_do_not_accumulate_report_records() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::default()),
                now,
            )
            .unwrap();
        let key = PrivateKey::from(1);
        let vote = ReceivedVote::new(
            Arc::new(Vote::new_of_kind(&key, VoteKind::First, vec![block.hash()])),
            VoteDelivery::Direct,
            None,
        );
        container.apply_vote(ApplyVoteArgs {
            vote: &vote.into(),
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(container.vote_records.len(), 0);
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

    /// RAI: epoch 0 starts when the setup is over and the epochs end at the
    /// multiples of the duration from then on, the same instants on every
    /// replica; an epoch without elections does not end by time, one with
    /// elections ends at the first boundary after its first election. The
    /// boundary of an epoch waits while the epoch before it is still closing.
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
        // The boundary: left at once, the open instance notwithstanding
        container.transition_time(at(108));
        assert!(!container.is_draining());
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));
        assert!(container.election_for_block(&block.hash()).is_some());

        // The next epoch ends at the boundary after its first election (the
        // boundaries are at 100 + 8k), and waits there: epoch 0 is closing
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
        assert_eq!(container.current_epoch(), ConsensusEpoch::new(1));
    }

    /// RAI: a vote for a block without an instance in the vote's epoch starts
    /// that instance. In an epoch this node has left it casts no vote there,
    /// in the current epoch it proposes the block
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
        assert!(!due.iter().any(|target| target.election == stale));
        assert!(due.contains(&VoteTarget {
            election: current,
            winner: block.hash(),
            vote_type: VoteType::NonFinal,
        }));
        assert_eq!(due.len(), 1);
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
