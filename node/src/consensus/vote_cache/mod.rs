mod stats;
mod tally_index;
mod voted_block;
mod voted_block_map;

pub use voted_block_map::TopEntry;

use std::{
    collections::HashMap,
    fmt::Debug,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Amount, BlockHash, Vote, VoteError};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use stats::VoteCacheStats;
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
/// It keeps track of votes in two internal structures: cache and queue.
/// Cache: Stores votes associated with a particular block hash with a bounded maximum number of votes per hash.
/// When cache size exceeds `max_size` oldest entries are evicted first.
pub struct VoteCache {
    blocks: VotedBlockMap,
    stats: VoteCacheStats,
    clock: SteadyClock,
}

impl VoteCache {
    pub fn new(config: VoteCacheConfig) -> Self {
        Self::new_impl(config, SteadyClock::default())
    }

    fn new_impl(config: VoteCacheConfig, clock: SteadyClock) -> Self {
        VoteCache {
            blocks: VotedBlockMap::new(config),
            stats: VoteCacheStats::default(),
            clock,
        }
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.blocks.contains(hash)
    }

    /// Adds a new vote to cache
    pub fn process(
        &mut self,
        vote: Arc<Vote>,
        rep_weight: Amount,
        results: &HashMap<BlockHash, Result<(), VoteError>>,
    ) {
        let now = self.clock.now();
        let inserted = self.blocks.process(vote, rep_weight, results, now);
        self.stats.inserted.fetch_add(inserted, Ordering::Relaxed);
    }

    pub fn empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn size(&self) -> usize {
        self.blocks.len()
    }

    pub fn collect_votes<'a>(&self, result: &mut Vec<Arc<Vote>>, hash: &BlockHash) {
        self.blocks.collect_votes(result, hash);
    }

    pub fn vote_count(&self, hash: &BlockHash) -> usize {
        self.blocks.vote_count(hash)
    }

    /// Removes an entry associated with block hash, does nothing if entry does not exist
    /// return true if hash existed and was erased, false otherwise
    pub fn remove(&mut self, hash: &BlockHash) -> bool {
        self.blocks.remove(hash).is_some()
    }

    pub fn clear(&mut self) {
        self.blocks.clear()
    }

    /// Returns blocks with highest observed tally, greater than `min_tally`
    /// The blocks are sorted in descending order by final tally, then by tally
    /// @param min_tally minimum tally threshold, entries below with their voting weight
    /// below this will be ignore
    pub fn top(&mut self, min_tally: impl Into<Amount>) -> Vec<TopEntry> {
        self.stats.top.fetch_add(1, Ordering::Relaxed);
        let now = self.clock.now();
        self.blocks.top(min_tally, now)
    }

    pub fn get_non_final_tally(&self, hash: &BlockHash) -> Amount {
        self.blocks
            .get(hash)
            .map(|b| b.non_final_tally())
            .unwrap_or_default()
    }
}

impl ContainerInfoProvider for VoteCache {
    fn container_info(&self) -> ContainerInfo {
        [("vote_cache", self.size(), 0)].into()
    }
}

impl StatsSource for VoteCache {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{PrivateKey, UnixMillisTimestamp};

    #[test]
    fn construction() {
        let cache = make_vote_cache();
        assert_eq!(cache.size(), 0);
        assert!(cache.empty());
        let hash = BlockHash::random();
        assert_eq!(cache.vote_count(&hash), 0);
    }

    #[test]
    fn insert_one_hash() {
        let mut cache = make_vote_cache();
        let rep = PrivateKey::from(1);
        let hash = BlockHash::from(1);
        let vote = create_vote(&rep, &hash, 1);

        cache.process(vote.clone(), Amount::raw(7), &HashMap::new());

        assert_eq!(cache.size(), 1);
        let mut votes = Vec::new();
        cache.collect_votes(&mut votes, &hash);
        assert_eq!(votes.len(), 1);
        assert_eq!(votes.first(), Some(&vote));
        assert_eq!(cache.contains(&hash), true);
    }

    #[test]
    fn remove() {
        let mut cache = make_vote_cache();
        let hash1 = BlockHash::from(1);
        let rep1 = PrivateKey::from(1);
        let vote1 = create_vote(&rep1, &hash1, 1);
        cache.process(vote1, Amount::raw(7), &HashMap::new());

        cache.remove(&hash1);

        assert_eq!(cache.size(), 0);
    }

    #[test]
    fn top_empty() {
        let mut cache = make_vote_cache();
        assert_eq!(cache.top(0), Vec::new());
    }

    #[test]
    fn top_one_entry() {
        let mut cache = make_vote_cache();
        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let vote = create_vote(&rep, &hash, 0);
        cache.process(vote, Amount::raw(1), &HashMap::new());

        assert_eq!(
            cache.top(0),
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
        VoteCache::new_impl(Default::default(), SteadyClock::new_null())
    }

    fn create_vote(rep: &PrivateKey, hash: &BlockHash, timestamp_offset: u64) -> Arc<Vote> {
        let timestamp = UnixMillisTimestamp::new(timestamp_offset * 1024 * 1024);
        Arc::new(Vote::new(&rep, timestamp, 0, vec![*hash]))
    }
}
