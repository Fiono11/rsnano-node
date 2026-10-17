use std::collections::{HashSet, VecDeque};

use rsnano_types::{Account, BlockHash, Signature};

/// The fork blocks the ledger verified the signature of recently. A fork
/// sibling reaches a node many times - rebroadcasts, evidence replies to
/// every request for its root, forced copies - and every copy was checked
/// again before the fork was found: ~35,000 fork results per node for
/// ~4,800 fork blocks at 10% forks. A block's hash does not cover its
/// signature, so the signature is part of the key: a copy carrying any
/// other signature is checked.
#[derive(Default)]
pub(crate) struct VerifiedForks {
    order: VecDeque<Key>,
    keys: HashSet<Key>,
}

type Key = (Account, BlockHash, Signature);

impl VerifiedForks {
    /// Several epochs of forks at the benchmark rates; the oldest go first
    pub const CAPACITY: usize = 16 * 1024;

    pub fn contains(&self, account: &Account, hash: &BlockHash, signature: &Signature) -> bool {
        self.keys.contains(&(*account, *hash, signature.clone()))
    }

    pub fn insert(&mut self, account: Account, hash: BlockHash, signature: Signature) {
        let key = (account, hash, signature);
        if !self.keys.insert(key.clone()) {
            return;
        }
        self.order.push_back(key);
        if self.order.len() > Self::CAPACITY
            && let Some(oldest) = self.order.pop_front()
        {
            self.keys.remove(&oldest);
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.keys.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verified_fork_is_known_with_its_signature_only() {
        let mut forks = VerifiedForks::default();
        forks.insert(Account::from(1), BlockHash::from(2), signature(3));
        assert!(forks.contains(&Account::from(1), &BlockHash::from(2), &signature(3)));
        assert!(!forks.contains(&Account::from(1), &BlockHash::from(2), &signature(4)));
        assert!(!forks.contains(&Account::from(5), &BlockHash::from(2), &signature(3)));
    }

    #[test]
    fn the_oldest_fork_is_forgotten_beyond_the_capacity() {
        let mut forks = VerifiedForks::default();
        for i in 0..=VerifiedForks::CAPACITY as u64 {
            forks.insert(Account::from(1), BlockHash::from(i), signature(1));
        }
        assert_eq!(forks.len(), VerifiedForks::CAPACITY);
        assert!(!forks.contains(&Account::from(1), &BlockHash::from(0), &signature(1)));
        assert!(forks.contains(
            &Account::from(1),
            &BlockHash::from(VerifiedForks::CAPACITY as u64),
            &signature(1)
        ));
    }

    #[test]
    fn inserting_a_known_fork_again_keeps_one_entry() {
        let mut forks = VerifiedForks::default();
        forks.insert(Account::from(1), BlockHash::from(2), signature(3));
        forks.insert(Account::from(1), BlockHash::from(2), signature(3));
        assert_eq!(forks.len(), 1);
    }

    /* Test helpers */

    fn signature(i: u8) -> Signature {
        Signature::from_bytes([i; 64])
    }
}
