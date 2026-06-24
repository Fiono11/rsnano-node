use std::{collections::BTreeMap, sync::Arc};

use rustc_hash::FxHashMap;

use rsnano_types::{Amount, PublicKey, Vote};

use crate::consensus::vote_cache::CachedVote;

#[derive(Default, Clone)]
pub(crate) struct CachedVoteMap {
    by_representative: FxHashMap<PublicKey, CachedVote>,
    by_weight: BTreeMap<Amount, Vec<PublicKey>>,
}

impl CachedVoteMap {
    pub fn insert(&mut self, vote: CachedVote) {
        let weight = vote.weight;
        let rep_key = vote.vote.voter;
        if let Some(existing) = self.by_representative.get_mut(&rep_key) {
            let old_weight = existing.weight;
            *existing = vote;
            self.remove_by_weight(&old_weight, &rep_key);
        } else {
            self.by_representative.insert(rep_key, vote);
        }
        self.add_by_weight(weight, rep_key);
    }

    pub fn iter_unordered(&self) -> impl Iterator<Item = &CachedVote> {
        self.by_representative.values()
    }

    pub fn find(&self, representative: &PublicKey) -> Option<&CachedVote> {
        self.by_representative.get(representative)
    }

    pub fn modify(&mut self, representative: &PublicKey, vote: Arc<Vote>, new_weight: Amount) {
        if let Some(entry) = self.by_representative.get_mut(representative) {
            let old_weight = entry.weight;
            entry.vote = vote;
            entry.weight = new_weight;
            if old_weight != new_weight {
                self.remove_by_weight(&old_weight, representative);
                self.add_by_weight(new_weight, *representative);
            }
        }
    }

    pub fn min_weight(&self) -> Option<Amount> {
        self.by_weight
            .first_key_value()
            .map(|(weight, _reps)| *weight)
    }

    pub fn remove_lowest_weight(&mut self) {
        if let Some((weight, mut reps)) = self.by_weight.pop_first() {
            // Only remove a single voter, even if multiple reps share the lowest weight
            if let Some(rep) = reps.pop() {
                self.by_representative.remove(&rep);
            }
            if !reps.is_empty() {
                self.by_weight.insert(weight, reps);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.by_representative.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_representative.is_empty()
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
}
