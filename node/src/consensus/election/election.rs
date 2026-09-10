use std::{
    collections::HashMap,
    fmt::Debug,
    time::{Duration, SystemTime},
};

use strum_macros::{EnumCount, EnumIter};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, Amount, Block, BlockHash, MaybeSavedBlock, PublicKey, QualifiedRoot, SavedBlock,
    UnixMillisTimestamp, Vote, VoteError,
};
use rsnano_utils::stats::DetailType;

use super::{ConfirmationType, ConfirmedElection, ElectionState, block_tallies::BlockTallies};
use rustc_hash::FxHashMap;

#[derive(PartialEq, Eq, Debug, Clone, Copy, Hash)]
pub enum VoteType {
    NonFinal,
    Final,
}

#[derive(Clone)]
pub struct Election {
    #[cfg(feature = "rai_protocol")]
    pub(crate) first_vote_observed: Option<Timestamp>,
    #[cfg(feature = "rai_protocol")]
    pub(super) kudzu: super::kudzu::KudzuVotes,
    qualified_root: QualifiedRoot,
    pub epoch: u64,
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
            #[cfg(feature = "rai_protocol")]
            kudzu: Default::default(),
            #[cfg(feature = "rai_protocol")]
            first_vote_observed: None,
            qualified_root: block.qualified_root(),
            epoch: 0,
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
            winner: MaybeSavedBlock::Saved(block),
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

    pub fn id(&self) -> rsnano_types::ElectionId {
        rsnano_types::ElectionId::new(self.qualified_root.clone(), self.epoch)
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

    #[cfg(feature = "rai_protocol")]
    pub fn add_kudzu_vote(
        &mut self,
        vote: std::sync::Arc<Vote>,
        hash: BlockHash,
        now: Timestamp,
    ) -> Result<(), VoteError> {
        if vote.epoch != self.epoch || !self.contains_block(&hash) || !vote.hashes.contains(&hash) {
            return Err(VoteError::Invalid);
        }
        self.kudzu.insert(vote.clone(), hash)?;
        // Phase delivery can be reordered. Keep a final summary for this value.
        if !self
            .votes()
            .get(&vote.voter)
            .is_some_and(|v| v.hash == hash && v.is_final_vote())
        {
            self.add_vote(vote.voter, hash, vote.timestamp(), now);
        }
        Ok(())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn needs_kudzu_vote(&self, rep: &PublicKey) -> bool {
        self.kudzu
            .needs_vote(rep, self.winner.hash(), self.has_quorum())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn has_kudzu_certificate(&self, hash: BlockHash, kind: rsnano_types::VoteKind) -> bool {
        self.kudzu.has_certificate(hash, kind)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn has_f_plus_one_first_votes(&self) -> bool {
        self.kudzu.has_f_plus_one_first_votes()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn termination_diagnostic(&self) -> serde_json::Value {
        serde_json::json!({"state":format!("{:?}",self.state),"root":self.qualified_root(),"epoch":self.epoch,"winner":self.winner().hash(),"candidates":self.candidate_blocks().keys().collect::<Vec<_>>(),"first":self.kudzu.first_tallies,"notarization":self.kudzu.notar_tallies,"final":self.kudzu.final_tallies,"participation":self.kudzu.participation_diagnostic(),"timeout_certificate":self.is_timed_out(),"timeout_eligible":self.should_timeout()})
    }

    #[cfg(feature = "rai_protocol")]
    pub fn is_timed_out(&self) -> bool {
        self.kudzu
            .has_certificate(self.winner.hash(), rsnano_types::VoteKind::Timeout)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn should_timeout(&self) -> bool {
        !self.is_confirmed()
            && !self.has_quorum
            && !self.is_timed_out()
            && self.kudzu.should_timeout()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn can_notarize(&self, hash: &BlockHash) -> bool {
        self.contains_block(hash) && self.kudzu.second_look(hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn update_kudzu_tallies(&mut self, weights: &FxHashMap<PublicKey, Amount>, total: Amount) {
        use rsnano_types::VoteKind;
        self.kudzu.tally(weights, total);
        // Finalized elections retain evidence, without changing their decided value.
        if self.is_confirmed() || self.state.has_ended() {
            return;
        }
        self.update_vote_weights(weights);
        // Stable tie breaking avoids dependence on HashMap iteration order.
        let best = self.candidate_blocks.keys().copied().max_by_key(|hash| {
            let fast = self.kudzu.has_certificate(*hash, VoteKind::First);
            let notar = self.kudzu.has_certificate(*hash, VoteKind::Notarize);
            let final_ = notar && self.kudzu.has_certificate(*hash, VoteKind::Final);
            (
                fast || final_,
                notar,
                self.kudzu
                    .first_tallies
                    .get(hash)
                    .copied()
                    .unwrap_or_default(),
                *hash,
            )
        });
        if let Some(hash) = best {
            if self.kudzu.second_look(&hash) || self.kudzu.has_certificate(hash, VoteKind::Notarize)
            {
                self.change_winner_to(&hash);
            }
        }
        self.tallies = BlockTallies::new();
        self.final_tallies = BlockTallies::new();
        for hash in self.candidate_blocks.keys() {
            self.tallies.insert(
                *hash,
                self.kudzu
                    .notar_tallies
                    .get(hash)
                    .copied()
                    .unwrap_or_default(),
            );
            self.final_tallies.insert(
                *hash,
                self.kudzu
                    .final_tallies
                    .get(hash)
                    .copied()
                    .unwrap_or_default(),
            );
        }
        self.update_winner_tally();
        let hash = self.winner.hash();
        self.has_quorum = self.kudzu.has_certificate(hash, VoteKind::Notarize);
        if self.kudzu.has_certificate(hash, VoteKind::First)
            || (self.has_quorum && self.kudzu.has_certificate(hash, VoteKind::Final))
        {
            self.state = ElectionState::Confirmed;
        }
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

        // RAI must retain unfinished elections and their authenticated evidence.
        // A local timer is not a notarization or an implicit/explicit finalization.
        #[cfg(not(feature = "rai_protocol"))]
        if !self.state.has_ended() && self.behavior.time_to_live() < duration {
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
        self.is_confirmed() || self.has_quorum()
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
        #[cfg(feature = "rai_protocol")]
        if self.is_confirmed() {
            return false;
        }
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
    }

    fn remove_block(&mut self, hash: &BlockHash) -> Option<MaybeSavedBlock> {
        if self.winner.hash() != *hash {
            let existing = self.candidate_blocks.remove(hash);
            if existing.is_some() {
                self.votes.retain(|_, v| v.hash != *hash);
                self.tallies.remove(hash);
                self.final_tallies.remove(hash);
                return existing;
            }
        }

        None
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
            epoch: self.epoch,
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
