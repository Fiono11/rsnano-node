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

#[cfg(feature = "rai_protocol")]
use crate::consensus::rai::{
    BlockHashOrTimeout, RaiCloseElectionId, RaiCloseKind, RaiElectionVoteState, RaiEpoch,
    RaiLocalResult, RaiOutcome,
};
#[cfg(feature = "rai_protocol")]
use rsnano_ledger::RepWeights;
#[cfg(feature = "rai_protocol")]
use std::sync::Arc;

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiElectionKind {
    Slot,
    CloseCut,
    CloseRecord,
}

#[cfg(feature = "rai_protocol")]
pub use rsnano_types::{RaiElectionId, RaiSlotId};

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
    #[cfg(feature = "rai_protocol")]
    rai_hash_candidates: HashSet<BlockHash>,
    #[cfg(feature = "rai_protocol")]
    rai_selected_hash: Option<BlockHash>,
    #[cfg(feature = "rai_protocol")]
    rai_timeout_expired: bool,
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

    #[cfg(feature = "rai_protocol")]
    rai_id: RaiElectionId,
    #[cfg(feature = "rai_protocol")]
    pub rai_votes: RaiElectionVoteState,
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
        #[cfg(feature = "rai_protocol")]
        return Self::new_slot(block, behavior, base_latency, now, RaiEpoch::ZERO);

        #[cfg(not(feature = "rai_protocol"))]
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
            winner: MaybeSavedBlock::Saved(block),
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn new_slot(
        block: SavedBlock,
        behavior: ElectionBehavior,
        base_latency: Duration,
        now: Timestamp,
        epoch: RaiEpoch,
    ) -> Self {
        let root = block.qualified_root();
        Self {
            qualified_root: root.clone(),
            votes: HashMap::new(),
            candidate_blocks: HashMap::from([(
                block.hash(),
                MaybeSavedBlock::Saved(block.clone()),
            )]),
            rai_hash_candidates: HashSet::new(),
            rai_selected_hash: None,
            rai_timeout_expired: false,
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
            rai_id: RaiElectionId::Slot(RaiSlotId { epoch, root }),
            rai_votes: RaiElectionVoteState::default(),
        }
    }

    /// Creates an active election whose candidate is a close-cut digest rather
    /// than a ledger block. The placeholder block only satisfies the legacy
    /// election projection; it is deliberately not installed as a candidate.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn new_close(
        id: RaiCloseElectionId,
        root: QualifiedRoot,
        candidate: BlockHash,
        committee: Arc<RepWeights>,
        base_latency: Duration,
        now: Timestamp,
    ) -> Self {
        let placeholder = SavedBlock::new_test_instance();
        Self {
            qualified_root: root,
            votes: HashMap::new(),
            candidate_blocks: HashMap::new(),
            rai_hash_candidates: HashSet::from([candidate]),
            rai_selected_hash: Some(candidate),
            rai_timeout_expired: false,
            state: ElectionState::Passive,
            tallies: BlockTallies::new(),
            final_tallies: BlockTallies::new(),
            winner_tally: Amount::ZERO,
            winner_final_tally: Amount::ZERO,
            behavior: ElectionBehavior::Manual,
            has_quorum: false,
            start: now,
            base_latency,
            account: Account::ZERO,
            winner: MaybeSavedBlock::Saved(placeholder),
            rai_id: match id.kind {
                RaiCloseKind::Cut => RaiElectionId::CloseCut {
                    epoch: id.epoch,
                    round: id.round,
                },
                RaiCloseKind::Record => RaiElectionId::CloseRecord {
                    epoch: id.epoch,
                    round: id.round,
                },
            },
            rai_votes: RaiElectionVoteState::new(vec![committee]),
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
    pub fn rai_kind(&self) -> RaiElectionKind {
        match &self.rai_id {
            RaiElectionId::Slot(_) => RaiElectionKind::Slot,
            RaiElectionId::CloseCut { .. } => RaiElectionKind::CloseCut,
            RaiElectionId::CloseRecord { .. } => RaiElectionKind::CloseRecord,
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_epoch(&self) -> RaiEpoch {
        match &self.rai_id {
            RaiElectionId::Slot(id) => id.epoch,
            RaiElectionId::CloseCut { epoch, .. } | RaiElectionId::CloseRecord { epoch, .. } => {
                *epoch
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_round(&self) -> u32 {
        match &self.rai_id {
            RaiElectionId::Slot(_) => 0,
            RaiElectionId::CloseCut { round, .. } | RaiElectionId::CloseRecord { round, .. } => {
                *round
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_id(&self) -> &RaiElectionId {
        &self.rai_id
    }

    /// Whether this election still carries protocol state needed to close its
    /// epoch.  In particular, a locally ended/notarized election is not a
    /// terminal RAI result and must not be removed by ordinary AEC cleanup.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_requires_retention(&self) -> bool {
        matches!(
            self.rai_votes.outcome,
            RaiOutcome::Pending | RaiOutcome::Notarized(_)
        )
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_is_terminal(&self) -> bool {
        matches!(
            self.rai_votes.outcome,
            RaiOutcome::Notarized(_) | RaiOutcome::Confirmed(_) | RaiOutcome::TimedOut
        )
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_vote_metadata(&self) -> rsnano_types::RaiVoteMetadata {
        rsnano_types::RaiVoteMetadata {
            election_id: self.rai_id.clone(),
            epoch: self.rai_epoch(),
            phase: self.rai_vote_phase(),
            ..Default::default()
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_vote_phase(&self) -> rsnano_types::RaiVotePhase {
        if self.is_final() {
            return rsnano_types::RaiVotePhase::Final;
        }
        if self.rai_timeout_notar_ready() {
            return rsnano_types::RaiVotePhase::Notar;
        }
        if self.rai_candidate_progressed() {
            rsnano_types::RaiVotePhase::Notar
        } else {
            rsnano_types::RaiVotePhase::First
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_timeout_notar_ready(&self) -> bool {
        !self.rai_votes.committees.is_empty()
            && (0..self.rai_votes.committees.len()).all(|index| self.rai_votes.timeout_ready(index))
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_candidate_progressed(&self) -> bool {
        use crate::consensus::rai::BlockHashOrTimeout;

        let hash = match self.rai_kind() {
            RaiElectionKind::Slot => self.winner.hash(),
            RaiElectionKind::CloseCut | RaiElectionKind::CloseRecord => self
                .rai_selected_hash
                .expect("close election must have a selected candidate"),
        };
        let value = BlockHashOrTimeout::Block(hash);
        !self.rai_votes.committees.is_empty()
            && self
                .rai_votes
                .committees
                .iter()
                .enumerate()
                .all(|(index, committee)| {
                    self.rai_votes.first_tally(index, value) >= committee.thresholds.progression
                })
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_should_first_timeout(&self) -> bool {
        self.rai_kind() == RaiElectionKind::Slot
            && self.rai_timeout_expired
            && !self.rai_candidate_progressed()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_request_hash(&self) -> BlockHash {
        if !self.rai_votes.committees.is_empty()
            && (0..self.rai_votes.committees.len()).all(|index| self.rai_votes.timeout_ready(index))
        {
            BlockHash::ZERO
        } else {
            self.voting_hash()
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn with_rai_committees(mut self, committees: Vec<Arc<RepWeights>>) -> Self {
        self.rai_votes = RaiElectionVoteState::new(committees);
        self
    }

    #[cfg(feature = "rai_protocol")]
    pub fn add_rai_vote(
        &mut self,
        voter: PublicKey,
        hash: BlockHash,
        metadata: rsnano_types::RaiVoteMetadata,
        vote_created: UnixMillisTimestamp,
        vote_received: Timestamp,
    ) -> Result<(), crate::consensus::rai::RaiVoteStateError> {
        if metadata.election_id != self.rai_id || metadata.epoch != self.rai_epoch() {
            return Err(crate::consensus::rai::RaiVoteStateError::WrongElectionContext);
        }
        let is_timeout = hash.is_zero() && metadata.phase != rsnano_types::RaiVotePhase::Final;
        let value = if is_timeout {
            BlockHashOrTimeout::Timeout
        } else {
            BlockHashOrTimeout::Block(hash)
        };
        self.rai_votes
            .record_vote(voter, value, metadata.phase, metadata.scope)?;
        // Compatibility/RPC projection only; certificate decisions never read this map.
        if !is_timeout {
            self.add_vote(voter, hash, vote_created, vote_received);
        }
        Ok(())
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

    /// Candidate membership shared by slot and synthetic RAI elections.
    #[cfg(feature = "rai_protocol")]
    pub fn contains_candidate(&self, hash: &BlockHash) -> bool {
        self.candidate_blocks.contains_key(hash) || self.rai_hash_candidates.contains(hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn candidate_hashes(&self) -> impl Iterator<Item = &BlockHash> {
        self.candidate_blocks
            .keys()
            .chain(self.rai_hash_candidates.iter())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn add_rai_hash_candidate(&mut self, hash: BlockHash) -> bool {
        self.rai_hash_candidates.insert(hash)
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
        #[cfg(not(feature = "rai_protocol"))]
        debug_assert!(self.candidate_blocks.contains_key(&hash));
        #[cfg(feature = "rai_protocol")]
        debug_assert!(self.contains_candidate(&hash));
        self.votes.insert(
            voter,
            VoteSummary::new(voter, hash, vote_created, vote_received),
        );
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

        // A ledger confirmation or provisional notarization is not a terminal
        // RAI certificate. Keep the election active (and therefore eligible
        // for confirm_req solicitation) until its persistent RAI evidence
        // reaches a fast/final/timeout outcome. RAI slots cannot wait through
        // the legacy passive window: a short epoch may close before their
        // first repair request is sent.
        #[cfg(feature = "rai_protocol")]
        if self.rai_requires_retention() {
            if self.rai_votes.outcome == RaiOutcome::Pending
                && self.rai_kind() == RaiElectionKind::Slot
                && self.base_latency * Self::PASSIVE_DURATION_FACTOR < duration
            {
                self.rai_timeout_expired = true;
            }
            self.state = ElectionState::Active;
            return;
        }

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

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn voting_hash(&self) -> BlockHash {
        if self.rai_timeout_notar_ready() || self.rai_should_first_timeout() {
            return BlockHash::ZERO;
        }
        match self.rai_kind() {
            RaiElectionKind::Slot => self.winner.hash(),
            RaiElectionKind::CloseCut | RaiElectionKind::CloseRecord => self
                .rai_selected_hash
                .expect("close election must have a selected candidate"),
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn is_rai_close(&self) -> bool {
        matches!(
            self.rai_kind(),
            RaiElectionKind::CloseCut | RaiElectionKind::CloseRecord
        )
    }

    pub fn force_confirm(&mut self) -> bool {
        #[cfg(feature = "rai_protocol")]
        if self.rai_votes.outcome == RaiOutcome::Pending {
            return false;
        }
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

        #[cfg(feature = "rai_protocol")]
        {
            let _ = (rep_weights, quorum_delta);
            self.update_rai_tallies();
            return;
        }

        #[cfg(not(feature = "rai_protocol"))]
        self.update_vote_weights(rep_weights);
        #[cfg(not(feature = "rai_protocol"))]
        self.recalculate_tallies();

        #[cfg(not(feature = "rai_protocol"))]
        if let Some(new_winner) = self.check_new_winner(quorum_delta) {
            tracing::warn!("Winner changed to {:?}!", new_winner);
            self.change_winner_to(&new_winner);
        }

        #[cfg(not(feature = "rai_protocol"))]
        self.update_winner_tally();
        #[cfg(not(feature = "rai_protocol"))]
        self.try_set_quorum(quorum_delta);
        #[cfg(not(feature = "rai_protocol"))]
        self.try_confirm(quorum_delta);
    }

    #[cfg(feature = "rai_protocol")]
    fn update_rai_tallies(&mut self) {
        let projected = self
            .candidate_hashes()
            .map(|hash| {
                let support = self
                    .rai_votes
                    .committees
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        self.rai_votes
                            .notarization_tally(index, BlockHashOrTimeout::Block(*hash))
                    })
                    .min()
                    .unwrap_or_default();
                (*hash, support)
            })
            .max_by_key(|(hash, support)| (*support, *hash));
        if let Some((hash, support)) = projected {
            if !support.is_zero()
                && self.candidate_blocks.contains_key(&hash)
                && self.winner.hash() != hash
            {
                tracing::warn!("Winner changed to {:?}!", hash);
                self.change_winner_to(&hash);
            }
            let winner = if self.rai_kind() == RaiElectionKind::Slot {
                self.winner.hash()
            } else {
                hash
            };
            self.winner_tally = self
                .rai_votes
                .committees
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    self.rai_votes
                        .notarization_tally(index, BlockHashOrTimeout::Block(winner))
                })
                .min()
                .unwrap_or_default();
            self.winner_final_tally = self
                .rai_votes
                .committees
                .iter()
                .enumerate()
                .map(|(index, _)| self.rai_votes.final_tally(index, winner))
                .min()
                .unwrap_or_default();
        }

        let results: Vec<_> = (0..self.rai_votes.committees.len())
            .map(|index| self.rai_votes.local_result(index))
            .collect();
        if results.is_empty() || results.iter().any(Option::is_none) {
            return;
        }
        let results: Vec<_> = results.into_iter().flatten().collect();
        let first_hash = match results[0] {
            RaiLocalResult::Notarized(hash)
            | RaiLocalResult::Fast(hash)
            | RaiLocalResult::Final(hash) => hash,
            RaiLocalResult::Timeout => {
                self.rai_votes.outcome = RaiOutcome::TimedOut;
                self.state = ElectionState::Confirmed;
                return;
            }
        };
        if results.iter().any(|result| {
            matches!(result, RaiLocalResult::Timeout)
                || matches!(result, RaiLocalResult::Notarized(hash) | RaiLocalResult::Fast(hash) | RaiLocalResult::Final(hash) if *hash != first_hash)
        }) {
            self.rai_votes.outcome = RaiOutcome::TimedOut;
            self.state = ElectionState::Confirmed;
            return;
        }

        if self.candidate_blocks.contains_key(&first_hash) && self.winner.hash() != first_hash {
            tracing::warn!("Winner changed to {:?}!", first_hash);
            self.change_winner_to(&first_hash);
        }
        self.has_quorum = true;
        if results.iter().all(|result| matches!(result, RaiLocalResult::Fast(hash) | RaiLocalResult::Final(hash) if *hash == first_hash)) {
            self.rai_votes.outcome = RaiOutcome::Confirmed(first_hash);
            self.state = ElectionState::Confirmed;
        } else if self.rai_kind() == RaiElectionKind::Slot {
            // Notarization settles the close-drain obligation, but it does not
            // finalize the slot. Keep the election active so later First or
            // Final evidence can still produce a fast/final certificate.
            self.rai_votes.outcome = RaiOutcome::Notarized(first_hash);
        }
    }

    #[cfg(not(feature = "rai_protocol"))]
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

    #[cfg(not(feature = "rai_protocol"))]
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
            #[cfg(feature = "rai_protocol")]
            rai_finalization_epoch: matches!(
                self.rai_votes.outcome,
                RaiOutcome::Confirmed(hash) if hash == self.winner.hash()
            )
            .then_some(self.rai_epoch()),
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

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_identity_tests {
    use super::*;
    use crate::consensus::rai::{rai_close_cut_root, rai_close_record_root};

    #[test]
    fn same_root_in_different_epochs_has_distinct_ids() {
        let root = QualifiedRoot::new_test_instance();
        let first = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(1),
            root: root.clone(),
        });
        let second = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(2),
            root,
        });

        assert_ne!(first, second);
    }

    #[test]
    fn election_constructors_preserve_the_full_id() {
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let slot = Election::new_slot(
            block,
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            Timestamp::new_test_instance(),
            RaiEpoch::new(7),
        );
        assert_eq!(
            slot.rai_id(),
            &RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::new(7),
                root,
            })
        );

        let close_id = RaiCloseElectionId {
            kind: RaiCloseKind::Cut,
            epoch: RaiEpoch::new(8),
            round: 3,
        };
        let close = Election::new_close(
            close_id,
            rai_close_cut_root(close_id.epoch, close_id.round),
            BlockHash::from(1),
            Arc::new(RepWeights::default()),
            Duration::from_secs(1),
            Timestamp::new_test_instance(),
        );
        assert_eq!(
            close.rai_id(),
            &RaiElectionId::CloseCut {
                epoch: RaiEpoch::new(8),
                round: 3,
            }
        );
    }

    #[test]
    fn close_kind_and_round_are_identity_components() {
        let epoch = RaiEpoch::new(9);
        let cut = RaiElectionId::CloseCut { epoch, round: 4 };
        let record = RaiElectionId::CloseRecord { epoch, round: 4 };
        let next_round = RaiElectionId::CloseCut { epoch, round: 5 };

        assert_ne!(cut, record);
        assert_ne!(cut, next_round);
        assert_ne!(
            rai_close_cut_root(epoch, 4),
            rai_close_record_root(epoch, 4)
        );
    }

    #[test]
    fn compatible_notarization_remains_live_and_can_fast_finalize() {
        use crate::consensus::rai::BlockHashOrTimeout;
        use rsnano_types::{Amount, PrivateKey, RaiCommitteeScope};

        let keys = (1..=6).map(PrivateKey::from).collect::<Vec<_>>();
        let committee = Arc::new(RepWeights::from([
            (keys[0].public_key(), Amount::raw(1)),
            (keys[1].public_key(), Amount::raw(1)),
            (keys[2].public_key(), Amount::raw(1)),
            (keys[3].public_key(), Amount::raw(1)),
            (keys[4].public_key(), Amount::raw(1)),
            (keys[5].public_key(), Amount::raw(1)),
        ]));
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let mut election = Election::new_slot(
            block,
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            Timestamp::new_test_instance(),
            RaiEpoch::ZERO,
        )
        .with_rai_committees(vec![committee]);

        // Four of six first votes cross notarization, but not the fast
        // threshold. The result remains provisional and retained.
        for key in keys.iter().take(4) {
            election
                .rai_votes
                .record_first_vote(
                    key.public_key(),
                    BlockHashOrTimeout::Block(hash),
                    RaiCommitteeScope::All,
                )
                .unwrap();
        }
        for key in keys.iter().take(4) {
            election
                .rai_votes
                .record_notarization_vote(
                    key.public_key(),
                    BlockHashOrTimeout::Block(hash),
                    RaiCommitteeScope::All,
                )
                .unwrap();
        }
        election.update_rai_tallies();

        assert_eq!(election.rai_votes.outcome, RaiOutcome::Notarized(hash));
        assert!(!election.state().has_ended());
        assert!(election.rai_requires_retention());
        assert!(
            election
                .into_confirmed_election(
                    Timestamp::new_test_instance(),
                    ConfirmationType::ActiveConfirmedQuorum,
                )
                .rai_finalization_epoch
                .is_none()
        );

        election
            .rai_votes
            .record_first_vote(
                keys[4].public_key(),
                BlockHashOrTimeout::Block(hash),
                RaiCommitteeScope::All,
            )
            .unwrap();
        election.update_rai_tallies();

        assert_eq!(election.rai_votes.outcome, RaiOutcome::Confirmed(hash));
        assert!(election.state().has_ended());
        assert_eq!(
            election
                .into_confirmed_election(
                    Timestamp::new_test_instance(),
                    ConfirmationType::ActiveConfirmedQuorum,
                )
                .rai_finalization_epoch,
            Some(RaiEpoch::ZERO)
        );
    }

    #[test]
    fn slot_timeout_votes_finish_the_election() {
        use rsnano_types::{Amount, PrivateKey, RaiVoteMetadata, RaiVotePhase};

        let keys = (1..=6).map(PrivateKey::from).collect::<Vec<_>>();
        let committee = Arc::new(RepWeights::from([
            (keys[0].public_key(), Amount::raw(1)),
            (keys[1].public_key(), Amount::raw(1)),
            (keys[2].public_key(), Amount::raw(1)),
            (keys[3].public_key(), Amount::raw(1)),
            (keys[4].public_key(), Amount::raw(1)),
            (keys[5].public_key(), Amount::raw(1)),
        ]));
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let mut election = Election::new_slot(
            block,
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            Timestamp::new_test_instance(),
            RaiEpoch::ZERO,
        )
        .with_rai_committees(vec![committee]);
        let metadata = RaiVoteMetadata {
            election_id: election.rai_id().clone(),
            epoch: election.rai_epoch(),
            ..Default::default()
        };
        let received = Timestamp::new_test_instance();

        for key in keys.iter().take(2) {
            election
                .add_rai_vote(
                    key.public_key(),
                    hash,
                    metadata.clone(),
                    UnixMillisTimestamp::new(1),
                    received,
                )
                .unwrap();
        }
        for key in keys.iter().skip(2) {
            election
                .add_rai_vote(
                    key.public_key(),
                    BlockHash::ZERO,
                    metadata.clone(),
                    UnixMillisTimestamp::new(1),
                    received,
                )
                .unwrap();
        }

        assert_eq!(election.voting_hash(), BlockHash::ZERO);
        assert_eq!(election.rai_vote_metadata().phase, RaiVotePhase::Notar);

        let timeout_notar = RaiVoteMetadata {
            phase: RaiVotePhase::Notar,
            ..metadata
        };
        for key in keys.iter().take(4) {
            election
                .add_rai_vote(
                    key.public_key(),
                    BlockHash::ZERO,
                    timeout_notar.clone(),
                    UnixMillisTimestamp::new(2),
                    received,
                )
                .unwrap();
        }
        election.update_rai_tallies();

        assert_eq!(election.rai_votes.outcome, RaiOutcome::TimedOut);
        assert!(election.state().has_ended());
    }

    #[test]
    fn pending_slot_activates_without_legacy_passive_delay() {
        let start = Timestamp::new_test_instance();
        let mut election = Election::new_slot(
            SavedBlock::new_test_instance(),
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            start,
            RaiEpoch::ZERO,
        );

        assert_eq!(election.state(), ElectionState::Passive);
        election.transition_time(start);
        assert_eq!(election.state(), ElectionState::Active);
        assert!(!election.rai_timeout_expired);
    }

    #[test]
    fn expired_slot_targets_a_timeout_first_vote() {
        use rsnano_types::{Amount, PrivateKey, RaiVotePhase};

        let key = PrivateKey::from(1);
        let committee = Arc::new(RepWeights::from([(key.public_key(), Amount::raw(1))]));
        let start = Timestamp::new_test_instance();
        let mut election = Election::new_slot(
            SavedBlock::new_test_instance(),
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            start,
            RaiEpoch::ZERO,
        )
        .with_rai_committees(vec![committee]);

        election.transition_time(start + Duration::from_secs(6));

        assert_eq!(election.voting_hash(), BlockHash::ZERO);
        assert_eq!(election.rai_vote_metadata().phase, RaiVotePhase::First);
        assert_eq!(election.rai_votes.outcome, RaiOutcome::Pending);
    }
}
