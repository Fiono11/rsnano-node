use std::{
    collections::HashMap,
    fmt::Debug,
    time::{Duration, SystemTime},
};

#[cfg(feature = "rai_protocol")]
use std::collections::HashSet;

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
    qualified_root: QualifiedRoot,
    winner: MaybeSavedBlock,
    state: ElectionState,
    // TODO: there can't be more than 10 blocks, so an array might be a lot faster
    candidate_blocks: HashMap<BlockHash, MaybeSavedBlock>,
    votes: HashMap<PublicKey, VoteSummary>,
    #[cfg(feature = "rai_protocol")]
    first_votes: HashMap<PublicKey, VoteSummary>,
    #[cfg(feature = "rai_protocol")]
    notarization_votes: HashMap<(PublicKey, BlockHash), VoteSummary>,
    #[cfg(feature = "rai_protocol")]
    timeout_votes: HashSet<PublicKey>,
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
            qualified_root: block.qualified_root(),
            votes: HashMap::new(),
            #[cfg(feature = "rai_protocol")]
            first_votes: HashMap::new(),
            #[cfg(feature = "rai_protocol")]
            notarization_votes: HashMap::new(),
            #[cfg(feature = "rai_protocol")]
            timeout_votes: HashSet::new(),
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
    pub fn add_rai_vote(&mut self, vote: &Vote, hash: BlockHash, vote_received: Timestamp) {
        use rsnano_types::RaiVoteKind;

        let summary = VoteSummary::new(vote.voter, hash, vote.timestamp(), vote_received);
        match vote.rai_kind() {
            RaiVoteKind::First => {
                if self.first_votes.contains_key(&vote.voter) {
                    return;
                }
                self.first_votes.insert(vote.voter, summary.clone());
                self.notarization_votes
                    .entry((vote.voter, hash))
                    .or_insert_with(|| summary.clone());
                if !self
                    .votes
                    .get(&vote.voter)
                    .is_some_and(VoteSummary::is_final_vote)
                {
                    self.votes.insert(vote.voter, summary);
                }
            }
            RaiVoteKind::Timeout => {
                self.timeout_votes.insert(vote.voter);
            }
            RaiVoteKind::Notarization => {
                self.notarization_votes
                    .insert((vote.voter, hash), summary.clone());
                if !self
                    .votes
                    .get(&vote.voter)
                    .is_some_and(VoteSummary::is_final_vote)
                {
                    self.votes.insert(vote.voter, summary);
                }
            }
            RaiVoteKind::Final => {
                // A final vote implies notarization. This is required when messages from
                // different representatives arrive in different protocol phases: without it,
                // notarization and final weight can become split below quorum forever.
                self.notarization_votes
                    .insert((vote.voter, hash), summary.clone());
                self.votes.insert(vote.voter, summary);
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn first_votes(&self) -> &HashMap<PublicKey, VoteSummary> {
        &self.first_votes
    }

    #[cfg(feature = "rai_protocol")]
    pub fn all_first_vote_weight(&self) -> Amount {
        self.first_votes.values().map(|vote| vote.weight).sum()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn max_first_vote_weight(&self) -> Amount {
        let mut tallies = BlockTallies::new();
        tallies.calculate(self.first_votes.values());
        tallies.winner().map(|(_, weight)| *weight).unwrap_or_default()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn many_votes(&self, total_weight: Amount) -> Vec<BlockHash> {
        let mut tallies = BlockTallies::new();
        tallies.calculate(self.first_votes.values());
        let threshold = percentage(total_weight, 40);
        tallies
            .iter()
            .filter_map(|(hash, weight)| (*weight > threshold).then_some(*hash))
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn should_notarize_timeout(&self, total_weight: Amount) -> bool {
        self.all_first_vote_weight() - self.max_first_vote_weight()
            > percentage(total_weight, 40)
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
        #[cfg_attr(feature = "rai_protocol", allow(unused_variables))] quorum_delta: Amount,
        #[cfg(feature = "rai_protocol")] total_weight: Amount,
    ) {
        if self.state.has_ended() {
            return;
        }

        self.update_vote_weights(rep_weights);
        #[cfg(feature = "rai_protocol")]
        self.update_rai_vote_weights(rep_weights);

        #[cfg(feature = "rai_protocol")]
        self.update_rai_tallies(total_weight);

        #[cfg(not(feature = "rai_protocol"))]
        {
            self.recalculate_tallies();

            if let Some(new_winner) = self.check_new_winner(quorum_delta) {
                tracing::warn!("Winner changed to {:?}!", new_winner);
                self.change_winner_to(&new_winner);
            }

            self.update_winner_tally();
            self.try_set_quorum(quorum_delta);
            self.try_confirm(quorum_delta);
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn update_rai_vote_weights(&mut self, rep_weights: &FxHashMap<PublicKey, Amount>) {
        for vote in self.first_votes.values_mut() {
            vote.weight = rep_weights.get(&vote.voter).copied().unwrap_or_default();
        }
        for vote in self.notarization_votes.values_mut() {
            vote.weight = rep_weights.get(&vote.voter).copied().unwrap_or_default();
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn update_rai_tallies(&mut self, total_weight: Amount) {
        let regular_threshold = percentage(total_weight, 60);
        let fast_threshold = percentage(total_weight, 80);

        self.tallies.calculate(self.notarization_votes.values());
        self.final_tallies
            .calculate(self.votes.values().filter(|vote| vote.is_final_vote()));

        let mut first_tallies = BlockTallies::new();
        first_tallies.calculate(self.first_votes.values());

        let old_winner = self.winner.hash();
        if let Some((hash, _)) = self.tallies.winner().copied()
            && hash != old_winner
            && self.candidate_blocks.contains_key(&hash)
        {
            self.change_winner_to(&hash);
        }

        self.update_winner_tally();
        self.has_quorum = self.winner_tally >= regular_threshold;
        let fast_finalized = first_tallies.get(&self.winner.hash()) >= fast_threshold;
        let regularly_finalized = self.winner_final_tally >= regular_threshold;
        if fast_finalized || regularly_finalized {
            self.state = ElectionState::Confirmed;
        }
    }

    fn update_vote_weights(&mut self, rep_weights: &FxHashMap<PublicKey, Amount>) {
        for vote in self.votes.values_mut() {
            vote.weight = rep_weights.get(&vote.voter).cloned().unwrap_or_default();
        }
    }

    #[cfg(not(feature = "rai_protocol"))]
    fn recalculate_tallies(&mut self) {
        self.tallies.calculate(self.votes.values());
        self.final_tallies
            .calculate(self.votes.values().filter(|v| v.is_final_vote()));
    }

    #[cfg(not(feature = "rai_protocol"))]
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

    #[cfg(not(feature = "rai_protocol"))]
    fn try_set_quorum(&mut self, quorum_delta: Amount) {
        if self.tallies.check_quorum(quorum_delta) {
            self.has_quorum = true;
        }
    }

    #[cfg(not(feature = "rai_protocol"))]
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

#[cfg(feature = "rai_protocol")]
fn percentage(weight: Amount, percent: u8) -> Amount {
    let value = primitive_types::U256::from(weight.number())
        * primitive_types::U256::from(percent)
        / primitive_types::U256::from(100);
    Amount::raw(value.as_u128())
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

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use super::*;
    use rsnano_types::{PrivateKey, RaiVoteKind};

    #[test]
    fn rai_votes_use_cached_total_weight_for_quorum() {
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let mut election = Election::new_test_instance_with(block);
        let keys: Vec<_> = (1..=6).map(PrivateKey::from).collect();
        let weights = keys
            .iter()
            .map(|key| (key.public_key(), Amount::raw(100)))
            .collect();

        for key in &keys[..2] {
            let vote = Vote::new_rai(key, 0, RaiVoteKind::First, vec![hash]);
            election.add_rai_vote(&vote, hash, Timestamp::new_test_instance());
        }
        for key in &keys[2..4] {
            let vote = Vote::new_rai(key, 0, RaiVoteKind::Final, vec![hash]);
            election.add_rai_vote(&vote, hash, Timestamp::new_test_instance());
        }

        election.update_tallies(&weights, Amount::raw(300), Amount::raw(600));

        assert_eq!(election.winner_tally(), Amount::raw(400));
        assert_eq!(election.winner_final_tally(), Amount::raw(200));
        assert!(election.has_quorum());
        assert!(!election.is_confirmed());
    }

    #[test]
    fn delayed_first_vote_does_not_downgrade_final_vote() {
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let mut election = Election::new_test_instance_with(block);
        let key = PrivateKey::from(1);
        let weights = FxHashMap::from_iter([(key.public_key(), Amount::raw(100))]);

        let final_vote = Vote::new_rai(&key, 0, RaiVoteKind::Final, vec![hash]);
        election.add_rai_vote(&final_vote, hash, Timestamp::new_test_instance());
        let delayed_first = Vote::new_rai(&key, 0, RaiVoteKind::First, vec![hash]);
        election.add_rai_vote(&delayed_first, hash, Timestamp::new_test_instance());
        election.update_tallies(&weights, Amount::ZERO, Amount::raw(100));

        assert!(election.votes().get(&key.public_key()).unwrap().is_final_vote());
        assert_eq!(election.winner_tally(), Amount::raw(100));
        assert_eq!(election.winner_final_tally(), Amount::raw(100));
    }
}
