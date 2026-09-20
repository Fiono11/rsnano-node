use std::{
    collections::{BTreeMap, HashMap},
    ops::Deref,
};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Amount, BlockHash, ConsensusEpoch, VoteDelivery, VoteError};
use rsnano_utils::sync::backpressure_channel::Sender;

use super::{
    AecFact, ApplyVoteArgs,
    epoch_close::{AgreedContent, EpochClose, is_late},
    epoch_committees::{EpochCommittees, live_committees},
    recently_confirmed_cache::RecentlyConfirmedCache,
    root_container::{Entry, RootContainer},
    stats::AecStats,
};
use crate::consensus::election::{Committees, ConfirmationType, Election, ElectionId, VoteSummary};

pub(super) struct ApplyVoteHelper<'a> {
    pub args: &'a ApplyVoteArgs<'a>,
    pub recently_confirmed: &'a mut RecentlyConfirmedCache,
    pub stats: &'a mut AecStats,
    pub observer: &'a Option<Sender<AecFact>>,
    pub roots: &'a mut RootContainer,
    /// RAI: the close elections, for the committee phase of each epoch
    pub closes: &'a BTreeMap<ConsensusEpoch, EpochClose>,
    /// RAI: the committees the instances of each epoch are counted in
    pub committees: &'a EpochCommittees,
    /// RAI: the content of the agreed epochs, to tell the late instances
    pub agreed: &'a AgreedContent,
}

impl<'a> ApplyVoteHelper<'a> {
    pub fn apply_vote(&mut self) -> ApplyVoteResult {
        let mut result = ApplyVoteResult::default();
        // Kudzu: certificate evidence is a batch handed over for one election; its
        // other hashes are of no interest and are skipped without a result
        let evidence = self.args.vote.delivery == VoteDelivery::Evidence;
        // RAI: a vote counts in the election of its own epoch only
        let epoch = self.args.vote.epoch;
        for block_hash in self.args.vote.filtered_blocks() {
            // Ignore duplicate hashes (should not happen with a well-behaved voting node)
            if result.per_block.contains_key(block_hash) {
                continue;
            }
            if evidence
                && self
                    .roots
                    .vote_router
                    .election_id(block_hash, epoch)
                    .is_none()
            {
                continue;
            }

            if let Some(election) = self
                .roots
                .election_for_block_in_epoch_mut(block_hash, epoch)
            {
                let id = election.id();
                {
                    let mut apply_to_election = ApplyVoteToElectionHelper {
                        args: self.args,
                        recently_confirmed: self.recently_confirmed,
                        stats: self.stats,
                        observer: self.observer,
                        election,
                        block_hash,
                        closes: self.closes,
                        committees: self.committees,
                        agreed: self.agreed,
                    };
                    let vote_result = apply_to_election.apply_vote();
                    result.per_block.insert(*block_hash, vote_result);
                }
                settle_election(self.roots, &id, self.observer, &mut result);
            } else if self.recently_confirmed.hash_exists(block_hash) {
                result.per_block.insert(*block_hash, Err(VoteError::Late));
            } else {
                result
                    .per_block
                    .insert(*block_hash, Err(VoteError::Indeterminate));
            }
        }

        result
    }
}

/// After an election's tallies were updated: a finalized election leaves
/// the roots, a terminated one leaves its bucket (Kudzu: the evidence is
/// kept, but it stops taking capacity)
pub(super) fn settle_election(
    roots: &mut RootContainer,
    id: &ElectionId,
    observer: &Option<Sender<AecFact>>,
    result: &mut ApplyVoteResult,
) {
    let Some(election) = roots.election(id) else {
        return;
    };
    let confirmed = election.is_confirmed();
    let terminated = election.state().is_terminated();
    if confirmed {
        if !roots.is_terminated(id) {
            result.decided.push(id.epoch);
        }
        if let Some(entry) = roots.erase(id) {
            result.confirmed.push(entry);
        }
    } else if terminated && !roots.is_terminated(id) {
        result.decided.push(id.epoch);
        roots.mark_terminated(id);
        if let Some(observer) = observer {
            observer
                .send(AecFact::ElectionTerminated(id.clone()))
                .unwrap();
        }
    }
}

#[derive(Default)]
pub(crate) struct ApplyVoteResult {
    pub per_block: HashMap<BlockHash, Result<(), VoteError>>,
    pub confirmed: Vec<Entry>,
    /// RAI: the epochs of the elections which got their first certificate
    pub decided: Vec<ConsensusEpoch>,
}

struct ApplyVoteToElectionHelper<'a> {
    pub args: &'a ApplyVoteArgs<'a>,
    pub recently_confirmed: &'a mut RecentlyConfirmedCache,
    pub stats: &'a mut AecStats,
    pub observer: &'a Option<Sender<AecFact>>,
    pub election: &'a mut Election,
    pub block_hash: &'a BlockHash,
    pub closes: &'a BTreeMap<ConsensusEpoch, EpochClose>,
    pub committees: &'a EpochCommittees,
    pub agreed: &'a AgreedContent,
}

impl<'a> ApplyVoteToElectionHelper<'a> {
    pub fn apply_vote(&mut self) -> Result<(), VoteError> {
        if self.election.is_confirmed() {
            return Err(VoteError::Late);
        }

        if cfg!(feature = "rai_protocol") {
            return self.apply_kudzu_vote();
        }

        let rep_weight = self.args.rep_weights.weight(&self.args.vote.voter);

        if let Some(last_vote) = self.election.votes().get(&self.args.vote.voter) {
            last_vote.ensure_no_replay(self.args.vote, self.block_hash)?;

            if self.should_cool_down(last_vote, rep_weight) {
                return Err(VoteError::Ignored);
            }
        }

        self.add_vote();
        Ok(())
    }

    fn should_cool_down(&self, last_vote: &VoteSummary, rep_weight: Amount) -> bool {
        if self.args.vote.delivery == VoteDelivery::Replayed {
            // Only cooldown live votes
            return false;
        }

        if last_vote.has_switched_to_final_vote(self.args.vote) {
            return false;
        }

        let cooldown = self.args.quorum_snapshot.cooldown_time(rep_weight);
        last_vote.vote_received.elapsed(self.args.now) < cooldown
    }

    /// Kudzu: every (representative, kind, hash) is counted once, there is no
    /// timestamp ordering and no cooldown
    fn apply_kudzu_vote(&mut self) -> Result<(), VoteError> {
        let vote = &self.args.vote;
        self.election
            .add_kudzu_vote(&vote.vote.vote, *self.block_hash, self.args.now)?;
        self.stats.vote_counter.count(vote.delivery);
        self.confirm_if_quorum();
        Ok(())
    }

    fn add_vote(&mut self) {
        self.election.add_vote(
            self.args.vote.voter,
            *self.block_hash,
            self.args.vote.timestamp(),
            self.args.now,
        );
        self.stats.vote_counter.count(self.args.vote.delivery);
        self.confirm_if_quorum();
    }

    pub fn confirm_if_quorum(&mut self) {
        if cfg!(feature = "rai_protocol") {
            // RAI: the instance counts in the committees of its epoch; a vote
            // of an epoch whose committee is not known here yet waits in the
            // pool until the committee is derived and the epoch is counted again
            let Some(committees) =
                election_committees(self.committees, self.closes, self.election, self.args)
            else {
                return;
            };
            count_kudzu_election(
                self.election,
                &committees,
                self.args.now,
                self.stats,
                self.observer,
                self.recently_confirmed,
                self.agreed,
            );
            return;
        }

        let old_winner = self.election.winner().hash();
        self.election.update_tallies(
            self.args.rep_weights,
            self.args.quorum_snapshot.quorum_delta,
        );
        notify_winner_changed(self.election, old_winner, self.observer);
        if self.election.is_final() && self.election.is_confirmed() {
            election_got_confirmed(
                self.election,
                self.args.now,
                self.observer,
                self.recently_confirmed,
                self.agreed,
            );
        }
    }
}

/// RAI: the committees an instance is counted in: those of its epoch, or
/// the ledger's live weights before the epochs of a run started
pub(super) fn election_committees(
    committees: &EpochCommittees,
    closes: &BTreeMap<ConsensusEpoch, EpochClose>,
    election: &Election,
    args: &ApplyVoteArgs,
) -> Option<Committees> {
    if !committees.started() {
        return Some(live_committees(args.rep_weights, args.quorum_snapshot));
    }
    let epoch = election.epoch();
    committees.for_epoch(epoch, previous_epoch_closed(closes, epoch))
}

/// RAI: whether the close election of the epoch before this one has
/// finalized its value; an epoch without a known predecessor counts as such
pub(super) fn previous_epoch_closed(
    closes: &BTreeMap<ConsensusEpoch, EpochClose>,
    epoch: ConsensusEpoch,
) -> bool {
    epoch
        .as_u64()
        .checked_sub(1)
        .and_then(|previous| closes.get(&ConsensusEpoch::new(previous)))
        .is_none_or(|previous| previous.is_closed())
}

/// Kudzu: count an election in the given committees, collect its
/// certificates and act on its transitions: the winner in the block tree,
/// the confirmation
pub(super) fn count_kudzu_election(
    election: &mut Election,
    committees: &Committees,
    now: Timestamp,
    stats: &mut AecStats,
    observer: &Option<Sender<AecFact>>,
    recently_confirmed: &mut RecentlyConfirmedCache,
    agreed: &AgreedContent,
) {
    let old_winner = election.winner().hash();
    let was_in_block_tree = election.certificates().has_block();
    let old_state = election.state();
    election.update_kudzu_tallies(committees);
    stats.kudzu_transition(old_state, election, was_in_block_tree, now);
    notify_winner_changed(election, old_winner, observer);
    if election.is_confirmed() {
        election_got_confirmed(election, now, observer, recently_confirmed, agreed);
    }
}

fn notify_winner_changed(
    election: &Election,
    old_winner: BlockHash,
    observer: &Option<Sender<AecFact>>,
) {
    if election.winner().hash() != old_winner {
        notify(
            observer,
            AecFact::WinnerChanged(old_winner, election.winner().deref().clone()),
        );
    }
}

fn election_got_confirmed(
    election: &Election,
    now: Timestamp,
    observer: &Option<Sender<AecFact>>,
    recently_confirmed: &mut RecentlyConfirmedCache,
    agreed: &AgreedContent,
) {
    // RAI: a late instance of an agreed epoch is discarded, not confirmed
    if is_late(agreed, election) {
        return;
    }
    recently_confirmed.put(election.qualified_root().clone(), election.winner().hash());

    let confirmed_election =
        election.into_confirmed_election(now, ConfirmationType::ActiveConfirmedQuorum);

    notify(observer, AecFact::ElectionConfirmed(confirmed_election));
}

fn notify(observer: &Option<Sender<AecFact>>, event: AecFact) {
    if let Some(o) = observer {
        o.send(event).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        consensus::{
            FilteredVote, ReceivedVote, active_elections::root_container::Entry,
            election::ElectionBehavior,
        },
        representatives::QuorumSnapshot,
    };
    use rsnano_ledger::RepWeights;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{
        Block, BlockPriority, ConsensusEpoch, PrivateKey, QualifiedRoot, SavedBlock,
        StateBlockArgs, UnixMillisTimestamp, Vote, VoteKind,
    };
    use rsnano_utils::sync::backpressure_channel::channel;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn ignore_duplicate_block_hashes_in_vote() {
        let mut fixture = Fixture::default();
        fixture.add_active_election();

        let result = fixture.apply_vote(vec![fixture.block_hash, fixture.block_hash]);

        assert_eq!(result.get(&fixture.block_hash).unwrap(), &Ok(()));
    }

    #[test]
    fn when_recently_confirmed_should_return_late_error() {
        let mut fixture = Fixture::default();
        fixture.add_recently_confirmed();

        let result = fixture.apply_vote(vec![fixture.block_hash]);

        assert_eq!(
            result.get(&fixture.block_hash).unwrap(),
            &Err(VoteError::Late)
        );
    }

    #[test]
    fn when_not_active_and_not_recently_confirmed_should_return_indeterminate() {
        let mut fixture = Fixture::default();

        let result = fixture.apply_vote(vec![fixture.block_hash]);

        assert_eq!(
            result.get(&fixture.block_hash).unwrap(),
            &Err(VoteError::Indeterminate)
        );
    }

    /// Legacy replay rule: Kudzu counts every (representative, kind, hash) once
    /// regardless of the timestamp
    #[cfg(not(feature = "rai_protocol"))]
    #[test]
    fn ignore_vote_with_lower_timestamp() {
        let mut fixture = FixtureForElection::default();
        fixture.add_processed_vote(UnixMillisTimestamp::new(2000), Duration::ZERO);

        let result = fixture.apply_vote_from(VoteDelivery::Direct, UnixMillisTimestamp::new(1000));

        assert_eq!(result, Err(VoteError::Replay));
    }

    /// Kudzu votes are one-shot, so there is nothing to cool down
    #[cfg(not(feature = "rai_protocol"))]
    #[test]
    fn cool_down_live_vote() {
        let mut fixture = FixtureForElection::default();
        fixture.add_processed_vote(UnixMillisTimestamp::new(1000), Duration::from_millis(500));

        let result = fixture.apply_vote_from(VoteDelivery::Direct, UnixMillisTimestamp::new(2000));

        assert_eq!(result, Err(VoteError::Ignored));
    }

    /// Legacy cooldown rule, Kudzu votes are one-shot
    #[cfg(not(feature = "rai_protocol"))]
    #[test]
    fn dont_cool_down_when_enough_space_between_votes() {
        let mut fixture = FixtureForElection::default();
        fixture.add_processed_vote(UnixMillisTimestamp::new(1000), Duration::from_secs(15));

        let result = fixture.apply_vote_from(VoteDelivery::Direct, UnixMillisTimestamp::new(1100));

        assert_eq!(result, Ok(()));
    }

    /// Legacy cooldown rule, Kudzu votes are one-shot
    #[cfg(not(feature = "rai_protocol"))]
    #[test]
    fn dont_cool_down_when_vote_comes_from_cache() {
        let mut fixture = FixtureForElection::default();
        fixture.add_processed_vote(UnixMillisTimestamp::new(1000), Duration::ZERO);

        let result =
            fixture.apply_vote_from(VoteDelivery::Replayed, UnixMillisTimestamp::new(1100));

        assert_eq!(result, Ok(()));
    }

    /// Legacy cooldown rule, Kudzu votes are one-shot
    #[cfg(not(feature = "rai_protocol"))]
    #[test]
    fn dont_cool_down_when_switched_to_final_vote() {
        let mut fixture = FixtureForElection::default();
        fixture.add_processed_vote(UnixMillisTimestamp::new(1000), Duration::ZERO);

        let result = fixture.apply_final_vote_from(VoteDelivery::Direct);

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn when_election_already_confirmed_should_return_late_error() {
        let mut fixture = FixtureForElection::default();
        fixture.election.force_confirm();

        let result = fixture.apply_final_vote_from(VoteDelivery::Direct);

        assert_eq!(result, Err(VoteError::Late));
    }

    #[test]
    fn notify_winner_changed() {
        let block = StateBlockArgs::new_test_instance();
        let key = block.key.clone();

        let fork: Block = StateBlockArgs {
            representative: 999888777.into(),
            ..block
        }
        .into();

        let block = SavedBlock::new_test_instance_with(block.into());

        let mut fixture = FixtureForElection::with_block(block.clone());
        fixture.rep_weights.put(key.public_key(), Amount::MAX);
        fixture.election.try_add_fork(&fork, Amount::ZERO);

        let vote = ReceivedVote::new(
            Vote::new(&key, UnixMillisTimestamp::new(1000), 0, vec![fork.hash()]).into(),
            VoteDelivery::Direct,
            None,
        );

        fixture.apply_vote(vote).unwrap();

        assert_eq!(fixture.election.winner().hash(), fork.hash());
        // Kudzu: the fork also gets fast finalized by that single vote
        let expected_events = if cfg!(feature = "rai_protocol") { 2 } else { 1 };
        assert_eq!(fixture.events.len(), expected_events);
        let AecFact::WinnerChanged(old_winner, new_winner) = &fixture.events[0] else {
            panic!("not a winner changed event");
        };
        assert_eq!(old_winner, &block.hash());
        assert_eq!(new_winner, &fork);
    }

    #[cfg(feature = "rai_protocol")]
    mod kudzu {
        use super::*;
        use crate::consensus::election::ElectionState;

        #[test]
        fn same_kind_and_hash_is_a_replay_regardless_of_timestamp_and_cooldown() {
            let mut fixture = FixtureForElection::default();
            fixture.add_processed_vote(UnixMillisTimestamp::new(2000), Duration::ZERO);

            let result =
                fixture.apply_vote_from(VoteDelivery::Direct, UnixMillisTimestamp::new(1000));

            assert_eq!(result, Err(VoteError::Replay));
        }

        #[test]
        fn a_later_vote_of_another_kind_is_not_cooled_down() {
            let mut fixture = FixtureForElection::default();
            fixture.add_processed_vote(UnixMillisTimestamp::new(1000), Duration::ZERO);

            let result = fixture.apply_final_vote_from(VoteDelivery::Direct);

            assert_eq!(result, Ok(()));
        }

        #[test]
        fn first_votes_alone_fast_finalize() {
            let mut fixture = FixtureForElection::default();
            // 90% of the online weight of 100M nano is above the fast threshold (81%)
            fixture
                .rep_weights
                .put(fixture.rep1_key.public_key(), Amount::nano(90_000_000));

            fixture
                .apply_vote_from(VoteDelivery::Direct, UnixMillisTimestamp::new(1000))
                .unwrap();

            assert_eq!(fixture.election.state(), ElectionState::Confirmed);
            assert_eq!(
                fixture.election.certificates().fast,
                Some(fixture.block.hash())
            );
            assert!(matches!(
                fixture.events.last(),
                Some(AecFact::ElectionConfirmed(_))
            ));
        }

        #[test]
        fn notarization_certificate_terminates_without_confirming() {
            let mut fixture = FixtureForElection::default();
            // 70% is a notarization certificate but not a fast certificate
            fixture
                .rep_weights
                .put(fixture.rep1_key.public_key(), Amount::nano(70_000_000));

            fixture
                .apply_vote_from(VoteDelivery::Direct, UnixMillisTimestamp::new(1000))
                .unwrap();

            assert!(fixture.election.state().is_terminated());
            assert!(!fixture.election.is_confirmed());
            assert!(fixture.election.is_final());
            assert!(fixture.events.is_empty());

            fixture.apply_final_vote_from(VoteDelivery::Direct).unwrap();
            assert_eq!(fixture.election.state(), ElectionState::Confirmed);
            assert!(matches!(
                fixture.events.last(),
                Some(AecFact::ElectionConfirmed(_))
            ));
        }

        #[test]
        fn vote_kinds_are_tallied_separately() {
            let mut fixture = FixtureForElection::default();
            fixture
                .rep_weights
                .put(fixture.rep1_key.public_key(), Amount::nano(10_000_000));
            let hash = fixture.block.hash();

            let notar = ReceivedVote::new(
                Vote::new_of_kind(&fixture.rep1_key, VoteKind::Notar, vec![hash]).into(),
                VoteDelivery::Direct,
                None,
            );
            fixture.apply_vote(notar).unwrap();
            let timeout = ReceivedVote::new(
                Vote::new_of_kind(&fixture.rep1_key, VoteKind::Timeout, vec![hash]).into(),
                VoteDelivery::Direct,
                None,
            );
            fixture.apply_vote(timeout).unwrap();

            let votes = fixture.election.kudzu_votes();
            assert_eq!(votes.notar_tallies().get(&hash), Amount::nano(10_000_000));
            assert_eq!(votes.first_tallies().get(&hash), Amount::ZERO);
            assert_eq!(votes.timeout_weight(), Amount::nano(10_000_000));
        }
    }

    #[test]
    fn notify_election_confirmed() {
        let mut fixture = FixtureForElection::default();
        fixture
            .rep_weights
            .put(fixture.rep1_key.public_key(), Amount::MAX);

        fixture.apply_final_vote_from(VoteDelivery::Direct).unwrap();

        assert_eq!(fixture.events.len(), 1);

        assert!(matches!(fixture.events[0], AecFact::ElectionConfirmed(_)));
    }

    // Test helpers:
    //--------------------------------------------------------------------------------

    struct Fixture {
        block: SavedBlock,
        root: QualifiedRoot,
        block_hash: BlockHash,
        roots: RootContainer,
        recently_confirmed: RecentlyConfirmedCache,
        rep_weights: RepWeights,
    }

    impl Fixture {
        fn with_block(block: SavedBlock) -> Self {
            let root = block.qualified_root();
            let block_hash = block.hash();
            Self {
                block,
                root,
                block_hash,
                roots: RootContainer::default(),
                recently_confirmed: RecentlyConfirmedCache::default(),
                rep_weights: RepWeights::default(),
            }
        }

        fn add_active_election(&mut self) {
            let election = Election::new_test_instance_with(self.block.clone());
            self.roots.insert(Entry {
                id: election.id(),
                election,
                priority: BlockPriority::new_test_instance(),
            });
        }

        fn add_recently_confirmed(&mut self) {
            self.recently_confirmed
                .put(self.root.clone(), self.block_hash);
        }

        fn apply_vote(
            &mut self,
            hashes: Vec<BlockHash>,
        ) -> HashMap<BlockHash, Result<(), VoteError>> {
            let vote = Vote::new(
                &PrivateKey::from(1),
                UnixMillisTimestamp::new(1000),
                0,
                hashes,
            );

            let vote: FilteredVote =
                ReceivedVote::new(vote.into(), VoteDelivery::Direct, None).into();
            let quorum_snapshot = QuorumSnapshot::new_test_instance();

            let args = ApplyVoteArgs {
                vote: &vote,
                rep_weights: &self.rep_weights,
                quorum_snapshot: &quorum_snapshot,
                now: Timestamp::new_test_instance(),
            };

            let mut stats = AecStats::default();

            let mut helper = ApplyVoteHelper {
                args: &args,
                recently_confirmed: &mut self.recently_confirmed,
                stats: &mut stats,
                observer: &None,
                roots: &mut self.roots,
                closes: &BTreeMap::new(),
                committees: &EpochCommittees::default(),
                agreed: &BTreeMap::new(),
            };

            let result = helper.apply_vote();
            result.per_block
        }
    }

    impl Default for Fixture {
        fn default() -> Self {
            let block = SavedBlock::new_test_instance();
            Self::with_block(block)
        }
    }

    struct FixtureForElection {
        now: Timestamp,
        block: SavedBlock,
        election: Election,
        rep1_key: PrivateKey,
        rep_weights: RepWeights,
        events: Vec<AecFact>,
    }

    impl FixtureForElection {
        fn add_processed_vote(&mut self, created: UnixMillisTimestamp, received_ago: Duration) {
            if cfg!(feature = "rai_protocol") {
                let vote = Arc::new(Vote::new_of_kind_at(
                    &self.rep1_key,
                    VoteKind::First,
                    created,
                    vec![self.block.hash()],
                ));
                self.election
                    .add_kudzu_vote(&vote, self.block.hash(), self.now - received_ago)
                    .unwrap();
            } else {
                self.election.add_vote(
                    self.rep1_key.public_key(),
                    self.block.hash(),
                    created,
                    self.now - received_ago,
                );
            }
        }

        fn apply_vote_from(
            &mut self,
            source: VoteDelivery,
            created: UnixMillisTimestamp,
        ) -> Result<(), VoteError> {
            let vote = ReceivedVote::new(
                Vote::new(&self.rep1_key, created, 0, vec![self.block.hash()]).into(),
                source,
                None,
            );

            self.apply_vote(vote)
        }

        fn apply_final_vote_from(&mut self, source: VoteDelivery) -> Result<(), VoteError> {
            let vote = ReceivedVote::new(
                Vote::new_final(&self.rep1_key, vec![self.block.hash()]).into(),
                source,
                None,
            );

            self.apply_vote(vote)
        }

        fn apply_vote(&mut self, vote: impl Into<FilteredVote>) -> Result<(), VoteError> {
            let vote = vote.into();

            let quorum_snapshot = QuorumSnapshot::new_test_instance();
            let mut recently_confirmed = RecentlyConfirmedCache::default();
            let mut stats = AecStats::default();
            let (tx, rx) = channel(1024);

            let result = {
                ApplyVoteToElectionHelper {
                    args: &ApplyVoteArgs {
                        vote: &vote,
                        rep_weights: &self.rep_weights,
                        quorum_snapshot: &quorum_snapshot,
                        now: Timestamp::new_test_instance(),
                    },
                    recently_confirmed: &mut recently_confirmed,
                    stats: &mut stats,
                    observer: &Some(tx),
                    election: &mut self.election,
                    closes: &BTreeMap::new(),
                    block_hash: &vote.hashes[0],
                    committees: &EpochCommittees::default(),
                    agreed: &BTreeMap::new(),
                }
                .apply_vote()
            };

            while let Ok(ev) = rx.recv() {
                self.events.push(ev);
            }

            result
        }

        fn with_block(block: SavedBlock) -> Self {
            let now = Timestamp::new_test_instance();

            let election = Election::new(
                block.clone(),
                ConsensusEpoch::ZERO,
                ElectionBehavior::Priority,
                Duration::from_secs(1),
                now,
            );

            let rep1_key = PrivateKey::from(1);

            let mut rep_weights = RepWeights::default();
            rep_weights.put(rep1_key.public_key(), Amount::nano(100_000));

            Self {
                now,
                block,
                election,
                rep1_key,
                events: Vec::new(),
                rep_weights,
            }
        }
    }

    impl Default for FixtureForElection {
        fn default() -> Self {
            let block = SavedBlock::new_test_instance();
            Self::with_block(block)
        }
    }
}
