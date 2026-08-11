use std::{cmp::Ordering, collections::BTreeSet};

#[cfg(feature = "rai_protocol")]
use std::collections::HashMap;

#[cfg(feature = "rai_protocol")]
use rsnano_types::Root;
use rsnano_types::{BlockHash, BlockPriority, QualifiedRoot, TimePriority};
use rustc_hash::FxHashMap;

#[cfg(not(feature = "rai_protocol"))]
use super::AecInsertRequest;
use super::vote_router::VoteRouter;
#[cfg(not(feature = "rai_protocol"))]
use crate::consensus::election::ElectionBehavior;
use crate::consensus::{
    AecSnapshot, BucketInfo,
    active_elections::aec_service::{BucketSnapshot, ElectionSnapshot},
    election::Election,
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
    root: QualifiedRoot,
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
    by_root: FxHashMap<QualifiedRoot, Entry>,
    #[cfg(feature = "rai_protocol")]
    rai_entries: HashMap<crate::consensus::election::RaiElectionId, Entry>,
    #[cfg(feature = "rai_protocol")]
    rai_by_id: HashMap<crate::consensus::election::RaiElectionId, QualifiedRoot>,
    #[cfg(feature = "rai_protocol")]
    rai_ids_by_root: HashMap<QualifiedRoot, BTreeSet<crate::consensus::election::RaiElectionId>>,
    /// Network ConfirmReq carries only the unqualified root. Keep an exact
    /// reverse index so zero-hash RAI repair never scans every active election
    /// to recover its protocol election id.
    #[cfg(feature = "rai_protocol")]
    rai_ids_by_request_root: HashMap<Root, BTreeSet<crate::consensus::election::RaiElectionId>>,
    #[cfg(feature = "rai_protocol")]
    rai_ids_by_candidate: HashMap<BlockHash, BTreeSet<crate::consensus::election::RaiElectionId>>,
    #[cfg(feature = "rai_protocol")]
    unindexed_roots_by_candidate: HashMap<BlockHash, BTreeSet<QualifiedRoot>>,
    buckets: Vec<BTreeSet<BucketEntry>>,
    bucket_infos: Vec<BucketInfo>,
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
            #[cfg(feature = "rai_protocol")]
            rai_entries: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_by_id: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_ids_by_root: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_ids_by_request_root: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_ids_by_candidate: Default::default(),
            #[cfg(feature = "rai_protocol")]
            unindexed_roots_by_candidate: Default::default(),
            vote_router: Default::default(),
            buckets: vec![BTreeSet::new(); bucket_count],
            bucket_infos: vec![BucketInfo::new(max_elections_per_bucket); bucket_count],
            max_elections_per_bucket,
        }
    }

    pub fn insert(&mut self, entry: Entry) {
        let root = entry.root.clone();
        let bucket_entry = BucketEntry {
            root: entry.root.clone(),
            priority: entry.priority,
        };

        let bucket = &mut self.buckets[entry.bucket()];
        bucket.insert(bucket_entry);

        let infos = &mut self.bucket_infos[entry.bucket()];
        infos.election_count = bucket.len();
        infos.lowest_priority = bucket.last().map(|i| i.priority).unwrap_or_default();

        #[cfg(feature = "rai_protocol")]
        {
            let indexed = self.rai_by_id.contains_key(entry.election.rai_id());
            for hash in entry.election.candidate_hashes() {
                self.vote_router.connect(*hash, root.clone());
                if !indexed {
                    self.unindexed_roots_by_candidate
                        .entry(*hash)
                        .or_default()
                        .insert(root.clone());
                }
            }
        }
        #[cfg(not(feature = "rai_protocol"))]
        self.vote_router
            .connect(entry.election.winner().hash(), root.clone());
        self.by_root.insert(root, entry);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_rai(&mut self, entry: Entry) -> bool {
        let id = entry.election.rai_id().clone();
        if self.rai_by_id.contains_key(&id) {
            return false;
        }

        let root = entry.root.clone();
        let candidate_hashes = entry
            .election
            .candidate_hashes()
            .copied()
            .collect::<Vec<_>>();
        self.rai_by_id.insert(id.clone(), root.clone());
        self.rai_ids_by_root
            .entry(root.clone())
            .or_default()
            .insert(id.clone());
        self.rai_ids_by_request_root
            .entry(root.root)
            .or_default()
            .insert(id.clone());
        for hash in &candidate_hashes {
            self.rai_ids_by_candidate
                .entry(*hash)
                .or_default()
                .insert(id.clone());
        }

        if self.by_root.contains_key(&root) {
            self.rai_entries.insert(id, entry);
        } else {
            self.insert(entry);
        }
        for hash in candidate_hashes {
            self.vote_router.connect(hash, root.clone());
        }
        true
    }

    #[cfg(feature = "rai_protocol")]
    pub fn election_for_rai_id(
        &self,
        id: &crate::consensus::election::RaiElectionId,
    ) -> Option<&Election> {
        let root = self.rai_by_id.get(id)?;
        if let Some(entry) = self.by_root.get(root)
            && entry.election.rai_id() == id
        {
            return Some(&entry.election);
        }
        self.rai_entries.get(id).map(|entry| &entry.election)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn election_for_rai_id_mut(
        &mut self,
        id: &crate::consensus::election::RaiElectionId,
    ) -> Option<&mut Election> {
        let root = self.rai_by_id.get(id)?.clone();
        if self
            .by_root
            .get(&root)
            .is_some_and(|entry| entry.election.rai_id() == id)
        {
            return self.by_root.get_mut(&root).map(|entry| &mut entry.election);
        }
        self.rai_entries
            .get_mut(id)
            .map(|entry| &mut entry.election)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn erase_rai_id(
        &mut self,
        id: &crate::consensus::election::RaiElectionId,
    ) -> Option<Entry> {
        let root = self.rai_by_id.get(id)?.clone();
        let projected = self
            .by_root
            .get(&root)
            .is_some_and(|entry| entry.election.rai_id() == id);
        let erased = if projected {
            self.erase(&root)
        } else {
            let erased = self.rai_entries.remove(id);
            if let Some(entry) = &erased {
                self.vote_router.disconnect_election(&entry.election);
            }
            erased
        };
        let disconnected_hashes = erased
            .as_ref()
            .map(|entry| {
                entry
                    .election
                    .candidate_hashes()
                    .copied()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.rai_by_id.remove(id);

        let mut promote = None;
        if let Some(ids) = self.rai_ids_by_root.get_mut(&root) {
            ids.remove(id);
            if ids.is_empty() {
                self.rai_ids_by_root.remove(&root);
            } else if projected {
                promote = ids
                    .iter()
                    .find_map(|remaining| self.rai_entries.remove(remaining));
            }
        }
        let remove_request_root = self
            .rai_ids_by_request_root
            .get_mut(&root.root)
            .is_some_and(|ids| {
                ids.remove(id);
                ids.is_empty()
            });
        if remove_request_root {
            self.rai_ids_by_request_root.remove(&root.root);
        }
        if let Some(entry) = promote {
            self.insert(entry);
        }
        for hash in &disconnected_hashes {
            let remove_bucket = self.rai_ids_by_candidate.get_mut(hash).is_some_and(|ids| {
                ids.remove(id);
                ids.is_empty()
            });
            if remove_bucket {
                self.rai_ids_by_candidate.remove(hash);
            }
        }
        self.reconnect_candidate_routes(disconnected_hashes);
        erased
    }

    #[cfg(feature = "rai_protocol")]
    fn reconnect_candidate_routes(&mut self, hashes: impl IntoIterator<Item = BlockHash>) {
        for hash in hashes {
            let root = self
                .rai_ids_by_candidate
                .get(&hash)
                .and_then(|ids| ids.iter().find_map(|id| self.rai_by_id.get(id)))
                .or_else(|| {
                    self.unindexed_roots_by_candidate
                        .get(&hash)
                        .and_then(|roots| roots.first())
                })
                .cloned();
            if let Some(root) = root {
                self.vote_router.connect(hash, root);
            } else {
                self.vote_router.disconnect(&hash);
            }
        }
    }

    pub fn get(&self, root: &QualifiedRoot) -> Option<&Entry> {
        self.by_root.get(root)
    }

    pub fn get_mut(&mut self, root: &QualifiedRoot) -> Option<&mut Entry> {
        self.by_root.get_mut(root)
    }

    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<&Election> {
        #[cfg(feature = "rai_protocol")]
        if self
            .rai_ids_by_root
            .get(root)
            .is_some_and(|ids| ids.len() > 1)
        {
            return None;
        }
        self.get(root).map(|i| &i.election)
    }

    pub fn election_for_root_mut(&mut self, root: &QualifiedRoot) -> Option<&mut Election> {
        #[cfg(feature = "rai_protocol")]
        if self
            .rai_ids_by_root
            .get(root)
            .is_some_and(|ids| ids.len() > 1)
        {
            return None;
        }
        self.get_mut(root).map(|i| &mut i.election)
    }

    #[cfg(feature = "rai_protocol")]
    #[allow(dead_code)] // Kept as the legacy root-qualified compatibility operation.
    pub fn add_rai_hash_candidate(&mut self, root: &QualifiedRoot, hash: BlockHash) -> bool {
        let id = {
            let Some(entry) = self.by_root.get_mut(root) else {
                return false;
            };
            if !entry.election.add_rai_hash_candidate(hash) {
                return false;
            }
            entry.election.rai_id().clone()
        };
        if self.rai_by_id.contains_key(&id) {
            self.rai_ids_by_candidate
                .entry(hash)
                .or_default()
                .insert(id);
        } else {
            self.unindexed_roots_by_candidate
                .entry(hash)
                .or_default()
                .insert(root.clone());
        }
        self.vote_router.connect(hash, root.clone());
        true
    }

    #[cfg(feature = "rai_protocol")]
    pub fn add_rai_hash_candidate_for_id(
        &mut self,
        id: &crate::consensus::election::RaiElectionId,
        hash: BlockHash,
    ) -> bool {
        let Some(root) = self.rai_by_id.get(id).cloned() else {
            return false;
        };
        let Some(election) = self.election_for_rai_id_mut(id) else {
            return false;
        };
        if !election.add_rai_hash_candidate(hash) {
            return false;
        }
        self.rai_ids_by_candidate
            .entry(hash)
            .or_default()
            .insert(id.clone());
        self.vote_router.connect(hash, root);
        true
    }

    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<&Election> {
        let root = self.vote_router.qualified_root(block_hash)?;
        self.election_for_root(root)
    }

    #[cfg(feature = "rai_protocol")]
    /// Returns every indexed RAI election whose stable candidate set contains
    /// the hash. `voting_hash` can temporarily become zero during timeout
    /// voting and is therefore not a membership test.
    pub fn rai_elections_for_candidate(
        &self,
        block_hash: &BlockHash,
    ) -> impl Iterator<Item = &Election> {
        let ids = self.rai_ids_by_candidate.get(block_hash);
        ids.into_iter()
            .flatten()
            .filter_map(|id| self.election_for_rai_id(id))
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_elections_for_request_root(&self, root: &Root) -> impl Iterator<Item = &Election> {
        self.rai_ids_by_request_root
            .get(root)
            .into_iter()
            .flatten()
            .filter_map(|id| self.election_for_rai_id(id))
    }

    pub fn election_for_block_mut(&mut self, block_hash: &BlockHash) -> Option<&mut Election> {
        let root = self.vote_router.qualified_root(block_hash)?.clone();
        self.get_mut(&root).map(|i| &mut i.election)
    }

    pub fn bucket_infos(&self) -> &[BucketInfo] {
        &self.bucket_infos
    }

    #[cfg(not(feature = "rai_protocol"))]
    pub fn try_upgrade_to_priority_election(
        &mut self,
        request: &AecInsertRequest,
    ) -> (bool, Option<ElectionBehavior>) {
        let root = request.block.qualified_root();

        let Some(entry) = self.get_mut(&root) else {
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
        let old_bucket = &mut self.buckets[old_bucket_index];
        old_bucket.remove(&BucketEntry {
            root: root.clone(),
            priority,
        });
        let old_infos = &mut self.bucket_infos[old_bucket_index];
        old_infos.election_count = old_bucket.len();
        old_infos.lowest_priority = old_bucket.last().map(|i| i.priority).unwrap_or_default();

        let new_bucket_index = bucket_index(ElectionBehavior::Priority, priority.balance);
        let new_bucket = &mut self.buckets[new_bucket_index];
        new_bucket.insert(BucketEntry {
            root: root.clone(),
            priority,
        });

        let new_infos = &mut self.bucket_infos[new_bucket_index];
        new_infos.election_count = new_bucket.len();
        new_infos.lowest_priority = new_bucket.last().map(|i| i.priority).unwrap_or_default();

        (true, Some(previous_behavior))
    }

    pub fn drain_filter(&mut self, mut predicate: impl FnMut(&Entry) -> bool) -> Vec<Entry> {
        #[cfg(feature = "rai_protocol")]
        let to_remove: Vec<_> = self
            .iter_rai()
            .filter_map(|entry| predicate(entry).then(|| entry.election.rai_id().clone()))
            .collect();
        #[cfg(not(feature = "rai_protocol"))]
        let to_remove: Vec<_> = self
            .by_root
            .values()
            .filter_map(|i| {
                if predicate(i) {
                    Some(i.root.clone())
                } else {
                    None
                }
            })
            .collect();

        let mut removed = Vec::new();
        for key in to_remove {
            #[cfg(feature = "rai_protocol")]
            let erased = self.erase_rai_id(&key);
            #[cfg(not(feature = "rai_protocol"))]
            let erased = self.erase(&key);
            if let Some(entry) = erased {
                removed.push(entry);
            }
        }

        removed
    }

    pub fn erase(&mut self, root: &QualifiedRoot) -> Option<Entry> {
        let erased = self.by_root.remove(root);
        if let Some(entry) = &erased {
            self.vote_router.disconnect_election(&entry.election);
            #[cfg(feature = "rai_protocol")]
            if !self.rai_by_id.contains_key(entry.election.rai_id()) {
                for hash in entry.election.candidate_hashes() {
                    let remove_bucket = self
                        .unindexed_roots_by_candidate
                        .get_mut(hash)
                        .is_some_and(|roots| {
                            roots.remove(root);
                            roots.is_empty()
                        });
                    if remove_bucket {
                        self.unindexed_roots_by_candidate.remove(hash);
                    }
                }
            }
            let bucket = &mut self.buckets[entry.bucket()];
            bucket.remove(&BucketEntry {
                root: entry.root.clone(),
                priority: entry.priority,
            });

            let bucket_info = &mut self.bucket_infos[entry.bucket()];
            bucket_info.election_count = bucket.len();
            bucket_info.lowest_priority = bucket.last().map(|i| i.priority).unwrap_or_default();
        }
        #[cfg(feature = "rai_protocol")]
        if let Some(entry) = &erased {
            self.reconnect_candidate_routes(entry.election.candidate_hashes().copied());
        }
        erased
    }

    pub fn clear(&mut self) {
        self.by_root.clear();
        #[cfg(feature = "rai_protocol")]
        {
            self.rai_entries.clear();
            self.rai_by_id.clear();
            self.rai_ids_by_root.clear();
            self.rai_ids_by_request_root.clear();
            self.rai_ids_by_candidate.clear();
            self.unindexed_roots_by_candidate.clear();
        }
        for bucket in self.buckets.iter_mut() {
            bucket.clear();
        }
        for i in &mut self.bucket_infos {
            *i = BucketInfo::new(self.max_elections_per_bucket);
        }
        self.vote_router = VoteRouter::default();
    }

    pub fn len(&self) -> usize {
        #[cfg(feature = "rai_protocol")]
        return self.by_root.len() + self.rai_entries.len();

        #[cfg(not(feature = "rai_protocol"))]
        self.by_root.len()
    }

    pub fn round_robin(&self) -> impl Iterator<Item = &Entry> {
        RoundRobinIterator::new(self).chain({
            #[cfg(feature = "rai_protocol")]
            {
                self.rai_entries.values()
            }
            #[cfg(not(feature = "rai_protocol"))]
            {
                std::iter::empty()
            }
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.by_root.values()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn iter_rai(&self) -> impl Iterator<Item = &Entry> {
        self.by_root.values().chain(self.rai_entries.values())
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entry> {
        self.by_root.values_mut().chain({
            #[cfg(feature = "rai_protocol")]
            {
                self.rai_entries.values_mut()
            }
            #[cfg(not(feature = "rai_protocol"))]
            {
                std::iter::empty()
            }
        })
    }

    pub fn bucket_len(&self, bucket_id: usize) -> usize {
        self.buckets[bucket_id].len()
    }

    pub fn lowest_priority(&self, bucket_id: usize) -> Option<(QualifiedRoot, TimePriority)> {
        self.buckets[bucket_id]
            .last()
            .map(|i| (i.root.clone(), i.priority.time))
    }

    pub fn find_bucket(&self, root: &QualifiedRoot) -> Option<usize> {
        self.by_root.get(root).map(|i| i.bucket())
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

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use super::*;
    use crate::consensus::{
        election::{Election, ElectionBehavior, RaiElectionId, RaiSlotId},
        rai::{RaiCloseElectionId, RaiCloseKind, rai_close_cut_root},
    };
    use rsnano_ledger::RepWeights;
    use rsnano_types::{Amount, Block, PrivateKey, RaiEpoch, SavedBlock, StateBlockArgs};
    use std::{sync::Arc, time::Duration};

    fn slot_entry(block: SavedBlock, epoch: RaiEpoch) -> Entry {
        let root = block.qualified_root();
        Entry {
            root,
            election: Election::new_slot(
                block,
                ElectionBehavior::Priority,
                Duration::ZERO,
                Timestamp::new_test_instance(),
                epoch,
            ),
            priority: BlockPriority::default(),
        }
    }

    fn same_root_blocks() -> (SavedBlock, SavedBlock) {
        let key = PrivateKey::from(1);
        let make_block = |balance| {
            SavedBlock::new_test_instance_with(Block::from(StateBlockArgs {
                key: &key,
                previous: BlockHash::from_bytes(*key.account().as_bytes()),
                representative: 789.into(),
                balance: Amount::raw(balance),
                link: 111.into(),
                work: 69420.into(),
            }))
        };
        (make_block(1), make_block(2))
    }

    fn close_cut_entry(epoch: RaiEpoch, round: u32, candidate: BlockHash) -> Entry {
        let id = RaiCloseElectionId {
            kind: RaiCloseKind::Cut,
            epoch,
            round,
        };
        let root = rai_close_cut_root(epoch, round);
        Entry {
            root: root.clone(),
            election: Election::new_close(
                id,
                root,
                candidate,
                Arc::new(RepWeights::default()),
                Duration::ZERO,
                Timestamp::new_test_instance(),
            ),
            priority: BlockPriority::default(),
        }
    }

    #[test]
    fn rai_elections_with_the_same_root_are_isolated_by_epoch() {
        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();
        let root = block.qualified_root();
        let old_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(1),
            root: root.clone(),
        });
        let new_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(2),
            root: root.clone(),
        });
        let mut roots = RootContainer::default();

        assert!(roots.insert_rai(slot_entry(block.clone(), RaiEpoch::new(1))));
        assert!(roots.insert_rai(slot_entry(block, RaiEpoch::new(2))));
        assert!(roots.election_for_rai_id(&old_id).is_some());
        assert!(roots.election_for_rai_id(&new_id).is_some());
        assert!(roots.election_for_root(&root).is_none());
        assert_eq!(
            roots
                .rai_elections_for_request_root(&root.root)
                .map(|election| election.rai_id().clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([old_id.clone(), new_id.clone()])
        );

        let candidate = BlockHash::from(42);
        assert!(roots.add_rai_hash_candidate_for_id(&new_id, candidate));
        assert!(
            !roots
                .election_for_rai_id(&old_id)
                .unwrap()
                .contains_candidate(&candidate)
        );
        assert!(
            roots
                .election_for_rai_id(&new_id)
                .unwrap()
                .contains_candidate(&candidate)
        );

        assert!(roots.erase_rai_id(&old_id).is_some());
        assert!(roots.election_for_rai_id(&old_id).is_none());
        assert!(roots.election_for_rai_id(&new_id).is_some());
        assert_eq!(roots.election_for_root(&root).unwrap().rai_id(), &new_id);
        assert_eq!(
            roots
                .rai_elections_for_request_root(&root.root)
                .map(|election| election.rai_id().clone())
                .collect::<Vec<_>>(),
            vec![new_id.clone()]
        );
        assert_eq!(
            roots.rai_ids_by_candidate.get(&block_hash),
            Some(&BTreeSet::from([new_id]))
        );

        roots.clear();
        assert!(
            roots
                .rai_elections_for_request_root(&root.root)
                .next()
                .is_none()
        );
    }

    #[test]
    fn expiring_an_old_election_does_not_remove_the_retry() {
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let old_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(1),
            root: root.clone(),
        });
        let retry_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(2),
            root,
        });
        let mut roots = RootContainer::default();
        roots.insert_rai(slot_entry(block.clone(), RaiEpoch::new(1)));
        roots.insert_rai(slot_entry(block, RaiEpoch::new(2)));

        let removed = roots.drain_filter(|entry| entry.election.rai_id() == &old_id);

        assert_eq!(removed.len(), 1);
        assert!(roots.election_for_rai_id(&old_id).is_none());
        assert!(roots.election_for_rai_id(&retry_id).is_some());
    }

    #[test]
    fn rai_candidate_lookup_prefers_the_indexed_election_over_an_ordinary_projection() {
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let hash = block.hash();
        let indexed_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(2),
            root: root.clone(),
        });
        let mut roots = RootContainer::default();

        // Model an ordinary election occupying the root projection. It is not
        // part of the RAI-id index even though it shares the candidate hash.
        roots.insert(slot_entry(block.clone(), RaiEpoch::new(1)));
        assert!(roots.insert_rai(slot_entry(block, RaiEpoch::new(2))));

        let elections = roots.rai_elections_for_candidate(&hash).collect::<Vec<_>>();
        assert_eq!(elections.len(), 1);
        assert_eq!(elections[0].rai_id(), &indexed_id);
    }

    #[test]
    fn rai_candidate_lookup_skips_a_same_root_non_candidate_sibling() {
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let old_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(1),
            root: root.clone(),
        });
        let matching_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(2),
            root,
        });
        let candidate = BlockHash::from(42);
        let mut roots = RootContainer::default();
        assert!(roots.insert_rai(slot_entry(block.clone(), RaiEpoch::new(1))));
        assert!(roots.insert_rai(slot_entry(block, RaiEpoch::new(2))));
        assert!(roots.add_rai_hash_candidate_for_id(&matching_id, candidate));
        assert!(
            !roots
                .election_for_rai_id(&old_id)
                .unwrap()
                .contains_candidate(&candidate)
        );

        let elections = roots
            .rai_elections_for_candidate(&candidate)
            .collect::<Vec<_>>();
        assert_eq!(elections.len(), 1);
        assert_eq!(elections[0].rai_id(), &matching_id);
    }

    #[test]
    fn inserting_a_same_root_rai_sibling_routes_its_distinct_initial_candidate() {
        let (first, second) = same_root_blocks();
        assert_eq!(first.qualified_root(), second.qualified_root());
        assert_ne!(first.hash(), second.hash());
        let second_hash = second.hash();
        let second_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(2),
            root: second.qualified_root(),
        });
        let mut roots = RootContainer::default();
        assert!(roots.insert_rai(slot_entry(first, RaiEpoch::new(1))));
        assert!(roots.insert_rai(slot_entry(second, RaiEpoch::new(2))));

        let elections = roots
            .rai_elections_for_candidate(&second_hash)
            .collect::<Vec<_>>();
        assert_eq!(elections.len(), 1);
        assert_eq!(elections[0].rai_id(), &second_id);
    }

    #[test]
    fn erasing_a_non_projected_sibling_restores_the_shared_candidate_route() {
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let root = block.qualified_root();
        let projected_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(1),
            root: root.clone(),
        });
        let sibling_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(2),
            root,
        });
        let mut roots = RootContainer::default();
        assert!(roots.insert_rai(slot_entry(block.clone(), RaiEpoch::new(1))));
        assert!(roots.insert_rai(slot_entry(block, RaiEpoch::new(2))));

        assert!(roots.erase_rai_id(&sibling_id).is_some());

        let elections = roots.rai_elections_for_candidate(&hash).collect::<Vec<_>>();
        assert_eq!(elections.len(), 1);
        assert_eq!(elections[0].rai_id(), &projected_id);
    }

    #[test]
    fn erasing_a_cross_root_close_round_restores_the_shared_candidate_route() {
        let epoch = RaiEpoch::new(3);
        let candidate = BlockHash::from(42);
        let first_id = RaiElectionId::CloseCut { epoch, round: 0 };
        let second_id = RaiElectionId::CloseCut { epoch, round: 1 };
        let mut roots = RootContainer::default();
        assert!(roots.insert_rai(close_cut_entry(epoch, 0, candidate)));
        assert!(roots.insert_rai(close_cut_entry(epoch, 1, candidate)));

        assert!(roots.erase_rai_id(&second_id).is_some());

        let elections = roots
            .rai_elections_for_candidate(&candidate)
            .collect::<Vec<_>>();
        assert_eq!(elections.len(), 1);
        assert_eq!(elections[0].rai_id(), &first_id);
    }

    #[test]
    fn erasing_an_indexed_sibling_restores_an_ordinary_projection_route() {
        let block = SavedBlock::new_test_instance();
        let candidate = block.hash();
        let ordinary_root = block.qualified_root();
        let epoch = RaiEpoch::new(3);
        let indexed_id = RaiElectionId::CloseCut { epoch, round: 0 };
        let indexed_root = rai_close_cut_root(epoch, 0);
        let mut roots = RootContainer::default();

        roots.insert(slot_entry(block, RaiEpoch::new(1)));
        assert_eq!(
            roots.unindexed_roots_by_candidate.get(&candidate),
            Some(&BTreeSet::from([ordinary_root.clone()]))
        );
        assert!(roots.insert_rai(close_cut_entry(epoch, 0, candidate)));
        assert_eq!(
            roots.vote_router.qualified_root(&candidate),
            Some(&indexed_root)
        );

        assert!(roots.erase_rai_id(&indexed_id).is_some());

        assert_eq!(
            roots.vote_router.qualified_root(&candidate),
            Some(&ordinary_root)
        );
        assert!(roots.election_for_block(&candidate).is_some());
        assert!(!roots.rai_ids_by_candidate.contains_key(&candidate));
        assert_eq!(
            roots.unindexed_roots_by_candidate.get(&candidate),
            Some(&BTreeSet::from([ordinary_root]))
        );
    }

    #[test]
    fn direct_erase_removes_unindexed_candidate_ownership_and_route() {
        let block = SavedBlock::new_test_instance();
        let candidate = block.hash();
        let root = block.qualified_root();
        let mut roots = RootContainer::default();
        roots.insert(slot_entry(block, RaiEpoch::new(1)));

        assert!(roots.erase(&root).is_some());

        assert!(!roots.unindexed_roots_by_candidate.contains_key(&candidate));
        assert!(!roots.vote_router.is_active(&candidate));
    }

    #[test]
    fn rai_candidate_index_tracks_insert_add_erase_and_clear() {
        let block = SavedBlock::new_test_instance();
        let initial = block.hash();
        let added = BlockHash::from(42);
        let id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(1),
            root: block.qualified_root(),
        });
        let mut roots = RootContainer::default();

        assert!(roots.insert_rai(slot_entry(block.clone(), RaiEpoch::new(1))));
        assert_eq!(
            roots.rai_ids_by_candidate.get(&initial),
            Some(&BTreeSet::from([id.clone()]))
        );
        assert!(roots.add_rai_hash_candidate_for_id(&id, added));
        assert_eq!(
            roots.rai_ids_by_candidate.get(&added),
            Some(&BTreeSet::from([id.clone()]))
        );

        assert!(roots.erase_rai_id(&id).is_some());
        assert!(!roots.rai_ids_by_candidate.contains_key(&initial));
        assert!(!roots.rai_ids_by_candidate.contains_key(&added));
        assert!(!roots.vote_router.is_active(&initial));
        assert!(!roots.vote_router.is_active(&added));

        assert!(roots.insert_rai(slot_entry(block, RaiEpoch::new(1))));
        roots.clear();
        assert!(roots.rai_ids_by_candidate.is_empty());
        assert!(roots.unindexed_roots_by_candidate.is_empty());
        assert!(!roots.vote_router.is_active(&initial));
        assert!(roots.rai_elections_for_candidate(&initial).next().is_none());
    }

    #[test]
    fn restoring_a_shared_candidate_consults_only_its_index_bucket() {
        let epoch = RaiEpoch::new(3);
        let shared = BlockHash::from(42);
        let first_id = RaiElectionId::CloseCut { epoch, round: 0 };
        let second_id = RaiElectionId::CloseCut { epoch, round: 1 };
        let mut roots = RootContainer::default();
        assert!(roots.insert_rai(close_cut_entry(epoch, 0, shared)));
        assert!(roots.insert_rai(close_cut_entry(epoch, 1, shared)));

        // Populate many unrelated elections. The shared hash's restoration
        // bucket remains proportional only to the elections sharing it.
        for round in 2..258 {
            assert!(roots.insert_rai(close_cut_entry(
                epoch,
                round,
                BlockHash::from(round as u64 + 1000),
            )));
        }
        assert_eq!(
            roots.rai_ids_by_candidate.get(&shared),
            Some(&BTreeSet::from([first_id.clone(), second_id.clone()]))
        );
        assert_eq!(roots.rai_elections_for_candidate(&shared).count(), 2);

        assert!(roots.erase_rai_id(&second_id).is_some());

        assert_eq!(
            roots.rai_ids_by_candidate.get(&shared),
            Some(&BTreeSet::from([first_id.clone()]))
        );
        assert_eq!(
            roots.vote_router.qualified_root(&shared),
            roots.rai_by_id.get(&first_id)
        );
        assert_eq!(roots.rai_elections_for_candidate(&shared).count(), 1);
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
