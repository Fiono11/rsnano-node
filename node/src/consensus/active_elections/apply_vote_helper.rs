use std::{collections::HashMap, ops::Deref};

#[cfg(not(feature = "rai_protocol"))]
use rsnano_types::{Amount, VoteDelivery};
use rsnano_types::{BlockHash, VoteError};
use rsnano_utils::sync::backpressure_channel::Sender;

use super::{
    AecFact, ApplyVoteArgs,
    recently_confirmed_cache::RecentlyConfirmedCache,
    root_container::{Entry, RootContainer},
    stats::VoteCounter,
};
#[cfg(not(feature = "rai_protocol"))]
use crate::consensus::election::VoteSummary;
use crate::consensus::election::{ConfirmationType, Election};

pub(super) struct ApplyVoteHelper<'a> {
    pub args: &'a ApplyVoteArgs<'a>,
    pub recently_confirmed: &'a mut RecentlyConfirmedCache,
    pub vote_counter: &'a mut VoteCounter,
    pub observer: &'a Option<Sender<AecFact>>,
    pub roots: &'a mut RootContainer,
}

impl<'a> ApplyVoteHelper<'a> {
    pub fn apply_vote(&mut self) -> ApplyVoteResult {
        let mut result = ApplyVoteResult::default();
        #[cfg(feature = "rai_protocol")]
        {
            let election_id = &self.args.vote.metadata.election_id;
            for block_hash in self.args.vote.filtered_blocks() {
                if result.per_block.contains_key(block_hash) {
                    continue;
                }
                let Some(election) = self.roots.election_for_rai_id_mut(election_id) else {
                    result
                        .per_block
                        .insert(*block_hash, Err(VoteError::Indeterminate));
                    continue;
                };
                let timeout_vote = block_hash.is_zero()
                    && self.args.vote.metadata.phase != rsnano_types::RaiVotePhase::Final;
                // A signed close First vote is also the authenticated start
                // witness used to discover a remote close version. Count it
                // for split/timeout derivation while reconciliation obtains
                // the preimage; notar/final votes still require an admitted
                // validated candidate.
                let close_first = election.is_rai_close()
                    && self.args.vote.metadata.phase == rsnano_types::RaiVotePhase::First;
                if !timeout_vote && !close_first && !election.contains_candidate(block_hash) {
                    result
                        .per_block
                        .insert(*block_hash, Err(VoteError::Indeterminate));
                    continue;
                }
                let vote_result = ApplyVoteToElectionHelper {
                    args: self.args,
                    recently_confirmed: self.recently_confirmed,
                    vote_counter: self.vote_counter,
                    observer: self.observer,
                    election,
                    block_hash,
                }
                .apply_vote();
                result.per_block.insert(*block_hash, vote_result);
                let confirmed = election.is_confirmed();
                if confirmed && let Some(entry) = self.roots.erase_rai_id(election_id) {
                    result.confirmed.push(entry);
                }
            }
            return result;
        }

        #[cfg(not(feature = "rai_protocol"))]
        for block_hash in self.args.vote.filtered_blocks() {
            // Ignore duplicate hashes (should not happen with a well-behaved voting node)
            if result.per_block.contains_key(block_hash) {
                continue;
            }

            let election = self.roots.election_for_block_mut(block_hash);
            if let Some(election) = election {
                {
                    let mut apply_to_election = ApplyVoteToElectionHelper {
                        args: self.args,
                        recently_confirmed: self.recently_confirmed,
                        vote_counter: self.vote_counter,
                        observer: self.observer,
                        election,
                        block_hash,
                    };
                    let vote_result = apply_to_election.apply_vote();
                    result.per_block.insert(*block_hash, vote_result);
                }

                if election.is_confirmed() {
                    #[cfg(not(feature = "rai_protocol"))]
                    let root = election.qualified_root().clone();
                    #[cfg(feature = "rai_protocol")]
                    let rai_id = election.rai_id().clone();
                    #[cfg(not(feature = "rai_protocol"))]
                    if let Some(entry) = self.roots.erase(&root) {
                        result.confirmed.push(entry);
                    }
                    #[cfg(feature = "rai_protocol")]
                    if let Some(entry) = self.roots.erase_rai_id(&rai_id) {
                        result.confirmed.push(entry);
                    }
                }
            } else if self.recently_confirmed.hash_exists(block_hash) {
                result.per_block.insert(*block_hash, Err(VoteError::Late));
            } else {
                result
                    .per_block
                    .insert(*block_hash, Err(VoteError::Indeterminate));
            }
        }

        #[cfg(not(feature = "rai_protocol"))]
        return result;
    }
}

#[derive(Default)]
pub(crate) struct ApplyVoteResult {
    pub per_block: HashMap<BlockHash, Result<(), VoteError>>,
    pub confirmed: Vec<Entry>,
}

struct ApplyVoteToElectionHelper<'a> {
    pub args: &'a ApplyVoteArgs<'a>,
    pub recently_confirmed: &'a mut RecentlyConfirmedCache,
    pub vote_counter: &'a mut VoteCounter,
    pub observer: &'a Option<Sender<AecFact>>,
    pub election: &'a mut Election,
    pub block_hash: &'a BlockHash,
}

impl<'a> ApplyVoteToElectionHelper<'a> {
    pub fn apply_vote(&mut self) -> Result<(), VoteError> {
        if self.election.is_confirmed() {
            return Err(VoteError::Late);
        }

        #[cfg(feature = "rai_protocol")]
        {
            self.election
                .add_rai_vote(
                    self.args.vote.voter,
                    *self.block_hash,
                    self.args.vote.metadata.clone(),
                    self.args.vote.timestamp(),
                    self.args.now,
                )
                .map_err(|_| VoteError::Invalid)?;
            self.vote_counter.count(self.args.vote.delivery);
            self.confirm_if_quorum();
            return Ok(());
        }

        #[cfg(not(feature = "rai_protocol"))]
        {
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
    }

    #[cfg(not(feature = "rai_protocol"))]
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

    #[cfg(not(feature = "rai_protocol"))]
    fn add_vote(&mut self) {
        self.election.add_vote(
            self.args.vote.voter,
            *self.block_hash,
            self.args.vote.timestamp(),
            self.args.now,
        );
        self.vote_counter.count(self.args.vote.delivery);
        self.confirm_if_quorum();
    }

    pub fn confirm_if_quorum(&mut self) {
        let old_winner = self.election.winner().hash();

        self.election.update_tallies(
            self.args.rep_weights,
            self.args.quorum_snapshot.quorum_delta,
        );

        self.notify_winner_changed(old_winner);

        if self.election.is_final() && self.election.is_confirmed() {
            #[cfg(feature = "rai_protocol")]
            if self.election.rai_kind() != crate::consensus::election::RaiElectionKind::Slot {
                return;
            }
            self.election_got_confirmed();
        }
    }

    fn notify_winner_changed(&mut self, old_winner: BlockHash) {
        let winner_changed = self.election.winner().hash() != old_winner;
        if winner_changed {
            self.notify(AecFact::WinnerChanged(
                old_winner,
                self.election.winner().deref().clone(),
            ));
        }
    }

    fn election_got_confirmed(&mut self) {
        self.insert_recently_confirmed();

        let confirmed_election = self
            .election
            .into_confirmed_election(self.args.now, ConfirmationType::ActiveConfirmedQuorum);

        self.notify(AecFact::ElectionConfirmed(confirmed_election));
    }

    fn insert_recently_confirmed(&mut self) {
        self.recently_confirmed.put(
            self.election.qualified_root().clone(),
            self.election.winner().hash(),
        );
    }

    fn notify(&self, event: AecFact) {
        if let Some(o) = self.observer {
            o.send(event).unwrap();
        }
    }
}

#[cfg(all(test, not(feature = "rai_protocol")))]
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
        Block, BlockPriority, PrivateKey, QualifiedRoot, SavedBlock, StateBlockArgs,
        UnixMillisTimestamp, Vote,
    };
    use rsnano_utils::sync::backpressure_channel::channel;
    use std::time::Duration;

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

    #[test]
    fn ignore_vote_with_lower_timestamp() {
        let mut fixture = FixtureForElection::default();
        fixture.add_processed_vote(UnixMillisTimestamp::new(2000), Duration::ZERO);

        let result = fixture.apply_vote_from(VoteDelivery::Direct, UnixMillisTimestamp::new(1000));

        assert_eq!(result, Err(VoteError::Replay));
    }

    #[test]
    fn cool_down_live_vote() {
        let mut fixture = FixtureForElection::default();
        fixture.add_processed_vote(UnixMillisTimestamp::new(1000), Duration::from_millis(500));

        let result = fixture.apply_vote_from(VoteDelivery::Direct, UnixMillisTimestamp::new(2000));

        assert_eq!(result, Err(VoteError::Ignored));
    }

    #[test]
    fn dont_cool_down_when_enough_space_between_votes() {
        let mut fixture = FixtureForElection::default();
        fixture.add_processed_vote(UnixMillisTimestamp::new(1000), Duration::from_secs(15));

        let result = fixture.apply_vote_from(VoteDelivery::Direct, UnixMillisTimestamp::new(1100));

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn dont_cool_down_when_vote_comes_from_cache() {
        let mut fixture = FixtureForElection::default();
        fixture.add_processed_vote(UnixMillisTimestamp::new(1000), Duration::ZERO);

        let result =
            fixture.apply_vote_from(VoteDelivery::Replayed, UnixMillisTimestamp::new(1100));

        assert_eq!(result, Ok(()));
    }

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
        assert_eq!(fixture.events.len(), 1);
        let AecFact::WinnerChanged(old_winner, new_winner) = &fixture.events[0] else {
            panic!("not a winner changed event");
        };
        assert_eq!(old_winner, &block.hash());
        assert_eq!(new_winner, &fork);
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
                root: self.root.clone(),
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

            let mut vote_counter = VoteCounter::default();

            let mut helper = ApplyVoteHelper {
                args: &args,
                recently_confirmed: &mut self.recently_confirmed,
                vote_counter: &mut vote_counter,
                observer: &None,
                roots: &mut self.roots,
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
            self.election.add_vote(
                self.rep1_key.public_key(),
                self.block.hash(),
                created,
                self.now - received_ago,
            );
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
            let mut vote_counter = VoteCounter::default();
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
                    vote_counter: &mut vote_counter,
                    observer: &Some(tx),
                    election: &mut self.election,
                    block_hash: &vote.hashes[0],
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

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use super::*;
    use crate::{
        consensus::{FilteredVote, ReceivedVote, election::ElectionBehavior},
        representatives::QuorumSnapshot,
    };
    use rsnano_ledger::RepWeights;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{
        Amount, PrivateKey, RaiVoteMetadata, SavedBlock, UnixMillisTimestamp, Vote, VoteDelivery,
    };
    use rsnano_utils::sync::backpressure_channel::channel;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn confirmed_rai_election_emits_existing_confirmation_event() {
        let block = SavedBlock::new_test_instance();
        let key = PrivateKey::from(1);
        let committee = Arc::new(RepWeights::from([(key.public_key(), Amount::raw(1))]));
        let mut election = Election::new(
            block.clone(),
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            Timestamp::new_test_instance(),
        )
        .with_rai_committees(vec![committee]);
        let metadata = RaiVoteMetadata {
            election_id: election.rai_id().clone(),
            epoch: election.rai_epoch(),
            ..Default::default()
        };
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                vec![block.hash()],
                metadata,
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        let rep_weights = RepWeights::default();
        let quorum_snapshot = QuorumSnapshot::new_test_instance();
        let mut recently_confirmed = RecentlyConfirmedCache::default();
        let mut vote_counter = VoteCounter::default();
        let (tx, rx) = channel(8);

        ApplyVoteToElectionHelper {
            args: &ApplyVoteArgs {
                vote: &vote,
                rep_weights: &rep_weights,
                quorum_snapshot: &quorum_snapshot,
                now: Timestamp::new_test_instance(),
            },
            recently_confirmed: &mut recently_confirmed,
            vote_counter: &mut vote_counter,
            observer: &Some(tx),
            election: &mut election,
            block_hash: &block.hash(),
        }
        .apply_vote()
        .unwrap();

        assert!(election.is_confirmed());
        let AecFact::ElectionConfirmed(confirmed) = rx.recv().unwrap() else {
            panic!("expected confirmation")
        };
        assert_eq!(confirmed.rai_finalization_epoch, Some(election.rai_epoch()));
    }
}
