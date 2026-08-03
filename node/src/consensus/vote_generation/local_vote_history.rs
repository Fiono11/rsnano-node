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
                        current.vote.metadata.phase == vote.metadata.phase
                    }
                    #[cfg(not(feature = "rai_protocol"))]
                    {
                        true
                    }
                };
                if &current.hash != hash
                    || (same_phase
                        && vote.voter == current.vote.voter
                        && current.vote.timestamp() <= vote.timestamp())
                {
                    ids_to_delete.push(i);
                } else if same_phase
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
        for entry in guard.history.values() {
            votes.insert((entry.vote.voter, entry.vote.hash()), entry.vote.clone());
        }
        let mut votes = votes.into_values().collect::<Vec<_>>();
        votes.sort_by_key(|vote| {
            let phase = match vote.metadata.phase {
                rsnano_types::RaiVotePhase::First => 0,
                rsnano_types::RaiVotePhase::Notar => 1,
                rsnano_types::RaiVotePhase::Final => 2,
            };
            // The ticker advances a cursor across this freshly built list.
            // HashMap iteration is randomized, so phase alone is not enough:
            // canonicalize within each phase to guarantee eventual coverage.
            (phase, vote.hash())
        });
        votes
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_votes(&self) -> Vec<Arc<Vote>> {
        self.rai_votes()
            .into_iter()
            .filter(|vote| vote.metadata.governing_hash.is_zero())
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_slot_votes(&self) -> Vec<Arc<Vote>> {
        self.rai_votes()
            .into_iter()
            .filter(|vote| !vote.metadata.governing_hash.is_zero())
            .collect()
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
}
