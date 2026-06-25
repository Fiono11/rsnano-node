mod stats;
mod tally_index;
mod vote_cache_processor;
mod voted_block;
mod voted_block_map;

pub use voted_block_map::TopEntry;

use std::{
    collections::HashMap,
    fmt::Debug,
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};

use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Amount, BlockHash, Vote, VoteDelivery, VoteError};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{Stats, StatsCollection, StatsSource},
    EventHandler,
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
    stats: VoteCacheStats,
    clock: SteadyClock,
    processor: Arc<VoteCacheProcessor>,
}

impl VoteCache {
    pub fn new(
        config: VoteCacheConfig,
        vote_queue: Arc<VoteProcessorQueue>,
        stats: Arc<Stats>,
    ) -> Self {
        Self::new_impl(config, vote_queue, stats, SteadyClock::default())
    }

    pub fn new_null() -> Self {
        let vote_queue = Arc::new(VoteProcessorQueue::new_null());
        let stats = Arc::new(Stats::default());
        Self::new_impl(
            VoteCacheConfig::default(),
            vote_queue,
            stats,
            SteadyClock::new_null(),
        )
    }

    fn new_impl(
        config: VoteCacheConfig,
        vote_queue: Arc<VoteProcessorQueue>,
        stats: Arc<Stats>,
        clock: SteadyClock,
    ) -> Self {
        let blocks = Arc::new(Mutex::new(VotedBlockMap::new(config)));
        let processor = Arc::new(VoteCacheProcessor::new(
            stats,
            blocks.clone(),
            vote_queue,
            16384,
        ));

        VoteCache {
            blocks,
            clock,
            processor,
            stats: VoteCacheStats::default(),
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
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{PrivateKey, UnixMillisTimestamp};

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

    /*
     * Test helpers
     */

    fn make_vote_cache() -> VoteCache {
        let vote_queue = Arc::new(VoteProcessorQueue::new_null());
        let stats = Arc::new(Stats::default());
        VoteCache::new_impl(
            Default::default(),
            vote_queue,
            stats,
            SteadyClock::new_null(),
        )
    }

    fn create_vote(rep: &PrivateKey, hash: &BlockHash, timestamp_offset: u64) -> Arc<Vote> {
        let timestamp = UnixMillisTimestamp::new(timestamp_offset * 1024 * 1024);
        Arc::new(Vote::new(&rep, timestamp, 0, vec![*hash]))
    }
}
