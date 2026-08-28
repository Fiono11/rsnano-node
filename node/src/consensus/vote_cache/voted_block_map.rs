use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use rustc_hash::FxHashMap;

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Amount, BlockHash, Vote, VoteError};

use super::VoteCacheConfig;
use super::tally_index::TallyIndex;
use super::voted_block::VotedBlock;

#[derive(PartialEq, Eq, Debug)]
pub struct TopEntry {
    pub hash: BlockHash,
    pub tally: Amount,
    pub final_tally: Amount,
}

pub(crate) struct VotedBlockMap {
    config: VoteCacheConfig,
    sequential: BTreeMap<usize, BlockHash>,
    by_hash: FxHashMap<BlockHash, VotedBlock>,
    by_tally: TallyIndex,
    next_id: usize,
    last_cleanup: Option<Timestamp>,
}

impl VotedBlockMap {
    pub fn new(config: VoteCacheConfig) -> Self {
        Self {
            config,
            sequential: Default::default(),
            by_hash: Default::default(),
            by_tally: Default::default(),
            next_id: Default::default(),
            last_cleanup: None,
        }
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.by_hash.contains_key(hash)
    }

    pub fn process(
        &mut self,
        vote: Arc<Vote>,
        rep_weight: Amount,
        results: &HashMap<BlockHash, Result<(), VoteError>>,
        now: Timestamp,
    ) -> u64 {
        let mut inserted = 0;
        // Results map should be empty or have the same hashes as the vote
        debug_assert!(results.is_empty() || vote.hashes.iter().all(|h| results.contains_key(h)));

        // If results map is empty, insert all hashes (meant for testing)
        if results.is_empty() {
            for hash in &vote.hashes {
                self.insert_vote(vote.clone(), hash, rep_weight, now);
                inserted += 1;
            }
        } else {
            for (hash, code) in results {
                // Cache votes with a corresponding election in case that election gets dropped
                #[cfg(not(feature = "rai_protocol"))]
                let should_cache = matches!(code, Ok(()) | Err(VoteError::Indeterminate));
                #[cfg(feature = "rai_protocol")]
                let should_cache = {
                    let _ = code;
                    true
                };
                // Every signature-valid RAI vote is epoch evidence. Its local
                // application result depends on whether this replica already
                // has that epoch-qualified election and which phase it has
                // reached; caching must not depend on that local state.
                if should_cache {
                    self.insert_vote(vote.clone(), hash, rep_weight, now);
                    inserted += 1;
                }
            }
        }
        inserted
    }

    pub fn insert_vote(
        &mut self,
        vote: Arc<Vote>,
        hash: &BlockHash,
        rep_weight: Amount,
        now: Timestamp,
    ) {
        let cache_entry_exists = self.modify_by_hash(hash, |existing| {
            existing.add_vote(vote.clone(), rep_weight, now);
        });

        if !cache_entry_exists {
            let id = self.next_id;
            self.next_id += 1;
            let block = VotedBlock::new(id, *hash, self.config.max_voters, vote, rep_weight, now);
            self.insert_block(block);

            // Remove the oldest entry if we have reached the capacity limit
            #[cfg(not(feature = "rai_protocol"))]
            if self.len() > self.config.max_size {
                self.pop_front();
            }
        }
    }

    pub fn insert_block(&mut self, entry: VotedBlock) {
        let old = self.sequential.insert(entry.id, *entry.block_hash());
        debug_assert!(old.is_none());

        let tally = entry.non_final_tally().into();
        self.by_tally.insert(tally, *entry.block_hash());

        let old = self.by_hash.insert(*entry.block_hash(), entry);
        debug_assert!(old.is_none());
    }

    fn modify_by_hash<F>(&mut self, hash: &BlockHash, f: F) -> bool
    where
        F: FnOnce(&mut VotedBlock),
    {
        if let Some(entry) = self.by_hash.get_mut(hash) {
            let old_tally = entry.non_final_tally();
            f(entry);
            let new_tally = entry.non_final_tally();
            let hash = *entry.block_hash();
            self.by_tally.update(hash, old_tally, new_tally);
            true
        } else {
            false
        }
    }

    fn pop_front(&mut self) -> Option<VotedBlock> {
        match self.sequential.pop_first() {
            Some((_, front_hash)) => {
                let entry = self.by_hash.remove(&front_hash).unwrap();
                self.by_tally.remove(&front_hash, entry.non_final_tally());
                Some(entry)
            }
            None => None,
        }
    }

    pub fn collect_votes<'a>(&self, result: &mut Vec<Arc<Vote>>, hash: &BlockHash) {
        if let Some(block) = self.by_hash.get(hash) {
            result.extend(block.iter_replay_votes().cloned());
        }
    }

    #[cfg(test)]
    pub fn votes<'a>(&'a self, hash: &BlockHash) -> impl Iterator<Item = &'a Arc<Vote>> {
        self.by_hash
            .get(hash)
            .into_iter()
            .flat_map(|i| i.iter_votes())
    }

    pub fn get(&self, hash: &BlockHash) -> Option<&VotedBlock> {
        self.by_hash.get(hash)
    }

    pub fn remove(&mut self, hash: &BlockHash) -> Option<VotedBlock> {
        match self.by_hash.remove(hash) {
            Some(entry) => {
                self.sequential.remove(&entry.id);
                self.by_tally.remove(hash, entry.non_final_tally());
                Some(entry)
            }
            None => None,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &VotedBlock> {
        self.by_hash.values()
    }

    pub fn iter_by_tally_desc(&self) -> impl Iterator<Item = &VotedBlock> {
        self.by_tally
            .iter_desc()
            .flat_map(|hash| self.by_hash.get(hash))
    }

    pub fn vote_count(&self, hash: &BlockHash) -> usize {
        self.by_hash
            .get(hash)
            .map(|i| i.vote_count())
            .unwrap_or_default()
    }

    /// Returns blocks with highest observed tally, greater than `min_tally`
    /// The blocks are sorted in descending order by final tally, then by tally
    /// @param min_tally minimum tally threshold, entries below with their voting weight
    /// below this will be ignore
    pub fn top(
        &mut self,
        results: &mut Vec<TopEntry>,
        min_tally: impl Into<Amount>,
        now: Timestamp,
    ) {
        let min_tally = min_tally.into();
        #[cfg(not(feature = "rai_protocol"))]
        if let Some(last) = self.last_cleanup {
            if last.elapsed(now) >= self.config.age_cutoff / 2 {
                self.cleanup(now);
                self.last_cleanup = Some(now);
            }
        } else {
            self.last_cleanup = Some(now);
        }
        #[cfg(feature = "rai_protocol")]
        let _ = now;

        for entry in self.iter_by_tally_desc() {
            let tally = entry.non_final_tally();
            if tally < min_tally {
                break;
            }
            results.push(TopEntry {
                hash: *entry.block_hash(),
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
    }

    pub fn len(&self) -> usize {
        self.sequential.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sequential.is_empty()
    }

    pub fn clear(&mut self) {
        self.sequential.clear();
        self.by_hash.clear();
        self.by_tally.clear();
    }

    pub fn cleanup(&mut self, now: Timestamp) {
        let to_delete: Vec<_> = self
            .iter()
            .filter(|i| i.last_modified().elapsed(now) >= self.config.age_cutoff)
            .map(|i| *i.block_hash())
            .collect();

        for hash in to_delete {
            self.remove(&hash);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{PrivateKey, UnixMillisTimestamp};
    use std::time::Duration;

    #[test]
    fn construction() {
        let cache = make_block_map();
        assert_eq!(cache.len(), 0);
        let hash = BlockHash::from(1);
        assert!(cache.get(&hash).is_none());
    }

    #[test]
    fn insert_one_hash() {
        let mut cache = make_block_map();
        let rep = PrivateKey::from(1);
        let hash = BlockHash::from(1);
        let vote = create_vote(&rep, &hash, 1);
        let now = Timestamp::new_test_instance();

        cache.process(vote.clone(), Amount::raw(7), &HashMap::new(), now);

        assert_eq!(cache.len(), 1);
        let peek = cache.get(&hash).unwrap();
        let votes = peek.iter_votes().cloned().collect::<Vec<_>>();
        assert_eq!(votes, vec![vote]);
    }

    #[test]
    fn contains() {
        let mut cache = make_block_map();
        let rep = PrivateKey::from(1);
        let hash = BlockHash::from(1);
        let vote = create_vote(&rep, &hash, 1);
        let now = Timestamp::new_test_instance();

        assert_eq!(cache.contains(&hash), false);

        cache.process(vote, Amount::raw(7), &HashMap::new(), now);

        assert_eq!(cache.contains(&hash), true);
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn caches_late_vote_for_epoch_alias_recovery() {
        let mut cache = make_block_map();
        let rep = PrivateKey::from(1);
        let hash = BlockHash::from(1);
        let vote = create_vote(&rep, &hash, 1);
        let results = HashMap::from([(hash, Err(VoteError::Late))]);

        assert_eq!(
            cache.process(
                vote,
                Amount::raw(7),
                &results,
                Timestamp::new_test_instance()
            ),
            1
        );
        assert!(cache.contains(&hash));
    }

    /*
     * Inserts multiple votes for single hash
     * Ensures all of them can be retrieved and that tally is properly accumulated
     */
    #[test]
    fn insert_one_hash_many_votes() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();

        let hash = BlockHash::from(42);
        let rep1 = PrivateKey::from(1);
        let rep2 = PrivateKey::from(2);
        let rep3 = PrivateKey::from(3);

        let vote1 = create_vote(&rep1, &hash, 1);
        let vote2 = create_vote(&rep2, &hash, 2);
        let vote3 = create_vote(&rep3, &hash, 3);

        cache.process(vote1, Amount::raw(7), &HashMap::new(), now);
        cache.process(vote2, Amount::raw(9), &HashMap::new(), now);
        cache.process(vote3, Amount::raw(11), &HashMap::new(), now);
        // We have 3 votes but for a single hash, so just one entry in vote cache
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.vote_count(&hash), 3);
    }

    #[test]
    fn insert_many_hashes_many_votes() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();

        // There will be 3 hashes to vote for
        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);

        // There will be 4 reps with different weights
        let rep1 = PrivateKey::from(1);
        let rep2 = PrivateKey::from(2);
        let rep3 = PrivateKey::from(3);
        let rep4 = PrivateKey::from(4);

        // Votes: rep1 > hash1, rep2 > hash2, rep3 > hash3, rep4 > hash1 (the same as rep1)
        let vote1 = create_vote(&rep1, &hash1, 1);
        let vote2 = create_vote(&rep2, &hash2, 1);
        let vote3 = create_vote(&rep3, &hash3, 1);
        let vote4 = create_vote(&rep4, &hash1, 1);

        // Insert first 3 votes in cache
        cache.process(vote1, Amount::raw(7), &HashMap::new(), now);
        cache.process(vote2, Amount::raw(9), &HashMap::new(), now);
        cache.process(vote3, Amount::raw(11), &HashMap::new(), now);

        // Ensure all of those are properly inserted
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.vote_count(&hash1), 1);
        assert_eq!(cache.vote_count(&hash2), 1);
        assert_eq!(cache.vote_count(&hash3), 1);

        // Now add a vote from rep4 with the highest voting weight
        cache.process(vote4, Amount::raw(13), &HashMap::new(), now);

        assert_eq!(cache.vote_count(&hash1), 2);
        assert_eq!(cache.vote_count(&hash3), 1);
    }

    /*
     * Ensure that duplicate votes are ignored
     */
    #[test]
    fn insert_duplicate() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();

        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let vote1 = create_vote(&rep, &hash, 1);
        let vote2 = create_vote(&rep, &hash, 1);

        cache.process(vote1, Amount::raw(9), &HashMap::new(), now);
        cache.process(vote2, Amount::raw(9), &HashMap::new(), now);

        assert_eq!(cache.len(), 1)
    }

    /*
     * Ensure that when processing vote from a representative that is already cached, we always update to the vote with the highest timestamp
     */
    #[test]
    fn insert_newer() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();

        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let vote1 = create_vote(&rep, &hash, 1);
        cache.process(vote1, Amount::raw(9), &HashMap::new(), now);

        let vote2 = Arc::new(Vote::new(
            &rep,
            Vote::TIMESTAMP_MAX,
            Vote::DURATION_MAX,
            vec![hash],
        ));
        cache.process(vote2, Amount::raw(9), &HashMap::new(), now);

        let mut votes = Vec::new();
        cache.collect_votes(&mut votes, &hash);
        #[cfg(not(feature = "rai_protocol"))]
        {
            assert_eq!(votes.len(), 1);
            assert!(votes[0].is_final());
        }
        #[cfg(feature = "rai_protocol")]
        assert_eq!(votes.len(), 2);
    }

    /*
     * Ensure that when processing vote from a representative that is already cached, votes with older timestamp are ignored
     */
    #[test]
    fn insert_older() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let vote1 = create_vote(&rep, &hash, 2);
        cache.process(vote1, Amount::raw(9), &HashMap::new(), now);
        let peek1: Vec<_> = cache.votes(&hash).cloned().collect();

        let vote2 = create_vote(&rep, &hash, 1);
        cache.process(vote2, Amount::raw(9), &HashMap::new(), now);
        let peek2: Vec<_> = cache.votes(&hash).cloned().collect();

        assert_eq!(cache.len(), 1);
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
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);

        let rep1 = PrivateKey::from(1);
        let rep2 = PrivateKey::from(2);
        let rep3 = PrivateKey::from(3);

        let vote1 = create_vote(&rep1, &hash1, 1);
        let vote2 = create_vote(&rep2, &hash2, 1);
        let vote3 = create_vote(&rep3, &hash3, 1);

        cache.process(vote1, Amount::raw(7), &HashMap::new(), now);
        cache.process(vote2, Amount::raw(9), &HashMap::new(), now);
        cache.process(vote3, Amount::raw(11), &HashMap::new(), now);

        assert_eq!(cache.len(), 3);
        assert_eq!(cache.vote_count(&hash1), 1);
        assert_eq!(cache.vote_count(&hash2), 1);
        assert_eq!(cache.vote_count(&hash3), 1);

        cache.remove(&hash2);

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.contains(&hash2), false);
        assert_eq!(cache.vote_count(&hash1), 1);
        assert_eq!(cache.vote_count(&hash2), 0);
        assert_eq!(cache.vote_count(&hash3), 1);
        cache.remove(&hash1);
        cache.remove(&hash3);

        assert!(cache.is_empty());
    }

    /*
     * Ensure that when cache is overfilled, we remove the oldest entries first
     */
    #[test]
    fn overfill() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();

        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);
        let hash4 = BlockHash::from(4);

        let rep1 = PrivateKey::from(1);
        let rep2 = PrivateKey::from(2);
        let rep3 = PrivateKey::from(3);
        let rep4 = PrivateKey::from(4);

        let vote1 = create_vote(&rep1, &hash1, 1);
        cache.process(vote1, Amount::raw(1), &HashMap::new(), now);

        let vote2 = create_vote(&rep2, &hash2, 1);
        cache.process(vote2, Amount::raw(2), &HashMap::new(), now);

        let vote3 = create_vote(&rep3, &hash3, 1);
        cache.process(vote3, Amount::raw(3), &HashMap::new(), now);

        let vote4 = create_vote(&rep4, &hash4, 1);
        cache.process(vote4, Amount::raw(4), &HashMap::new(), now);

        #[cfg(not(feature = "rai_protocol"))]
        assert_eq!(cache.len(), 3);
        #[cfg(feature = "rai_protocol")]
        assert_eq!(cache.len(), 4);

        // Check that oldest votes are dropped first
        assert_eq!(cache.vote_count(&hash4), 1);
        assert_eq!(cache.vote_count(&hash3), 1);
        assert_eq!(cache.vote_count(&hash2), 1);
        #[cfg(not(feature = "rai_protocol"))]
        assert_eq!(cache.vote_count(&hash1), 0);
        #[cfg(feature = "rai_protocol")]
        assert_eq!(cache.vote_count(&hash1), 1);
    }

    /*
     * Check that when a single vote cache entry is overfilled, it ignores any new votes
     */
    #[test]
    fn overfill_entry() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let hash = BlockHash::from(1);

        let rep1 = PrivateKey::from(1);
        let vote1 = create_vote(&rep1, &hash, 1);
        cache.process(vote1, Amount::raw(9), &HashMap::new(), now);

        let rep2 = PrivateKey::from(2);
        let vote2 = create_vote(&rep2, &hash, 1);
        cache.process(vote2, Amount::raw(9), &HashMap::new(), now);

        let rep3 = PrivateKey::from(3);
        let vote3 = create_vote(&rep3, &hash, 1);
        cache.process(vote3, Amount::raw(9), &HashMap::new(), now);

        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn change_vote_to_final_vote() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let hash = BlockHash::from(1);

        let rep = PrivateKey::from(1);
        let vote = create_vote(&rep, &hash, 1);
        let final_vote = create_final_vote(&rep, &hash);
        cache.process(vote, Amount::raw(9), &HashMap::new(), now);
        cache.process(final_vote, Amount::raw(9), &HashMap::new(), now);

        let vote = cache.get(&hash).unwrap().iter_votes().next().unwrap();
        assert!(vote.is_final());
    }

    #[test]
    fn add_final_vote() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let hash = BlockHash::from(1);

        let rep = PrivateKey::from(1);
        let vote = create_final_vote(&rep, &hash);
        cache.process(vote, Amount::raw(9), &HashMap::new(), now);

        let vote = cache.get(&hash).unwrap().iter_votes().next().unwrap();
        assert!(vote.is_final());
    }

    #[test]
    fn top_empty() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let mut top = Vec::new();
        cache.top(&mut top, 0, now);
        assert_eq!(top, Vec::new());
    }

    #[test]
    fn top_one_entry() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let hash = BlockHash::from(1);
        add_test_vote(&mut cache, &PrivateKey::from(1), &hash, Amount::raw(1), now);
        let mut top = Vec::new();
        cache.top(&mut top, 0, now);

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
    fn top_multiple_entries_sorted_by_tally() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);
        add_test_vote(
            &mut cache,
            &PrivateKey::from(1),
            &hash1,
            Amount::raw(1),
            now,
        );
        add_test_vote(
            &mut cache,
            &PrivateKey::from(2),
            &hash2,
            Amount::raw(4),
            now,
        );
        add_test_vote(
            &mut cache,
            &PrivateKey::from(3),
            &hash3,
            Amount::raw(3),
            now,
        );
        add_test_final_vote(
            &mut cache,
            &PrivateKey::from(4),
            &hash2,
            Amount::raw(5),
            now,
        );
        add_test_final_vote(
            &mut cache,
            &PrivateKey::from(5),
            &hash3,
            Amount::raw(5),
            now,
        );

        let mut top = Vec::new();
        cache.top(&mut top, 0, now);

        assert_eq!(top.len(), 3);
        assert_eq!(top[0].hash, hash2);
        assert_eq!(top[1].hash, hash3);
        assert_eq!(top[2].hash, hash1);
    }

    #[test]
    fn top_min_tally() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let hash1 = BlockHash::from(1);
        let hash2 = BlockHash::from(2);
        let hash3 = BlockHash::from(3);
        add_test_vote(
            &mut cache,
            &PrivateKey::from(1),
            &hash1,
            Amount::raw(1),
            now,
        );
        add_test_vote(
            &mut cache,
            &PrivateKey::from(2),
            &hash2,
            Amount::raw(2),
            now,
        );
        add_test_vote(
            &mut cache,
            &PrivateKey::from(3),
            &hash3,
            Amount::raw(3),
            now,
        );

        let mut top = Vec::new();
        cache.top(&mut top, 2, now);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].hash, hash3);
        assert_eq!(top[1].hash, hash2);
    }

    #[test]
    fn top_age_cutoff() {
        let mut cache = make_block_map();
        let start = Timestamp::new_test_instance();
        let hash = BlockHash::from(1);
        add_test_vote(
            &mut cache,
            &PrivateKey::from(1),
            &hash,
            Amount::raw(1),
            start,
        );
        let mut top = Vec::new();
        cache.top(&mut top, 0, start + Duration::from_secs(150));
        assert_eq!(top.len(), 1);
        top.clear();
        cache.top(&mut top, 0, start + Duration::from_secs(300));
        #[cfg(not(feature = "rai_protocol"))]
        assert_eq!(top.len(), 0);
        #[cfg(feature = "rai_protocol")]
        assert_eq!(top.len(), 1);
    }

    /*
     * Ensure that entries with a higher final tally are ranked above entries with a higher
     * regular tally (final tally is the primary sort key in `top`).
     */
    #[test]
    fn top_sorted_by_final_tally_first() {
        let mut cache = make_block_map();
        let now = Timestamp::new_test_instance();
        let hash_regular = BlockHash::from(1);
        let hash_final = BlockHash::from(2);

        // hash_regular has the higher regular tally, but no final votes
        add_test_vote(
            &mut cache,
            &PrivateKey::from(1),
            &hash_regular,
            Amount::raw(10),
            now,
        );
        // hash_final has a lower regular tally, but it is all final weight
        add_test_final_vote(
            &mut cache,
            &PrivateKey::from(2),
            &hash_final,
            Amount::raw(5),
            now,
        );

        let mut top = Vec::new();
        cache.top(&mut top, 0, now);

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
        let mut cache = make_block_map_with_max_voters(max_voters);
        let now = Timestamp::new_test_instance();
        let hash = BlockHash::from(1);

        // Fill the entry up to max_voters
        for i in 1..=max_voters as u64 {
            let rep = PrivateKey::from(i);
            let vote = create_vote(&rep, &hash, 1);
            cache.process(vote, Amount::raw(i as u128), &HashMap::new(), now);
        }

        // The entry must hold exactly max_voters voters, not max_voters - 1
        assert_eq!(cache.vote_count(&hash), max_voters);

        // A higher weight vote evicts the lowest and keeps the count at max_voters
        let high_rep = PrivateKey::from(max_voters as u64 + 1);
        let high_vote = create_vote(&high_rep, &hash, 1);
        cache.process(high_vote, Amount::raw(100), &HashMap::new(), now);
        assert_eq!(cache.vote_count(&hash), max_voters);

        // A vote below the minimum weight is rejected, count stays the same
        let low_rep = PrivateKey::from(max_voters as u64 + 2);
        let low_vote = create_vote(&low_rep, &hash, 1);
        cache.process(low_vote, Amount::raw(1), &HashMap::new(), now);
        assert_eq!(cache.vote_count(&hash), max_voters);
    }

    /*
     * Ensure that only a single voter is evicted when several reps share the lowest weight,
     * instead of dropping the whole weight bucket.
     */
    #[test]
    fn evicts_only_one_voter_on_tie() {
        let max_voters = 2;
        let mut cache = make_block_map_with_max_voters(max_voters);
        let now = Timestamp::new_test_instance();
        let hash = BlockHash::from(1);

        // Two reps tie at the lowest weight
        let rep1 = PrivateKey::from(1);
        cache.process(
            create_vote(&rep1, &hash, 1),
            Amount::raw(5),
            &HashMap::new(),
            now,
        );
        let rep2 = PrivateKey::from(2);
        cache.process(
            create_vote(&rep2, &hash, 1),
            Amount::raw(5),
            &HashMap::new(),
            now,
        );
        assert_eq!(cache.vote_count(&hash), 2);

        // A higher weight vote exceeds capacity and must evict exactly one of the tied voters
        let rep3 = PrivateKey::from(3);
        cache.process(
            create_vote(&rep3, &hash, 1),
            Amount::raw(10),
            &HashMap::new(),
            now,
        );
        assert_eq!(cache.vote_count(&hash), 2);
    }

    fn add_test_vote(
        cache: &mut VotedBlockMap,
        rep: &PrivateKey,
        hash: &BlockHash,
        rep_weight: Amount,
        now: Timestamp,
    ) {
        let vote = create_vote(rep, hash, 0);
        cache.process(vote, rep_weight, &HashMap::new(), now);
    }

    fn add_test_final_vote(
        cache: &mut VotedBlockMap,
        rep: &PrivateKey,
        hash: &BlockHash,
        rep_weight: Amount,
        now: Timestamp,
    ) {
        let vote = create_final_vote(rep, hash);
        cache.process(vote, rep_weight, &HashMap::new(), now);
    }

    /*
     * Test helpers
     */

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

    fn make_block_map() -> VotedBlockMap {
        VotedBlockMap::new(test_config())
    }

    fn make_block_map_with_max_voters(max_voters: usize) -> VotedBlockMap {
        let config = VoteCacheConfig {
            max_voters,
            ..test_config()
        };
        VotedBlockMap::new(config)
    }
}
