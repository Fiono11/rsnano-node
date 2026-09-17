use std::{
    collections::HashMap,
    fmt::Debug,
    time::{Duration, SystemTime},
};

use strum_macros::{EnumCount, EnumIter};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, Amount, Block, BlockHash, MaybeSavedBlock, PublicKey, QualifiedRoot, SavedBlock,
    UnixMillisTimestamp, Vote, VoteError, VoteKind,
};
use rsnano_utils::stats::DetailType;

use super::{
    ConfirmationType, ConfirmedElection, ElectionState,
    block_tallies::BlockTallies,
    kudzu::{Certificates, KudzuThresholds, LocalSlotState, SlotVotes},
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
}

impl From<VoteKind> for VoteType {
    fn from(kind: VoteKind) -> Self {
        match kind {
            VoteKind::First => VoteType::NonFinal,
            VoteKind::Final => VoteType::Final,
            VoteKind::Notar => VoteType::Notar,
            VoteKind::Timeout => VoteType::Timeout,
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
        }
    }
}

#[derive(Clone)]
pub struct Election {
    qualified_root: QualifiedRoot,
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
    /// Minimum time between broadcasts of the current winner of an election, as a backup to requesting confirmations
    base_latency: Duration,
    account: Account,
    height: u64,

    /// Kudzu vote pool (Section 4.2)
    kudzu: SlotVotes,
    /// Kudzu certificates collected so far
    certificates: Certificates,
    /// Thresholds used for the last Kudzu tally
    thresholds: Option<KudzuThresholds>,
}

impl Election {
    const PASSIVE_DURATION_FACTOR: u32 = 5;
    pub const MAX_BLOCKS: usize = 10;

    pub fn new(
        block: SavedBlock,
        behavior: ElectionBehavior,
        base_latency: Duration,
        now: Timestamp,
    ) -> Self {
        Self {
            qualified_root: block.qualified_root(),
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
            base_latency,
            account: block.account(),
            height: block.height(),
            winner: MaybeSavedBlock::Saved(block),
            kudzu: SlotVotes::default(),
            certificates: Certificates::default(),
            thresholds: None,
        }
    }

    pub fn new_test_instance_with(block: SavedBlock) -> Self {
        Self::new(
            block,
            ElectionBehavior::Priority,
            Duration::from_millis(1000),
            Timestamp::new_test_instance(),
        )
    }

    pub fn qualified_root(&self) -> &QualifiedRoot {
        &self.qualified_root
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

    pub fn certificates(&self) -> &Certificates {
        &self.certificates
    }

    pub fn kudzu_votes(&self) -> &SlotVotes {
        &self.kudzu
    }

    pub fn thresholds(&self) -> Option<&KudzuThresholds> {
        self.thresholds.as_ref()
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

    /// Adds a vote to the Kudzu vote pool. Fails on a replay of the same
    /// (representative, kind, hash) or when the representative exceeded its
    /// notarization vote budget.
    pub fn add_kudzu_vote(
        &mut self,
        voter: PublicKey,
        hash: BlockHash,
        kind: VoteKind,
        vote_created: UnixMillisTimestamp,
        vote_received: Timestamp,
    ) -> Result<(), VoteError> {
        debug_assert!(self.candidate_blocks.contains_key(&hash));
        self.kudzu.add(voter, hash, kind)?;
        self.votes.insert(
            voter,
            VoteSummary::new(voter, hash, vote_created, vote_received),
        );
        Ok(())
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

    /// Kudzu: recalculate tallies, collect certificates and update the state
    pub fn update_kudzu_tallies(
        &mut self,
        rep_weights: &FxHashMap<PublicKey, Amount>,
        thresholds: KudzuThresholds,
    ) {
        if self.state.has_ended() {
            return;
        }

        self.thresholds = Some(thresholds);
        self.kudzu.calculate(rep_weights);
        self.kudzu
            .update_certificates(&thresholds, &mut self.certificates);
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

        self.update_winner_tally();
        self.has_quorum = self.certificates.is_notarized(&self.winner.hash());
        self.state = self.kudzu_state(&thresholds);
    }

    /// Line 11: the final vote to cast at exit, if any
    pub fn kudzu_final_vote_due(&self, slot: &LocalSlotState) -> Option<(BlockHash, VoteKind)> {
        let winner = self.winner.hash();
        (self.kudzu_is_final() && slot.final_voted.is_none() && slot.notarized_only(&winner))
            .then_some((winner, VoteKind::Final))
    }

    fn kudzu_state(&self, thresholds: &KudzuThresholds) -> ElectionState {
        let certs = &self.certificates;
        if certs.is_finalized() {
            ElectionState::Confirmed
        } else if certs.has_block() {
            if self
                .kudzu
                .is_settled(thresholds, certs, self.candidate_blocks.keys())
            {
                ElectionState::Settled
            } else {
                ElectionState::Terminated
            }
        } else if certs.timeout {
            ElectionState::TimedOut
        } else {
            self.state
        }
    }

    /// Protocol 1 for this node's representatives: the votes to broadcast now,
    /// given what was already cast for this slot. Previously cast votes are
    /// included so that they get re-broadcast until the election is finalized.
    pub fn kudzu_votes_due(&self, slot: &LocalSlotState) -> Vec<(BlockHash, VoteKind)> {
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

        // Lines 18–21: first vote for the valid proposal, i.e. our ledger block
        if !finalized
            && slot.first_voted.is_none()
            && matches!(self.winner, MaybeSavedBlock::Saved(_))
        {
            due.push((winner, VoteKind::First));
        }

        // Lines 9–11: exit with the tree block and a final vote if notarized ⊆ {B}.
        // This is cast even when the block was already fast finalized here, because
        // replicas which missed a first vote depend on it (the 3δ path).
        if let Some(final_vote) = self.kudzu_final_vote_due(slot) {
            due.push(final_vote);
        }

        // Lines 28–35 only run while the slot is not done
        if let Some(thresholds) = &self.thresholds
            && slot.first_voted.is_some()
            && !self.certificates.is_terminated()
        {
            for block in self.kudzu.many_votes(thresholds) {
                if self.candidate_blocks.contains_key(&block) && !slot.looked_at(&block) {
                    due.push((block, VoteKind::Notar));
                }
            }
            if slot.timeout_voted.is_none() && self.kudzu.should_timeout(thresholds) {
                due.push((winner, VoteKind::Timeout));
            }
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
            election_end: SystemTime::now(),
            confirmation_type: result,
            votes,
        }
    }
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
    use rsnano_types::{PrivateKey, StateBlockArgs};

    #[test]
    fn first_vote_is_due_for_our_ledger_block_and_only_once() {
        let (election, block, _) = election_with_fork();
        let mut slot = LocalSlotState::default();

        assert_eq!(
            election.kudzu_votes_due(&slot),
            vec![(block, VoteKind::First)]
        );

        slot.mark_voted(block, VoteKind::First);
        // The first vote is re-broadcast but not decided again
        assert_eq!(
            election.kudzu_votes_due(&slot),
            vec![(block, VoteKind::First)]
        );
    }

    #[test]
    fn notarization_certificate_terminates_and_triggers_the_final_vote() {
        let (mut election, block, _) = election_with_fork();
        let mut slot = LocalSlotState::default();
        slot.mark_voted(block, VoteKind::First);

        notarization_certificate(&mut election, block);
        election.update_kudzu_tallies(&weights(&[(1, 40), (2, 27), (3, 33)]), thresholds());

        assert_eq!(election.state(), ElectionState::Terminated);
        assert!(election.has_quorum());
        assert_eq!(election.certificates().notar, vec![block]);
        assert_eq!(election.winner_tally(), Amount::raw(67));
        assert_eq!(
            election.kudzu_votes_due(&slot),
            vec![(block, VoteKind::First), (block, VoteKind::Final)]
        );

        slot.mark_voted(block, VoteKind::Final);
        assert_eq!(
            election.kudzu_votes_due(&slot),
            vec![(block, VoteKind::First), (block, VoteKind::Final)]
        );
    }

    #[test]
    fn fast_finalization_by_first_votes_alone() {
        let (mut election, block, _) = election_with_fork();
        first_votes(&mut election, block, &[1, 2, 3]);
        election.update_kudzu_tallies(&weights(&[(1, 40), (2, 27), (3, 20)]), thresholds());

        assert_eq!(election.state(), ElectionState::Confirmed);
        assert!(election.is_confirmed());
        assert_eq!(election.certificates().fast, Some(block));
        assert!(election.certificates().final_.is_none());
        // Line 11 still applies at exit: replicas which missed a first vote need it
        assert_eq!(
            election.kudzu_votes_due(&LocalSlotState::default()),
            vec![(block, VoteKind::Final)]
        );
    }

    #[test]
    fn finalization_certificate_confirms() {
        let (mut election, block, _) = election_with_fork();
        notarization_certificate(&mut election, block);
        let weights = weights(&[(1, 40), (2, 27), (3, 33)]);
        election.update_kudzu_tallies(&weights, thresholds());
        assert_eq!(election.state(), ElectionState::Terminated);

        vote(&mut election, 1, block, VoteKind::Final);
        election.update_kudzu_tallies(&weights, thresholds());
        assert_eq!(election.state(), ElectionState::Terminated);

        vote(&mut election, 2, block, VoteKind::Final);
        election.update_kudzu_tallies(&weights, thresholds());
        assert_eq!(election.state(), ElectionState::Confirmed);
        assert_eq!(election.certificates().final_, Some(block));
        assert_eq!(election.winner_final_tally(), Amount::raw(67));
    }

    #[test]
    fn second_look_notarizes_a_fork_with_many_first_votes_and_forbids_the_final_vote() {
        let (mut election, block, fork) = election_with_fork();
        let mut slot = LocalSlotState::default();
        slot.mark_voted(block, VoteKind::First);

        first_votes(&mut election, fork, &[2, 3]);
        let weights = weights(&[(1, 40), (2, 27), (3, 33)]);
        election.update_kudzu_tallies(&weights, thresholds());
        assert_eq!(election.state(), ElectionState::Passive);
        assert_eq!(
            election.kudzu_votes_due(&slot),
            vec![(block, VoteKind::First), (fork, VoteKind::Notar)]
        );

        slot.mark_voted(fork, VoteKind::Notar);
        vote(&mut election, 1, fork, VoteKind::Notar);
        election.update_kudzu_tallies(&weights, thresholds());
        // The fork is now in the block tree and became the winner
        assert_eq!(election.state(), ElectionState::Terminated);
        assert_eq!(election.winner().hash(), fork);
        // notarized = {block, fork} ⊄ {fork}: no final vote
        assert_eq!(
            election.kudzu_votes_due(&slot),
            vec![(block, VoteKind::First), (fork, VoteKind::Notar)]
        );
    }

    #[test]
    fn second_look_is_not_taken_before_the_first_vote() {
        let (mut election, _, fork) = election_with_fork();
        first_votes(&mut election, fork, &[2, 3]);
        election.update_kudzu_tallies(&weights(&[(1, 40), (2, 27), (3, 33)]), thresholds());

        let due = election.kudzu_votes_due(&LocalSlotState::default());
        assert!(!due.iter().any(|(_, kind)| *kind == VoteKind::Notar));
    }

    #[test]
    fn split_first_votes_trigger_the_timeout_vote() {
        let (mut election, block, fork) = election_with_fork();
        let mut slot = LocalSlotState::default();
        slot.mark_voted(block, VoteKind::First);

        vote(&mut election, 1, block, VoteKind::First);
        vote(&mut election, 2, fork, VoteKind::First);
        vote(&mut election, 3, fork, VoteKind::First);
        // allVotes − maxVotes = 100 − 60 = 40 ≥ 34
        let weights = weights(&[(1, 40), (2, 27), (3, 33)]);
        election.update_kudzu_tallies(&weights, thresholds());

        let due = election.kudzu_votes_due(&slot);
        assert!(due.contains(&(block, VoteKind::Timeout)));

        slot.mark_voted(block, VoteKind::Timeout);
        assert!(
            election
                .kudzu_votes_due(&slot)
                .contains(&(block, VoteKind::Timeout))
        );
    }

    #[test]
    fn timeout_certificate_terminates_without_a_block_and_rules_out_explicit_finalization() {
        let (mut election, block, _) = election_with_fork();
        let mut slot = LocalSlotState::default();
        slot.mark_voted(block, VoteKind::First);
        let weights = weights(&[(1, 40), (2, 27), (3, 33)]);

        vote(&mut election, 1, block, VoteKind::Timeout);
        vote(&mut election, 2, block, VoteKind::Timeout);
        election.update_kudzu_tallies(&weights, thresholds());
        assert_eq!(election.state(), ElectionState::TimedOut);
        assert!(election.certificates().timeout);
        assert!(!election.has_quorum());
        assert!(election.state().is_terminated());

        // Lines 28–35 no longer run: no timeout vote from us even if the rule holds
        let due = election.kudzu_votes_due(&slot);
        assert_eq!(due, vec![(block, VoteKind::First)]);

        // A late notarization certificate still puts the block into the tree
        notarization_certificate(&mut election, block);
        election.update_kudzu_tallies(&weights, thresholds());
        assert_eq!(election.state(), ElectionState::Terminated);
        assert!(election.has_quorum());
        assert_eq!(election.certificates().notar, vec![block]);
        // but explicit finalization is impossible
        assert!(!election.certificates().explicit_finalization_possible());
        assert!(
            !election
                .kudzu_votes_due(&slot)
                .contains(&(block, VoteKind::Final))
        );
    }

    #[test]
    fn settled_once_no_other_notarization_certificate_can_form() {
        let (mut election, block, fork) = election_with_fork();
        let weights = weights(&[(1, 40), (2, 27), (3, 33)]);
        notarization_certificate(&mut election, block);
        election.update_kudzu_tallies(&weights, thresholds());
        assert_eq!(election.state(), ElectionState::Terminated);

        vote(&mut election, 3, fork, VoteKind::First);
        vote(&mut election, 1, block, VoteKind::Final);
        election.update_kudzu_tallies(&weights, thresholds());
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
        election.update_kudzu_tallies(&weights(&[(1, 40), (2, 27), (3, 33)]), thresholds());
        assert_eq!(election.state(), ElectionState::Settled);
        assert!(election.has_quorum());
    }

    #[test]
    fn certificates_survive_weight_changes() {
        let (mut election, block, _) = election_with_fork();
        notarization_certificate(&mut election, block);
        election.update_kudzu_tallies(&weights(&[(1, 40), (2, 27), (3, 33)]), thresholds());
        assert_eq!(election.state(), ElectionState::Terminated);

        election.update_kudzu_tallies(&weights(&[(1, 1), (2, 1), (3, 1)]), thresholds());
        assert_eq!(election.state(), ElectionState::Terminated);
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
    fn thresholds() -> KudzuThresholds {
        KudzuThresholds::new(Amount::raw(100))
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
        election.add_kudzu_vote(
            PrivateKey::from(rep).public_key(),
            hash,
            kind,
            UnixMillisTimestamp::new(1000),
            Timestamp::new_test_instance(),
        )
    }

    fn vote(election: &mut Election, rep: u64, hash: BlockHash, kind: VoteKind) {
        try_vote(election, rep, hash, kind).unwrap();
    }

    fn first_votes(election: &mut Election, hash: BlockHash, reps: &[u64]) {
        for rep in reps {
            vote(election, *rep, hash, VoteKind::First);
        }
    }

    /// Rep 1 (40) first votes and rep 2 (27) notarizes: a certificate with 60
    /// of weight not first voted yet, so the election terminates without settling
    fn notarization_certificate(election: &mut Election, hash: BlockHash) {
        vote(election, 1, hash, VoteKind::First);
        vote(election, 2, hash, VoteKind::Notar);
    }

    fn weights(entries: &[(u64, u128)]) -> FxHashMap<PublicKey, Amount> {
        entries
            .iter()
            .map(|(r, w)| (PrivateKey::from(*r).public_key(), Amount::raw(*w)))
            .collect()
    }
}
