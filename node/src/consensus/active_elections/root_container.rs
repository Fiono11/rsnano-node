use std::{cmp::Ordering, collections::BTreeSet};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, BlockPriority, ConsensusEpoch, QualifiedRoot, TimePriority};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};
use rustc_hash::FxHashMap;

use super::{AecInsertRequest, vote_router::VoteRouter};
use crate::consensus::{
    AecSnapshot, BucketInfo,
    active_elections::aec_service::{BucketSnapshot, ElectionSnapshot},
    election::{Election, ElectionBehavior, ElectionId},
    election_schedulers::priority::{bucket_count, bucket_index},
};

pub(crate) struct Entry {
    pub id: ElectionId,
    pub election: Election,
    pub priority: BlockPriority,
}

impl Entry {
    pub fn bucket(&self) -> usize {
        bucket_index(self.election.behavior(), self.priority.balance)
    }
}

/// Ordered by descending time priority
/// => So highest priority entries are first!
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct BucketEntry {
    id: ElectionId,
    priority: BlockPriority,
}

impl BucketEntry {
    fn of(entry: &Entry) -> Self {
        Self {
            id: entry.id.clone(),
            priority: entry.priority,
        }
    }
}

impl Ord for BucketEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        match other.priority.time.cmp(&self.priority.time) {
            Ordering::Equal => match other.priority.balance.cmp(&self.priority.balance) {
                Ordering::Equal => other.id.cmp(&self.id),
                result => result,
            },
            result => result,
        }
    }
}

impl PartialOrd for BucketEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Contains elections and their qualified roots. RAI: a root may have one
/// election per consensus epoch, the entries of a root are ascending by epoch.
pub(crate) struct RootContainer {
    by_root: FxHashMap<QualifiedRoot, Vec<Entry>>,
    len: usize,
    buckets: Vec<BTreeSet<BucketEntry>>,
    bucket_infos: Vec<BucketInfo>,
    /// Kudzu: terminated elections leave their priority bucket. They keep
    /// their certificates available, take no capacity and are never evicted.
    terminated: BTreeSet<BucketEntry>,
    pub vote_router: VoteRouter,
    max_elections_per_bucket: usize,
}

impl Default for RootContainer {
    fn default() -> Self {
        Self::new(5000)
    }
}

impl RootContainer {
    pub const ELEMENT_SIZE: usize = size_of::<QualifiedRoot>() * 2 + size_of::<Election>();

    pub fn new(max_elections: usize) -> Self {
        let bucket_count = bucket_count();
        let max_elections_per_bucket = max_elections / bucket_count;
        Self {
            by_root: Default::default(),
            len: 0,
            vote_router: Default::default(),
            buckets: vec![BTreeSet::new(); bucket_count],
            bucket_infos: vec![BucketInfo::new(max_elections_per_bucket); bucket_count],
            terminated: BTreeSet::new(),
            max_elections_per_bucket,
        }
    }

    /// Kudzu: move a terminated election out of its priority bucket
    pub fn mark_terminated(&mut self, id: &ElectionId) {
        let Some(entry) = self.get(id) else {
            return;
        };
        let bucket_entry = BucketEntry::of(entry);
        let bucket_index = entry.bucket();
        if self.buckets[bucket_index].remove(&bucket_entry) {
            self.update_bucket_info(bucket_index);
            self.terminated.insert(bucket_entry);
        }
    }

    pub fn is_terminated(&self, id: &ElectionId) -> bool {
        self.get(id)
            .is_some_and(|entry| self.terminated.contains(&BucketEntry::of(entry)))
    }

    /// Elections which still take capacity in the priority buckets
    pub fn active_len(&self) -> usize {
        self.len - self.terminated.len()
    }

    pub fn terminated_len(&self) -> usize {
        self.terminated.len()
    }

    fn update_bucket_info(&mut self, bucket_index: usize) {
        let bucket = &self.buckets[bucket_index];
        let info = &mut self.bucket_infos[bucket_index];
        info.election_count = bucket.len();
        info.lowest_priority = bucket.last().map(|i| i.priority).unwrap_or_default();
    }

    pub fn insert(&mut self, entry: Entry) {
        debug_assert_eq!(entry.id, entry.election.id());
        debug_assert!(self.get(&entry.id).is_none());
        let id = entry.id.clone();
        let hash = entry.election.winner().hash();
        let bucket_entry = BucketEntry::of(&entry);

        let bucket = &mut self.buckets[entry.bucket()];
        bucket.insert(bucket_entry);

        let infos = &mut self.bucket_infos[entry.bucket()];
        infos.election_count = bucket.len();
        infos.lowest_priority = bucket.last().map(|i| i.priority).unwrap_or_default();

        let entries = self.by_root.entry(id.root.clone()).or_default();
        let position = entries
            .iter()
            .position(|e| e.id.epoch > id.epoch)
            .unwrap_or(entries.len());
        entries.insert(position, entry);
        self.len += 1;
        self.vote_router.connect(hash, id);
    }

    pub fn get(&self, id: &ElectionId) -> Option<&Entry> {
        self.by_root
            .get(&id.root)?
            .iter()
            .find(|e| e.id.epoch == id.epoch)
    }

    pub fn get_mut(&mut self, id: &ElectionId) -> Option<&mut Entry> {
        self.by_root
            .get_mut(&id.root)?
            .iter_mut()
            .find(|e| e.id.epoch == id.epoch)
    }

    pub fn election(&self, id: &ElectionId) -> Option<&Election> {
        self.get(id).map(|i| &i.election)
    }

    pub fn election_mut(&mut self, id: &ElectionId) -> Option<&mut Election> {
        self.get_mut(id).map(|i| &mut i.election)
    }

    /// The elections of this root, ascending by epoch
    pub fn elections_for_root(&self, root: &QualifiedRoot) -> impl Iterator<Item = &Election> {
        self.by_root
            .get(root)
            .into_iter()
            .flatten()
            .map(|e| &e.election)
    }

    pub fn elections_for_root_mut(
        &mut self,
        root: &QualifiedRoot,
    ) -> impl Iterator<Item = &mut Election> {
        self.by_root
            .get_mut(root)
            .into_iter()
            .flatten()
            .map(|e| &mut e.election)
    }

    /// The election of the newest epoch for this root
    pub fn latest_election_for_root(&self, root: &QualifiedRoot) -> Option<&Election> {
        self.elections_for_root(root).last()
    }

    pub fn latest_election_for_root_mut(&mut self, root: &QualifiedRoot) -> Option<&mut Election> {
        self.elections_for_root_mut(root).last()
    }

    /// The election of the newest epoch this block is a candidate in
    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<&Election> {
        let id = self.vote_router.latest_election_id(block_hash)?;
        self.election(&id)
    }

    pub fn election_for_block_mut(&mut self, block_hash: &BlockHash) -> Option<&mut Election> {
        let id = self.vote_router.latest_election_id(block_hash)?;
        self.election_mut(&id)
    }

    pub fn election_for_block_in_epoch(
        &self,
        block_hash: &BlockHash,
        epoch: ConsensusEpoch,
    ) -> Option<&Election> {
        let id = self.vote_router.election_id(block_hash, epoch)?;
        self.election(&id)
    }

    pub fn election_for_block_in_epoch_mut(
        &mut self,
        block_hash: &BlockHash,
        epoch: ConsensusEpoch,
    ) -> Option<&mut Election> {
        let id = self.vote_router.election_id(block_hash, epoch)?;
        self.election_mut(&id)
    }

    pub fn bucket_infos(&self) -> &[BucketInfo] {
        &self.bucket_infos
    }

    pub fn try_upgrade_to_priority_election(
        &mut self,
        request: &AecInsertRequest,
        epoch: ConsensusEpoch,
    ) -> (bool, Option<ElectionBehavior>) {
        let id = ElectionId::new(request.block.qualified_root(), epoch);

        let Some(entry) = self.get_mut(&id) else {
            return (false, None);
        };

        let previous_behavior = entry.election.behavior();
        if request.behavior != ElectionBehavior::Priority {
            return (false, Some(previous_behavior));
        }

        let priority = entry.priority;
        let upgraded = entry.election.maybe_upgrade_to(ElectionBehavior::Priority);
        if !upgraded {
            return (false, Some(previous_behavior));
        }

        let bucket_entry = BucketEntry { id, priority };
        if self.terminated.contains(&bucket_entry) {
            // Not in any priority bucket, nothing to move
            return (true, Some(previous_behavior));
        }

        let old_bucket_index = bucket_index(previous_behavior, priority.balance);
        let old_bucket = &mut self.buckets[old_bucket_index];
        old_bucket.remove(&bucket_entry);
        let old_infos = &mut self.bucket_infos[old_bucket_index];
        old_infos.election_count = old_bucket.len();
        old_infos.lowest_priority = old_bucket.last().map(|i| i.priority).unwrap_or_default();

        let new_bucket_index = bucket_index(ElectionBehavior::Priority, priority.balance);
        let new_bucket = &mut self.buckets[new_bucket_index];
        new_bucket.insert(bucket_entry);

        let new_infos = &mut self.bucket_infos[new_bucket_index];
        new_infos.election_count = new_bucket.len();
        new_infos.lowest_priority = new_bucket.last().map(|i| i.priority).unwrap_or_default();

        (true, Some(previous_behavior))
    }

    pub fn drain_filter(&mut self, mut predicate: impl FnMut(&Entry) -> bool) -> Vec<Entry> {
        let to_remove: Vec<_> = self
            .iter()
            .filter_map(|i| {
                if predicate(i) {
                    Some(i.id.clone())
                } else {
                    None
                }
            })
            .collect();

        let mut removed = Vec::new();
        for id in to_remove {
            if let Some(entry) = self.erase(&id) {
                removed.push(entry);
            }
        }

        removed
    }

    pub fn erase(&mut self, id: &ElectionId) -> Option<Entry> {
        let entries = self.by_root.get_mut(&id.root)?;
        let position = entries.iter().position(|e| e.id.epoch == id.epoch)?;
        let entry = entries.remove(position);
        if entries.is_empty() {
            self.by_root.remove(&id.root);
        }
        self.len -= 1;
        self.vote_router.disconnect_election(&entry.election);
        let bucket_entry = BucketEntry::of(&entry);
        if !self.terminated.remove(&bucket_entry) {
            let bucket_index = entry.bucket();
            self.buckets[bucket_index].remove(&bucket_entry);
            self.update_bucket_info(bucket_index);
        }
        Some(entry)
    }

    /// Erase the elections of all epochs of this root
    pub fn erase_root(&mut self, root: &QualifiedRoot) -> Vec<Entry> {
        let ids: Vec<_> = self.elections_for_root(root).map(|e| e.id()).collect();
        ids.iter().filter_map(|id| self.erase(id)).collect()
    }

    pub fn clear(&mut self) {
        self.by_root.clear();
        self.len = 0;
        self.terminated.clear();
        for bucket in self.buckets.iter_mut() {
            bucket.clear();
        }
        for i in &mut self.bucket_infos {
            *i = BucketInfo::new(self.max_elections_per_bucket);
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn round_robin(&self) -> impl Iterator<Item = &Entry> {
        RoundRobinIterator::new(self)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.by_root.values().flatten()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entry> {
        self.by_root.values_mut().flatten()
    }

    pub fn bucket_len(&self, bucket_id: usize) -> usize {
        self.buckets[bucket_id].len()
    }

    pub fn lowest_priority(&self, bucket_id: usize) -> Option<(ElectionId, TimePriority)> {
        self.buckets[bucket_id]
            .last()
            .map(|i| (i.id.clone(), i.priority.time))
    }

    pub fn find_bucket(&self, id: &ElectionId) -> Option<usize> {
        self.get(id).map(|i| i.bucket())
    }

    pub fn bucket_count(&self) -> usize {
        self.bucket_infos.len()
    }

    pub fn snapshot(&self, now: Timestamp) -> AecSnapshot {
        AecSnapshot {
            buckets: self
                .buckets
                .iter()
                .enumerate()
                .map(|(i, b)| BucketSnapshot {
                    bucket_index: i,
                    election_count: b.len(),
                    elections: b
                        .iter()
                        .take(3)
                        .map(|entry| {
                            let election = self.election(&entry.id).unwrap();
                            ElectionSnapshot {
                                account: election.account(),
                                winner_hash: election.winner().hash(),
                                non_final_tally: election.winner_tally(),
                                final_tally: election.winner_final_tally(),
                                root: election.qualified_root().clone(),
                                state: election.state(),
                                candidate_blocks: election
                                    .candidate_blocks()
                                    .keys()
                                    .cloned()
                                    .collect(),
                                is_final: election.is_final(),
                                elapsed: election.start().elapsed(now),
                            }
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

impl ContainerInfoProvider for RootContainer {
    fn container_info(&self) -> ContainerInfo {
        let mut result = ContainerInfo::builder();
        for (i, b) in self.buckets.iter().enumerate() {
            result = result.leaf(format!("bucket {}", i), b.len(), 0);
        }
        result.finish()
    }
}

struct RoundRobinIterator<'a> {
    roots: &'a RootContainer,
    bucket_iters: Vec<std::collections::btree_set::Iter<'a, BucketEntry>>,
    current: usize,
    yielded: bool,
}

impl<'a> RoundRobinIterator<'a> {
    fn new(aec: &'a RootContainer) -> Self {
        let mut bucket_iters = Vec::with_capacity(bucket_count());
        for bucket in aec.buckets.iter().rev() {
            if !bucket.is_empty() {
                bucket_iters.push(bucket.iter())
            }
        }
        // Terminated elections come last: they only re-broadcast and collect evidence
        if !aec.terminated.is_empty() {
            bucket_iters.push(aec.terminated.iter());
        }
        Self {
            roots: aec,
            bucket_iters,
            current: 0,
            yielded: false,
        }
    }
}

impl<'a> Iterator for RoundRobinIterator<'a> {
    type Item = &'a Entry;

    fn next(&mut self) -> Option<Self::Item> {
        while self.current < self.bucket_iters.len() {
            let item = self.bucket_iters[self.current].next();
            if item.is_some() {
                self.yielded = true;
            }

            self.current += 1;
            if self.current >= self.bucket_iters.len() && self.yielded {
                self.current = 0;
                self.yielded = false;
            }

            if let Some(item) = item {
                return self.roots.get(&item.id);
            }
        }

        None
    }
}
