use std::{
    collections::HashMap,
    fmt::Debug,
    ops::Deref,
    sync::Arc,
    time::{Duration, SystemTime},
};

use strum_macros::{EnumCount, EnumIter};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, Amount, Block, BlockHash, ConsensusEpoch, MaybeSavedBlock, PublicKey, QualifiedRoot,
    SavedBlock, UnixMillisTimestamp, Vote, VoteError, VoteKind,
};
use rsnano_utils::stats::DetailType;

use super::{
    ConfirmationType, ConfirmedElection, ElectionId, ElectionState,
    block_tallies::BlockTallies,
    committee::Committees,
    kudzu::{Certificates, LocalSlotState, SlotVotes, kudzu_state},
};
use rustc_hash::FxHashMap;

#[derive(PartialEq, Eq, Debug, Clone, Copy, Hash, EnumCount, EnumIter)]
pub enum VoteType {
    /// Legacy non-final vote. Under the Kudzu rules this is the FirstVote.
    NonFinal,
    Final,
    /// Kudzu NotarVote (second look)
    Notar,
    /// Kudzu NotarVote for the timeout block
    Timeout,
    /// RAI: the timeout vote of a replica that never proposed in the instance
    Abstain,
}

impl From<VoteKind> for VoteType {
    fn from(kind: VoteKind) -> Self {
        match kind {
            VoteKind::First => VoteType::NonFinal,
            VoteKind::Final => VoteType::Final,
            VoteKind::Notar => VoteType::Notar,
            VoteKind::Timeout => VoteType::Timeout,
            VoteKind::Abstain => VoteType::Abstain,
        }
    }
}

impl From<VoteType> for VoteKind {
    fn from(vote_type: VoteType) -> Self {
        match vote_type {
            VoteType::NonFinal => VoteKind::First,
            VoteType::Final => VoteKind::Final,
            VoteType::Notar => VoteKind::Notar,
            VoteType::Timeout => VoteKind::Timeout,
            VoteType::Abstain => VoteKind::Abstain,
        }
    }
}

/// Kudzu slot (account height) within one consensus epoch
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EpochSlot {
    pub account: Account,
    pub height: u64,
    pub epoch: ConsensusEpoch,
}

#[derive(Clone)]
pub struct Election {
    qualified_root: QualifiedRoot,
    /// RAI: the consensus epoch this election belongs to
    epoch: ConsensusEpoch,
    winner: MaybeSavedBlock,
    state: ElectionState,
    // TODO: there can't be more than 10 blocks, so an array might be a lot faster
    candidate_blocks: HashMap<BlockHash, MaybeSavedBlock>,
    votes: HashMap<PublicKey, VoteSummary>,
    winner_tally: Amount,
    winner_final_tally: Amount,

    /// All tallies (non-final or final)
    tallies: BlockTallies,
    final_tallies: BlockTallies,

    behavior: ElectionBehavior,
    has_quorum: bool,

    start: Timestamp,
    /// RAI: when the last statement arrived; evidence is solicited only while
    /// the instance still makes progress
    last_vote: Timestamp,
    /// Minimum time between broadcasts of the current winner of an election, as a backup to requesting confirmations
    base_latency: Duration,
    account: Account,
    height: u64,

    /// Kudzu vote pool (Section 4.2)
    kudzu: SlotVotes,
    /// Kudzu certificates collected so far
    certificates: Certificates,
    /// RAI: the committees of the last Kudzu tally
    committees: Option<Committees>,
    /// RAI, "Where a block may be voted on": the checkpoint of the epoch
    /// before this instance's is decided here. Until it is, "even a fast
    /// vote tally is provisional: it cannot justify a final vote"; first and
    /// notarization votes proceed.
    predecessor_decided: bool,
    /// RAI, the overlap exceptions of "Attachment and eligibility": the
    /// block may be finalized before the predecessor checkpoint is known,
    /// because it carries a verified closing-epoch NC and is complete here,
    /// or holds a predecessor-backed NC on an admissible parent
    overlap_eligible: bool,
    /// Diagnostic: when the instance got its notarization certificate
    notarized_at: Option<Timestamp>,
    /// Diagnostic: when finality was first allowed, by a decided predecessor
    /// checkpoint or an overlap exception
    eligible_at: Option<Timestamp>,
}

impl Election {
    const PASSIVE_DURATION_FACTOR: u32 = 5;
    pub const MAX_BLOCKS: usize = 10;

    pub fn new(
        block: SavedBlock,
        epoch: ConsensusEpoch,
        behavior: ElectionBehavior,
        base_latency: Duration,
        now: Timestamp,
    ) -> Self {
        Self {
            qualified_root: block.qualified_root(),
            epoch,
            votes: HashMap::new(),
            candidate_blocks: HashMap::from([(
                block.hash(),
                MaybeSavedBlock::Saved(block.clone()),
            )]),
            state: ElectionState::Passive,
            tallies: BlockTallies::new(),
            final_tallies: BlockTallies::new(),
            winner_tally: Amount::ZERO,
            winner_final_tally: Amount::ZERO,
            behavior,
            has_quorum: false,
            start: now,
            last_vote: now,
            base_latency,
            account: block.account(),
            height: block.height(),
            winner: MaybeSavedBlock::Saved(block),
            kudzu: SlotVotes::account_domain(),
            certificates: Certificates::default(),
            committees: None,
            predecessor_decided: true,
            overlap_eligible: false,
            notarized_at: None,
            eligible_at: None,
        }
    }

    pub fn new_test_instance_with(block: SavedBlock) -> Self {
        Self::new(
            block,
            ConsensusEpoch::ZERO,
            ElectionBehavior::Priority,
            Duration::from_millis(1000),
            Timestamp::new_test_instance(),
        )
    }

    pub fn qualified_root(&self) -> &QualifiedRoot {
        &self.qualified_root
    }

    pub fn epoch(&self) -> ConsensusEpoch {
        self.epoch
    }

    pub fn id(&self) -> ElectionId {
        ElectionId::new(self.qualified_root.clone(), self.epoch)
    }

    pub fn behavior(&self) -> ElectionBehavior {
        self.behavior
    }

    pub fn account(&self) -> Account {
        self.account
    }

    pub fn height(&self) -> u64 {
        self.height
    }

    /// The Kudzu slot this election belongs to
    pub fn slot(&self) -> (Account, u64) {
        (self.account, self.height)
    }

    /// Kudzu: the local slot state is per epoch, every epoch is its own Kudzu instance
    pub fn epoch_slot(&self) -> EpochSlot {
        EpochSlot {
            account: self.account,
            height: self.height,
            epoch: self.epoch,
        }
    }

    pub fn certificates(&self) -> &Certificates {
        &self.certificates
    }

    pub fn kudzu_votes(&self) -> &SlotVotes {
        &self.kudzu
    }

    /// RAI: the committees of the last Kudzu tally
    pub fn committees(&self) -> Option<&Committees> {
        self.committees.as_ref()
    }

    pub fn state(&self) -> ElectionState {
        self.state
    }

    pub fn candidate_blocks(&self) -> &HashMap<BlockHash, MaybeSavedBlock> {
        &self.candidate_blocks
    }

    pub fn contains_block(&self, hash: &BlockHash) -> bool {
        self.candidate_blocks.contains_key(hash)
    }

    pub fn block_count(&self) -> usize {
        self.candidate_blocks.len()
    }

    pub fn has_max_blocks(&self) -> bool {
        self.block_count() >= Self::MAX_BLOCKS
    }

    pub fn try_add_fork(&mut self, fork: &Block, fork_tally: Amount) -> AddForkResult {
        // Do not insert new blocks if already confirmed
        if self.state.has_ended() {
            return AddForkResult::ElectionEnded;
        }

        if self.contains_block(&fork.hash()) {
            return AddForkResult::Duplicate;
        }

        let mut removed = None;
        if self.has_max_blocks() {
            removed = self.remove_tally_below(fork_tally);
            if removed.is_none() {
                return AddForkResult::TallyTooLow;
            }
        }

        self.tallies.insert(fork.hash(), fork_tally);
        self.candidate_blocks
            .insert(fork.hash(), MaybeSavedBlock::Unsaved(fork.clone()));

        match removed {
            Some(removed) => AddForkResult::Replaced(removed),
            None => AddForkResult::Added,
        }
    }

    pub fn votes(&self) -> &HashMap<PublicKey, VoteSummary> {
        &self.votes
    }

    pub fn add_vote(
        &mut self,
        voter: PublicKey,
        hash: BlockHash,
        vote_created: UnixMillisTimestamp,
        vote_received: Timestamp,
    ) {
        debug_assert!(self.candidate_blocks.contains_key(&hash));
        self.votes.insert(
            voter,
            VoteSummary::new(voter, hash, vote_created, vote_received),
        );
    }

    /// Adds a signed vote to the Kudzu vote pool. Fails on a replay of the same
    /// (representative, kind, hash) or when the representative exceeded its
    /// notarization vote budget.
    pub fn add_kudzu_vote(
        &mut self,
        vote: &Arc<Vote>,
        hash: BlockHash,
        vote_received: Timestamp,
    ) -> Result<(), VoteError> {
        debug_assert!(self.candidate_blocks.contains_key(&hash));
        // A vote of another epoch belongs to another Kudzu instance
        if vote.epoch != self.epoch {
            return Err(VoteError::Indeterminate);
        }
        // RAI, single-support voting: an account domain has first votes and
        // final votes only. Nobody correct issues a notarization or timeout
        // vote in one, and one received supports nothing.
        if !matches!(vote.kind(), VoteKind::First | VoteKind::Final) {
            return Err(VoteError::Ignored);
        }
        self.kudzu.add(vote.voter, hash, vote.kind())?;
        self.votes.insert(
            vote.voter,
            VoteSummary::new(vote.voter, hash, vote.timestamp(), vote_received),
        );
        self.last_vote = vote_received;
        Ok(())
    }

    /// Kudzu: this node's statements for a terminated election, to be re-signed
    /// for a replica which asks for them, and the candidate blocks: a replica can
    /// only count a certificate for a block it holds, as Kudzu ships the payload
    /// fragments inside the notarization votes.
    pub fn certificate_evidence(&self, slot: &LocalSlotState) -> Option<CertificateEvidence> {
        self.certificates
            .is_terminated()
            .then(|| CertificateEvidence {
                statements: slot.statements_for(self.candidate_blocks.keys(), self.winner.hash()),
                blocks: self
                    .candidate_blocks
                    .values()
                    .map(|b| b.deref().clone())
                    .collect(),
            })
    }

    pub fn winner_tally(&self) -> Amount {
        self.winner_tally
    }

    pub fn winner_final_tally(&self) -> Amount {
        self.winner_final_tally
    }

    /// Tallies for the candidate blocks, ordered by descending tally
    pub fn tallies(&self) -> &BlockTallies {
        &self.tallies
    }

    pub fn transition_time(&mut self, now: Timestamp) {
        let duration = self.start.elapsed(now);
        match self.state {
            ElectionState::Passive => {
                if self.base_latency * Self::PASSIVE_DURATION_FACTOR < duration {
                    self.state = ElectionState::Active;
                }
            }
            ElectionState::Confirmed => {
                self.state = ElectionState::ExpiredConfirmed;
            }
            _ => {}
        }

        // Kudzu elections never expire: every slot terminates under synchrony
        if !cfg!(feature = "rai_protocol")
            && !self.state.has_ended()
            && self.behavior.time_to_live() < duration
        {
            self.state = ElectionState::ExpiredUnconfirmed;
        }
    }

    pub fn base_latency(&self) -> Duration {
        self.base_latency
    }

    /// RAI: how long a settled instance keeps asking for the final votes it
    /// lacks after the last statement reached it. They may never come: the
    /// representatives made them in another epoch's instance.
    const EVIDENCE_WINDOW: Duration = Duration::from_secs(30);

    /// Kudzu: a terminated election asks for the certificate evidence it may be
    /// missing, but only once it has been around for the passive period, so
    /// that the ordinary certificate -> finalization window is never solicited.
    /// A settled election keeps asking while its notarized block can still be
    /// finalized: the final votes it lacks may simply have been dropped. Either
    /// stops once nothing new has arrived for a while. RAI: an instance of an
    /// epoch this node has left asks at once, its epoch's close waits for it;
    /// and one settled without a block keeps asking (slowly, see the
    /// solicitor) so that a replica which missed the instance learns of it
    /// and its state agrees.
    pub fn should_solicit_evidence(&self, now: Timestamp, epoch_left: bool) -> bool {
        let collecting = match self.state {
            ElectionState::Terminated | ElectionState::TimedOut => true,
            ElectionState::Settled => {
                (epoch_left && !self.certificates.finalized().is_some())
                    || (self.kudzu_can_finalize()
                        && self.last_vote.elapsed(now) < Self::EVIDENCE_WINDOW)
            }
            _ => false,
        };
        collecting
            && (epoch_left
                || self.base_latency * Self::PASSIVE_DURATION_FACTOR < self.start.elapsed(now))
    }

    /// Kudzu: whether a finalization certificate can still form for one of the
    /// notarized blocks
    pub fn kudzu_can_finalize(&self) -> bool {
        self.certificates
            .notar
            .iter()
            .any(|hash| self.kudzu.can_finalize(hash))
    }

    pub fn has_quorum(&self) -> bool {
        self.has_quorum
    }

    /// Returns true if final votes should be generated
    pub fn is_final(&self) -> bool {
        if cfg!(feature = "rai_protocol") {
            self.kudzu_is_final()
        } else {
            self.is_confirmed() || self.has_quorum()
        }
    }

    /// Line 11: a final vote is cast at exit, i.e. once the winner is in the
    /// block tree and explicit finalization is still possible (Lemma 5.7).
    /// The per-slot precondition notarized ⊆ {winner} is checked by the caller.
    fn kudzu_is_final(&self) -> bool {
        self.has_quorum() && self.certificates.explicit_finalization_possible()
    }

    pub fn vote_type(&self) -> VoteType {
        if self.is_final() {
            VoteType::Final
        } else {
            VoteType::NonFinal
        }
    }

    pub fn cancel(&mut self) {
        if !self.state.has_ended() {
            self.state = ElectionState::Cancelled;
        }
    }

    pub fn vote_count(&self) -> usize {
        self.votes.len()
    }

    pub fn transition_active(&mut self) {
        if self.state == ElectionState::Passive {
            self.state = ElectionState::Active;
        }
    }

    pub fn maybe_upgrade_to(&mut self, new_behavior: ElectionBehavior) -> bool {
        if new_behavior != ElectionBehavior::Priority {
            // Only upgrades to priority elections are allowed to enable immediate vote broadcasting!
            return false;
        }

        if matches!(
            self.behavior,
            ElectionBehavior::Priority | ElectionBehavior::Manual
        ) {
            // Nothing to do;
            return false;
        }

        self.behavior = ElectionBehavior::Priority;
        true
    }

    pub fn is_confirmed(&self) -> bool {
        self.state.is_confirmed()
    }

    pub fn winner(&self) -> &MaybeSavedBlock {
        &self.winner
    }

    pub fn force_confirm(&mut self) -> bool {
        if !self.state.has_ended() {
            self.state = ElectionState::Confirmed;
            true
        } else {
            false
        }
    }

    /// RAI, "Only the joint decision installs its checkpoint": the decided
    /// state of the epoch finalized this candidate. The instance is
    /// confirmed with it as the winner whatever its own certificates say:
    /// the checkpoint recovered a finality the votes seen here did not
    /// show, or chose the sole survivor of the position. Returns the winner
    /// it replaces, if the winner changed.
    pub fn finalize_by_checkpoint(&mut self, hash: &BlockHash) -> Option<BlockHash> {
        if self.state.has_ended() || !self.candidate_blocks.contains_key(hash) {
            return None;
        }
        let previous = self.winner.hash();
        if previous != *hash {
            self.change_winner_to(hash);
        }
        self.state = ElectionState::Confirmed;
        Some(previous)
    }

    pub fn start(&self) -> Timestamp {
        self.start
    }

    pub fn remove_tally_below(&mut self, min_tally: Amount) -> Option<MaybeSavedBlock> {
        if min_tally.is_zero() {
            return None;
        }

        let mut block_to_remove = BlockHash::ZERO;
        let winner_hash = self.winner.hash();

        // Replace if lowest tally is below inactive cache new block weight
        if self.tallies.len() < Self::MAX_BLOCKS {
            // If count of tally items is less than 10, remove any block without tally
            for hash in self.candidate_blocks.keys() {
                if !self.tallies.contains(hash) && *hash != winner_hash {
                    block_to_remove = *hash;
                    break;
                }
            }
        }

        if block_to_remove.is_zero() {
            let (lowest_hash, lowest_tally) = self.tallies.lowest().unwrap();
            if min_tally > *lowest_tally {
                if *lowest_hash != winner_hash {
                    block_to_remove = *lowest_hash;
                } else {
                    // Avoid removing winner
                    let (second_lowest_hash, second_lowest_tally) =
                        self.tallies.iter().rev().nth(1).unwrap();

                    if min_tally > *second_lowest_tally {
                        block_to_remove = *second_lowest_hash;
                    }
                }
            }
        }

        if !block_to_remove.is_zero() {
            self.remove_block(&block_to_remove)
        } else {
            None
        }
    }

    /// Calculate tallies and try to confirm this election
    pub fn update_tallies(
        &mut self,
        rep_weights: &FxHashMap<PublicKey, Amount>,
        quorum_delta: Amount,
    ) {
        if self.state.has_ended() {
            return;
        }

        self.update_vote_weights(rep_weights);
        self.recalculate_tallies();

        if let Some(new_winner) = self.check_new_winner(quorum_delta) {
            tracing::warn!("Winner changed to {:?}!", new_winner);
            self.change_winner_to(&new_winner);
        }

        self.update_winner_tally();
        self.try_set_quorum(quorum_delta);
        self.try_confirm(quorum_delta);
    }

    fn update_vote_weights(&mut self, rep_weights: &FxHashMap<PublicKey, Amount>) {
        for vote in self.votes.values_mut() {
            vote.weight = rep_weights.get(&vote.voter).cloned().unwrap_or_default();
        }
    }

    fn recalculate_tallies(&mut self) {
        self.tallies.calculate(self.votes.values());
        self.final_tallies
            .calculate(self.votes.values().filter(|v| v.is_final_vote()));
    }

    fn check_new_winner(&self, quorum_delta: Amount) -> Option<BlockHash> {
        if self.tallies.sum() < quorum_delta {
            // The winner can only be changed after a super majority of votes has been observed!
            return None;
        }

        let old_winner = self.winner.hash();
        let new_winner = self.tallies.winner().map(|(h, _)| *h).unwrap_or(old_winner);
        if new_winner != old_winner {
            Some(new_winner)
        } else {
            None
        }
    }

    fn change_winner_to(&mut self, new_winner: &BlockHash) {
        self.winner = self.candidate_blocks().get(new_winner).unwrap().clone();
    }

    fn update_winner_tally(&mut self) {
        let winner_hash = self.winner.hash();
        self.winner_tally = self.tallies.get(&winner_hash);
        self.winner_final_tally = self.final_tallies.get(&winner_hash);
    }

    fn try_set_quorum(&mut self, quorum_delta: Amount) {
        if self.tallies.check_quorum(quorum_delta) {
            self.has_quorum = true;
        }
    }

    fn try_confirm(&mut self, quorum_delta: Amount) {
        if self.winner_final_tally >= quorum_delta {
            self.state = ElectionState::Confirmed;
        }
    }

    pub fn remove_vote(&mut self, voter: &PublicKey) {
        self.votes.remove(voter);
        self.kudzu.remove_rep(voter);
    }

    fn remove_block(&mut self, hash: &BlockHash) -> Option<MaybeSavedBlock> {
        if self.winner.hash() != *hash {
            let existing = self.candidate_blocks.remove(hash);
            if existing.is_some() {
                self.votes.retain(|_, v| v.hash != *hash);
                self.tallies.remove(hash);
                self.final_tallies.remove(hash);
                self.kudzu.remove_block(hash);
                return existing;
            }
        }

        None
    }

    /// Kudzu: recalculate tallies in the committees the instance is counted
    /// in, collect certificates and update the state. RAI: called again
    /// with other committees once the instance's epoch leaves its joint
    /// phase, so that the certificates a single committee supports form.
    pub fn update_kudzu_tallies(&mut self, committees: &Committees) {
        if self.state.has_ended() {
            return;
        }

        self.committees = Some(committees.clone());
        self.kudzu.calculate(committees);
        self.kudzu.update_certificates(&mut self.certificates);
        self.tallies = self.kudzu.notar_tallies().clone();
        self.final_tallies = self.kudzu.final_tallies().clone();

        // Line 10: B_p ← B for the block found in the complete block tree
        let tree_block = self
            .certificates
            .finalized()
            .or(self.certificates.notar.first().copied());
        if let Some(new_winner) = tree_block
            && new_winner != self.winner.hash()
        {
            tracing::warn!("Winner changed to {:?}!", new_winner);
            self.change_winner_to(&new_winner);
        }

        // RAI: no finality before the predecessor checkpoint is decided,
        // unless an overlap exception makes the block eligible. The tallies
        // stay; the certificates come out of them once it is.
        if !self.predecessor_decided && !self.overlap_eligible {
            self.certificates.fast = None;
            self.certificates.final_ = None;
        }
        self.update_winner_tally();
        self.has_quorum = self.certificates.is_notarized(&self.winner.hash());
        self.state = kudzu_state(
            self.state,
            &self.kudzu,
            &self.certificates,
            self.candidate_blocks.keys(),
            true,
        );
    }

    /// RAI: whether the checkpoint of the epoch before this instance's is
    /// decided here, which is what lets its finality be applied
    pub fn predecessor_decided(&self) -> bool {
        self.predecessor_decided
    }

    /// RAI, "Where a block may be voted on": the predecessor checkpoint is
    /// decided (or not yet). Once it is, the certificates the tallies
    /// already support come out, and the final vote may be cast.
    pub fn set_predecessor_decided(&mut self, decided: bool) {
        self.predecessor_decided = decided;
        if decided && let Some(committees) = self.committees.clone() {
            self.update_kudzu_tallies(&committees);
        }
    }

    /// Diagnostic: remember when the instance was first notarized and when
    /// its finality was first allowed
    pub fn note_milestones(&mut self, now: Timestamp) {
        if self.notarized_at.is_none() && self.certificates.has_block() {
            self.notarized_at = Some(now);
        }
        if self.eligible_at.is_none() && (self.predecessor_decided || self.overlap_eligible) {
            self.eligible_at = Some(now);
        }
    }

    /// RAI: whether an overlap exception lets this instance finalize before
    /// the predecessor checkpoint is decided
    pub fn overlap_eligible(&self) -> bool {
        self.overlap_eligible
    }

    /// RAI, "Attachment and eligibility": the block became eligible through
    /// an overlap exception. The certificates the tallies already support
    /// come out, and the final vote may be cast.
    pub fn set_overlap_eligible(&mut self, eligible: bool) {
        self.overlap_eligible = eligible;
        if eligible && let Some(committees) = self.committees.clone() {
            self.update_kudzu_tallies(&committees);
        }
    }

    /// RAI, single-support voting: "A validator may final-vote only for the
    /// block it first-voted, once that block is complete and eligible."
    /// Not before the predecessor checkpoint is decided, unless an overlap
    /// exception applies.
    pub fn kudzu_final_vote_due(&self, slot: &LocalSlotState) -> Option<(BlockHash, VoteKind)> {
        let winner = self.winner.hash();
        ((self.predecessor_decided || self.overlap_eligible)
            && self.kudzu_is_final()
            && slot.final_voted.is_none()
            && slot.first_voted == Some(winner))
        .then_some((winner, VoteKind::Final))
    }

    /// Protocol 1 for this node's representatives: the votes to broadcast now,
    /// given what was already cast for this slot. Previously cast votes are
    /// included so that they get re-broadcast until the election is finalized.
    /// `proposal_valid` tells whether the winner is a valid proposal here
    /// (line 18): under RAI a block whose dependencies are not finalized yet
    /// is not first voted; a replica that lags cements them later and votes
    /// then, or never does, which the others' certificate does not need.
    pub fn kudzu_votes_due(
        &self,
        slot: &LocalSlotState,
        proposal_valid: impl Fn(&BlockHash) -> bool,
    ) -> Vec<(BlockHash, VoteKind)> {
        let finalized = self.is_confirmed();
        if self.state.has_ended() && !finalized {
            return Vec::new();
        }
        let winner = self.winner.hash();
        // A finalized election is about to be erased, only the exit vote matters
        let mut due: Vec<_> = if finalized {
            Vec::new()
        } else {
            slot.cast_votes().collect()
        };

        // A replica issues at most one first vote per slot: the valid
        // proposal, if it has one. RAI: there is no account timeout vote of
        // any kind; an unresolved position is carried to the epoch decision.
        if !finalized && slot.first_voted.is_none() {
            // Lines 18–21: first vote for the valid proposal, i.e. our ledger
            // block whose dependencies are finalized
            let proposable = !slot.stale
                && matches!(self.winner, MaybeSavedBlock::Saved(_))
                && proposal_valid(&winner);
            if proposable {
                due.push((winner, VoteKind::First));
            }
        }

        // The exit: a final vote for the first-voted block once it is
        // complete and eligible. Cast even when the block was already fast
        // finalized here, because replicas which missed a first vote depend
        // on it (the 3δ path). There is no second look in an account domain:
        // "it issues no separate account notarization votes and never
        // second-looks a rival".
        if let Some(final_vote) = self.kudzu_final_vote_due(slot) {
            due.push(final_vote);
        }
        due
    }

    /// TODO: Remove as soon as possible
    pub fn change_received_timestamp(&mut self, voter: &PublicKey, new_timestamp: Timestamp) {
        self.votes.get_mut(voter).unwrap().vote_received = new_timestamp;
    }

    pub fn into_confirmed_election(
        &self,
        now: Timestamp,
        result: ConfirmationType,
    ) -> ConfirmedElection {
        let votes = self.votes().clone();

        ConfirmedElection {
            winner: self.winner().clone(),
            tally: self.winner_tally(),
            final_tally: self.winner_final_tally(),
            block_count: self.block_count() as u32,
            voter_count: self.votes().len() as u32,
            election_duration: self.start().elapsed(now),
            notarized_after: self.notarized_at.map(|at| self.start.elapsed(at)),
            eligible_after: self.eligible_at.map(|at| self.start.elapsed(at)),
            election_end: SystemTime::now(),
            confirmation_type: result,
            votes,
        }
    }
}

/// Kudzu: what a replica hands out for a terminated election
#[derive(Clone, Debug)]
pub struct CertificateEvidence {
    /// The node's own statements, by kind, to be signed by each of its representatives
    pub statements: Vec<(VoteKind, Vec<BlockHash>)>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteSummary {
    pub voter: PublicKey,
    pub vote_created: UnixMillisTimestamp,
    pub vote_received: Timestamp, // TODO use Instant
    pub hash: BlockHash,
    pub weight: Amount,
}

impl VoteSummary {
    pub fn new(
        voter: PublicKey,
        hash: BlockHash,
        vote_created: UnixMillisTimestamp,
        vote_received: Timestamp,
    ) -> Self {
        Self {
            voter,
            vote_received,
            vote_created,
            hash,
            weight: Amount::ZERO,
        }
    }

    pub fn is_final_vote(&self) -> bool {
        self.vote_created == UnixMillisTimestamp::MAX
    }

    pub fn ensure_no_replay(
        &self,
        new_vote: &Vote,
        block_hash: &BlockHash,
    ) -> Result<(), VoteError> {
        if self.vote_created > new_vote.timestamp() {
            Err(VoteError::Replay)
        } else if self.vote_created == new_vote.timestamp() && self.hash >= *block_hash {
            Err(VoteError::Replay)
        } else {
            Ok(())
        }
    }

    pub fn has_switched_to_final_vote(&self, new_vote: &Vote) -> bool {
        new_vote.is_final() && self.vote_created < new_vote.timestamp()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, EnumCount, EnumIter)]
pub enum ElectionBehavior {
    Manual,
    Priority,
    /**
     * Hinted elections:
     * - shorter timespan
     * - limited space inside AEC
     */
    Hinted,
    /**
     * Optimistic elections:
     * - shorter timespan
     * - limited space inside AEC
     * - more frequent confirmation requests
     */
    Optimistic,
}

impl ElectionBehavior {
    fn time_to_live(&self) -> Duration {
        match self {
            ElectionBehavior::Manual | ElectionBehavior::Priority => Duration::from_mins(5),
            ElectionBehavior::Hinted | ElectionBehavior::Optimistic => Duration::from_secs(30),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ElectionBehavior::Manual => "manual",
            ElectionBehavior::Priority => "priority",
            ElectionBehavior::Hinted => "hinted",
            ElectionBehavior::Optimistic => "optimistic",
        }
    }
}

impl From<ElectionBehavior> for DetailType {
    fn from(value: ElectionBehavior) -> Self {
        match value {
            ElectionBehavior::Manual => DetailType::Manual,
            ElectionBehavior::Priority => DetailType::Priority,
            ElectionBehavior::Hinted => DetailType::Hinted,
            ElectionBehavior::Optimistic => DetailType::Optimistic,
        }
    }
}

pub enum AddForkResult {
    Added,
    Replaced(MaybeSavedBlock),
    TallyTooLow,
    Duplicate,
    ElectionEnded,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::{Committee, SlotOutcome, slot_outcome};
    use rsnano_types::{PrivateKey, StateBlockArgs};

    #[test]
    fn first_vote_is_due_for_our_ledger_block_and_only_once() {
        let (election, block, _) = election_with_fork();
        let mut slot = LocalSlotState::default();

        assert_eq!(
            election.kudzu_votes_due(&slot, |_| true),
            vec![(block, VoteKind::First)]
        );

        slot.mark_voted(block, VoteKind::First);
        // The first vote is re-broadcast but not decided again
        assert_eq!(
            election.kudzu_votes_due(&slot, |_| true),
            vec![(block, VoteKind::First)]
        );
    }

    /// RAI, single-support voting: an account domain has first and final
    /// votes only; a notarization, timeout or abstaining vote received
    /// there supports nothing
    #[test]
    fn an_account_domain_has_first_and_final_votes_only() {
        let (mut election, block, _) = election_with_fork();
        let committees = committees(&[(1, 40), (2, 27), (3, 33)]);
        for kind in [VoteKind::Notar, VoteKind::Timeout, VoteKind::Abstain] {
            assert_eq!(
                try_vote(&mut election, 1, block, kind),
                Err(VoteError::Ignored)
            );
        }
        vote(&mut election, 1, block, VoteKind::First);
        election.update_kudzu_tallies(&committees);
        assert!(!election.has_quorum());
        assert!(!election.certificates().timeout);
        assert_eq!(election.winner_tally(), Amount::raw(40));
    }

    /// RAI, single-support voting: a rival with many first votes gets no
    /// second look. Split first votes make no certificate, and the domain
    /// settles unresolved, for the checkpoint or an uncontested child.
    #[test]
    fn no_second_look_in_an_account_domain() {
        let (mut election, block, fork) = election_with_fork();
        let mut slot = LocalSlotState::default();
        slot.mark_voted(block, VoteKind::First);
        vote(&mut election, 1, block, VoteKind::First);
        first_votes(&mut election, fork, &[2, 3]);
        let committees = committees(&[(1, 40), (2, 27), (3, 33)]);
        election.update_kudzu_tallies(&committees);

        assert_eq!(
            election.kudzu_votes_due(&slot, |_| true),
            vec![(block, VoteKind::First)]
        );
        assert!(!election.has_quorum());
        assert!(election.certificates().notar.is_empty());
        assert_eq!(election.kudzu_final_vote_due(&slot), None);
        // Everybody voted and neither block can reach a certificate
        assert_eq!(election.state(), ElectionState::Settled);
        assert_eq!(
            slot_outcome(election.state(), election.certificates()),
            SlotOutcome::Empty
        );
        assert!(!election.kudzu_can_finalize());
    }

    /// RAI, single-support voting: the final vote goes to the first-voted
    /// block only. A validator that first-voted nothing in the domain, or
    /// the rival, final-votes nothing.
    #[test]
    fn the_final_vote_is_for_the_first_voted_block_only() {
        let (mut election, block, fork) = election_with_fork();
        notarization_certificate(&mut election, block);
        election.update_kudzu_tallies(&committees(&[(1, 40), (2, 27), (3, 33)]));
        assert!(election.has_quorum());

        assert_eq!(
            election.kudzu_final_vote_due(&LocalSlotState::default()),
            None
        );
        let mut rival = LocalSlotState::default();
        rival.mark_voted(fork, VoteKind::First);
        assert_eq!(election.kudzu_final_vote_due(&rival), None);
        let mut supporter = LocalSlotState::default();
        supporter.mark_voted(block, VoteKind::First);
        assert_eq!(
            election.kudzu_final_vote_due(&supporter),
            Some((block, VoteKind::Final))
        );
    }

    /// A four-two split: the majority's first votes notarize their block,
    /// and their final votes finalize it; the minority can not add to
    /// either
    #[test]
    fn a_four_two_fork_finalizes_by_the_first_voters_final_votes() {
        let (mut election, block, fork) = election_with_fork();
        let committees = committees(&[(1, 40), (2, 40), (3, 40), (4, 40), (5, 40), (6, 40)]);
        first_votes(&mut election, block, &[1, 2, 3, 4]);
        first_votes(&mut election, fork, &[5, 6]);
        // Only two of the four final votes arrived here
        for rep in 1..=2 {
            vote(&mut election, rep, block, VoteKind::Final);
        }
        election.update_kudzu_tallies(&committees);
        assert_eq!(election.certificates().notar, vec![block]);
        assert_eq!(election.state(), ElectionState::Settled);
        assert!(!election.is_confirmed());

        // Representatives 3 and 4 first-voted the block, so their final
        // votes can still finalize it and the election keeps asking
        assert!(election.kudzu_can_finalize());
        let later = election.start() + election.base_latency() * 10;
        assert!(election.should_solicit_evidence(later, false));
        // but not before the passive period, unless its epoch was left
        let early = election.start() + election.base_latency();
        assert!(!election.should_solicit_evidence(early, false));
        assert!(election.should_solicit_evidence(early, true));
        // and not for good: the final votes may have been cast in another
        // epoch's instance and never come
        let much_later = election.start() + Election::EVIDENCE_WINDOW + Duration::from_secs(1);
        assert!(!election.should_solicit_evidence(much_later, false));

        // The minority's final votes count for nothing: they first-voted
        // the fork, and their final votes are for it
        for rep in 5..=6 {
            vote(&mut election, rep, fork, VoteKind::Final);
        }
        election.update_kudzu_tallies(&committees);
        assert!(!election.is_confirmed());
        for rep in 3..=4 {
            vote(&mut election, rep, block, VoteKind::Final);
        }
        election.update_kudzu_tallies(&committees);
        assert_eq!(election.state(), ElectionState::Confirmed);
        assert_eq!(election.certificates().final_, Some(block));
    }

    #[test]
    fn notarization_certificate_settles_and_triggers_the_final_vote() {
        let (mut election, block, _) = election_with_fork();
        let mut slot = LocalSlotState::default();
        slot.mark_voted(block, VoteKind::First);

        notarization_certificate(&mut election, block);
        election.update_kudzu_tallies(&committees(&[(1, 40), (2, 27), (3, 33)]));

        // Settled at once: the 33 left can not notarize anything else
        assert_eq!(election.state(), ElectionState::Settled);
        assert!(election.state().is_terminated());
        assert!(election.has_quorum());
        assert_eq!(election.certificates().notar, vec![block]);
        assert_eq!(election.winner_tally(), Amount::raw(67));
        assert_eq!(
            election.kudzu_votes_due(&slot, |_| true),
            vec![(block, VoteKind::First), (block, VoteKind::Final)]
        );

        slot.mark_voted(block, VoteKind::Final);
        assert_eq!(
            election.kudzu_votes_due(&slot, |_| true),
            vec![(block, VoteKind::First), (block, VoteKind::Final)]
        );
    }

    #[test]
    fn fast_finalization_by_first_votes_alone() {
        let (mut election, block, _) = election_with_fork();
        first_votes(&mut election, block, &[1, 2, 3]);
        election.update_kudzu_tallies(&committees(&[(1, 40), (2, 27), (3, 20)]));

        assert_eq!(election.state(), ElectionState::Confirmed);
        assert!(election.is_confirmed());
        assert_eq!(election.certificates().fast, Some(block));
        assert!(election.certificates().final_.is_none());
        // The exit final vote still applies: replicas which missed a first
        // vote need it. For the first-voted block only.
        let mut slot = LocalSlotState::default();
        slot.mark_voted(block, VoteKind::First);
        assert_eq!(
            election.kudzu_votes_due(&slot, |_| true),
            vec![(block, VoteKind::Final)]
        );
        assert!(
            election
                .kudzu_votes_due(&LocalSlotState::default(), |_| true)
                .is_empty()
        );
    }

    #[test]
    fn finalization_certificate_confirms() {
        let (mut election, block, _) = election_with_fork();
        notarization_certificate(&mut election, block);
        let committees = committees(&[(1, 40), (2, 27), (3, 33)]);
        election.update_kudzu_tallies(&committees);
        assert_eq!(election.state(), ElectionState::Settled);

        vote(&mut election, 1, block, VoteKind::Final);
        election.update_kudzu_tallies(&committees);
        assert_eq!(election.state(), ElectionState::Settled);

        vote(&mut election, 2, block, VoteKind::Final);
        election.update_kudzu_tallies(&committees);
        assert_eq!(election.state(), ElectionState::Confirmed);
        assert_eq!(election.certificates().final_, Some(block));
        assert_eq!(election.winner_final_tally(), Amount::raw(67));
    }

    /// RAI, "Where a block may be voted on": until the predecessor checkpoint
    /// is decided, no final vote is cast and a fast tally stays provisional;
    /// once it is, both come out of the votes already held
    #[test]
    fn no_finality_before_the_predecessor_checkpoint_is_decided() {
        let (mut election, block, _) = election_with_fork();
        let mut slot = LocalSlotState::default();
        slot.mark_voted(block, VoteKind::First);
        election.set_predecessor_decided(false);
        let committees = committees(&[(1, 40), (2, 27), (3, 33)]);

        // A notarization certificate, but no final vote
        first_votes(&mut election, block, &[1, 2]);
        election.update_kudzu_tallies(&committees);
        assert!(election.has_quorum());
        assert_eq!(election.kudzu_final_vote_due(&slot), None);

        // Every first vote: a fast certificate in Kudzu, provisional here
        vote(&mut election, 3, block, VoteKind::First);
        election.update_kudzu_tallies(&committees);
        assert!(!election.certificates().is_finalized());
        assert_ne!(election.state(), ElectionState::Confirmed);

        // Decided: the fast certificate is applied and the final vote due
        election.set_predecessor_decided(true);
        assert_eq!(election.certificates().fast, Some(block));
        assert_eq!(election.state(), ElectionState::Confirmed);
    }

    #[test]
    fn settled_once_no_other_notarization_certificate_can_form() {
        let (mut election, block, fork) = election_with_fork();
        let committees = committees(&[(1, 40), (2, 27), (3, 33)]);
        notarization_certificate(&mut election, block);
        election.update_kudzu_tallies(&committees);
        assert_eq!(election.state(), ElectionState::Settled);

        vote(&mut election, 3, fork, VoteKind::First);
        vote(&mut election, 1, block, VoteKind::Final);
        election.update_kudzu_tallies(&committees);
        assert_eq!(election.state(), ElectionState::Settled);
        assert!(!election.is_confirmed());
        assert!(election.state().is_terminated());
    }

    #[test]
    fn a_certificate_from_first_votes_alone_settles_immediately() {
        // 67 first voted the block, the remaining 33 cannot give any other block
        // a second look (needs 39) nor a certificate
        let (mut election, block, _) = election_with_fork();
        first_votes(&mut election, block, &[1, 2]);
        election.update_kudzu_tallies(&committees(&[(1, 40), (2, 27), (3, 33)]));
        assert_eq!(election.state(), ElectionState::Settled);
        assert!(election.has_quorum());
    }

    #[test]
    fn certificates_survive_weight_changes() {
        let (mut election, block, _) = election_with_fork();
        notarization_certificate(&mut election, block);
        election.update_kudzu_tallies(&committees(&[(1, 40), (2, 27), (3, 33)]));
        assert_eq!(election.state(), ElectionState::Settled);

        // With every weight one, the one left may still make a certificate
        election.update_kudzu_tallies(&committees(&[(1, 1), (2, 1), (3, 1)]));
        assert!(election.state().is_terminated());
        assert!(election.has_quorum());
    }

    #[test]
    fn kudzu_elections_do_not_expire() {
        let (mut election, _, _) = election_with_fork();
        let later = Timestamp::new_test_instance() + Duration::from_secs(60 * 60);
        election.transition_time(later);
        if cfg!(feature = "rai_protocol") {
            assert_eq!(election.state(), ElectionState::Active);
        } else {
            assert_eq!(election.state(), ElectionState::ExpiredUnconfirmed);
        }
    }

    /// RAI: in an instance of an epoch this node has left without proposing
    /// it casts no first vote of any kind. Nor a second look: a second look
    /// follows a first vote.
    #[test]
    fn stale_instance_casts_no_first_vote() {
        let (mut election, block, _) = election_with_fork();
        let slot = LocalSlotState::stale();
        assert!(election.kudzu_votes_due(&slot, |_| true).is_empty());

        notarization_certificate(&mut election, block);
        election.update_kudzu_tallies(&committees(&[(1, 40), (2, 27), (3, 33)]));
        assert!(election.has_quorum());
        assert!(
            !election
                .kudzu_votes_due(&slot, |_| true)
                .iter()
                .any(|(_, kind)| matches!(kind, VoteKind::First | VoteKind::Notar))
        );
    }

    #[test]
    fn replayed_kudzu_votes_are_rejected() {
        let (mut election, block, _) = election_with_fork();
        assert_eq!(try_vote(&mut election, 1, block, VoteKind::First), Ok(()));
        assert_eq!(
            try_vote(&mut election, 1, block, VoteKind::First),
            Err(VoteError::Replay)
        );
        assert_eq!(try_vote(&mut election, 1, block, VoteKind::Final), Ok(()));
        assert_eq!(
            try_vote(&mut election, 1, block, VoteKind::Final),
            Err(VoteError::Replay)
        );
        assert_eq!(election.vote_count(), 1);
    }

    /*
     * Test helpers
     */

    /// Thresholds for an online weight of 100: certificate 62, fast 81, many 39
    /// A single committee of the given weights, n their sum: certificate
    /// 62%, fast 81%, many 38% + 1
    fn committees(entries: &[(u64, u128)]) -> Committees {
        Committees::single(Arc::new(Committee::new(weights(entries))))
    }

    fn election_with_fork() -> (Election, BlockHash, BlockHash) {
        let args = StateBlockArgs::new_test_instance();
        let block = SavedBlock::new_test_instance_with(args.clone().into());
        let fork: Block = StateBlockArgs {
            representative: 999.into(),
            ..args
        }
        .into();
        let mut election = Election::new_test_instance_with(block.clone());
        election.try_add_fork(&fork, Amount::ZERO);
        (election, block.hash(), fork.hash())
    }

    fn try_vote(
        election: &mut Election,
        rep: u64,
        hash: BlockHash,
        kind: VoteKind,
    ) -> Result<(), VoteError> {
        let vote = Arc::new(Vote::new_of_kind_at(
            &PrivateKey::from(rep),
            kind,
            UnixMillisTimestamp::new(1000),
            vec![hash],
        ));
        election.add_kudzu_vote(&vote, hash, Timestamp::new_test_instance())
    }

    fn vote(election: &mut Election, rep: u64, hash: BlockHash, kind: VoteKind) {
        try_vote(election, rep, hash, kind).unwrap();
    }

    fn first_votes(election: &mut Election, hash: BlockHash, reps: &[u64]) {
        for rep in reps {
            vote(election, *rep, hash, VoteKind::First);
        }
    }

    /// Reps 1 (40) and 2 (27) first vote: a notarization certificate with
    /// rep 3 (33) still to vote
    fn notarization_certificate(election: &mut Election, hash: BlockHash) {
        first_votes(election, hash, &[1, 2]);
    }

    fn weights(entries: &[(u64, u128)]) -> FxHashMap<PublicKey, Amount> {
        entries
            .iter()
            .map(|(r, w)| (PrivateKey::from(*r).public_key(), Amount::raw(*w)))
            .collect()
    }
}
