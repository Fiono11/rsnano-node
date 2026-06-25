use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use rustc_hash::FxHashMap;

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Amount, BlockHash, Vote, VoteError};

use super::tally_index::TallyIndex;
use super::voted_block::VotedBlock;
use crate::consensus::VoteCacheConfig;

pub(crate) struct VotedBlockMap {
    config: VoteCacheConfig,
    sequential: BTreeMap<usize, BlockHash>,
    by_hash: FxHashMap<BlockHash, VotedBlock>,
    by_tally: TallyIndex,
    next_id: usize,
}

impl VotedBlockMap {
    pub fn new(config: VoteCacheConfig) -> Self {
        Self {
            config,
            sequential: Default::default(),
            by_hash: Default::default(),
            by_tally: Default::default(),
            next_id: Default::default(),
        }
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.by_hash.contains_key(hash)
    }

    pub fn process_vote_results(
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
                if matches!(code, Ok(()) | Err(VoteError::Indeterminate)) {
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
            existing.vote(vote.clone(), rep_weight, now);
        });

        if !cache_entry_exists {
            let id = self.next_id;
            self.next_id += 1;
            let block = VotedBlock::new(id, *hash, self.config.max_voters, vote, rep_weight, now);
            self.insert_block(block);

            // Remove the oldest entry if we have reached the capacity limit
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

    pub fn modify_by_hash<F>(&mut self, hash: &BlockHash, f: F) -> bool
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

    pub fn pop_front(&mut self) -> Option<VotedBlock> {
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
            result.extend(block.iter_votes().cloned());
        }
    }

    pub fn get_by_hash(&self, hash: &BlockHash) -> Option<&VotedBlock> {
        self.by_hash.get(hash)
    }

    pub fn remove_by_hash(&mut self, hash: &BlockHash) -> Option<VotedBlock> {
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
            self.remove_by_hash(&hash);
        }
    }
}
