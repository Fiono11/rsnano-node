mod enqueuer;
mod stats;
mod tally_index;
mod vote_cache_processor;
mod voted_block;
mod voted_block_map;

pub use voted_block_map::TopEntry;

use std::{
    collections::HashMap,
    fmt::Debug,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

use rsnano_nullable_clock::SteadyClock;
#[cfg(feature = "rai_protocol")]
use rsnano_types::VoteType;
use rsnano_types::{Amount, BlockHash, Vote, VoteDelivery, VoteError};
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    thread_factory::ThreadFactory,
};

use crate::consensus::{AecFact, VoteProcessorQueue};
use stats::VoteCacheStats;
use vote_cache_processor::VoteCacheProcessor;
use voted_block_map::VotedBlockMap;

#[derive(Clone, Debug, PartialEq)]
pub struct VoteCacheConfig {
    pub max_size: usize,
    pub max_voters: usize,
    pub age_cutoff: Duration,
}

impl Default for VoteCacheConfig {
    fn default() -> Self {
        Self {
            max_size: 1024 * 64,
            max_voters: 64,
            age_cutoff: Duration::from_mins(15),
        }
    }
}

/// A container holding votes that do not match any active or recently finished elections.
/// Stores votes associated with a particular block hash with a bounded maximum number of votes per hash.
/// When cache size exceeds `max_size` oldest entries are evicted first.
pub struct VoteCache {
    blocks: Arc<Mutex<VotedBlockMap>>,
    stats: Arc<VoteCacheStats>,
    clock: SteadyClock,
    processor: VoteCacheProcessor,
}

impl VoteCache {
    pub fn new(config: VoteCacheConfig, vote_queue: Arc<VoteProcessorQueue>) -> Self {
        Self::new_impl(
            config,
            vote_queue,
            SteadyClock::default(),
            ThreadFactory::default(),
        )
    }

    pub fn new_null() -> Self {
        let vote_queue = Arc::new(VoteProcessorQueue::new_null());
        Self::new_impl(
            VoteCacheConfig::default(),
            vote_queue,
            SteadyClock::new_null(),
            ThreadFactory::new_null(),
        )
    }

    fn new_impl(
        config: VoteCacheConfig,
        vote_queue: Arc<VoteProcessorQueue>,
        clock: SteadyClock,
        thread_factory: ThreadFactory,
    ) -> Self {
        let blocks = Arc::new(Mutex::new(VotedBlockMap::new(config)));
        let stats = Arc::new(VoteCacheStats::default());
        let processor = VoteCacheProcessor::new(
            blocks.clone(),
            vote_queue,
            stats.clone(),
            16384,
            thread_factory,
        );

        VoteCache {
            blocks,
            clock,
            processor,
            stats,
        }
    }

    pub fn start(&self) {
        self.processor.start();
    }

    pub fn stop(&self) {
        self.processor.stop();
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.blocks.lock().unwrap().contains(hash)
    }

    pub fn get(&self, hash: &BlockHash) -> Vec<Arc<Vote>> {
        let blocks = self.blocks.lock().unwrap();
        let Some(block) = blocks.get(hash) else {
            return Vec::new();
        };
        block.iter_votes().cloned().collect()
    }

    /// Adds a new vote to cache
    pub fn process(
        &self,
        vote: Arc<Vote>,
        rep_weight: Amount,
        results: &HashMap<BlockHash, Result<(), VoteError>>,
    ) {
        let now = self.clock.now();
        let inserted = self
            .blocks
            .lock()
            .unwrap()
            .process(vote, rep_weight, results, now);
        self.stats.inserted.fetch_add(inserted, Ordering::Relaxed);
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.lock().unwrap().is_empty()
    }

    pub fn len(&self) -> usize {
        self.blocks.lock().unwrap().len()
    }

    pub fn collect_votes<'a>(&self, result: &mut Vec<Arc<Vote>>, hash: &BlockHash) {
        self.blocks.lock().unwrap().collect_votes(result, hash);
    }

    pub fn vote_count(&self, hash: &BlockHash) -> usize {
        self.blocks.lock().unwrap().vote_count(hash)
    }

    /// Removes an entry associated with block hash, does nothing if entry does not exist
    /// return true if hash existed and was erased, false otherwise
    pub fn remove(&self, hash: &BlockHash) -> bool {
        self.blocks.lock().unwrap().remove(hash).is_some()
    }

    pub fn clear(&self) {
        self.blocks.lock().unwrap().clear()
    }

    /// Returns blocks with highest observed tally, greater than `min_tally`
    /// The blocks are sorted in descending order by final tally, then by tally
    /// @param min_tally minimum tally threshold, entries below with their voting weight
    /// below this will be ignore
    pub fn top(&self, result: &mut Vec<TopEntry>, min_tally: impl Into<Amount>) {
        self.stats.top.fetch_add(1, Ordering::Relaxed);
        let now = self.clock.now();
        self.blocks.lock().unwrap().top(result, min_tally, now);
    }

    pub fn get_non_final_tally(&self, hash: &BlockHash) -> Amount {
        self.blocks
            .lock()
            .unwrap()
            .get(hash)
            .map(|b| b.non_final_tally())
            .unwrap_or_default()
    }

    /// Returns hashes whose cached votes for `epoch` have distinct representative
    /// weight strictly greater than the Byzantine fault budget.
    #[cfg(feature = "rai_protocol")]
    pub fn supported_hashes_for_epoch(&self, epoch: u64, faulty_weight: Amount) -> Vec<BlockHash> {
        self.blocks
            .lock()
            .unwrap()
            .iter()
            .filter(|block| block.tally_for_epoch(epoch) > faulty_weight)
            .map(|block| *block.block_hash())
            .collect()
    }

    /// Returns hashes carrying a complete fast or final certificate for `epoch`.
    #[cfg(feature = "rai_protocol")]
    pub fn finalized_hashes_for_epoch(
        &self,
        epoch: u64,
        fast_threshold: Amount,
        final_threshold: Amount,
    ) -> Vec<BlockHash> {
        self.blocks
            .lock()
            .unwrap()
            .iter()
            .filter(|block| {
                block.phase_tally_for_epoch(epoch, VoteType::First) >= fast_threshold
                    || block.phase_tally_for_epoch(epoch, VoteType::Final) >= final_threshold
            })
            .map(|block| *block.block_hash())
            .collect()
    }
}

impl ContainerInfoProvider for VoteCache {
    fn container_info(&self) -> ContainerInfo {
        [
            ("blocks", self.len(), 0),
            ("processor", self.processor.len(), 0),
        ]
        .into()
    }
}

impl StatsSource for VoteCache {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result)
    }
}

impl EventHandler<AecFact> for VoteCache {
    fn handle(&self, event: &AecFact) {
        match event {
            AecFact::ElectionStarted(hash, _root) => self.processor.trigger(*hash),
            AecFact::BlockAddedToElection(hash) => self.processor.trigger(*hash),
            AecFact::VoteProcessed(vote, voter_weight, results) => {
                // Cache the votes that didn't match any election
                if vote.delivery != VoteDelivery::Replayed {
                    self.process(vote.vote.clone(), *voter_weight, results);
                    // ElectionStarted and VoteProcessed facts can be produced by
                    // different threads and observed in either order. If the election
                    // trigger ran before this indeterminate vote reached the cache,
                    // trigger again now. Vote routing includes the RAI epoch, so a
                    // replay cannot leak into an election for another epoch.
                    for (hash, result) in results {
                        if matches!(result, Err(VoteError::Indeterminate)) {
                            self.processor.trigger(*hash);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::ReceivedVote;
    #[cfg(feature = "rai_protocol")]
    use rsnano_types::VoteType;
    use rsnano_types::{PrivateKey, QualifiedRoot, UnixMillisTimestamp};

    #[test]
    fn construction() {
        let cache = make_vote_cache();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
        let hash = BlockHash::random();
        assert_eq!(cache.vote_count(&hash), 0);
    }

    #[test]
    fn insert_one_hash() {
        let cache = make_vote_cache();
        let rep = PrivateKey::from(1);
        let hash = BlockHash::from(1);
        let vote = create_vote(&rep, &hash, 1);

        cache.process(vote.clone(), Amount::raw(7), &HashMap::new());

        assert_eq!(cache.len(), 1);
        let mut votes = Vec::new();
        cache.collect_votes(&mut votes, &hash);
        assert_eq!(votes.len(), 1);
        assert_eq!(votes.first(), Some(&vote));
        assert_eq!(cache.contains(&hash), true);
    }

    #[test]
    fn remove() {
        let cache = make_vote_cache();
        let hash1 = BlockHash::from(1);
        let rep1 = PrivateKey::from(1);
        let vote1 = create_vote(&rep1, &hash1, 1);
        cache.process(vote1, Amount::raw(7), &HashMap::new());

        cache.remove(&hash1);

        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn top_empty() {
        let cache = make_vote_cache();
        let mut top = Vec::new();
        cache.top(&mut top, 0);
        assert_eq!(top, Vec::new());
    }

    #[test]
    fn top_one_entry() {
        let cache = make_vote_cache();
        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let vote = create_vote(&rep, &hash, 0);
        cache.process(vote, Amount::raw(1), &HashMap::new());

        let mut top = Vec::new();
        cache.top(&mut top, 0);

        assert_eq!(
            top,
            vec![TopEntry {
                hash,
                tally: Amount::raw(1),
                final_tally: Amount::ZERO
            }]
        );
    }

    #[test]
    fn enqueues_cached_vote_when_election_started() {
        let cache = make_vote_cache();
        let trigger_tracker = cache.processor.track_trigger();

        let block_hash = BlockHash::from(1);
        cache.handle(&AecFact::ElectionStarted(
            block_hash,
            QualifiedRoot::new_test_instance(),
        ));

        let triggers = trigger_tracker.output();
        assert_eq!(triggers, vec![block_hash]);
    }

    #[test]
    fn enqueues_cached_vote_when_block_added_to_election() {
        let cache = make_vote_cache();
        let trigger_tracker = cache.processor.track_trigger();

        let block_hash = BlockHash::from(1);
        cache.handle(&AecFact::BlockAddedToElection(block_hash));

        let triggers = trigger_tracker.output();
        assert_eq!(triggers, vec![block_hash]);
    }

    #[test]
    fn adds_processed_vote_to_cache() {
        let cache = make_vote_cache();
        let block_hash = BlockHash::from(1);
        let vote = Arc::new(Vote::build_test_instance().blocks([block_hash]).finish());
        let recv_vote = ReceivedVote::new(vote, VoteDelivery::Direct, None);

        cache.handle(&AecFact::VoteProcessed(
            recv_vote,
            Amount::nano(1000),
            HashMap::new(),
        ));

        assert_eq!(cache.vote_count(&block_hash), 1);
    }

    #[test]
    fn indeterminate_vote_triggers_replay_after_cache_insertion() {
        let cache = make_vote_cache();
        let trigger_tracker = cache.processor.track_trigger();
        let block_hash = BlockHash::from(1);
        let vote = Arc::new(Vote::build_test_instance().blocks([block_hash]).finish());
        let recv_vote = ReceivedVote::new(vote, VoteDelivery::Direct, None);

        // Model the racy ordering: the one-shot election trigger is handled before
        // the indeterminate vote fact inserts the vote into the cache.
        cache.handle(&AecFact::ElectionStarted(
            block_hash,
            QualifiedRoot::new_test_instance(),
        ));
        cache.handle(&AecFact::VoteProcessed(
            recv_vote,
            Amount::nano(1000),
            HashMap::from([(block_hash, Err(VoteError::Indeterminate))]),
        ));

        assert_eq!(cache.vote_count(&block_hash), 1);
        assert_eq!(trigger_tracker.output(), vec![block_hash, block_hash]);
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn closing_epoch_recovery_requires_strictly_more_than_f_weight() {
        let cache = make_vote_cache();
        let hash = BlockHash::from(1);
        let other_epoch_hash = BlockHash::from(2);
        let first = PrivateKey::from(1);
        let second = PrivateKey::from(2);

        cache.process(
            Arc::new(Vote::new_rai(&first, 7, VoteType::First, vec![hash])),
            Amount::raw(5),
            &HashMap::new(),
        );
        cache.process(
            Arc::new(Vote::new_rai(
                &first,
                8,
                VoteType::First,
                vec![other_epoch_hash],
            )),
            Amount::raw(100),
            &HashMap::new(),
        );

        assert!(
            cache
                .supported_hashes_for_epoch(7, Amount::raw(5))
                .is_empty()
        );

        cache.process(
            Arc::new(Vote::new_rai(&second, 7, VoteType::First, vec![hash])),
            Amount::raw(1),
            &HashMap::new(),
        );

        assert_eq!(
            cache.supported_hashes_for_epoch(7, Amount::raw(5)),
            vec![hash]
        );
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn epoch_alias_support_survives_votes_for_same_hash_in_another_epoch() {
        let cache = make_vote_cache();
        let hash = BlockHash::from(1);
        let first = PrivateKey::from(1);
        let second = PrivateKey::from(2);
        let late = HashMap::from([(hash, Err(VoteError::Late))]);

        cache.process(
            Arc::new(Vote::new_rai(&first, 1, VoteType::First, vec![hash])),
            Amount::raw(5),
            &HashMap::new(),
        );
        cache.process(
            Arc::new(Vote::new_rai(&first, 2, VoteType::First, vec![hash])),
            Amount::raw(5),
            &late,
        );
        cache.process(
            Arc::new(Vote::new_rai(&second, 2, VoteType::First, vec![hash])),
            Amount::raw(1),
            &late,
        );

        assert_eq!(
            cache.supported_hashes_for_epoch(2, Amount::raw(5)),
            vec![hash]
        );
    }

    /*
     * Test helpers
     */

    fn make_vote_cache() -> VoteCache {
        let vote_queue = Arc::new(VoteProcessorQueue::new_null());
        VoteCache::new_impl(
            Default::default(),
            vote_queue,
            SteadyClock::new_null(),
            ThreadFactory::new_null(),
        )
    }

    fn create_vote(rep: &PrivateKey, hash: &BlockHash, timestamp_offset: u64) -> Arc<Vote> {
        let timestamp = UnixMillisTimestamp::new(timestamp_offset * 1024 * 1024);
        Arc::new(Vote::new(&rep, timestamp, 0, vec![*hash]))
    }
}
