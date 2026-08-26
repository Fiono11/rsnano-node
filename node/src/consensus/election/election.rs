use std::{
    collections::{HashMap, HashSet},
    fmt::Debug,
    time::{Duration, SystemTime},
};

use strum_macros::{EnumCount, EnumIter};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, Amount, Block, BlockHash, MaybeSavedBlock, PublicKey, QualifiedRoot, SavedBlock,
    UnixMillisTimestamp, Vote, VoteError, VoteTimestamp,
};
use rsnano_utils::stats::DetailType;

use super::{ConfirmationType, ConfirmedElection, ElectionState, block_tallies::BlockTallies};
use rustc_hash::FxHashMap;

pub use rsnano_types::VoteType;

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
    #[cfg(feature = "rai_protocol")]
    first_tallies: BlockTallies,
    #[cfg(feature = "rai_protocol")]
    second_look: HashSet<BlockHash>,
    #[cfg(feature = "rai_protocol")]
    timeout_predicate: bool,
    #[cfg(feature = "rai_protocol")]
    #[cfg(feature = "rai_protocol")]
    terminated: bool,
    #[cfg(feature = "rai_protocol")]
    terminated_by_timeout: bool,
    #[cfg(feature = "rai_protocol")]
    vote_generation_enabled: bool,

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
            candidate_blocks: HashMap::from([(
                block.hash(),
                MaybeSavedBlock::Saved(block.clone()),
            )]),
            state: ElectionState::Passive,
            tallies: BlockTallies::new(),
            final_tallies: BlockTallies::new(),
            #[cfg(feature = "rai_protocol")]
            first_tallies: BlockTallies::new(),
            #[cfg(feature = "rai_protocol")]
            second_look: HashSet::new(),
            #[cfg(feature = "rai_protocol")]
            timeout_predicate: false,
            #[cfg(feature = "rai_protocol")]
            #[cfg(feature = "rai_protocol")]
            terminated: false,
            #[cfg(feature = "rai_protocol")]
            terminated_by_timeout: false,
            #[cfg(feature = "rai_protocol")]
            vote_generation_enabled: true,
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

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn set_qualified_root(&mut self, root: QualifiedRoot) {
        debug_assert!(root.epoch > 0);
        debug_assert_eq!(root.slot(), self.qualified_root.slot());
        self.qualified_root = root;
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn suppress_vote_generation(&mut self) {
        self.vote_generation_enabled = false;
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn resume_vote_generation(&mut self) {
        self.vote_generation_enabled = true;
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn vote_generation_enabled(&self) -> bool {
        self.vote_generation_enabled
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
        let mut summary = VoteSummary::new(voter, hash, vote_created, vote_received);
        #[cfg(feature = "rai_protocol")]
        summary
            .apply_phase(
                VoteTimestamp::new(
                    vote_created,
                    if vote_created == UnixMillisTimestamp::MAX {
                        Vote::DURATION_MAX
                    } else {
                        0
                    },
                )
                .rai_vote_type(),
                hash,
                vote_created,
                vote_received,
            )
            .expect("new vote summary accepts its first phase");
        self.votes.insert(voter, summary);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_rai_vote(
        &mut self,
        vote: &Vote,
        hash: BlockHash,
        vote_received: Timestamp,
    ) -> Result<(), VoteError> {
        debug_assert!(self.candidate_blocks.contains_key(&hash));
        let vote_type = vote.vote_type();
        if let Some(summary) = self.votes.get_mut(&vote.voter) {
            summary.apply_phase(vote_type, hash, vote.timestamp(), vote_received)
        } else {
            let mut summary = VoteSummary::new(vote.voter, hash, vote.timestamp(), vote_received);
            summary.apply_phase(vote_type, hash, vote.timestamp(), vote_received)?;
            self.votes.insert(vote.voter, summary);
            Ok(())
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

    #[cfg(feature = "rai_protocol")]
    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    #[cfg(feature = "rai_protocol")]
    pub fn terminated_by_timeout(&self) -> bool {
        self.terminated_by_timeout
    }

    /// Returns true if final votes should be generated
    pub fn is_final(&self) -> bool {
        self.is_confirmed() || self.has_quorum()
    }

    #[cfg(not(feature = "rai_protocol"))]
    pub fn vote_type(&self) -> VoteType {
        if self.is_final() {
            VoteType::Final
        } else {
            VoteType::NonFinal
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn vote_type(&self) -> Option<VoteType> {
        if self.votes.is_empty() {
            Some(VoteType::First)
        } else if self.timeout_predicate {
            Some(VoteType::Timeout)
        } else if self.has_quorum {
            Some(VoteType::Final)
        } else if !self.second_look.is_empty() {
            Some(VoteType::NonFinal)
        } else {
            None
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn second_look_targets(&self) -> impl Iterator<Item = BlockHash> + '_ {
        self.second_look.iter().copied()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn should_vote_timeout(&self) -> bool {
        self.timeout_predicate
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

    #[cfg(feature = "rai_protocol")]
    pub fn update_rai_tallies(
        &mut self,
        rep_weights: &FxHashMap<PublicKey, Amount>,
        quorum: &crate::representatives::QuorumSnapshot,
    ) {
        if self.state.has_ended() {
            return;
        }
        self.update_vote_weights(rep_weights);
        self.first_tallies.clear();
        self.tallies.clear();
        self.final_tallies.clear();
        for vote in self.votes.values() {
            if let Some(hash) = vote.first {
                self.first_tallies.add(hash, vote.weight);
            }
            for hash in &vote.notarized {
                self.tallies.add(*hash, vote.weight);
            }
            if let Some(hash) = vote.final_vote {
                self.final_tallies.add(hash, vote.weight);
            }
        }
        self.first_tallies.sort();
        self.tallies.sort();
        self.final_tallies.sort();
        let old_winner = self.winner.hash();
        let new_winner = self.tallies.winner().map(|(h, _)| *h).unwrap_or(old_winner);
        if new_winner != old_winner {
            self.change_winner_to(&new_winner);
        }
        self.update_winner_tally();
        let w = quorum.total_weight;
        let f = quorum.faulty_weight;
        let p = quorum.slack_weight;
        if w > f * 3 + p * 2 {
            let certificate = w - f - p;
            self.second_look.clear();
            for (hash, weight) in self.first_tallies.iter() {
                if *weight > f + p {
                    self.second_look.insert(*hash);
                }
            }
            let timeout_weight: Amount = self
                .votes
                .values()
                .filter(|vote| vote.timeout)
                .map(|vote| vote.weight)
                .sum();
            let all_vote_weight: Amount = self
                .votes
                .values()
                .filter(|vote| vote.first.is_some())
                .map(|vote| vote.weight)
                .sum();
            let max_candidate_weight = self
                .first_tallies
                .winner()
                .map(|(_, weight)| *weight)
                .unwrap_or_default();
            self.timeout_predicate = all_vote_weight - max_candidate_weight > f + p;
            self.has_quorum |= self.winner_tally >= certificate;
            if !self.terminated {
                self.terminated_by_timeout = !self.has_quorum && timeout_weight >= certificate;
                self.terminated = self.has_quorum || timeout_weight >= certificate;
            }
            if self.winner_final_tally >= certificate
                || self.first_tallies.get(&self.winner.hash()) >= w - p
            {
                self.state = ElectionState::Confirmed;
            }
            if timeout_weight >= certificate {
                // A timeout certificate terminates the protocol instance, but it does not
                // close it: a later finalization certificate may still confirm a candidate.
            }
        }
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
        #[cfg(feature = "rai_protocol")]
        let epoch = self.qualified_root().epoch;
        #[cfg(not(feature = "rai_protocol"))]
        let epoch = 0;

        ConfirmedElection {
            epoch,
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
    #[cfg(feature = "rai_protocol")]
    pub latest_type: Option<VoteType>,
    #[cfg(feature = "rai_protocol")]
    pub first: Option<BlockHash>,
    #[cfg(feature = "rai_protocol")]
    pub notarized: HashSet<BlockHash>,
    #[cfg(feature = "rai_protocol")]
    pub timeout: bool,
    #[cfg(feature = "rai_protocol")]
    pub final_vote: Option<BlockHash>,
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
            #[cfg(feature = "rai_protocol")]
            latest_type: None,
            #[cfg(feature = "rai_protocol")]
            first: None,
            #[cfg(feature = "rai_protocol")]
            notarized: HashSet::new(),
            #[cfg(feature = "rai_protocol")]
            timeout: false,
            #[cfg(feature = "rai_protocol")]
            final_vote: None,
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_phase(
        &mut self,
        vote_type: VoteType,
        hash: BlockHash,
        created: UnixMillisTimestamp,
        received: Timestamp,
    ) -> Result<(), VoteError> {
        // Final and timeout are mutually exclusive terminal votes for an epoch
        // slot. Allowing one representative to sign both destroys the quorum-
        // intersection argument which prevents conflicting certificates.
        if (vote_type == VoteType::Final && self.timeout)
            || (vote_type == VoteType::Timeout && self.final_vote.is_some())
        {
            return Err(VoteError::Invalid);
        }
        let existing = match vote_type {
            VoteType::First => self.first,
            VoteType::NonFinal if self.notarized.contains(&hash) => Some(hash),
            VoteType::NonFinal => None,
            VoteType::Final => self.final_vote,
            VoteType::Timeout if self.timeout => return Err(VoteError::Replay),
            VoteType::Timeout => None,
        };
        if let Some(existing) = existing {
            return if existing == hash {
                Err(VoteError::Replay)
            } else {
                Err(VoteError::Invalid)
            };
        }
        if vote_type == VoteType::Final && self.notarized.iter().any(|notarized| *notarized != hash)
        {
            return Err(VoteError::Invalid);
        }
        match vote_type {
            VoteType::First => {
                self.first = Some(hash);
                self.notarized.insert(hash);
            }
            VoteType::NonFinal => {
                self.notarized.insert(hash);
            }
            VoteType::Final => {
                // A final vote carries the same support as a non-final vote plus finality.
                // This is also required when rebuilding an evicted election from the latest
                // cached vote: legacy Final votes contribute to both tallies, so RAI Final
                // votes must reconstruct the notarization tally as well.
                self.notarized.insert(hash);
                self.final_vote = Some(hash);
            }
            VoteType::Timeout => self.timeout = true,
        }
        self.latest_type = Some(vote_type);
        self.hash = hash;
        self.vote_created = created;
        self.vote_received = received;
        Ok(())
    }

    pub fn is_final_vote(&self) -> bool {
        #[cfg(feature = "rai_protocol")]
        {
            self.final_vote.is_some()
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.vote_created == UnixMillisTimestamp::MAX
        }
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
mod rai_voting_tests {
    use super::*;
    use crate::representatives::QuorumSnapshot;
    use rsnano_ledger::RepWeights;
    use rsnano_types::PrivateKey;

    fn summary() -> VoteSummary {
        VoteSummary::new(
            PublicKey::from(1),
            BlockHash::ZERO,
            UnixMillisTimestamp::ZERO,
            Timestamp::new_test_instance(),
        )
    }

    #[test]
    fn first_vote_also_notarizes_its_hash() {
        let mut vote = summary();
        let hash = BlockHash::from(1);
        vote.apply_phase(
            VoteType::First,
            hash,
            UnixMillisTimestamp::ZERO,
            Timestamp::new_test_instance(),
        )
        .unwrap();

        assert_eq!(vote.first, Some(hash));
        assert_eq!(vote.notarized, HashSet::from([hash]));
    }

    #[test]
    fn second_look_can_notarize_a_different_hash() {
        let mut vote = summary();
        let first = BlockHash::from(1);
        let second = BlockHash::from(2);
        vote.apply_phase(
            VoteType::First,
            first,
            0.into(),
            Timestamp::new_test_instance(),
        )
        .unwrap();
        vote.apply_phase(
            VoteType::NonFinal,
            second,
            1.into(),
            Timestamp::new_test_instance(),
        )
        .unwrap();

        assert_eq!(vote.notarized, HashSet::from([first, second]));
    }

    #[test]
    fn cannot_final_vote_after_notarizing_conflicting_hashes() {
        let mut vote = summary();
        let first = BlockHash::from(1);
        let second = BlockHash::from(2);
        vote.apply_phase(
            VoteType::First,
            first,
            0.into(),
            Timestamp::new_test_instance(),
        )
        .unwrap();
        vote.apply_phase(
            VoteType::NonFinal,
            second,
            1.into(),
            Timestamp::new_test_instance(),
        )
        .unwrap();

        assert_eq!(
            vote.apply_phase(
                VoteType::Final,
                first,
                2.into(),
                Timestamp::new_test_instance()
            ),
            Err(VoteError::Invalid)
        );
    }

    #[test]
    fn final_vote_reconstructs_both_notarized_and_final_support() {
        let mut vote = summary();
        let hash = BlockHash::from(1);

        vote.apply_phase(
            VoteType::Final,
            hash,
            UnixMillisTimestamp::MAX,
            Timestamp::new_test_instance(),
        )
        .unwrap();

        assert_eq!(vote.notarized, HashSet::from([hash]));
        assert_eq!(vote.final_vote, Some(hash));
    }

    #[test]
    fn timeout_vote_locks_out_final_vote() {
        let mut vote = summary();
        let hash = BlockHash::from(1);
        vote.apply_phase(
            VoteType::Timeout,
            hash,
            1.into(),
            Timestamp::new_test_instance(),
        )
        .unwrap();

        assert_eq!(
            vote.apply_phase(
                VoteType::Final,
                hash,
                2.into(),
                Timestamp::new_test_instance(),
            ),
            Err(VoteError::Invalid)
        );
    }

    #[test]
    fn final_vote_locks_out_timeout_vote() {
        let mut vote = summary();
        let hash = BlockHash::from(1);
        vote.apply_phase(
            VoteType::Final,
            hash,
            1.into(),
            Timestamp::new_test_instance(),
        )
        .unwrap();

        assert_eq!(
            vote.apply_phase(
                VoteType::Timeout,
                hash,
                2.into(),
                Timestamp::new_test_instance(),
            ),
            Err(VoteError::Invalid)
        );
    }

    #[test]
    fn single_viable_first_candidate_requests_second_look() {
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let mut election = Election::new_test_instance_with(block);
        let rep = PrivateKey::from(1);
        election
            .apply_rai_vote(
                &Vote::new_rai(&rep, 1, VoteType::First, vec![hash]),
                hash,
                Timestamp::new_test_instance(),
            )
            .unwrap();

        let quorum = QuorumSnapshot::new_test_instance();
        let mut weights = RepWeights::default();
        weights.put(rep.public_key(), quorum.total_weight / 2);
        election.update_rai_tallies(&weights, &quorum);

        assert_eq!(election.second_look, HashSet::from([hash]));
        assert_eq!(election.vote_type(), Some(VoteType::NonFinal));
    }

    #[test]
    fn timeout_takes_precedence_over_final_after_conflicting_notarization() {
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let mut election = Election::new_test_instance_with(block);
        let rep = PrivateKey::from(1);
        election
            .apply_rai_vote(
                &Vote::new_rai(&rep, 1, VoteType::First, vec![hash]),
                hash,
                Timestamp::new_test_instance(),
            )
            .unwrap();
        election.has_quorum = true;
        election.timeout_predicate = true;

        assert_eq!(election.vote_type(), Some(VoteType::Timeout));
    }

    #[test]
    fn timeout_certificate_terminates_election_unconfirmed() {
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let mut election = Election::new_test_instance_with(block);
        let quorum = QuorumSnapshot::new_test_instance();
        let certificate = quorum.total_weight - quorum.faulty_weight - quorum.slack_weight;
        let rep = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(rep.public_key(), certificate);

        election
            .apply_rai_vote(
                &Vote::new_rai(&rep, 1, VoteType::Timeout, vec![hash]),
                hash,
                Timestamp::new_test_instance(),
            )
            .unwrap();
        election.update_rai_tallies(&weights, &quorum);

        assert!(election.is_terminated());
        assert!(!election.is_confirmed());
        assert!(!election.state().has_ended());
    }

    #[test]
    fn expiry_does_not_request_timeout_or_end_election() {
        let block = SavedBlock::new_test_instance();
        let mut election = Election::new_test_instance_with(block);
        let expired_at = election.start() + Duration::from_mins(5) + Duration::from_millis(1);

        assert!(!election.should_vote_timeout());
        election.transition_time(expired_at);

        assert!(!election.should_vote_timeout());
        assert!(!election.state().has_ended());
    }
}
