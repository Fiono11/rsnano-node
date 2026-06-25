use std::{collections::BTreeMap, sync::Arc};

use rsnano_types::{Amount, BlockHash, PublicKey, Vote};

use rsnano_nullable_clock::Timestamp;
use rustc_hash::FxHashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedVote {
    pub vote: Arc<Vote>,
    pub weight: Amount,
}

impl CachedVote {
    pub fn new(vote: Arc<Vote>, weight: Amount) -> Self {
        Self { vote, weight }
    }

    pub fn is_newer_than(&self, other: &CachedVote) -> bool {
        self.vote.timestamp() > other.vote.timestamp()
    }
}

/// Stores votes associated with a single block hash
#[derive(Clone)]
pub(crate) struct VotedBlock {
    pub id: usize,
    block_hash: BlockHash,
    last_modified: Timestamp,
    non_final_tally: Amount,
    final_tally: Amount,
    max_voters: usize,
    by_representative: FxHashMap<PublicKey, CachedVote>,
    by_weight: BTreeMap<Amount, Vec<PublicKey>>,
}

impl VotedBlock {
    pub fn new(
        id: usize,
        hash: BlockHash,
        max_voters: usize,
        vote: Arc<Vote>,
        rep_weight: Amount,
        now: Timestamp,
    ) -> Self {
        let mut block = VotedBlock {
            id,
            block_hash: hash,
            non_final_tally: Amount::ZERO,
            final_tally: Amount::ZERO,
            max_voters,
            by_representative: FxHashMap::default(),
            by_weight: BTreeMap::new(),
            last_modified: now,
        };

        block.vote(vote, rep_weight, now);
        block
    }

    pub fn block_hash(&self) -> &BlockHash {
        &self.block_hash
    }

    pub fn non_final_tally(&self) -> Amount {
        self.non_final_tally
    }

    pub fn final_tally(&self) -> Amount {
        self.final_tally
    }

    pub fn votes(&self) -> Vec<Arc<Vote>> {
        self.by_representative
            .values()
            .map(|i| Arc::clone(&i.vote))
            .collect()
    }

    pub fn iter_votes<'a>(&'a self) -> impl Iterator<Item = &'a Arc<Vote>> {
        self.by_representative.values().map(|i| &i.vote)
    }

    pub fn last_modified(&self) -> Timestamp {
        self.last_modified
    }

    /// Adds a vote into a list, checks for duplicates and updates timestamp if new one is greater
    /// Returns true if the vote was accepted (new representative, or a newer vote from an
    /// already known one), false if it was rejected as a duplicate/older vote or due to capacity
    pub fn vote(&mut self, vote: Arc<Vote>, rep_weight: Amount, now: Timestamp) -> bool {
        let inserted = self.insert(CachedVote::new(vote, rep_weight));
        if inserted {
            self.calculate_tallies();
            self.last_modified = now;
            true
        } else {
            false
        }
    }

    fn insert(&mut self, vote: CachedVote) -> bool {
        let rep_key = vote.vote.voter;
        let new_weight = vote.weight;
        if let Some(existing) = self.by_representative.get_mut(&rep_key) {
            if !vote.is_newer_than(existing) {
                return false;
            }
            let old_weight = existing.weight;
            *existing = vote;
            if old_weight != new_weight {
                self.remove_by_weight(&old_weight, &rep_key);
                self.add_by_weight(new_weight, rep_key);
            }
            return true;
        }

        if !self.can_insert(&vote) {
            return false;
        }

        self.by_representative.insert(rep_key, vote);
        self.add_by_weight(new_weight, rep_key);

        if self.by_representative.len() > self.max_voters {
            self.remove_lowest_weight();
        }
        true
    }

    fn can_insert(&self, vote: &CachedVote) -> bool {
        self.has_free_capacity() || vote.weight > self.min_weight().unwrap_or_default()
    }

    fn has_free_capacity(&self) -> bool {
        self.by_representative.len() < self.max_voters
    }

    fn min_weight(&self) -> Option<Amount> {
        self.by_weight
            .first_key_value()
            .map(|(weight, _reps)| *weight)
    }

    fn calculate_tallies(&mut self) {
        self.non_final_tally = Amount::ZERO;
        self.final_tally = Amount::ZERO;
        for vote in self.by_representative.values() {
            self.non_final_tally = self.non_final_tally.wrapping_add(vote.weight);
            if vote.vote.is_final() {
                self.final_tally = self.final_tally.wrapping_add(vote.weight);
            }
        }
    }

    fn add_by_weight(&mut self, weight: Amount, representative: PublicKey) {
        self.by_weight
            .entry(weight)
            .or_default()
            .push(representative);
    }

    fn remove_by_weight(&mut self, weight: &Amount, representative: &PublicKey) {
        if let Some(mut accounts) = self.by_weight.remove(weight)
            && accounts.len() > 1
        {
            accounts.retain(|a| a != representative);
            self.by_weight.insert(*weight, accounts);
        }
    }

    fn remove_lowest_weight(&mut self) {
        let Some((weight, mut reps)) = self.by_weight.pop_first() else {
            return;
        };
        // Only remove a single voter, even if multiple reps share the lowest weight
        if let Some(rep) = reps.pop() {
            self.by_representative.remove(&rep);
        }
        if !reps.is_empty() {
            self.by_weight.insert(weight, reps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{PrivateKey, UnixMillisTimestamp};

    #[test]
    fn new_stores_first_vote_and_initializes_tally() {
        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let vote = create_vote(&rep, &hash, 1);
        let now = Timestamp::new(1);

        let block = VotedBlock::new(7, hash, 64, vote.clone(), Amount::raw(5), now);

        assert_eq!(block.id, 7);
        assert_eq!(*block.block_hash(), hash);
        assert_eq!(block.non_final_tally(), Amount::raw(5));
        assert_eq!(block.final_tally(), Amount::ZERO);
        assert_eq!(block.votes(), vec![vote]);
        assert_eq!(block.last_modified(), now);
    }

    #[test]
    fn new_with_final_vote_counts_towards_final_tally() {
        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);

        let block = VotedBlock::new(
            1,
            hash,
            64,
            create_final_vote(&rep, &hash),
            Amount::raw(5),
            Timestamp::new(1),
        );

        assert_eq!(block.non_final_tally(), Amount::raw(5));
        assert_eq!(block.final_tally(), Amount::raw(5));
    }

    #[test]
    fn vote_from_new_representative_increases_tally_and_voter_count() {
        let hash = BlockHash::from(1);
        let rep1 = PrivateKey::from(1);
        let rep2 = PrivateKey::from(2);
        let mut block = VotedBlock::new(
            1,
            hash,
            64,
            create_vote(&rep1, &hash, 1),
            Amount::raw(5),
            Timestamp::new(1),
        );

        let changed = block.vote(
            create_vote(&rep2, &hash, 1),
            Amount::raw(7),
            Timestamp::new(2),
        );

        assert!(changed);
        assert_eq!(block.non_final_tally(), Amount::raw(12));
        assert_eq!(block.votes().len(), 2);
    }

    #[test]
    fn final_tally_only_includes_final_votes() {
        let hash = BlockHash::from(1);
        let final_rep = PrivateKey::from(1);
        let normal_rep = PrivateKey::from(2);

        let mut block = VotedBlock::new(
            1,
            hash,
            64,
            create_final_vote(&final_rep, &hash),
            Amount::raw(10),
            Timestamp::new(1),
        );
        block.vote(
            create_vote(&normal_rep, &hash, 1),
            Amount::raw(7),
            Timestamp::new(1),
        );

        assert_eq!(block.non_final_tally(), Amount::raw(17));
        assert_eq!(block.final_tally(), Amount::raw(10));
    }

    #[test]
    fn duplicate_vote_with_same_timestamp_is_ignored() {
        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let mut block = VotedBlock::new(
            1,
            hash,
            64,
            create_vote(&rep, &hash, 1),
            Amount::raw(5),
            Timestamp::new(1),
        );

        // Same timestamp, even with a different weight, must be ignored
        let changed = block.vote(
            create_vote(&rep, &hash, 1),
            Amount::raw(9),
            Timestamp::new(2),
        );

        assert!(!changed);
        assert_eq!(block.votes().len(), 1);
        assert_eq!(block.non_final_tally(), Amount::raw(5));
        assert_eq!(block.last_modified(), Timestamp::new(1));
    }

    #[test]
    fn newer_vote_from_same_representative_replaces_old_vote() {
        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let mut block = VotedBlock::new(
            1,
            hash,
            64,
            create_vote(&rep, &hash, 1),
            Amount::raw(5),
            Timestamp::new(1),
        );

        let newer_vote = create_final_vote(&rep, &hash);
        let changed = block.vote(newer_vote.clone(), Amount::raw(5), Timestamp::new(2));

        assert!(changed);
        assert_eq!(block.votes(), vec![newer_vote]);
        assert_eq!(block.final_tally(), Amount::raw(5));
        assert_eq!(block.last_modified(), Timestamp::new(2));
    }

    #[test]
    fn older_vote_from_same_representative_is_ignored() {
        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let current_vote = create_vote(&rep, &hash, 5);
        let mut block = VotedBlock::new(
            1,
            hash,
            64,
            current_vote.clone(),
            Amount::raw(5),
            Timestamp::new(1),
        );

        let changed = block.vote(
            create_vote(&rep, &hash, 1),
            Amount::raw(5),
            Timestamp::new(2),
        );

        assert!(!changed);
        assert_eq!(block.votes(), vec![current_vote]);
        assert_eq!(block.last_modified(), Timestamp::new(1));
    }

    #[test]
    fn weight_change_from_same_representative_replaces_rather_than_adds() {
        let hash = BlockHash::from(1);
        let rep = PrivateKey::from(1);
        let mut block = VotedBlock::new(
            1,
            hash,
            64,
            create_vote(&rep, &hash, 1),
            Amount::raw(5),
            Timestamp::new(1),
        );

        block.vote(
            create_vote(&rep, &hash, 2),
            Amount::raw(100),
            Timestamp::new(2),
        );

        assert_eq!(block.non_final_tally(), Amount::raw(100));
        assert_eq!(block.votes().len(), 1);
    }

    /*
     * Regression test: when an existing representative's weight changes, the internal
     * weight->representative index must move with it, so that a later eviction picks the
     * actual lowest-weight voter rather than one that merely used to be the lowest.
     */
    #[test]
    fn weight_change_is_reflected_in_eviction_order() {
        let hash = BlockHash::from(1);
        let max_voters = 3;

        let rep_a = PrivateKey::from(1);
        let mut block = VotedBlock::new(
            1,
            hash,
            max_voters,
            create_vote(&rep_a, &hash, 1),
            Amount::raw(1),
            Timestamp::new(1),
        );
        let rep_b = PrivateKey::from(2);
        block.vote(
            create_vote(&rep_b, &hash, 1),
            Amount::raw(10),
            Timestamp::new(1),
        );
        let rep_c = PrivateKey::from(3);
        block.vote(
            create_vote(&rep_c, &hash, 1),
            Amount::raw(20),
            Timestamp::new(1),
        );

        // rep_a, originally the lowest weight voter, now becomes the highest
        block.vote(
            create_vote(&rep_a, &hash, 2),
            Amount::raw(1000),
            Timestamp::new(2),
        );

        // A new voter arrives with a weight between rep_b and rep_c, exceeding capacity
        let rep_d = PrivateKey::from(4);
        block.vote(
            create_vote(&rep_d, &hash, 1),
            Amount::raw(15),
            Timestamp::new(3),
        );

        // rep_b now holds the lowest weight (10) and must be the one evicted, not rep_a
        let voters: Vec<_> = block.votes().iter().map(|v| v.voter).collect();
        assert!(voters.contains(&rep_a.public_key()));
        assert!(!voters.contains(&rep_b.public_key()));
        assert!(voters.contains(&rep_c.public_key()));
        assert!(voters.contains(&rep_d.public_key()));
        assert_eq!(block.non_final_tally(), Amount::raw(1000 + 20 + 15));
    }

    #[test]
    fn entry_can_hold_exactly_max_voters() {
        let hash = BlockHash::from(1);
        let (block, _reps) = full_block(hash, &[1, 2, 3]);

        assert_eq!(block.votes().len(), 3);
    }

    #[test]
    fn vote_with_higher_weight_evicts_lowest_weight_voter_when_full() {
        let hash = BlockHash::from(1);
        let (mut block, reps) = full_block(hash, &[10, 20, 30]);

        let rep4 = PrivateKey::from(4);
        let changed = block.vote(
            create_vote(&rep4, &hash, 1),
            Amount::raw(100),
            Timestamp::new(2),
        );

        assert!(changed);
        let voters: Vec<_> = block.votes().iter().map(|v| v.voter).collect();
        assert_eq!(voters.len(), 3);
        assert!(!voters.contains(&reps[0].public_key()));
        assert!(voters.contains(&rep4.public_key()));
        assert_eq!(block.non_final_tally(), Amount::raw(20 + 30 + 100));
    }

    #[test]
    fn vote_below_min_weight_is_rejected_when_full() {
        let hash = BlockHash::from(1);
        let (mut block, _reps) = full_block(hash, &[10, 20, 30]);
        let tally_before = block.non_final_tally();

        let rep4 = PrivateKey::from(4);
        let changed = block.vote(
            create_vote(&rep4, &hash, 1),
            Amount::raw(5),
            Timestamp::new(2),
        );

        assert!(!changed);
        assert_eq!(block.votes().len(), 3);
        assert_eq!(block.non_final_tally(), tally_before);
        assert!(!block.votes().iter().any(|v| v.voter == rep4.public_key()));
    }

    #[test]
    fn vote_tied_with_min_weight_is_rejected_when_full() {
        let hash = BlockHash::from(1);
        let (mut block, _reps) = full_block(hash, &[10, 20, 30]);

        let rep4 = PrivateKey::from(4);
        let changed = block.vote(
            create_vote(&rep4, &hash, 1),
            Amount::raw(10),
            Timestamp::new(2),
        );

        assert!(!changed);
        assert_eq!(block.votes().len(), 3);
        assert!(!block.votes().iter().any(|v| v.voter == rep4.public_key()));
    }

    #[test]
    fn only_one_tied_voter_is_evicted_on_overflow() {
        let hash = BlockHash::from(1);
        let max_voters = 2;
        let rep1 = PrivateKey::from(1);
        let mut block = VotedBlock::new(
            1,
            hash,
            max_voters,
            create_vote(&rep1, &hash, 1),
            Amount::raw(5),
            Timestamp::new(1),
        );
        let rep2 = PrivateKey::from(2);
        block.vote(
            create_vote(&rep2, &hash, 1),
            Amount::raw(5),
            Timestamp::new(1),
        );

        let rep3 = PrivateKey::from(3);
        block.vote(
            create_vote(&rep3, &hash, 1),
            Amount::raw(10),
            Timestamp::new(2),
        );

        let voters: Vec<_> = block.votes().iter().map(|v| v.voter).collect();
        assert_eq!(voters.len(), max_voters);
        assert!(voters.contains(&rep3.public_key()));
        let tied_survivors = [rep1.public_key(), rep2.public_key()]
            .into_iter()
            .filter(|k| voters.contains(k))
            .count();
        assert_eq!(tied_survivors, 1);
    }

    #[test]
    fn cached_vote_is_newer_than_uses_strict_greater_than() {
        let rep = PrivateKey::from(1);
        let hash = BlockHash::from(1);
        let older = CachedVote::new(create_vote(&rep, &hash, 1), Amount::raw(1));
        let newer = CachedVote::new(create_vote(&rep, &hash, 2), Amount::raw(1));
        let same_timestamp = CachedVote::new(create_vote(&rep, &hash, 1), Amount::raw(1));

        assert!(newer.is_newer_than(&older));
        assert!(!older.is_newer_than(&newer));
        assert!(!same_timestamp.is_newer_than(&older));
    }

    /* Test helpers */

    fn create_vote(rep: &PrivateKey, hash: &BlockHash, timestamp_offset: u64) -> Arc<Vote> {
        let timestamp = UnixMillisTimestamp::new(timestamp_offset * 1024 * 1024);
        Arc::new(Vote::new(rep, timestamp, 0, vec![*hash]))
    }

    fn create_final_vote(rep: &PrivateKey, hash: &BlockHash) -> Arc<Vote> {
        Arc::new(Vote::new_final(rep, vec![*hash]))
    }

    /// Builds a `VotedBlock` that already holds exactly `weights.len()` voters, one per weight.
    fn full_block(hash: BlockHash, weights: &[u128]) -> (VotedBlock, Vec<PrivateKey>) {
        let max_voters = weights.len();
        let reps: Vec<PrivateKey> = (0..max_voters)
            .map(|i| PrivateKey::from((i + 1) as u64))
            .collect();
        let mut block = VotedBlock::new(
            1,
            hash,
            max_voters,
            create_vote(&reps[0], &hash, 1),
            Amount::raw(weights[0]),
            Timestamp::new(1),
        );
        for (rep, weight) in reps.iter().zip(weights.iter()).skip(1) {
            block.vote(
                create_vote(rep, &hash, 1),
                Amount::raw(*weight),
                Timestamp::new(1),
            );
        }
        (block, reps)
    }
}
