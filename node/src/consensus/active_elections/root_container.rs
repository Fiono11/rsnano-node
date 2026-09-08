use std::{cmp::Ordering, collections::BTreeSet};

use rsnano_types::ElectionId;
use rsnano_types::{BlockHash, BlockPriority, QualifiedRoot, TimePriority};
use rustc_hash::FxHashMap;

use super::{AecInsertRequest, vote_router::VoteRouter};
use crate::consensus::{
    AecSnapshot, BucketInfo,
    active_elections::aec_service::{BucketSnapshot, ElectionSnapshot},
    election::{Election, ElectionBehavior},
    election_schedulers::priority::{bucket_count, bucket_index},
};
use rsnano_nullable_clock::Timestamp;
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

pub(crate) struct Entry {
    pub root: QualifiedRoot,
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
    root: ElectionId,
    priority: BlockPriority,
}

impl Ord for BucketEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        match other.priority.time.cmp(&self.priority.time) {
            Ordering::Equal => match other.priority.balance.cmp(&self.priority.balance) {
                Ordering::Equal => other.root.cmp(&self.root),
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

/// Contains elections and their qualified roots
pub(crate) struct RootContainer {
    by_root: FxHashMap<ElectionId, Entry>,
    epochs_by_root: FxHashMap<QualifiedRoot, BTreeSet<u64>>,
    buckets: Vec<BTreeSet<BucketEntry>>,
    bucket_infos: Vec<BucketInfo>,
    pub vote_router: VoteRouter,
    max_elections_per_bucket: usize,
    notarized_ids: std::collections::HashSet<ElectionId>,
    notarized_by_bucket: Vec<usize>,
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
            epochs_by_root: Default::default(),
            vote_router: Default::default(),
            buckets: vec![BTreeSet::new(); bucket_count],
            bucket_infos: vec![BucketInfo::new(max_elections_per_bucket); bucket_count],
            max_elections_per_bucket,
            notarized_ids: Default::default(),
            notarized_by_bucket: vec![0; bucket_count],
        }
    }

    pub fn insert(&mut self, entry: Entry) {
        let root = entry.election.id();
        let hash = entry.election.winner().hash();
        let bucket_entry = BucketEntry {
            root: entry.election.id(),
            priority: entry.priority,
        };

        let bucket = &mut self.buckets[entry.bucket()];
        bucket.insert(bucket_entry);

        let infos = &mut self.bucket_infos[entry.bucket()];
        infos.election_count = bucket.len() - self.notarized_by_bucket[entry.bucket()];
        infos.lowest_priority = bucket.last().map(|i| i.priority).unwrap_or_default();

        self.epochs_by_root
            .entry(entry.root.clone())
            .or_default()
            .insert(root.epoch);
        self.by_root.insert(root.clone(), entry);
        self.vote_router.connect_epoch(hash, root);
    }

    /// Release admission capacity without removing the election or its vote routes.
    /// It remains in round-robin processing for additional certificates/final votes.
    #[cfg(feature = "rai_protocol")]
    pub fn mark_notarized(&mut self, id: &ElectionId) {
        let Some(entry) = self.by_root.get(id) else {
            return;
        };
        if self.notarized_ids.insert(id.clone()) {
            let bucket = entry.bucket();
            self.notarized_by_bucket[bucket] += 1;
            self.bucket_infos[bucket].election_count =
                self.buckets[bucket].len() - self.notarized_by_bucket[bucket];
        }
    }

    pub fn scheduling_len(&self) -> usize {
        self.by_root.len() - self.notarized_ids.len()
    }

    pub fn get(&self, root: &QualifiedRoot) -> Option<&Entry> {
        self.by_root.get(&ElectionId::new(
            root.clone(),
            *self.epochs_by_root.get(root)?.first()?,
        ))
    }

    pub fn get_mut(&mut self, root: &QualifiedRoot) -> Option<&mut Entry> {
        self.by_root.get_mut(&ElectionId::new(
            root.clone(),
            *self.epochs_by_root.get(root)?.first()?,
        ))
    }

    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<&Election> {
        self.get(root).map(|i| &i.election)
    }

    pub fn election_for_root_mut(&mut self, root: &QualifiedRoot) -> Option<&mut Election> {
        self.get_mut(root).map(|i| &mut i.election)
    }

    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<&Election> {
        let id = self.vote_router.id(block_hash)?;
        self.by_root.get(id).map(|e| &e.election)
    }

    pub fn election_for_block_mut(&mut self, block_hash: &BlockHash) -> Option<&mut Election> {
        let id = self.vote_router.id(block_hash)?.clone();
        self.by_root.get_mut(&id).map(|i| &mut i.election)
    }

    pub fn bucket_infos(&self) -> &[BucketInfo] {
        &self.bucket_infos
    }

    pub fn try_upgrade_to_priority_election(
        &mut self,
        request: &AecInsertRequest,
        epoch: u64,
    ) -> (bool, Option<ElectionBehavior>) {
        let root = ElectionId::new(request.block.qualified_root(), epoch);

        let Some(entry) = self.by_root.get_mut(&root) else {
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

        let old_bucket_index = bucket_index(previous_behavior, priority.balance);
        let new_bucket_index = bucket_index(ElectionBehavior::Priority, priority.balance);
        if self.notarized_ids.contains(&root) {
            self.notarized_by_bucket[old_bucket_index] -= 1;
            self.notarized_by_bucket[new_bucket_index] += 1;
        }
        let old_bucket = &mut self.buckets[old_bucket_index];
        old_bucket.remove(&BucketEntry {
            root: root.clone(),
            priority,
        });
        let old_infos = &mut self.bucket_infos[old_bucket_index];
        old_infos.election_count = old_bucket.len() - self.notarized_by_bucket[old_bucket_index];
        old_infos.lowest_priority = old_bucket.last().map(|i| i.priority).unwrap_or_default();

        let new_bucket = &mut self.buckets[new_bucket_index];
        new_bucket.insert(BucketEntry {
            root: root.clone(),
            priority,
        });

        let new_infos = &mut self.bucket_infos[new_bucket_index];
        new_infos.election_count = new_bucket.len() - self.notarized_by_bucket[new_bucket_index];
        new_infos.lowest_priority = new_bucket.last().map(|i| i.priority).unwrap_or_default();

        (true, Some(previous_behavior))
    }

    pub fn drain_filter(&mut self, mut predicate: impl FnMut(&Entry) -> bool) -> Vec<Entry> {
        let to_remove: Vec<_> = self
            .by_root
            .values()
            .filter_map(|i| {
                if predicate(i) {
                    Some(i.election.id())
                } else {
                    None
                }
            })
            .collect();

        let mut removed = Vec::new();
        for root in to_remove {
            if let Some(entry) = self.erase_id(&root) {
                removed.push(entry);
            }
        }

        removed
    }

    pub fn erase(&mut self, root: &QualifiedRoot) -> Option<Entry> {
        let id = self.get(root)?.election.id();
        self.erase_id(&id)
    }

    pub fn get_id(&self, id: &ElectionId) -> Option<&Entry> {
        self.by_root.get(id)
    }

    pub fn get_id_mut(&mut self, id: &ElectionId) -> Option<&mut Entry> {
        self.by_root.get_mut(id)
    }
    pub fn erase_lowest(&mut self, bucket_id: usize) -> Option<Entry> {
        let id = self.buckets[bucket_id].last()?.root.clone();
        self.erase_id(&id)
    }
    pub fn election_for_epoch_mut(
        &mut self,
        hash: &BlockHash,
        epoch: u64,
    ) -> Option<&mut Election> {
        let id = self.vote_router.id_in_epoch(hash, epoch)?.clone();
        self.by_root.get_mut(&id).map(|e| &mut e.election)
    }
    pub fn erase_id(&mut self, root: &ElectionId) -> Option<Entry> {
        let erased = self.by_root.remove(root);
        if let Some(epochs) = self.epochs_by_root.get_mut(&root.root) {
            epochs.remove(&root.epoch);
            if epochs.is_empty() {
                self.epochs_by_root.remove(&root.root);
            }
        }
        if let Some(entry) = &erased {
            if self.notarized_ids.remove(root) {
                self.notarized_by_bucket[entry.bucket()] -= 1;
            }
            self.vote_router.disconnect_election(&entry.election);
            let bucket = &mut self.buckets[entry.bucket()];
            bucket.remove(&BucketEntry {
                root: entry.election.id(),
                priority: entry.priority,
            });

            let bucket_info = &mut self.bucket_infos[entry.bucket()];
            bucket_info.election_count = bucket.len() - self.notarized_by_bucket[entry.bucket()];
            bucket_info.lowest_priority = bucket.last().map(|i| i.priority).unwrap_or_default();
        }
        erased
    }

    pub fn clear(&mut self) {
        self.by_root.clear();
        self.notarized_ids.clear();
        self.notarized_by_bucket.fill(0);
        self.epochs_by_root.clear();
        self.vote_router = Default::default();
        for bucket in self.buckets.iter_mut() {
            bucket.clear();
        }
        for i in &mut self.bucket_infos {
            *i = BucketInfo::new(self.max_elections_per_bucket);
        }
    }

    pub fn len(&self) -> usize {
        self.by_root.len()
    }

    pub fn round_robin(&self) -> impl Iterator<Item = &Entry> {
        RoundRobinIterator::new(self)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.by_root.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entry> {
        self.by_root.values_mut()
    }

    pub fn bucket_len(&self, bucket_id: usize) -> usize {
        self.buckets[bucket_id].len() - self.notarized_by_bucket[bucket_id]
    }

    pub fn lowest_priority(&self, bucket_id: usize) -> Option<(QualifiedRoot, TimePriority)> {
        self.buckets[bucket_id]
            .last()
            .map(|i| (i.root.root.clone(), i.priority.time))
    }

    pub fn find_bucket(&self, root: &QualifiedRoot) -> Option<usize> {
        self.by_root
            .get(&ElectionId::new(
                root.clone(),
                *self.epochs_by_root.get(root)?.first()?,
            ))
            .map(|i| i.bucket())
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
                            let election = &self.by_root.get(&entry.root).unwrap().election;
                            ElectionSnapshot {
                                epoch: election.epoch,
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
                return self.roots.by_root.get(&item.root);
            }
        }

        None
    }
}
