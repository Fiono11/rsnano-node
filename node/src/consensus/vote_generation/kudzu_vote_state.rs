use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};

use rsnano_types::{BlockHash, PublicKey, QualifiedRoot, VoteKind};

/// Node-lifetime signing restrictions. RAI assumes continuous operation: this state
/// is shared by request/broadcast generators and survives election/cache eviction,
/// but is deliberately not persisted across process restarts.
#[derive(Default)]
pub(super) struct KudzuVoteState {
    roots: HashMap<(QualifiedRoot, PublicKey), RootVotes>,
}

#[derive(Default)]
struct RootVotes {
    first: Option<BlockHash>,
    final_hash: Option<BlockHash>,
    notarized: HashSet<BlockHash>,
    statements: HashSet<(u64, VoteKind, BlockHash)>,
}

impl KudzuVoteState {
    pub fn has_notarization(
        &self,
        root: &QualifiedRoot,
        rep: PublicKey,
        hash: BlockHash,
        epoch: u64,
    ) -> bool {
        self.roots
            .get(&(root.clone(), rep))
            .is_some_and(|s| s.statements.contains(&(epoch, VoteKind::Notarize, hash)))
    }

    /// A cross-notarization permanently rules out final voting for this root.
    /// Unknown roots remain eligible; certificate and ledger-lock checks still run
    /// in the generator before signing.
    pub fn can_finalize(&self, root: &QualifiedRoot, rep: PublicKey, hash: BlockHash) -> bool {
        self.roots.get(&(root.clone(), rep)).is_none_or(|state| {
            !state.final_hash.is_some_and(|h| h != hash)
                && !state.notarized.iter().any(|h| *h != hash)
        })
    }

    pub fn needs_second_look(&self, root: &QualifiedRoot, rep: PublicKey, hash: BlockHash) -> bool {
        self.roots
            .get(&(root.clone(), rep))
            .is_some_and(|s| s.first.is_some_and(|h| h != hash))
    }

    pub fn first_value(&self, root: &QualifiedRoot, rep: PublicKey) -> Option<BlockHash> {
        self.roots.get(&(root.clone(), rep))?.first
    }

    pub fn has_first(
        &self,
        root: &QualifiedRoot,
        rep: PublicKey,
        hash: BlockHash,
        epoch: u64,
    ) -> bool {
        self.roots
            .get(&(root.clone(), rep))
            .is_some_and(|s| s.statements.contains(&(epoch, VoteKind::First, hash)))
    }

    pub fn authorize(
        &mut self,
        root: &QualifiedRoot,
        rep: PublicKey,
        hash: BlockHash,
        epoch: u64,
        final_requested: bool,
        second_look: bool,
        notarized: bool,
        final_lock: Option<BlockHash>,
    ) -> Option<VoteKind> {
        if final_lock.is_some_and(|h| h != hash) {
            return None;
        }
        let state = self.roots.entry((root.clone(), rep)).or_default();
        if state.final_hash.is_some_and(|h| h != hash) {
            return None;
        }
        let kind = if final_requested {
            if !(notarized || state.statements.contains(&(epoch, VoteKind::Final, hash)))
                || state.notarized.iter().any(|h| *h != hash)
            {
                return None;
            }
            state.final_hash = Some(hash);
            VoteKind::Final
        } else if state.first.is_none() || state.first == Some(hash) {
            state.first = Some(hash);
            VoteKind::First
        } else {
            if !(second_look
                || state
                    .statements
                    .contains(&(epoch, VoteKind::Notarize, hash)))
                || !state
                    .statements
                    .contains(&(epoch, VoteKind::First, state.first.unwrap()))
            {
                return None;
            }
            VoteKind::Notarize
        };
        if kind != VoteKind::Final {
            if !state.notarized.contains(&hash) && state.notarized.len() >= 3 {
                return None;
            }
            state.notarized.insert(hash);
        }
        state.statements.insert((epoch, kind, hash));
        Some(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kudzu_first_value_is_not_separately_notarized() {
        let mut state = KudzuVoteState::default();
        let root = QualifiedRoot::new_test_instance();
        let rep = PublicKey::from(1);
        let a = BlockHash::from(10);
        let b = BlockHash::from(11);
        assert_eq!(
            state.authorize(&root, rep, a, 0, false, false, false, None),
            Some(VoteKind::First)
        );
        assert_eq!(
            state.authorize(&root, rep, a, 0, false, true, false, None),
            Some(VoteKind::First)
        );
        assert_eq!(
            state.authorize(&root, rep, b, 0, false, false, false, None),
            None
        );
        assert_eq!(
            state.authorize(&root, rep, a, 0, true, false, false, None),
            None
        );
        assert_eq!(
            state.authorize(&root, rep, b, 0, false, true, false, None),
            Some(VoteKind::Notarize)
        );
        assert_eq!(
            state.authorize(&root, rep, a, 0, true, false, true, None),
            None
        );
        assert_eq!(
            state.authorize(&root, rep, b, 0, true, false, true, None),
            None
        );
        // Recover an already authorized statement after the active election is gone.
        assert_eq!(
            state.authorize(&root, rep, b, 0, false, false, false, None),
            Some(VoteKind::Notarize)
        );
        // Epoch changes do not reset first-vote restrictions.
        assert_eq!(
            state.authorize(&root, rep, b, 1, false, true, false, None),
            None
        );
        assert_eq!(
            state.authorize(&root, rep, a, 1, false, false, false, None),
            Some(VoteKind::First)
        );
        assert_eq!(
            state.authorize(&root, rep, b, 1, false, true, false, None),
            Some(VoteKind::Notarize)
        );
    }

    #[test]
    fn cross_notarization_disables_final_work_but_preserves_notarization_recovery() {
        let mut state = KudzuVoteState::default();
        let root = QualifiedRoot::new_test_instance();
        let rep = PublicKey::from(1);
        let a = BlockHash::from(10);
        let b = BlockHash::from(11);
        assert!(state.can_finalize(&root, rep, a));
        state
            .authorize(&root, rep, a, 0, false, false, false, None)
            .unwrap();
        assert!(state.can_finalize(&root, rep, a));
        assert!(!state.can_finalize(&root, rep, b));
        state
            .authorize(&root, rep, b, 0, false, true, false, None)
            .unwrap();
        assert!(!state.can_finalize(&root, rep, a));
        assert!(!state.can_finalize(&root, rep, b));
        assert_eq!(
            state.authorize(&root, rep, b, 0, false, false, false, None),
            Some(VoteKind::Notarize)
        );
    }

    #[test]
    fn kudzu_final_replay_is_epoch_scoped_and_respects_legacy_lock() {
        let mut state = KudzuVoteState::default();
        let root = QualifiedRoot::new_test_instance();
        let rep = PublicKey::from(1);
        let a = BlockHash::from(10);
        let b = BlockHash::from(11);
        assert_eq!(
            state.authorize(&root, rep, a, 2, true, false, true, None),
            Some(VoteKind::Final)
        );
        // The reservation protects the gap before the legacy write commits.
        assert_eq!(
            state.authorize(&root, rep, b, 2, false, true, false, None),
            None
        );
        assert_eq!(
            state.authorize(&root, rep, a, 2, true, false, false, Some(a)),
            Some(VoteKind::Final)
        );
        assert_eq!(
            state.authorize(&root, rep, a, 1, true, false, false, Some(a)),
            None
        );
        assert_eq!(
            state.authorize(&root, rep, b, 1, false, false, false, Some(a)),
            None
        );
    }
}
