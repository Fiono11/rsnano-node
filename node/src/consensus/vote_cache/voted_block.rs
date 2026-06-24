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
    tally: Amount,
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
            tally: Amount::ZERO,
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

    pub fn tally(&self) -> Amount {
        self.tally
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

    pub fn last_modified(&self) -> Timestamp {
        self.last_modified
    }

    /// Adds a vote into a list, checks for duplicates and updates timestamp if new one is greater
    /// returns true if current tally changed, false otherwise
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
        self.tally = Amount::ZERO;
        self.final_tally = Amount::ZERO;
        for vote in self.by_representative.values() {
            self.tally = self.tally.wrapping_add(vote.weight);
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
