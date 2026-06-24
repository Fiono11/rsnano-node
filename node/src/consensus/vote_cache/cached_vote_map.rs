use std::{collections::BTreeMap, sync::Arc};

use rustc_hash::FxHashMap;

use rsnano_types::{Amount, PublicKey, Vote};

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

#[derive(Clone)]
pub(crate) struct CachedVoteMap {
    by_representative: FxHashMap<PublicKey, CachedVote>,
    by_weight: BTreeMap<Amount, Vec<PublicKey>>,
    max_votes: usize,
}

impl CachedVoteMap {
    pub fn new(max_votes: usize) -> Self {
        Self {
            by_representative: FxHashMap::default(),
            by_weight: BTreeMap::new(),
            max_votes,
        }
    }

    pub fn insert(&mut self, vote: CachedVote) -> bool {
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

        if self.by_representative.len() > self.max_votes {
            self.remove_lowest_weight();
        }
        true
    }

    fn can_insert(&self, vote: &CachedVote) -> bool {
        self.has_free_capacity() || vote.weight > self.min_weight().unwrap_or_default()
    }

    fn has_free_capacity(&self) -> bool {
        self.by_representative.len() < self.max_votes
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

    pub fn iter(&self) -> impl Iterator<Item = &CachedVote> {
        self.by_representative.values()
    }

    pub fn min_weight(&self) -> Option<Amount> {
        self.by_weight
            .first_key_value()
            .map(|(weight, _reps)| *weight)
    }

    fn remove_by_weight(&mut self, weight: &Amount, representative: &PublicKey) {
        if let Some(mut accounts) = self.by_weight.remove(weight)
            && accounts.len() > 1
        {
            accounts.retain(|a| a != representative);
            self.by_weight.insert(*weight, accounts);
        }
    }

    fn add_by_weight(&mut self, weight: Amount, representative: PublicKey) {
        self.by_weight
            .entry(weight)
            .or_default()
            .push(representative);
    }

    pub fn calculate_tally(&self) -> (Amount, Amount) {
        let mut tally = Amount::ZERO;
        let mut final_tally = Amount::ZERO;
        for vote in self.by_representative.values() {
            tally = tally.wrapping_add(vote.weight);
            if vote.vote.is_final() {
                final_tally = final_tally.wrapping_add(vote.weight);
            }
        }
        (tally, final_tally)
    }
}
