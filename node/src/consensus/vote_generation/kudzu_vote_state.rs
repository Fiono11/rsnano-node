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
    participation_epoch: Option<u64>,
    timeout_epochs: HashSet<u64>,
    final_hash: Option<BlockHash>,
    notarized: HashSet<BlockHash>,
    statements: HashSet<(u64, VoteKind, BlockHash)>,
}

impl KudzuVoteState {
    /// Recreate only this representative's previous timeout statement. A routing
    /// hash can differ between representatives; it carries no block weight.
    pub fn timeout_statement(
        &self,
        root: &QualifiedRoot,
        rep: PublicKey,
        epoch: u64,
    ) -> Option<(VoteKind, BlockHash)> {
        self.roots
            .get(&(root.clone(), rep))?
            .statements
            .iter()
            .find(|(e, kind, _)| {
                *e == epoch && matches!(kind, VoteKind::Timeout | VoteKind::FirstTimeout)
            })
            .map(|(_, kind, hash)| (*kind, *hash))
    }

    /// A later timeout notarizes only the timeout outcome; it never changes FIRST.
    pub fn authorize_timeout(
        &mut self,
        root: &QualifiedRoot,
        rep: PublicKey,
        hash: BlockHash,
        epoch: u64,
        eligible: bool,
        final_lock: Option<BlockHash>,
    ) -> Option<VoteKind> {
        if !eligible || final_lock.is_some() {
            return None;
        }
        let state = self.roots.get_mut(&(root.clone(), rep))?;
        if state.final_hash.is_some()
            || !state.statements.iter().any(|(e, kind, _)| {
                *e == epoch && matches!(kind, VoteKind::First | VoteKind::FirstTimeout)
            })
        {
            return None;
        }
        state.timeout_epochs.insert(epoch);
        state.statements.insert((epoch, VoteKind::Timeout, hash));
        Some(VoteKind::Timeout)
    }

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

    pub fn needs_second_look(
        &self,
        root: &QualifiedRoot,
        rep: PublicKey,
        hash: BlockHash,
        epoch: u64,
    ) -> bool {
        self.roots.get(&(root.clone(), rep)).is_some_and(|s| {
            s.first.is_some_and(|h| h != hash) || s.participation_epoch.is_some_and(|e| e != epoch)
        })
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
        let other_epoch = state.participation_epoch.is_some_and(|e| e != epoch);
        if other_epoch && !final_requested {
            if state
                .statements
                .contains(&(epoch, VoteKind::Notarize, hash))
            {
                return Some(VoteKind::Notarize);
            }
            if !state.timeout_epochs.contains(&epoch) || !second_look {
                state.timeout_epochs.insert(epoch);
                state
                    .statements
                    .insert((epoch, VoteKind::FirstTimeout, hash));
                return Some(VoteKind::FirstTimeout);
            }
            if !state.notarized.contains(&hash) && state.notarized.len() >= 3 {
                return None;
            }
            state.notarized.insert(hash);
            state.statements.insert((epoch, VoteKind::Notarize, hash));
            return Some(VoteKind::Notarize);
        }
        if final_requested && (other_epoch || state.timeout_epochs.contains(&epoch)) {
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
        state.participation_epoch.get_or_insert(epoch);
        state.statements.insert((epoch, kind, hash));
        Some(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_epoch_first_votes_terminate_with_timeout_certificates() {
        use crate::consensus::election::Election;
        use rsnano_nullable_clock::Timestamp;
        use rsnano_types::{Amount, Block, PrivateKey, SavedBlock, StateBlockArgs, Vote};
        use std::sync::Arc;

        let args = StateBlockArgs::new_test_instance();
        let a = SavedBlock::new_test_instance_with(args.clone().into());
        let b: Block = StateBlockArgs {
            representative: 999.into(),
            ..args
        }
        .into();
        let hashes = [a.hash(), b.hash()];
        let root = a.qualified_root();
        let mut elections: Vec<_> = (0..2)
            .map(|epoch| {
                let mut e = Election::new_test_instance_with(a.clone());
                e.epoch = epoch;
                e.try_add_fork(&b, Amount::ZERO);
                e
            })
            .collect();
        let keys: Vec<_> = (1..=6).map(PrivateKey::from).collect();
        let weights = keys
            .iter()
            .map(|key| (key.public_key(), Amount::raw(100)))
            .collect();
        let mut state = KudzuVoteState::default();
        // Observed six-PR fork: epoch 0 gets one First per candidate;
        // epoch 1 gets two per candidate. Each remaining voter times out.
        let origins = [0, 0, 1, 1, 1, 1];
        for (i, key) in keys.iter().enumerate() {
            let hash = hashes[i % 2];
            let epoch = origins[i];
            assert_eq!(
                state.authorize(
                    &root,
                    key.public_key(),
                    hash,
                    epoch,
                    false,
                    false,
                    false,
                    None
                ),
                Some(VoteKind::First)
            );
            elections[epoch as usize]
                .add_kudzu_vote(
                    Arc::new(Vote::new_with_kind(key, vec![hash], epoch, VoteKind::First)),
                    hash,
                    Timestamp::new_test_instance(),
                )
                .unwrap();
        }
        // Repeated requests with complete delivery cannot unlock second look.
        for _ in 0..3 {
            for e in &mut elections {
                e.update_kudzu_tallies(&weights, Amount::raw(600));
                for hash in hashes {
                    assert!(!e.can_notarize(&hash));
                }
                for (i, key) in keys.iter().enumerate() {
                    let hash = hashes[i % 2];
                    let kind = state
                        .authorize(
                            &root,
                            key.public_key(),
                            hash,
                            e.epoch,
                            false,
                            e.can_notarize(&hash),
                            false,
                            None,
                        )
                        .unwrap();
                    assert_eq!(
                        kind,
                        if e.epoch == origins[i] {
                            VoteKind::First
                        } else {
                            VoteKind::FirstTimeout
                        }
                    );
                    let _ = e.add_kudzu_vote(
                        Arc::new(Vote::new_with_kind(key, vec![hash], e.epoch, kind)),
                        hash,
                        Timestamp::new_test_instance(),
                    );
                }
                e.update_kudzu_tallies(&weights, Amount::raw(600));
                assert!(!e.has_quorum());
                assert!(!e.is_confirmed());
            }
        }
        for e in &mut elections {
            let eligible = e.should_timeout();
            for (i, key) in keys.iter().enumerate() {
                let hash = hashes[i % 2];
                if let Some(kind) =
                    state.authorize_timeout(&root, key.public_key(), hash, e.epoch, eligible, None)
                {
                    let _ = e.add_kudzu_vote(
                        Arc::new(Vote::new_with_kind(key, vec![hash], e.epoch, kind)),
                        hash,
                        Timestamp::new_test_instance(),
                    );
                }
            }
            e.update_kudzu_tallies(&weights, Amount::raw(600));
            assert!(e.is_timed_out());
            assert!(!e.is_confirmed());
        }
    }

    #[test]
    fn later_timeout_requires_first_and_blocks_final_but_preserves_first_value() {
        let mut state = KudzuVoteState::default();
        let root = QualifiedRoot::new_test_instance();
        let rep = PublicKey::from(1);
        let hash = BlockHash::from(10);
        assert_eq!(
            state.authorize_timeout(&root, rep, hash, 0, true, None),
            None
        );
        assert_eq!(
            state.authorize(&root, rep, hash, 0, false, false, false, None),
            Some(VoteKind::First)
        );
        assert_eq!(
            state.authorize_timeout(&root, rep, hash, 0, false, None),
            None
        );
        assert_eq!(
            state.authorize_timeout(&root, rep, hash, 0, true, Some(hash)),
            None
        );
        assert_eq!(
            state.authorize_timeout(&root, rep, hash, 0, true, None),
            Some(VoteKind::Timeout)
        );
        assert_eq!(state.first_value(&root, rep), Some(hash));
        assert!(state.has_first(&root, rep, hash, 0));
        assert_eq!(
            state.authorize(&root, rep, hash, 0, true, true, true, None),
            None
        );
        // The reverse ordering must also be safe while a final reservation is
        // still only in memory, before its durable ledger lock is written.
        let other_rep = PublicKey::from(2);
        assert_eq!(
            state.authorize(&root, other_rep, hash, 0, false, false, false, None),
            Some(VoteKind::First)
        );
        assert_eq!(
            state.authorize(&root, other_rep, hash, 0, true, true, true, None),
            Some(VoteKind::Final)
        );
        assert_eq!(
            state.authorize_timeout(&root, other_rep, hash, 0, true, None),
            None
        );
    }

    #[test]
    fn timeout_first_in_other_epoch_allows_second_look_but_not_finalization() {
        let mut state = KudzuVoteState::default();
        let root = QualifiedRoot::new_test_instance();
        let rep = PublicKey::from(1);
        let a = BlockHash::from(10);
        assert_eq!(
            state.authorize(&root, rep, a, 1, false, false, false, None),
            Some(VoteKind::First)
        );
        assert_eq!(
            state.authorize(&root, rep, a, 0, false, false, false, None),
            Some(VoteKind::FirstTimeout)
        );
        assert_eq!(
            state.authorize(&root, rep, a, 0, false, true, false, None),
            Some(VoteKind::Notarize)
        );
        assert_eq!(
            state.authorize(&root, rep, a, 0, true, true, true, None),
            None
        );
        assert_eq!(
            state.authorize(&root, rep, a, 1, false, false, false, None),
            Some(VoteKind::First)
        );
    }

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
            Some(VoteKind::FirstTimeout)
        );
        assert_eq!(
            state.authorize(&root, rep, a, 1, false, false, false, None),
            Some(VoteKind::FirstTimeout)
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

#[cfg(test)]
mod timeout_recovery_tests {
    use super::*;

    #[test]
    fn recovery_preserves_timeout_kind_epoch_and_representative() {
        let root = rsnano_types::SavedBlock::new_test_instance().qualified_root();
        let rep = PublicKey::from(1);
        let hash = BlockHash::from(2);
        let mut state = KudzuVoteState::default();
        assert_eq!(state.timeout_statement(&root, rep, 0), None);
        assert_eq!(
            state.authorize(&root, rep, hash, 0, false, false, false, None),
            Some(VoteKind::First)
        );
        assert_eq!(state.timeout_statement(&root, rep, 0), None);
        assert_eq!(
            state.authorize_timeout(&root, rep, hash, 0, true, None),
            Some(VoteKind::Timeout)
        );
        assert_eq!(
            state.timeout_statement(&root, rep, 0),
            Some((VoteKind::Timeout, hash))
        );
        assert_eq!(state.timeout_statement(&root, PublicKey::from(3), 0), None);
        assert_eq!(state.timeout_statement(&root, rep, 1), None);
        assert_eq!(
            state.authorize(&root, rep, hash, 1, false, false, false, None),
            Some(VoteKind::FirstTimeout)
        );
        assert_eq!(
            state.timeout_statement(&root, rep, 1),
            Some((VoteKind::FirstTimeout, hash))
        );
    }
}
