use std::{
    collections::{BTreeMap, HashMap, HashSet},
    mem::size_of,
    sync::{Arc, Mutex},
};

use rsnano_types::{BlockHash, NetworkType, PrivateKey, PublicKey, Root, Vote};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

pub struct LocalVoteHistory {
    data: Mutex<LocalVoteHistoryData>,
    max_cache: usize,
}

#[derive(Default)]
struct LocalVoteHistoryData {
    history: BTreeMap<usize, LocalVote>,
    #[cfg(not(feature = "rai_protocol"))]
    history_by_root: HashMap<Root, HashSet<usize>>,
    #[cfg(feature = "rai_protocol")]
    history_by_root: HashMap<(Root, u64), HashSet<usize>>,
    /// A local vote for an epoch record finalizes the value named for each slot.
    /// Keep that cross-protocol lock beside slot vote history so every slot vote
    /// generation path observes it for the lifetime of the process.
    #[cfg(feature = "rai_protocol")]
    record_locks: HashMap<(Root, u64, PublicKey), BlockHash>,
}

impl LocalVoteHistoryData {
    fn new() -> Self {
        Default::default()
    }
}

struct LocalVote {
    root: Root,
    #[cfg(feature = "rai_protocol")]
    epoch: u64,
    hash: BlockHash,
    vote: Arc<Vote>,
}

impl LocalVoteHistory {
    pub fn new(network: NetworkType) -> Self {
        Self::with_max_cache(Self::max_cache_for(network))
    }

    fn max_cache_for(network: NetworkType) -> usize {
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
        let mut data_lk = self.data.lock().unwrap();
        let data: &mut LocalVoteHistoryData = &mut data_lk;
        #[cfg(not(feature = "rai_protocol"))]
        clean(data, self.max_cache);

        let mut add_vote = true;
        let mut remove_root = false;
        let mut ids_to_delete = Vec::new();
        // Erase any vote that is not for this hash, or duplicate by account, and if new timestamp is higher
        #[cfg(not(feature = "rai_protocol"))]
        let history_key = *root;
        #[cfg(feature = "rai_protocol")]
        let history_key = (*root, vote.epoch());
        if let Some(ids) = data.history_by_root.get_mut(&history_key) {
            for &i in ids.iter() {
                let current = &data.history[&i];
                #[cfg(not(feature = "rai_protocol"))]
                let superseded = &current.hash != hash
                    || (vote.voter == current.vote.voter
                        && current.vote.timestamp() <= vote.timestamp());
                #[cfg(feature = "rai_protocol")]
                let superseded = vote.voter == current.vote.voter
                    && vote.vote_type() == current.vote.vote_type()
                    && (vote.vote_type() != rsnano_types::VoteType::NonFinal
                        || current.hash == *hash);
                if superseded {
                    ids_to_delete.push(i);
                } else if cfg!(not(feature = "rai_protocol"))
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
            data.history_by_root.remove(&history_key);
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
                    #[cfg(feature = "rai_protocol")]
                    epoch: vote.epoch(),
                    hash: hash.to_owned(),
                    vote: vote.clone(),
                },
            );
            data.history_by_root
                .entry(history_key)
                .or_default()
                .insert(id);
        }
    }

    /// Returns an existing final vote for `hash`, or creates it if the local
    /// representative is still allowed to finalize this epoch slot.
    ///
    /// Recovery paths must use the same Kudzu support lock as ordinary vote
    /// generation: a representative that timed out or notarized another value
    /// cannot later manufacture a final vote just because the block is present
    /// in its confirmed ledger.  The checks and insertion share one lock so a
    /// concurrent terminal vote cannot be recorded between them.
    #[cfg(feature = "rai_protocol")]
    pub fn get_or_create_final_vote(
        &self,
        root: &Root,
        epoch: u64,
        hash: BlockHash,
        key: &PrivateKey,
    ) -> Option<Arc<Vote>> {
        let voter = key.public_key();
        let mut data = self.data.lock().unwrap();
        let history_key = (*root, epoch);

        let entries: Vec<_> = data
            .history_by_root
            .get(&history_key)
            .into_iter()
            .flatten()
            .filter_map(|id| data.history.get(id))
            .filter(|entry| entry.vote.voter == voter)
            .collect();

        if let Some(existing) = entries.iter().find(|entry| {
            entry.hash == hash && entry.vote.vote_type() == rsnano_types::VoteType::Final
        }) {
            return Some(existing.vote.clone());
        }

        if data.record_locks.contains_key(&(*root, epoch, voter))
            || entries.iter().any(|entry| {
                entry.vote.vote_type() == rsnano_types::VoteType::Timeout
                    || (entry.vote.vote_type() != rsnano_types::VoteType::Timeout
                        && entry.hash != hash)
            })
        {
            return None;
        }

        let vote = Arc::new(Vote::new_rai(
            key,
            epoch,
            rsnano_types::VoteType::Final,
            vec![hash],
        ));
        let id = data
            .history
            .iter()
            .next_back()
            .map(|(id, _)| id + 1)
            .unwrap_or_default();
        data.history.insert(
            id,
            LocalVote {
                root: *root,
                epoch,
                hash,
                vote: vote.clone(),
            },
        );
        data.history_by_root
            .entry(history_key)
            .or_default()
            .insert(id);
        Some(vote)
    }

    pub fn erase_batch(&self, roots: impl IntoIterator<Item = Root>) {
        let mut guard = self.data.lock().unwrap();
        for root in roots {
            #[cfg(not(feature = "rai_protocol"))]
            let removed_sets: Vec<_> = guard.history_by_root.remove(&root).into_iter().collect();
            #[cfg(feature = "rai_protocol")]
            let removed_sets: Vec<_> = guard
                .history_by_root
                .extract_if(|(r, _), _| *r == root)
                .map(|(_, ids)| ids)
                .collect();
            for removed in removed_sets {
                for &id in &removed {
                    guard.history.remove(&id);
                }
            }
        }
    }

    pub fn erase(&self, root: &Root) {
        let mut data_lk = self.data.lock().unwrap();
        #[cfg(not(feature = "rai_protocol"))]
        let removed_sets: Vec<_> = data_lk.history_by_root.remove(root).into_iter().collect();
        #[cfg(feature = "rai_protocol")]
        let removed_sets: Vec<_> = data_lk
            .history_by_root
            .extract_if(|(r, _), _| r == root)
            .map(|(_, ids)| ids)
            .collect();
        for removed in removed_sets {
            for &id in &removed {
                data_lk.history.remove(&id);
            }
        }
    }

    pub fn votes(&self, root: &Root, hash: &BlockHash, is_final: bool) -> Vec<Arc<Vote>> {
        let data_lk = self.data.lock().unwrap();
        let mut result = Vec::new();
        #[cfg(not(feature = "rai_protocol"))]
        let id_sets: Vec<_> = data_lk.history_by_root.get(root).into_iter().collect();
        #[cfg(feature = "rai_protocol")]
        let id_sets: Vec<_> = data_lk
            .history_by_root
            .iter()
            .filter_map(|((r, _), ids)| (r == root).then_some(ids))
            .collect();
        for ids in id_sets {
            for &id in ids.iter() {
                let entry = &data_lk.history[&id];
                if &entry.hash == hash && (!is_final || entry.vote.is_final()) {
                    result.push(entry.vote.clone())
                }
            }
        }
        result
    }

    #[cfg(feature = "rai_protocol")]
    pub fn try_lock_record_values(
        &self,
        entries: &[(rsnano_types::QualifiedRoot, BlockHash)],
        voter: PublicKey,
    ) -> bool {
        let mut data = self.data.lock().unwrap();

        // Validate the complete record candidate before installing any lock.
        // A locally signed final slot vote is a permanent lock against putting a
        // conflicting value in a record. A timeout is deliberately not checked:
        // a decided record is allowed to finalize a validated block after a node
        // locally terminated the slot by timeout.
        for (root, hash) in entries {
            let key = (root.root, root.epoch, voter);
            if data
                .record_locks
                .get(&key)
                .is_some_and(|locked| locked != hash)
            {
                return false;
            }
            let conflicting_final = data
                .history_by_root
                .get(&(root.root, root.epoch))
                .into_iter()
                .flatten()
                .filter_map(|id| data.history.get(id))
                .any(|entry| {
                    entry.vote.voter == voter
                        && entry.vote.vote_type() == rsnano_types::VoteType::Final
                        && entry.hash != *hash
                });
            if conflicting_final {
                return false;
            }
        }

        for (root, hash) in entries {
            data.record_locks
                .insert((root.root, root.epoch, voter), *hash);
        }
        true
    }

    #[cfg(feature = "rai_protocol")]
    pub fn is_record_locked(&self, root: &Root, epoch: u64, voter: PublicKey) -> bool {
        self.data
            .lock()
            .unwrap()
            .record_locks
            .contains_key(&(*root, epoch, voter))
    }

    #[cfg(feature = "rai_protocol")]
    pub fn votes_for_epoch(
        &self,
        root: &Root,
        epoch: u64,
        hash: &BlockHash,
        vote_type: Option<rsnano_types::VoteType>,
    ) -> Vec<Arc<Vote>> {
        let data = self.data.lock().unwrap();
        data.history_by_root
            .get(&(*root, epoch))
            .into_iter()
            .flatten()
            .filter_map(|id| {
                let entry = &data.history[id];
                (&entry.hash == hash && vote_type.is_none_or(|kind| entry.vote.vote_type() == kind))
                    .then(|| entry.vote.clone())
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn all_votes_for_epoch(&self, root: &Root, epoch: u64) -> Vec<Arc<Vote>> {
        let data = self.data.lock().unwrap();
        data.history_by_root
            .get(&(*root, epoch))
            .into_iter()
            .flatten()
            .filter_map(|id| data.history.get(id).map(|entry| entry.vote.clone()))
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn vote_for_epoch(
        &self,
        root: &Root,
        epoch: u64,
        hash: &BlockHash,
        vote_type: rsnano_types::VoteType,
        voter: PublicKey,
    ) -> Option<Arc<Vote>> {
        let data = self.data.lock().unwrap();
        data.history_by_root
            .get(&(*root, epoch))?
            .iter()
            .filter_map(|id| data.history.get(id))
            .find(|entry| {
                entry.hash == *hash
                    && entry.vote.vote_type() == vote_type
                    && entry.vote.voter == voter
            })
            .map(|entry| entry.vote.clone())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn has_vote_type(
        &self,
        root: &Root,
        epoch: u64,
        vote_type: rsnano_types::VoteType,
        voter: PublicKey,
    ) -> bool {
        let data = self.data.lock().unwrap();
        data.history_by_root
            .get(&(*root, epoch))
            .into_iter()
            .flatten()
            .filter_map(|id| data.history.get(id))
            .any(|entry| entry.vote.voter == voter && entry.vote.vote_type() == vote_type)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn has_conflicting_terminal_vote(
        &self,
        root: &Root,
        epoch: u64,
        vote_type: rsnano_types::VoteType,
        voter: PublicKey,
    ) -> bool {
        let conflicting = match vote_type {
            rsnano_types::VoteType::Final => rsnano_types::VoteType::Timeout,
            rsnano_types::VoteType::Timeout => rsnano_types::VoteType::Final,
            _ => return false,
        };
        self.has_vote_type(root, epoch, conflicting, voter)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn non_timeout_notarization_count(
        &self,
        root: &Root,
        epoch: u64,
        voter: PublicKey,
    ) -> usize {
        let data = self.data.lock().unwrap();
        data.history_by_root
            .get(&(*root, epoch))
            .into_iter()
            .flatten()
            .filter_map(|id| data.history.get(id))
            .filter(|entry| {
                entry.vote.voter == voter
                    && entry.vote.vote_type() == rsnano_types::VoteType::NonFinal
            })
            .count()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn has_notarized(
        &self,
        root: &Root,
        epoch: u64,
        hash: &BlockHash,
        voter: PublicKey,
    ) -> bool {
        let data = self.data.lock().unwrap();
        data.history_by_root
            .get(&(*root, epoch))
            .into_iter()
            .flatten()
            .filter_map(|id| data.history.get(id))
            .any(|entry| {
                entry.hash == *hash
                    && entry.vote.voter == voter
                    && matches!(
                        entry.vote.vote_type(),
                        rsnano_types::VoteType::First | rsnano_types::VoteType::NonFinal
                    )
            })
    }

    #[cfg(feature = "rai_protocol")]
    pub fn can_second_look(
        &self,
        root: &Root,
        epoch: u64,
        hash: &BlockHash,
        voter: PublicKey,
    ) -> bool {
        let data = self.data.lock().unwrap();
        let votes = data
            .history_by_root
            .get(&(*root, epoch))
            .into_iter()
            .flatten()
            .filter_map(|id| data.history.get(id))
            .filter(|entry| entry.vote.voter == voter)
            .collect::<Vec<_>>();
        let first_is_different = votes.iter().any(|entry| {
            entry.vote.vote_type() == rsnano_types::VoteType::First && entry.hash != *hash
        });
        let already_notarized = votes.iter().any(|entry| {
            entry.hash == *hash
                && matches!(
                    entry.vote.vote_type(),
                    rsnano_types::VoteType::First | rsnano_types::VoteType::NonFinal
                )
        });
        first_is_different && !already_notarized
    }

    #[cfg(feature = "rai_protocol")]
    pub fn has_no_conflicting_notarization(
        &self,
        root: &Root,
        epoch: u64,
        hash: &BlockHash,
        voter: PublicKey,
    ) -> bool {
        let data = self.data.lock().unwrap();
        let notarized = data
            .history_by_root
            .get(&(*root, epoch))
            .into_iter()
            .flatten()
            .filter_map(|id| {
                let entry = &data.history[id];
                let vote = &entry.vote;
                (vote.voter == voter
                    && matches!(
                        vote.vote_type(),
                        rsnano_types::VoteType::First | rsnano_types::VoteType::NonFinal
                    ))
                .then_some(entry.hash)
            })
            .collect::<Vec<_>>();
        notarized.iter().all(|notarized| notarized == hash)
    }

    pub fn exists(&self, root: &Root) -> bool {
        let data_lk = self.data.lock().unwrap();
        #[cfg(not(feature = "rai_protocol"))]
        {
            data_lk.history_by_root.contains_key(root)
        }
        #[cfg(feature = "rai_protocol")]
        {
            data_lk.history_by_root.keys().any(|(r, _)| r == root)
        }
    }

    pub fn size(&self) -> usize {
        self.data.lock().unwrap().history.len()
    }
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
            #[cfg(not(feature = "rai_protocol"))]
            {
                (*id, vote.root)
            }
            #[cfg(feature = "rai_protocol")]
            {
                (*id, (vote.root, vote.epoch))
            }
        };
        data.history.remove(&id);
        let mut root_empty = false;
        if let Some(ids) = data.history_by_root.get_mut(&root) {
            ids.remove(&id);
            root_empty = ids.is_empty();
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
        let vote2 = Arc::new(Vote::new(&keys, UnixMillisTimestamp::ZERO, 0, Vec::new()));
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
    #[cfg(not(feature = "rai_protocol"))]
    fn basic2() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let vote1a = Arc::new(Vote::null());
        let vote1b = Arc::new(Vote::null());
        let keys1 = PrivateKey::new();
        let vote2 = Arc::new(Vote::new(&keys1, UnixMillisTimestamp::ZERO, 0, Vec::new()));
        let keys2 = PrivateKey::new();
        let vote3 = Arc::new(Vote::new(&keys2, UnixMillisTimestamp::ZERO, 0, Vec::new()));
        history.add(&root, &hash, &vote1a);
        history.add(&root, &hash, &vote1b);
        history.add(&root, &hash, &vote2);
        history.add(&root, &BlockHash::from(3), &vote3);
        assert_eq!(history.size(), 1);
        let votes = history.votes(&root, &BlockHash::from(3), false);
        assert_eq!(votes.len(), 1);
        assert!(Arc::ptr_eq(&votes[0], &vote3));
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn retains_multiple_second_look_notarizations() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let hash_a = BlockHash::from(2);
        let hash_b = BlockHash::from(3);
        let key = PrivateKey::from(1);
        let first = Arc::new(Vote::new_rai(
            &key,
            1,
            rsnano_types::VoteType::First,
            vec![hash_a],
        ));
        let second_a = Arc::new(Vote::new_rai(
            &key,
            1,
            rsnano_types::VoteType::NonFinal,
            vec![hash_a],
        ));
        let second_b = Arc::new(Vote::new_rai(
            &key,
            1,
            rsnano_types::VoteType::NonFinal,
            vec![hash_b],
        ));

        history.add(&root, &hash_a, &first);
        history.add(&root, &hash_a, &second_a);
        history.add(&root, &hash_b, &second_b);

        assert_eq!(history.size(), 3);
        assert!(!history.has_no_conflicting_notarization(&root, 1, &hash_a, key.public_key()));
        assert!(!history.has_no_conflicting_notarization(&root, 1, &hash_b, key.public_key()));
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn first_vote_suppresses_redundant_second_look() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let key = PrivateKey::from(1);
        let first = Arc::new(Vote::new_rai(
            &key,
            1,
            rsnano_types::VoteType::First,
            vec![hash],
        ));
        history.add(&root, &hash, &first);

        assert!(history.has_notarized(&root, 1, &hash, key.public_key()));
        assert!(history.has_no_conflicting_notarization(&root, 1, &hash, key.public_key()));
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn second_look_requires_a_different_first_vote() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let hash_a = BlockHash::from(2);
        let hash_b = BlockHash::from(3);
        let key = PrivateKey::from(1);

        assert!(!history.can_second_look(&root, 1, &hash_b, key.public_key()));

        let first = Arc::new(Vote::new_rai(
            &key,
            1,
            rsnano_types::VoteType::First,
            vec![hash_a],
        ));
        history.add(&root, &hash_a, &first);

        assert!(!history.can_second_look(&root, 1, &hash_a, key.public_key()));
        assert!(history.can_second_look(&root, 1, &hash_b, key.public_key()));

        let second = Arc::new(Vote::new_rai(
            &key,
            1,
            rsnano_types::VoteType::NonFinal,
            vec![hash_b],
        ));
        history.add(&root, &hash_b, &second);

        assert!(!history.can_second_look(&root, 1, &hash_b, key.public_key()));
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn missing_local_first_vote_is_not_a_conflict() {
        let history = LocalVoteHistory::with_max_cache(256);

        assert!(history.has_no_conflicting_notarization(
            &Root::from(1),
            1,
            &BlockHash::from(2),
            PrivateKey::from(1).public_key()
        ));
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn recovery_final_is_rejected_after_timeout() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let key = PrivateKey::from(1);
        let timeout = Arc::new(Vote::new_rai(
            &key,
            1,
            rsnano_types::VoteType::Timeout,
            vec![hash],
        ));
        history.add(&root, &hash, &timeout);

        assert!(
            history
                .get_or_create_final_vote(&root, 1, hash, &key)
                .is_none()
        );
        assert!(
            history
                .votes_for_epoch(&root, 1, &hash, Some(rsnano_types::VoteType::Final))
                .is_empty()
        );
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn recovery_final_is_rejected_after_conflicting_notarization() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let selected = BlockHash::from(2);
        let conflicting = BlockHash::from(3);
        let key = PrivateKey::from(1);
        let notarization = Arc::new(Vote::new_rai(
            &key,
            1,
            rsnano_types::VoteType::NonFinal,
            vec![conflicting],
        ));
        history.add(&root, &conflicting, &notarization);

        assert!(
            history
                .get_or_create_final_vote(&root, 1, selected, &key)
                .is_none()
        );
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn recovery_final_is_allowed_after_compatible_notarization() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let key = PrivateKey::from(1);
        let first = Arc::new(Vote::new_rai(
            &key,
            1,
            rsnano_types::VoteType::First,
            vec![hash],
        ));
        history.add(&root, &hash, &first);

        let final_vote = history
            .get_or_create_final_vote(&root, 1, hash, &key)
            .unwrap();

        assert_eq!(final_vote.vote_type(), rsnano_types::VoteType::Final);
        assert_eq!(final_vote.hashes, vec![hash]);
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn recovery_reuses_existing_final_vote() {
        let history = LocalVoteHistory::with_max_cache(256);
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let key = PrivateKey::from(1);

        let first = history
            .get_or_create_final_vote(&root, 1, hash, &key)
            .unwrap();
        let second = history
            .get_or_create_final_vote(&root, 1, hash, &key)
            .unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(history.size(), 1);
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn record_lock_rejects_conflicting_local_final_vote() {
        let history = LocalVoteHistory::with_max_cache(256);
        let key = PrivateKey::from(1);
        let root = Root::from(1);
        let finalized = BlockHash::from(2);
        let record_value = BlockHash::from(3);
        let final_vote = Arc::new(Vote::new_rai(
            &key,
            7,
            rsnano_types::VoteType::Final,
            vec![finalized],
        ));
        history.add(&root, &finalized, &final_vote);

        let entry = rsnano_types::QualifiedRoot::new(root, BlockHash::from(9)).with_epoch(7);
        assert!(!history.try_lock_record_values(&[(entry, record_value)], key.public_key()));
        assert!(!history.is_record_locked(&root, 7, key.public_key()));
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn record_lock_blocks_later_slot_votes_but_allows_local_timeout_exception() {
        let history = LocalVoteHistory::with_max_cache(256);
        let key = PrivateKey::from(1);
        let root = Root::from(1);
        let value = BlockHash::from(2);
        let timeout = Arc::new(Vote::new_rai(
            &key,
            7,
            rsnano_types::VoteType::Timeout,
            vec![value],
        ));
        history.add(&root, &value, &timeout);

        let entry = rsnano_types::QualifiedRoot::new(root, BlockHash::from(9)).with_epoch(7);
        assert!(history.try_lock_record_values(&[(entry, value)], key.public_key()));
        assert!(history.is_record_locked(&root, 7, key.public_key()));
    }
}
