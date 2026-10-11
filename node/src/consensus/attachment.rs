use rsnano_ledger::{AnySet, LedgerSet};
use rsnano_types::{BlockHash, SavedBlock};

use crate::consensus::election::{AccountSlot, EpochLedger, RetainedKind};

/// Which dependency keeps a block from being proposed here
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unattached {
    /// The block sits at a position the latest checkpoint retains: a
    /// shorter retained branch can not reopen it
    Retained,
    /// The previous block is neither final nor a notarization-locked tip of
    /// the checkpoint. Reported only when the source of a receive is final,
    /// so that a complete current-epoch parent can stand in for it (RAI,
    /// "Subsequent children require complete epoch-e parents"). A tip the
    /// checkpoint holds under recovery records alone is reported here too:
    /// it is a complete parent only once re-notarized in the child's epoch.
    Previous,
    /// The source of a receive is not final
    Link,
    /// The block itself is not held in the ledger
    Block,
}

/// Attachment subset for owner recovery: a parent must be confirmed or a
/// maximum-depth tip the latest checkpoint holds under a notarization lock;
/// any other parent must be complete in the child's epoch, which the
/// container checks (`Unattached::Previous`). Receive sources must
/// be confirmed. Full overlap eligibility and signed-evidence validation are
/// separate protocol requirements; this helper does not establish them.
/// Returns which dependency is missing, if any.
pub(crate) fn unattached_dependency(
    any: &dyn AnySet,
    block: &SavedBlock,
    checkpoint: Option<&EpochLedger>,
) -> Option<Unattached> {
    let dependencies = any.block_dependencies(block);
    let confirmed = any.confirmed();
    // A receive needs proof that its send is final, whatever its parent
    if !dependencies
        .link()
        .is_none_or(|link| confirmed.block_exists(&link))
    {
        return Some(Unattached::Link);
    }
    let final_or_locked = |hash: BlockHash| {
        confirmed.block_exists(&hash) || checkpoint.is_some_and(|state| is_locked(any, state, hash))
    };
    if !dependencies.previous().is_none_or(final_or_locked) {
        return Some(Unattached::Previous);
    }
    // The block a lock names is continued, not reopened. A rival at a
    // retained depth is named as such, last: whether the records it bypasses
    // are discharged depends on the exclusion witnesses the container holds
    // (see `EpochLedger::admits`)
    if checkpoint.is_some_and(|state| {
        let slot = AccountSlot::new(block.account(), block.height());
        state
            .retained_depth(block.account())
            .is_some_and(|depth| block.height() <= depth)
            && !state.is_locked(&slot, &block.hash())
    }) {
        return Some(Unattached::Retained);
    }
    None
}

/// RAI, re-notarization rule: a maximum-depth tip stands in for a final
/// parent only under a notarization lock, which no rival can ever pass. A
/// tip held by recovery records alone may lose to a rival that becomes final
/// in the open epoch; a child finalizing it would then finalize both sides.
fn is_locked(any: &dyn AnySet, state: &EpochLedger, hash: BlockHash) -> bool {
    any.get_block(&hash).is_some_and(|block| {
        state.retained_depth(block.account()) == Some(block.height())
            && state.is_locked(&AccountSlot::new(block.account(), block.height()), &hash)
            && state.retained_kind(&hash) == RetainedKind::Notarized
    })
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use rsnano_ledger::{Ledger, test_helpers::UnsavedBlockLatticeBuilder};
    use rsnano_types::PrivateKey;

    #[test]
    fn a_child_of_an_unfinalized_parent_is_not_attachable() {
        let (ledger, _, child, _) = locked_parent_fixture();

        assert!(!attachable(&ledger, &child, None));
        assert!(!attachable(&ledger, &child, Some(&EpochLedger::new())));
    }

    #[test]
    fn a_child_of_a_checkpoint_lock_is_attachable() {
        let (ledger, parent, child, _) = locked_parent_fixture();

        assert!(attachable(&ledger, &child, Some(&lock_of(&parent))));
    }

    /// RAI, re-notarization rule: a recovery-only tip is a complete parent
    /// only once it is notarized in the child's epoch, which the container
    /// checks for `Previous`
    #[test]
    fn a_child_of_a_recovery_lock_needs_its_parent_complete_in_the_epoch() {
        let (ledger, parent, child, _) = locked_parent_fixture();
        let mut checkpoint = EpochLedger::new();
        checkpoint.retain_recovery_for_test(
            AccountSlot::new(parent.account(), parent.height()),
            parent.hash(),
            parent.previous(),
        );

        assert_eq!(
            unattached_dependency(&ledger.any(), &child, Some(&checkpoint)),
            Some(Unattached::Previous)
        );
        assert!(attachable(&ledger, &parent, Some(&checkpoint)));
    }

    #[test]
    fn a_rival_at_a_retained_depth_cannot_reopen_that_position() {
        let (ledger, parent, child, _) = locked_parent_fixture();
        let checkpoint = rival_lock_at(&parent, &child);
        assert!(!attachable(&ledger, &child, Some(&checkpoint)));
    }

    /// RAI, §6.2: the block the lock names may be continued. It
    /// still needs an admissible parent: here a lock below the tip is not
    /// one.
    #[test]
    fn the_locked_block_itself_is_not_refused_as_retained() {
        let (ledger, parent, child, _) = locked_parent_fixture();
        let checkpoint = locks_of(&[&parent, &child]);
        assert_eq!(
            unattached_dependency(&ledger.any(), &child, Some(&checkpoint)),
            Some(Unattached::Previous)
        );
        assert!(attachable(&ledger, &parent, Some(&checkpoint)));
    }

    /// A rival at a retained depth is named as such once its other
    /// dependencies hold: whether the records it bypasses are discharged is
    /// for the container to tell
    #[test]
    fn a_reopened_retained_position_is_named_as_such() {
        let (ledger, parent, child, _) = locked_parent_fixture();
        let checkpoint = rival_lock_at(&parent, &child);
        assert_eq!(
            unattached_dependency(&ledger.any(), &child, Some(&checkpoint)),
            Some(Unattached::Previous)
        );
        ledger.confirm(parent.hash());
        let mut rival_only = EpochLedger::new();
        rival_only.retain_for_test(
            AccountSlot::new(child.account(), child.height()),
            BlockHash::from(999),
            child.previous(),
        );
        assert_eq!(
            unattached_dependency(&ledger.any(), &child, Some(&rival_only)),
            Some(Unattached::Retained)
        );
    }

    /// A complete parent may stand in for a final one, a final send may
    /// not be skipped: an unfinalized source is reported before the parent
    #[test]
    fn a_receive_with_an_unfinalized_send_names_the_send_before_its_parent() {
        let ledger = Ledger::new_null();
        let mut lattice = UnsavedBlockLatticeBuilder::with_stub_work();
        let key = PrivateKey::from(1);
        let first = lattice.genesis().send(&key, 1);
        let second = lattice.genesis().send(&key, 1);
        let open = lattice.account(&key).receive(&first);
        let receive = lattice.account(&key).receive(&second);
        for block in [&first, &second, &open] {
            ledger.process_one(block).unwrap();
        }
        let receive = ledger.process_one(&receive).unwrap();

        assert_eq!(
            unattached_dependency(&ledger.any(), &receive, None),
            Some(Unattached::Link)
        );
    }

    #[test]
    fn a_receive_of_a_locked_send_is_not_attachable() {
        let (ledger, parent, _, receive) = locked_parent_fixture();

        assert!(!attachable(&ledger, &receive, Some(&lock_of(&parent))));
        assert_eq!(
            unattached_dependency(&ledger.any(), &receive, Some(&lock_of(&parent))),
            Some(Unattached::Link)
        );
    }

    #[test]
    fn an_unfinalized_parent_is_named_as_the_missing_dependency() {
        let (ledger, _, child, _) = locked_parent_fixture();

        assert_eq!(
            unattached_dependency(&ledger.any(), &child, None),
            Some(Unattached::Previous)
        );
    }

    /* Test helpers */

    /// Genesis sends to a new account (the parent, unconfirmed) and
    /// sends again on top of it (the child); the new account receives the
    /// parent
    fn locked_parent_fixture() -> (Ledger, SavedBlock, SavedBlock, SavedBlock) {
        let ledger = Ledger::new_null();
        let mut lattice = UnsavedBlockLatticeBuilder::with_stub_work();
        let key = PrivateKey::from(1);
        let parent = lattice.genesis().send(&key, 1);
        let child = lattice.genesis().send(&key, 1);
        let receive = lattice.account(&key).receive(&parent);
        let parent = ledger.process_one(&parent).unwrap();
        let child = ledger.process_one(&child).unwrap();
        let receive = ledger.process_one(&receive).unwrap();
        (ledger, parent, child, receive)
    }

    fn lock_of(block: &SavedBlock) -> EpochLedger {
        locks_of(&[block])
    }

    fn locks_of(blocks: &[&SavedBlock]) -> EpochLedger {
        let mut state = EpochLedger::new();
        for block in blocks {
            state.retain_for_test(
                AccountSlot::new(block.account(), block.height()),
                block.hash(),
                block.previous(),
            );
        }
        state
    }

    /// The parent locked, and another block than `block` locked at its
    /// position
    fn rival_lock_at(parent: &SavedBlock, block: &SavedBlock) -> EpochLedger {
        let mut state = lock_of(parent);
        state.retain_for_test(
            AccountSlot::new(block.account(), block.height()),
            BlockHash::from(999),
            block.previous(),
        );
        state
    }

    fn attachable(ledger: &Ledger, block: &SavedBlock, checkpoint: Option<&EpochLedger>) -> bool {
        unattached_dependency(&ledger.any(), block, checkpoint).is_none()
    }
}
