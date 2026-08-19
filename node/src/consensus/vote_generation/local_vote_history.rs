use std::{
    collections::{BTreeMap, HashMap, HashSet},
    mem::size_of,
    sync::{Arc, Mutex},
};

use rsnano_types::{BlockHash, NetworkType, Root, Vote};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

pub struct LocalVoteHistory {
    data: Mutex<LocalVoteHistoryData>,
    max_cache: usize,
}

#[derive(Default)]
struct LocalVoteHistoryData {
    history: BTreeMap<usize, LocalVote>,
    history_by_root: HashMap<Root, HashSet<usize>>,
}

impl LocalVoteHistoryData {
    fn new() -> Self {
        Default::default()
    }
}

struct LocalVote {
    root: Root,
    hash: BlockHash,
    #[cfg(feature = "rai_protocol")]
    metadata: rsnano_types::RaiVoteMetadata,
    vote: Arc<Vote>,
}

impl LocalVoteHistory {
    pub fn new(network: NetworkType) -> Self {
        Self::with_max_cache(Self::max_cache_for(network))
    }

    fn max_cache_for(network: NetworkType) -> usize {
        #[cfg(feature = "rai_protocol")]
        {
            // RAI replays signed First/Notar/Final material while draining a
            // certified close cut.  The dev-network cache of 256 entries can
            // evict the beginning of an ordinary multi-hundred-slot cut before
            // a lagging replica receives it, permanently stalling close.
            let _ = network;
            return 128 * 1024;
        }
        #[cfg(not(feature = "rai_protocol"))]
        match network {
            NetworkType::NanoDevNetwork => 256,
            _ => 128 * 1024,
        }
    }

    pub fn with_max_cache(max_cache: usize) -> Self {
        Self {
            data: Mutex::new(LocalVoteHistoryData::new()),
            max_cache,
        }
    }

    pub fn add(&self, root: &Root, hash: &BlockHash, vote: &Arc<Vote>) {
        #[cfg(feature = "rai_protocol")]
        {
            let metadata = vote
                .hashes()
                .position(|candidate| candidate == hash)
                .and_then(|index| vote.rai_metadata(index))
                .cloned()
                .unwrap_or_default();
            self.add_rai(root, hash, &metadata, vote);
        }

        #[cfg(not(feature = "rai_protocol"))]
        self.add_impl(root, hash, vote);
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn add_rai(
        &self,
        root: &Root,
        hash: &BlockHash,
        metadata: &rsnano_types::RaiVoteMetadata,
        vote: &Arc<Vote>,
    ) {
        self.add_impl(root, hash, metadata.clone(), vote);
    }

    fn add_impl(
        &self,
        root: &Root,
        hash: &BlockHash,
        #[cfg(feature = "rai_protocol")] metadata: rsnano_types::RaiVoteMetadata,
        vote: &Arc<Vote>,
    ) {
        let mut data_lk = self.data.lock().unwrap();
        let data: &mut LocalVoteHistoryData = &mut data_lk;
        clean(data, self.max_cache);

        let mut add_vote = true;
        let mut remove_root = false;
        let mut ids_to_delete = Vec::new();
        // Erase any vote that is not for this hash, or duplicate by account, and if new timestamp is higher
        if let Some(ids) = data.history_by_root.get_mut(root) {
            for &i in ids.iter() {
                let current = &data.history[&i];
                let same_phase = {
                    #[cfg(feature = "rai_protocol")]
                    {
                        current.metadata.phase == metadata.phase
                    }
                    #[cfg(not(feature = "rai_protocol"))]
                    {
                        true
                    }
                };
                let same_election = {
                    #[cfg(feature = "rai_protocol")]
                    {
                        current.metadata.election_id == metadata.election_id
                            && current.metadata.scope == metadata.scope
                    }
                    #[cfg(not(feature = "rai_protocol"))]
                    {
                        true
                    }
                };
                let replace = {
                    #[cfg(feature = "rai_protocol")]
                    {
                        same_election
                            && same_phase
                            && vote.voter == current.vote.voter
                            && (metadata.phase != rsnano_types::RaiVotePhase::Notar
                                || &current.hash == hash)
                            && current.vote.timestamp() <= vote.timestamp()
                    }
                    #[cfg(not(feature = "rai_protocol"))]
                    {
                        &current.hash != hash
                            || (vote.voter == current.vote.voter
                                && current.vote.timestamp() <= vote.timestamp())
                    }
                };
                if replace {
                    ids_to_delete.push(i);
                } else if same_election
                    && same_phase
                    && vote.voter == current.vote.voter
                    && current.vote.timestamp() > vote.timestamp()
                {
                    add_vote = false;
                }
            }

            for &i in &ids_to_delete {
                ids.remove(&i);
                data.history.remove(&i);
                remove_root = ids.is_empty();
            }
        }

        if remove_root && !add_vote {
            data.history_by_root.remove(root);
        }

        // Do not add new vote to cache if representative account is same and timestamp is lower
        if add_vote {
            let id = data
                .history
                .iter()
                .next_back()
                .map(|(k, _)| k + 1)
                .unwrap_or_default();
            data.history.insert(
                id,
                LocalVote {
                    root: root.to_owned(),
                    hash: hash.to_owned(),
                    #[cfg(feature = "rai_protocol")]
                    metadata,
                    vote: vote.clone(),
                },
            );
            data.history_by_root
                .entry(root.to_owned())
                .or_default()
                .insert(id);
        }
    }

    pub fn erase_batch(&self, roots: impl IntoIterator<Item = Root>) {
        let mut guard = self.data.lock().unwrap();
        for root in roots {
            if let Some(removed) = guard.history_by_root.remove(&root) {
                for &id in &removed {
                    guard.history.remove(&id);
                }
            }
        }
    }

    pub fn erase(&self, root: &Root) {
        let mut data_lk = self.data.lock().unwrap();
        if let Some(removed) = data_lk.history_by_root.remove(root) {
            for &id in &removed {
                data_lk.history.remove(&id);
            }
        }
    }

    pub fn votes(&self, root: &Root, hash: &BlockHash, is_final: bool) -> Vec<Arc<Vote>> {
        let data_lk = self.data.lock().unwrap();
        let mut result = Vec::new();
        if let Some(ids) = data_lk.history_by_root.get(root) {
            for &id in ids.iter() {
                let entry = &data_lk.history[&id];
                if &entry.hash == hash && (!is_final || entry.vote.is_final()) {
                    result.push(entry.vote.clone())
                }
            }
        }
        result
    }

    pub fn exists(&self, root: &Root) -> bool {
        let data_lk = self.data.lock().unwrap();
        data_lk.history_by_root.contains_key(root)
    }

    /// A RAI signer has one immutable logical vote slot per election phase and
    /// overlapping committee scope. Candidate changes inside the same close
    /// round must not authorize a second signature for a different value;
    /// disjoint old/new committee scopes remain independent.
    #[cfg(feature = "rai_protocol")]
    pub fn rai_phase_vote_exists(
        &self,
        root: &Root,
        hash: &BlockHash,
        voter: &rsnano_types::PublicKey,
        metadata: &rsnano_types::RaiVoteMetadata,
    ) -> bool {
        let guard = self.data.lock().unwrap();
        guard.history_by_root.get(root).is_some_and(|ids| {
            ids.iter().any(|id| {
                let current = &guard.history[id];
                current.vote.voter == *voter
                    && current.metadata.election_id == metadata.election_id
                    && current.metadata.phase == metadata.phase
                    && rai_scopes_overlap(current.metadata.scope, metadata.scope)
                    // Notarization plurality remains legal, but repeatedly
                    // signing the same value is not a new logical vote.
                    && (metadata.phase != rsnano_types::RaiVotePhase::Notar
                        || current.hash == *hash)
            })
        })
    }

    /// Whether this signer's previously signed First/Notar support in the
    /// overlapping committee scope is compatible with a Final vote for
    /// `hash`. Empty support is compatible, as permitted by the slot final
    /// rule; any different supported value makes that Final vote invalid.
    #[cfg(feature = "rai_protocol")]
    pub fn rai_support_is_compatible(
        &self,
        root: &Root,
        hash: &BlockHash,
        voter: &rsnano_types::PublicKey,
        metadata: &rsnano_types::RaiVoteMetadata,
    ) -> bool {
        let guard = self.data.lock().unwrap();
        guard.history_by_root.get(root).is_none_or(|ids| {
            ids.iter().all(|id| {
                let current = &guard.history[id];
                current.vote.voter != *voter
                    || current.metadata.election_id != metadata.election_id
                    || current.metadata.phase == rsnano_types::RaiVotePhase::Final
                    || !rai_scopes_overlap(current.metadata.scope, metadata.scope)
                    || current.hash == *hash
            })
        })
    }

    pub fn size(&self) -> usize {
        self.data.lock().unwrap().history.len()
    }

    /// Signed vote batches are durable RAI quorum material.  A peer may learn
    /// the referenced block after the original vote broadcast, so the epoch
    /// ticker periodically replays every retained batch.  One vectorized vote
    /// is indexed under each of its roots; return it only once.
    #[cfg(feature = "rai_protocol")]
    pub fn rai_votes(&self) -> Vec<Arc<Vote>> {
        let guard = self.data.lock().unwrap();
        let mut votes = HashMap::new();
        for (id, entry) in &guard.history {
            // Every leaf index holds the same Arc. Pointer identity avoids
            // rehashing the full canonical batch once per indexed leaf.
            votes
                .entry(Arc::as_ptr(&entry.vote))
                .or_insert((*id, entry.vote.clone()));
        }
        drop(guard);

        // A batch can contain different phases for independent elections.
        // Preserve signing order so every election's First evidence is
        // replayed before a later batch containing its Notar evidence.
        let mut votes = votes.into_values().collect::<Vec<_>>();
        votes.sort_by_key(|(id, vote)| (*id, vote.hash()));
        votes.into_iter().map(|(_, vote)| vote).collect()
    }

    /// Returns retained batches covering one root/hash. `context == None`
    /// means a contextless legacy ConfirmReq and permits every RAI context;
    /// otherwise only the governing election and epoch are returned.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_votes_for_candidate(
        &self,
        root: &Root,
        hash: &BlockHash,
        context: Option<&rsnano_types::RaiVoteMetadata>,
    ) -> Vec<Arc<Vote>> {
        self.rai_votes_from_root(root, |entry| {
            entry.hash == *hash
                && context.is_none_or(|context| {
                    entry.metadata.election_id == context.election_id
                        && entry.metadata.epoch == context.epoch
                })
        })
    }

    /// Returns retained evidence for one election through the per-root index.
    /// ConfirmReq repair is root-qualified, so scanning and sorting the entire
    /// process-lifetime RAI history here is unnecessary and can hold the
    /// signing-path mutex for seconds once the cache is large.
    #[cfg(feature = "rai_protocol")]
    pub fn rai_votes_for_election(
        &self,
        root: &Root,
        election_id: &rsnano_types::RaiElectionId,
    ) -> Vec<Arc<Vote>> {
        self.rai_votes_from_root(root, |entry| entry.metadata.election_id == *election_id)
    }

    /// Returns retained slot transports for a request root without requiring
    /// live AEC metadata. This is a replay-only recovery path for a peer which
    /// has already pruned a closed epoch while another peer is still draining;
    /// the signed leaves retain their exact epoch/election qualification.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_slot_votes_for_root(
        &self,
        root: &Root,
        excluded_election: Option<&rsnano_types::RaiElectionId>,
    ) -> Vec<Arc<Vote>> {
        self.rai_votes_from_root(root, |entry| {
            matches!(
                entry.metadata.election_id,
                rsnano_types::RaiElectionId::Slot(_)
            ) && excluded_election.is_none_or(|excluded| entry.metadata.election_id != *excluded)
        })
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_votes(&self) -> Vec<Arc<Vote>> {
        self.rai_votes_matching(|entry| {
            matches!(
                entry.metadata.election_id,
                rsnano_types::RaiElectionId::CloseCut { .. }
                    | rsnano_types::RaiElectionId::CloseRecord { .. }
            )
        })
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_slot_votes(&self) -> Vec<Arc<Vote>> {
        self.rai_votes_matching(|entry| {
            matches!(
                entry.metadata.election_id,
                rsnano_types::RaiElectionId::Slot(_)
            )
        })
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_votes_from_root(
        &self,
        root: &Root,
        predicate: impl Fn(&LocalVote) -> bool,
    ) -> Vec<Arc<Vote>> {
        let guard = self.data.lock().unwrap();
        let mut votes = HashMap::new();
        for id in guard.history_by_root.get(root).into_iter().flatten() {
            let entry = &guard.history[id];
            if predicate(entry) {
                let order = (entry.metadata.phase as u8, *id);
                votes
                    .entry(Arc::as_ptr(&entry.vote))
                    .and_modify(|current: &mut ((u8, usize), Arc<Vote>)| {
                        if order < current.0 {
                            *current = (order, entry.vote.clone());
                        }
                    })
                    .or_insert((order, entry.vote.clone()));
            }
        }
        drop(guard);

        // First evidence must precede Notar evidence during targeted repair.
        let mut votes = votes.into_values().collect::<Vec<_>>();
        votes.sort_by_key(|(order, vote)| (*order, vote.hash()));
        votes.into_iter().map(|(_, vote)| vote).collect()
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_votes_matching(&self, predicate: impl Fn(&LocalVote) -> bool) -> Vec<Arc<Vote>> {
        let guard = self.data.lock().unwrap();
        let mut votes = HashMap::new();
        for (id, entry) in &guard.history {
            if predicate(entry) {
                votes
                    .entry(Arc::as_ptr(&entry.vote))
                    .or_insert((*id, entry.vote.clone()));
            }
        }
        drop(guard);

        let mut votes = votes.into_values().collect::<Vec<_>>();
        votes.sort_by_key(|(id, vote)| (*id, vote.hash()));
        votes.into_iter().map(|(_, vote)| vote).collect()
    }
}

#[cfg(feature = "rai_protocol")]
fn rai_scopes_overlap(
    first: rsnano_types::RaiCommitteeScope,
    second: rsnano_types::RaiCommitteeScope,
) -> bool {
    use rsnano_types::RaiCommitteeScope;
    first == RaiCommitteeScope::All || second == RaiCommitteeScope::All || first == second
}

impl ContainerInfoProvider for LocalVoteHistory {
    fn container_info(&self) -> ContainerInfo {
        [(
            "history",
            self.data.lock().unwrap().history.len(),
            size_of::<LocalVote>(),
        )]
        .into()
    }
}

fn clean(data: &mut LocalVoteHistoryData, max_cache: usize) {
    debug_assert!(max_cache > 0);
    while data.history.len() > max_cache {
        let (id, root) = {
            let (id, vote) = data.history.iter().next().unwrap();
            (*id, vote.root)
        };
        data.history.remove(&id);
        let mut root_empty = false;
        if let Some(root) = data.history_by_root.get_mut(&root) {
            root.remove(&id);
            root_empty = root.is_empty();
        }

        if root_empty {
            data.history_by_root.remove(&root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{PrivateKey, UnixMillisTimestamp};

    #[test]
    fn empty_history() {
        let history = LocalVoteHistory::with_max_cache(256);
        assert!(!history.exists(&Root::from(1)));
        assert_eq!(
            history
                .votes(&Root::from(1), &BlockHash::from(2), false)
                .len(),
            0
        );
        assert_eq!(history.size(), 0);
    }

    #[test]
    fn add_one_vote() {
        let history = LocalVoteHistory::with_max_cache(256);
        let vote = Arc::new(Vote::null());
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        history.add(&root, &hash, &vote);
        assert_eq!(history.size(), 1);
        assert_eq!(history.exists(&root), true);
        assert_eq!(history.exists(&Root::from(2)), false);
        let votes = history.votes(&root, &hash, false);
        assert_eq!(votes.len(), 1);
        assert_eq!(Arc::ptr_eq(&votes[0], &vote), true);
        assert_eq!(history.votes(&root, &BlockHash::from(3), false).len(), 0);
        assert_eq!(
            history
                .votes(&Root::from(2), &BlockHash::from(2), false)
                .len(),
            0
        );
    }

    #[test]
    fn add_two_votes() {
        let history = LocalVoteHistory::with_max_cache(256);
        let vote1a = Arc::new(Vote::null());
        let vote1b = Arc::new(Vote::null());
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        history.add(&root, &hash, &vote1a);
        history.add(&root, &hash, &vote1b);
        let votes = history.votes(&root, &hash, false);
        assert_eq!(votes.len(), 1);
        assert_eq!(Arc::ptr_eq(&votes[0], &vote1b), true);
        assert_eq!(Arc::ptr_eq(&votes[0], &vote1a), false);
    }

    #[test]
    fn basic() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let vote1a = Arc::new(Vote::null());
        let vote1b = Arc::new(Vote::null());
        let keys = PrivateKey::new();
        let vote2 = Arc::new(Vote::new(&keys, UnixMillisTimestamp::ZERO, 0, vec![hash]));
        history.add(&root, &hash, &vote1a);
        history.add(&root, &hash, &vote1b);
        history.add(&root, &hash, &vote2);
        assert_eq!(history.size(), 2);

        let votes = history.votes(&root, &hash, false);
        assert_eq!(votes.len(), 2);
        assert!(Arc::ptr_eq(&votes[0], &vote1b) || Arc::ptr_eq(&votes[1], &vote1b));
        assert!(Arc::ptr_eq(&votes[0], &vote2) || Arc::ptr_eq(&votes[1], &vote2));
    }

    #[test]
    fn basic2() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let vote1a = Arc::new(Vote::null());
        let vote1b = Arc::new(Vote::null());
        let keys1 = PrivateKey::new();
        let vote2 = Arc::new(Vote::new(&keys1, UnixMillisTimestamp::ZERO, 0, vec![hash]));
        let keys2 = PrivateKey::new();
        let other_hash = BlockHash::from(3);
        let vote3 = Arc::new(Vote::new(
            &keys2,
            UnixMillisTimestamp::ZERO,
            0,
            vec![other_hash],
        ));
        history.add(&root, &hash, &vote1a);
        history.add(&root, &hash, &vote1b);
        history.add(&root, &hash, &vote2);
        history.add(&root, &other_hash, &vote3);
        #[cfg(not(feature = "rai_protocol"))]
        assert_eq!(history.size(), 1);
        #[cfg(feature = "rai_protocol")]
        assert_eq!(history.size(), 3);
        let votes = history.votes(&root, &other_hash, false);
        assert_eq!(votes.len(), 1);
        assert!(Arc::ptr_eq(&votes[0], &vote3));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_votes_returns_each_vectorized_batch_once() {
        let history = LocalVoteHistory::with_max_cache(256);
        let key = PrivateKey::new();
        let first_hash = BlockHash::from(11);
        let second_hash = BlockHash::from(12);
        let vote = Arc::new(Vote::new(
            &key,
            UnixMillisTimestamp::new(1),
            0,
            vec![first_hash, second_hash],
        ));

        history.add(&Root::from(1), &first_hash, &vote);
        history.add(&Root::from(2), &second_hash, &vote);

        let replay = history.rai_votes();
        assert_eq!(replay.len(), 1);
        assert!(Arc::ptr_eq(&replay[0], &vote));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn slot_votes_can_be_replayed_by_root_without_live_aec_context() {
        use rsnano_types::{
            QualifiedRoot, RaiElectionId, RaiEpoch, RaiSlotId, RaiVoteMetadata, RaiVotePhase,
        };

        let history = LocalVoteHistory::with_max_cache(256);
        let key = PrivateKey::new();
        let root = Root::from(21);
        let hash = BlockHash::from(22);
        let metadata = RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::new(3),
                root: QualifiedRoot::new(root, BlockHash::from(20)),
            }),
            epoch: RaiEpoch::new(3),
            phase: RaiVotePhase::First,
            ..Default::default()
        };
        let vote = Arc::new(Vote::new_rai(
            &key,
            UnixMillisTimestamp::new(16),
            0,
            hash,
            metadata.clone(),
        ));
        history.add_rai(&root, &hash, &metadata, &vote);

        let replay = history.rai_slot_votes_for_root(&root, None);
        assert_eq!(replay.len(), 1);
        assert!(Arc::ptr_eq(&replay[0], &vote));
        assert!(
            history
                .rai_slot_votes_for_root(&Root::from(99), None)
                .is_empty()
        );

        let newer_metadata = RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::new(4),
                root: QualifiedRoot::new(root, BlockHash::from(20)),
            }),
            epoch: RaiEpoch::new(4),
            phase: RaiVotePhase::First,
            ..Default::default()
        };
        let newer_vote = Arc::new(Vote::new_rai(
            &key,
            UnixMillisTimestamp::new(32),
            0,
            BlockHash::from(23),
            newer_metadata.clone(),
        ));
        history.add_rai(&root, &BlockHash::from(23), &newer_metadata, &newer_vote);

        let archived = history.rai_slot_votes_for_root(&root, Some(&newer_metadata.election_id));
        assert_eq!(archived.len(), 1);
        assert!(Arc::ptr_eq(&archived[0], &vote));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_batch_is_indexed_with_leaf_metadata_and_replayed_once() {
        use rsnano_types::{
            QualifiedRoot, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId, RaiVoteMetadata,
            RaiVotePhase,
        };

        let history = LocalVoteHistory::with_max_cache(256);
        let key = PrivateKey::new();
        let roots = [Root::from(1), Root::from(2)];
        let hashes = [BlockHash::from(11), BlockHash::from(12)];
        let metadata = roots.map(|root| RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::new(3),
                root: QualifiedRoot::new(root, BlockHash::from(99)),
            }),
            phase: if root == roots[0] {
                RaiVotePhase::First
            } else {
                RaiVotePhase::Notar
            },
            epoch: RaiEpoch::new(3),
            scope: RaiCommitteeScope::All,
        });
        let vote = Arc::new(Vote::new_rai_batch(
            &key,
            UnixMillisTimestamp::new(16),
            0,
            metadata
                .iter()
                .cloned()
                .zip(hashes)
                .map(|(metadata, hash)| (metadata, hash)),
        ));

        history.add_rai(&roots[0], &hashes[0], &metadata[0], &vote);
        history.add_rai(&roots[1], &hashes[1], &metadata[1], &vote);

        assert_eq!(history.size(), 2);
        assert!(history.rai_phase_vote_exists(
            &roots[0],
            &hashes[0],
            &key.public_key(),
            &metadata[0]
        ));
        assert!(history.rai_phase_vote_exists(
            &roots[1],
            &hashes[1],
            &key.public_key(),
            &metadata[1]
        ));
        assert!(!history.rai_phase_vote_exists(
            &roots[0],
            &hashes[0],
            &key.public_key(),
            &metadata[1]
        ));

        let replay = history.rai_votes();
        assert_eq!(replay.len(), 1);
        assert!(Arc::ptr_eq(&replay[0], &vote));
        for index in 0..2 {
            let targeted = history.rai_votes_for_candidate(
                &roots[index],
                &hashes[index],
                Some(&metadata[index]),
            );
            assert_eq!(targeted.len(), 1);
            assert!(Arc::ptr_eq(&targeted[0], &vote));
        }
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_mixed_phase_batches_replay_in_signing_order() {
        use rsnano_types::{
            QualifiedRoot, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId, RaiVoteMetadata,
            RaiVotePhase,
        };

        let history = LocalVoteHistory::with_max_cache(256);
        let key = PrivateKey::new();
        let roots = [Root::from(1), Root::from(2)];
        let hashes = [BlockHash::from(11), BlockHash::from(12)];
        let metadata = |root, phase| RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::new(3),
                root: QualifiedRoot::new(root, BlockHash::from(99)),
            }),
            phase,
            epoch: RaiEpoch::new(3),
            scope: RaiCommitteeScope::All,
        };
        let first_a = metadata(roots[0], RaiVotePhase::First);
        let notar_a = metadata(roots[0], RaiVotePhase::Notar);
        let notar_b = metadata(roots[1], RaiVotePhase::Notar);
        let mixed = Arc::new(Vote::new_rai_batch(
            &key,
            UnixMillisTimestamp::new(16),
            0,
            [(first_a.clone(), hashes[0]), (notar_b.clone(), hashes[1])],
        ));
        let later = Arc::new(Vote::new_rai(
            &key,
            UnixMillisTimestamp::new(32),
            0,
            hashes[0],
            notar_a.clone(),
        ));
        history.add_rai(&roots[0], &hashes[0], &first_a, &mixed);
        history.add_rai(&roots[1], &hashes[1], &notar_b, &mixed);
        history.add_rai(&roots[0], &hashes[0], &notar_a, &later);

        let replay = history.rai_votes();
        assert_eq!(replay.len(), 2);
        assert!(Arc::ptr_eq(&replay[0], &mixed));
        assert!(Arc::ptr_eq(&replay[1], &later));

        let election = history.rai_votes_for_election(&roots[0], &first_a.election_id);
        assert_eq!(election.len(), 2);
        assert!(Arc::ptr_eq(&election[0], &mixed));
        assert!(Arc::ptr_eq(&election[1], &later));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_history_retains_first_and_final_votes() {
        let history = LocalVoteHistory::with_max_cache(256);
        let key = PrivateKey::new();
        let root = Root::from(1);
        let hash = BlockHash::from(11);
        let first = Arc::new(Vote::new(&key, UnixMillisTimestamp::new(1), 0, vec![hash]));
        let final_vote = Arc::new(Vote::new_final(&key, vec![hash]));

        history.add(&root, &hash, &first);
        history.add(&root, &hash, &final_vote);

        let replay = history.rai_votes();
        assert_eq!(replay.len(), 2);
        assert!(replay.iter().any(|vote| Arc::ptr_eq(vote, &first)));
        assert!(replay.iter().any(|vote| Arc::ptr_eq(vote, &final_vote)));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_dev_history_can_retain_a_complete_large_close_cut() {
        let history = LocalVoteHistory::new(NetworkType::NanoDevNetwork);
        let key = PrivateKey::new();

        for value in 1..=1_214 {
            let hash = BlockHash::from(value);
            let vote = Arc::new(Vote::new(
                &key,
                UnixMillisTimestamp::new(value),
                0,
                vec![hash],
            ));
            history.add(&Root::from(value), &hash, &vote);
        }

        assert_eq!(history.rai_votes().len(), 1_214);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_replay_order_is_stable_across_snapshots() {
        let history = LocalVoteHistory::with_max_cache(256);
        let key = PrivateKey::new();
        for value in 1..=64 {
            let hash = BlockHash::from(value);
            let vote = Arc::new(Vote::new(
                &key,
                UnixMillisTimestamp::new(value),
                0,
                vec![hash],
            ));
            history.add(&Root::from(value), &hash, &vote);
        }

        let expected = history
            .rai_votes()
            .into_iter()
            .map(|vote| vote.hash())
            .collect::<Vec<_>>();
        for _ in 0..10 {
            assert_eq!(
                history
                    .rai_votes()
                    .into_iter()
                    .map(|vote| vote.hash())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_election_lookup_uses_root_index_and_preserves_phase_order() {
        use rsnano_types::{
            QualifiedRoot, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId, RaiVoteMetadata,
            RaiVotePhase,
        };

        let history = LocalVoteHistory::with_max_cache(256);
        let key = PrivateKey::new();
        let root = Root::from(1);
        let other_root = Root::from(2);
        let election_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(3),
            root: QualifiedRoot::new(root, BlockHash::from(10)),
        });
        let other_election_id = RaiElectionId::Slot(RaiSlotId {
            epoch: RaiEpoch::new(3),
            root: QualifiedRoot::new(root, BlockHash::from(20)),
        });
        let make_vote = |hash, phase, election_id| {
            Arc::new(Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(16),
                0,
                hash,
                RaiVoteMetadata {
                    election_id,
                    phase,
                    epoch: RaiEpoch::new(3),
                    scope: RaiCommitteeScope::All,
                },
            ))
        };

        let notar = make_vote(
            BlockHash::from(11),
            RaiVotePhase::Notar,
            election_id.clone(),
        );
        let first = make_vote(
            BlockHash::from(11),
            RaiVotePhase::First,
            election_id.clone(),
        );
        let other_election = make_vote(BlockHash::from(12), RaiVotePhase::First, other_election_id);
        let other_root_vote = make_vote(
            BlockHash::from(13),
            RaiVotePhase::First,
            election_id.clone(),
        );
        history.add(&root, &BlockHash::from(11), &notar);
        history.add(&root, &BlockHash::from(11), &first);
        history.add(&root, &BlockHash::from(12), &other_election);
        history.add(&other_root, &BlockHash::from(13), &other_root_vote);

        let votes = history.rai_votes_for_election(&root, &election_id);
        assert_eq!(votes.len(), 2);
        assert!(Arc::ptr_eq(&votes[0], &first));
        assert!(Arc::ptr_eq(&votes[1], &notar));
    }
}
