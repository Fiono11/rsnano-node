mod cached_vote_map;
mod voted_block;
mod voted_block_map;

#[cfg(not(test))]
use std::time::Instant;

use std::{
    cmp::Ordering, collections::HashMap, fmt::Debug, mem::size_of, sync::Arc, time::Duration,
};

#[cfg(test)]
use mock_instant::thread_local::Instant;

use rsnano_types::{Amount, BlockHash, Vote, VoteError};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, StatType, Stats},
};

use cached_vote_map::CachedVoteMap;
use voted_block::VotedBlock;
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
    config: VoteCacheConfig,
    blocks: VotedBlockMap,
    next_id: usize,
    last_cleanup: Instant,
    stats: Arc<Stats>,
}

impl VoteCache {
    pub fn new(config: VoteCacheConfig, stats: Arc<Stats>) -> Self {
        VoteCache {
            last_cleanup: Instant::now(),
            config,
            blocks: VotedBlockMap::default(),
            next_id: 0,
            stats,
        }
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.blocks.contains(hash)
    }

    /// Adds a new vote to cache
    pub fn insert(
        &mut self,
        vote: &Arc<Vote>,
        rep_weight: Amount,
        results: &HashMap<BlockHash, Result<(), VoteError>>,
    ) {
        // Results map should be empty or have the same hashes as the vote
        debug_assert!(results.is_empty() || vote.hashes.iter().all(|h| results.contains_key(h)));

        // If results map is empty, insert all hashes (meant for testing)
        if results.is_empty() {
            for hash in &vote.hashes {
                self.insert_impl(vote, hash, rep_weight);
            }
        } else {
            for (hash, code) in results {
                // Cache votes with a corresponding active election (indicated by `vote_code::vote`) in case that election gets dropped
                if matches!(code, Ok(()) | Err(VoteError::Indeterminate)) {
                    self.insert_impl(vote, hash, rep_weight)
                }
            }
        }
    }

    fn insert_impl(&mut self, vote: &Arc<Vote>, hash: &BlockHash, rep_weight: Amount) {
        let cache_entry_exists = self.blocks.modify_by_hash(hash, |existing| {
            self.stats.inc(StatType::VoteCache, DetailType::Update);
            existing.vote(vote, rep_weight, self.config.max_voters);
        });

        if !cache_entry_exists {
            self.stats.inc(StatType::VoteCache, DetailType::Insert);
            let id = self.next_id;
            self.next_id += 1;
            let mut cache_entry = VotedBlock::new(id, *hash);
            cache_entry.vote(vote, rep_weight, self.config.max_voters);
            self.blocks.insert(cache_entry);

            // Remove the oldest entry if we have reached the capacity limit
            if self.blocks.len() > self.config.max_size {
                self.blocks.pop_front();
            }
        }
    }

    pub fn empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn size(&self) -> usize {
        self.blocks.len()
    }

    /// Tries to find an entry associated with block hash
    pub fn find(&self, hash: &BlockHash) -> Vec<Arc<Vote>> {
        self.blocks
            .get_by_hash(hash)
            .map(|entry| entry.votes())
            .unwrap_or_default()
    }

    /// Removes an entry associated with block hash, does nothing if entry does not exist
    /// return true if hash existed and was erased, false otherwise
    pub fn erase(&mut self, hash: &BlockHash) -> bool {
        self.blocks.remove_by_hash(hash).is_some()
    }

    pub fn clear(&mut self) {
        self.blocks.clear()
    }

    /// Returns blocks with highest observed tally, greater than `min_tally`
    /// The blocks are sorted in descending order by final tally, then by tally
    /// @param min_tally minimum tally threshold, entries below with their voting weight
    /// below this will be ignore
    pub fn top(&mut self, min_tally: impl Into<Amount>) -> Vec<TopEntry> {
        let min_tally = min_tally.into();
        self.stats.inc(StatType::VoteCache, DetailType::Top);
        if self.last_cleanup.elapsed() >= self.config.age_cutoff / 2 {
            self.cleanup();
            self.last_cleanup = Instant::now();
        }

        let mut results = Vec::new();
        for entry in self.blocks.iter_by_tally_desc() {
            let tally = entry.tally();
            if tally < min_tally {
                break;
            }
            results.push(TopEntry {
                hash: entry.hash,
                tally,
                final_tally: entry.final_tally(),
            })
        }

        // Sort by final tally then by normal tally, descending
        results.sort_by(|a, b| {
            let res = b.final_tally.cmp(&a.final_tally);
            if res == Ordering::Equal {
                b.tally.cmp(&a.tally)
            } else {
                res
            }
        });

        results
    }

    fn cleanup(&mut self) {
        self.stats.inc(StatType::VoteCache, DetailType::Cleanup);
        let to_delete: Vec<_> = self
            .blocks
            .iter()
            .filter(|i| i.last_vote.elapsed() >= self.config.age_cutoff)
            .map(|i| i.hash)
            .collect();
        for hash in to_delete {
            self.blocks.remove_by_hash(&hash);
        }
    }
}

impl ContainerInfoProvider for VoteCache {
    fn container_info(&self) -> ContainerInfo {
        [("cache", self.size(), size_of::<VotedBlock>())].into()
    }
}

#[derive(PartialEq, Eq, Debug)]
pub struct TopEntry {
    pub hash: BlockHash,
    pub tally: Amount,
    pub final_tally: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedVote {
    pub vote: Arc<Vote>,
    pub weight: Amount,
}

impl CachedVote {
    pub fn new(vote: Arc<Vote>, weight: Amount) -> Self {
        Self { vote, weight }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mock_instant::thread_local::MockClock;
    use rsnano_types::{PrivateKey, UnixMillisTimestamp};
    use rsnano_utils::stats::Direction;

    fn create_vote(rep: &PrivateKey, hash: &BlockHash, timestamp_offset: u64) -> Arc<Vote> {
        let timestamp = UnixMillisTimestamp::new(timestamp_offset * 1024 * 1024);
        Arc::new(Vote::new(&rep, timestamp, 0, vec![*hash]))
    }

    fn create_final_vote(rep: &PrivateKey, hash: &BlockHash) -> Arc<Vote> {
        Arc::new(Vote::new_final(rep, vec![*hash]))
    }

    fn test_config() -> VoteCacheConfig {
        VoteCacheConfig {
            max_size: 3,
            max_voters: 80,
            age_cutoff: Duration::from_mins(5),
        }
    }

    fn create_vote_cache() -> VoteCache {
        VoteCache::new(test_config(), Arc::new(Stats::new(Default::default())))
    }

    fn create_vote_cache_with_max_voters(max_voters: usize) -> VoteCache {
        let config = VoteCacheConfig {
            max_voters,
            ..test_config()
        };
        VoteCache::new(config, Arc::new(Stats::new(Default::default())))
    }

    #[test]
    fn construction() {
        let cache = create_vote_cache();
        assert_eq!(cache.size(), 0);
        assert!(cache.empty());
        let hash = BlockHash::random();
        assert!(cache.find(&hash).is_empty());
    }

    #[test]
    fn insert_one_hash() {
        let mut cache = create_vote_cache();
        let rep = PrivateKey::new();
        let hash = BlockHash::from(1);
        let vote = create_vote(&rep, &hash, 1);

        cache.insert(&vote, Amount::raw(7), &HashMap::new());

        assert_eq!(cache.size(), 1);
        let peek = cache.find(&hash);
        assert_eq!(peek.len(), 1);
        assert_eq!(peek.first(), Some(&vote));
    }

    #[test]
    fn contains() {
        let mut cache = create_vote_cache();
        let rep = PrivateKey::new();
        let hash = BlockHash::from(1);
        let vote = create_vote(&rep, &hash, 1);

        assert_eq!(cache.contains(&hash), false);

        cache.insert(&vote, Amount::raw(7), &HashMap::new());

        assert_eq!(cache.contains(&hash), true);
    }

    /*
     * Inserts multiple votes for single hash
     * Ensures all of them can be retrieved and that tally is properly accumulated
     */
    #[test]
    fn insert_one_hash_many_votes() {
        let mut cache = create_vote_cache();

        let hash = BlockHash::random();
        let rep1 = PrivateKey::new();
        let rep2 = PrivateKey::new();
        let rep3 = PrivateKey::new();

        let vote1 = create_vote(&rep1, &hash, 1);
        let vote2 = create_vote(&rep2, &hash, 2);
        let vote3 = create_vote(&rep3, &hash, 3);

        cache.insert(&vote1, Amount::raw(7), &HashMap::new());
        cache.insert(&vote2, Amount::raw(9), &HashMap::new());
        cache.insert(&vote3, Amount::raw(11), &HashMap::new());
        // We have 3 votes but for a single hash, so just one entry in vote cache
        assert_eq!(cache.size(), 1);
        let votes = cache.find(&hash);
        assert_eq!(votes.len(), 3);
    }

    #[test]
    fn insert_many_hashes_many_votes() {
        let mut cache = create_vote_cache();

        // There will be 3 hashes to vote for
        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);

        // There will be 4 reps with different weights
        let rep1 = PrivateKey::new();
        let rep2 = PrivateKey::new();
        let rep3 = PrivateKey::new();
        let rep4 = PrivateKey::new();

        // Votes: rep1 > hash1, rep2 > hash2, rep3 > hash3, rep4 > hash1 (the same as rep1)
        let vote1 = create_vote(&rep1, &hash1, 1);
        let vote2 = create_vote(&rep2, &hash2, 1);
        let vote3 = create_vote(&rep3, &hash3, 1);
        let vote4 = create_vote(&rep4, &hash1, 1);

        // Insert first 3 votes in cache
        cache.insert(&vote1, Amount::raw(7), &HashMap::new());
        cache.insert(&vote2, Amount::raw(9), &HashMap::new());
        cache.insert(&vote3, Amount::raw(11), &HashMap::new());

        // Ensure all of those are properly inserted
        assert_eq!(cache.size(), 3);
        assert_eq!(cache.find(&hash1).len(), 1);
        assert_eq!(cache.find(&hash2).len(), 1);
        assert_eq!(cache.find(&hash3).len(), 1);

        // Now add a vote from rep4 with the highest voting weight
        cache.insert(&vote4, Amount::raw(13), &HashMap::new());

        let pop1 = cache.find(&hash1);
        assert_eq!(pop1.len(), 2);

        let pop2 = cache.find(&hash3);
        assert_eq!(pop2.len(), 1);
    }

    /*
     * Ensure that duplicate votes are ignored
     */
    #[test]
    fn insert_duplicate() {
        let mut cache = create_vote_cache();

        let hash = BlockHash::from(1);
        let rep = PrivateKey::new();
        let vote1 = create_vote(&rep, &hash, 1);
        let vote2 = create_vote(&rep, &hash, 1);

        cache.insert(&vote1, Amount::raw(9), &HashMap::new());
        cache.insert(&vote2, Amount::raw(9), &HashMap::new());

        assert_eq!(cache.size(), 1)
    }

    /*
     * Ensure that when processing vote from a representative that is already cached, we always update to the vote with the highest timestamp
     */
    #[test]
    fn insert_newer() {
        let mut cache = create_vote_cache();

        let hash = BlockHash::from(1);
        let rep = PrivateKey::new();
        let vote1 = create_vote(&rep, &hash, 1);
        cache.insert(&vote1, Amount::raw(9), &HashMap::new());

        let vote2 = Arc::new(Vote::new(
            &rep,
            Vote::TIMESTAMP_MAX,
            Vote::DURATION_MAX,
            vec![hash],
        ));
        cache.insert(&vote2, Amount::raw(9), &HashMap::new());

        let peek2 = cache.find(&hash);
        assert_eq!(peek2.len(), 1);
        assert!(peek2.first().unwrap().is_final());
    }

    /*
     * Ensure that when processing vote from a representative that is already cached, votes with older timestamp are ignored
     */
    #[test]
    fn insert_older() {
        let mut cache = create_vote_cache();
        let hash = BlockHash::from(1);
        let rep = PrivateKey::new();
        let vote1 = create_vote(&rep, &hash, 2);
        cache.insert(&vote1, Amount::raw(9), &HashMap::new());
        let peek1 = cache.find(&hash);

        let vote2 = create_vote(&rep, &hash, 1);
        cache.insert(&vote2, Amount::raw(9), &HashMap::new());
        let peek2 = cache.find(&hash);

        assert_eq!(cache.size(), 1);
        assert_eq!(peek2.len(), 1);
        assert_eq!(
            peek2.first().unwrap().timestamp(),
            peek1.first().unwrap().timestamp()
        );
    }

    /*
     * Ensure that erase functionality works
     */
    #[test]
    fn erase() {
        let mut cache = create_vote_cache();
        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);

        let rep1 = PrivateKey::new();
        let rep2 = PrivateKey::new();
        let rep3 = PrivateKey::new();

        let vote1 = create_vote(&rep1, &hash1, 1);
        let vote2 = create_vote(&rep2, &hash2, 1);
        let vote3 = create_vote(&rep3, &hash3, 1);

        cache.insert(&vote1, Amount::raw(7), &HashMap::new());
        cache.insert(&vote2, Amount::raw(9), &HashMap::new());
        cache.insert(&vote3, Amount::raw(11), &HashMap::new());

        assert_eq!(cache.size(), 3);
        assert_eq!(cache.find(&hash1).len(), 1);
        assert_eq!(cache.find(&hash2).len(), 1);
        assert_eq!(cache.find(&hash3).len(), 1);

        cache.erase(&hash2);

        assert_eq!(cache.size(), 2);
        assert_eq!(cache.contains(&hash2), false);
        assert_eq!(cache.find(&hash1).len(), 1);
        assert_eq!(cache.find(&hash2).len(), 0);
        assert_eq!(cache.find(&hash3).len(), 1);
        cache.erase(&hash1);
        cache.erase(&hash3);

        assert!(cache.empty());
    }

    /*
     * Ensure that when cache is overfilled, we remove the oldest entries first
     */
    #[test]
    fn overfill() {
        let mut cache = create_vote_cache();

        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);
        let hash4 = BlockHash::from(4);

        let rep1 = PrivateKey::new();
        let rep2 = PrivateKey::new();
        let rep3 = PrivateKey::new();
        let rep4 = PrivateKey::new();

        let vote1 = create_vote(&rep1, &hash1, 1);
        cache.insert(&vote1, Amount::raw(1), &HashMap::new());

        let vote2 = create_vote(&rep2, &hash2, 1);
        cache.insert(&vote2, Amount::raw(2), &HashMap::new());

        let vote3 = create_vote(&rep3, &hash3, 1);
        cache.insert(&vote3, Amount::raw(3), &HashMap::new());

        let vote4 = create_vote(&rep4, &hash4, 1);
        cache.insert(&vote4, Amount::raw(4), &HashMap::new());

        assert_eq!(cache.size(), 3);

        // Check that oldest votes are dropped first
        assert_eq!(cache.find(&hash4).len(), 1);
        assert_eq!(cache.find(&hash3).len(), 1);
        assert_eq!(cache.find(&hash2).len(), 1);
        assert_eq!(cache.find(&hash1).len(), 0);
    }

    /*
     * Check that when a single vote cache entry is overfilled, it ignores any new votes
     */
    #[test]
    fn overfill_entry() {
        let mut cache = create_vote_cache();
        let hash = BlockHash::from(1);

        let rep1 = PrivateKey::new();
        let vote1 = create_vote(&rep1, &hash, 1);
        cache.insert(&vote1, Amount::raw(9), &HashMap::new());

        let rep2 = PrivateKey::new();
        let vote2 = create_vote(&rep2, &hash, 1);
        cache.insert(&vote2, Amount::raw(9), &HashMap::new());

        let rep3 = PrivateKey::new();
        let vote3 = create_vote(&rep3, &hash, 1);
        cache.insert(&vote3, Amount::raw(9), &HashMap::new());

        assert_eq!(cache.size(), 1);
    }

    #[test]
    fn change_vote_to_final_vote() {
        let mut cache = create_vote_cache();
        let hash = BlockHash::from(1);

        let rep = PrivateKey::new();
        let vote = create_vote(&rep, &hash, 1);
        let final_vote = create_final_vote(&rep, &hash);
        cache.insert(&vote, Amount::raw(9), &HashMap::new());
        cache.insert(&final_vote, Amount::raw(9), &HashMap::new());

        let votes = cache.find(&hash);
        let vote = votes.first().unwrap();
        assert!(vote.is_final());
    }

    #[test]
    fn add_final_vote() {
        let mut cache = create_vote_cache();
        let hash = BlockHash::from(1);

        let rep = PrivateKey::new();
        let vote = create_final_vote(&rep, &hash);
        cache.insert(&vote, Amount::raw(9), &HashMap::new());

        let votes = cache.find(&hash);
        let vote = votes.first().unwrap();
        assert!(vote.is_final());
    }

    #[test]
    fn top_empty() {
        let mut cache = create_vote_cache();
        assert_eq!(cache.top(0), Vec::new());
    }

    #[test]
    fn top_one_entry() {
        let mut cache = create_vote_cache();
        let hash = BlockHash::from(1);
        add_test_vote(&mut cache, &hash, Amount::raw(1));

        assert_eq!(
            cache.top(0),
            vec![TopEntry {
                hash,
                tally: Amount::raw(1),
                final_tally: Amount::ZERO
            }]
        );
    }

    #[test]
    fn top_multiple_entries_sorted_by_tally() {
        let mut cache = create_vote_cache();
        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);
        add_test_vote(&mut cache, &hash1, Amount::raw(1));
        add_test_vote(&mut cache, &hash2, Amount::raw(4));
        add_test_vote(&mut cache, &hash3, Amount::raw(3));
        add_test_final_vote(&mut cache, &hash2, Amount::raw(5));
        add_test_final_vote(&mut cache, &hash3, Amount::raw(5));

        let top = cache.top(0);

        assert_eq!(top.len(), 3);
        assert_eq!(top[0].hash, hash2);
        assert_eq!(top[1].hash, hash3);
        assert_eq!(top[2].hash, hash1);
    }

    #[test]
    fn top_min_tally() {
        let mut cache = create_vote_cache();
        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);
        add_test_vote(&mut cache, &hash1, Amount::raw(1));
        add_test_vote(&mut cache, &hash2, Amount::raw(2));
        add_test_vote(&mut cache, &hash3, Amount::raw(3));

        let top = cache.top(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].hash, hash3);
        assert_eq!(top[1].hash, hash2);
    }

    #[test]
    fn top_age_cutoff() {
        let stats = Arc::new(Stats::new(Default::default()));
        let mut cache = VoteCache::new(test_config(), Arc::clone(&stats));
        let hash = BlockHash::from(1);
        add_test_vote(&mut cache, &hash, Amount::raw(1));
        assert_eq!(
            stats.count(StatType::VoteCache, DetailType::Cleanup, Direction::In),
            0
        );
        MockClock::advance(Duration::from_secs(150));
        assert_eq!(cache.top(0).len(), 1);
        assert_eq!(
            stats.count(StatType::VoteCache, DetailType::Cleanup, Direction::In),
            1
        );
        MockClock::advance(Duration::from_secs(150));
        assert_eq!(cache.top(0).len(), 0);
        assert_eq!(
            stats.count(StatType::VoteCache, DetailType::Cleanup, Direction::In),
            2
        );
    }

    /*
     * Ensure that entries with a higher final tally are ranked above entries with a higher
     * regular tally (final tally is the primary sort key in `top`).
     */
    #[test]
    fn top_sorted_by_final_tally_first() {
        let mut cache = create_vote_cache();
        let hash_regular = BlockHash::from(1);
        let hash_final = BlockHash::from(2);

        // hash_regular has the higher regular tally, but no final votes
        add_test_vote(&mut cache, &hash_regular, Amount::raw(10));
        // hash_final has a lower regular tally, but it is all final weight
        add_test_final_vote(&mut cache, &hash_final, Amount::raw(5));

        let top = cache.top(0);

        assert_eq!(top.len(), 2);
        assert_eq!(top[0].hash, hash_final);
        assert_eq!(top[0].final_tally, Amount::raw(5));
        assert_eq!(top[1].hash, hash_regular);
        assert_eq!(top[1].final_tally, Amount::ZERO);
    }

    /*
     * Ensure that a single entry can hold exactly `max_voters` voters (off-by-one regression)
     * and that once exceeded the lowest weight voter is evicted.
     */
    #[test]
    fn entry_holds_exactly_max_voters() {
        let max_voters = 3;
        let mut cache = create_vote_cache_with_max_voters(max_voters);
        let hash = BlockHash::from(1);

        // Fill the entry up to max_voters
        for i in 1..=max_voters as u128 {
            let rep = PrivateKey::new();
            let vote = create_vote(&rep, &hash, 1);
            cache.insert(&vote, Amount::raw(i), &HashMap::new());
        }

        // The entry must hold exactly max_voters voters, not max_voters - 1
        assert_eq!(cache.find(&hash).len(), max_voters);

        // A higher weight vote evicts the lowest and keeps the count at max_voters
        let high_rep = PrivateKey::new();
        let high_vote = create_vote(&high_rep, &hash, 1);
        cache.insert(&high_vote, Amount::raw(100), &HashMap::new());
        assert_eq!(cache.find(&hash).len(), max_voters);

        // A vote below the minimum weight is rejected, count stays the same
        let low_rep = PrivateKey::new();
        let low_vote = create_vote(&low_rep, &hash, 1);
        cache.insert(&low_vote, Amount::raw(1), &HashMap::new());
        assert_eq!(cache.find(&hash).len(), max_voters);
    }

    /*
     * Ensure that only a single voter is evicted when several reps share the lowest weight,
     * instead of dropping the whole weight bucket.
     */
    #[test]
    fn evicts_only_one_voter_on_tie() {
        let max_voters = 2;
        let mut cache = create_vote_cache_with_max_voters(max_voters);
        let hash = BlockHash::from(1);

        // Two reps tie at the lowest weight
        let rep1 = PrivateKey::new();
        cache.insert(
            &create_vote(&rep1, &hash, 1),
            Amount::raw(5),
            &HashMap::new(),
        );
        let rep2 = PrivateKey::new();
        cache.insert(
            &create_vote(&rep2, &hash, 1),
            Amount::raw(5),
            &HashMap::new(),
        );
        assert_eq!(cache.find(&hash).len(), 2);

        // A higher weight vote exceeds capacity and must evict exactly one of the tied voters
        let rep3 = PrivateKey::new();
        cache.insert(
            &create_vote(&rep3, &hash, 1),
            Amount::raw(10),
            &HashMap::new(),
        );
        assert_eq!(cache.find(&hash).len(), 2);
    }

    fn add_test_vote(cache: &mut VoteCache, hash: &BlockHash, rep_weight: Amount) {
        let vote = create_vote(&PrivateKey::new(), &hash, 0);
        cache.insert(&vote, rep_weight, &HashMap::new());
    }

    fn add_test_final_vote(cache: &mut VoteCache, hash: &BlockHash, rep_weight: Amount) {
        let vote = create_final_vote(&PrivateKey::new(), &hash);
        cache.insert(&vote, rep_weight, &HashMap::new());
    }
}
