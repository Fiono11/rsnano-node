use rsnano_ledger::{AnySet, LedgerSet};
use rsnano_types::{BlockHash, SavedBlock};

use crate::consensus::election::{AccountSlot, EpochLedger};

/// Attachment subset for owner recovery: a parent must be confirmed or a
/// maximum-depth tip retained by the latest checkpoint. Receive sources must
/// be confirmed. Full overlap eligibility and signed-evidence validation are
/// separate protocol requirements; this helper does not establish them.
pub(crate) fn dependencies_attachable(
    any: &dyn AnySet,
    block: &SavedBlock,
    checkpoint: Option<&EpochLedger>,
) -> bool {
    unattached_dependency(any, block, checkpoint).is_none()
}

/// Which dependency keeps a block from being proposed here, if any (see
/// `dependencies_attachable`)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unattached {
    /// The previous block is neither final nor a lock of the checkpoint
    Previous,
    /// The source of a receive is not final
    Link,
}

pub(crate) fn unattached_dependency(
    any: &dyn AnySet,
    block: &SavedBlock,
    checkpoint: Option<&EpochLedger>,
) -> Option<Unattached> {
    if checkpoint.is_some_and(|state| {
        state
            .retained_depth(block.account())
            .is_some_and(|depth| block.height() <= depth)
    }) {
        return Some(Unattached::Previous);
    }
    let dependencies = any.block_dependencies(block);
    let confirmed = any.confirmed();
    let final_or_locked = |hash: BlockHash| {
        confirmed.block_exists(&hash) || checkpoint.is_some_and(|state| is_locked(any, state, hash))
    };
    if !dependencies.previous().is_none_or(final_or_locked) {
        return Some(Unattached::Previous);
    }
    if !dependencies
        .link()
        .is_none_or(|link| confirmed.block_exists(&link))
    {
        return Some(Unattached::Link);
    }
    None
}

/// RAI, "new epoch-e voting starts at depth d_a(S_{e-1}) + 1": the first
/// block of an account's chain, from `hash` at `slot` upwards, at a position
/// the latest checkpoint does not keep as a lock. None while the owner has
/// not extended the lock yet.
pub(crate) fn first_unretained(
    any: &dyn AnySet,
    mut slot: AccountSlot,
    mut hash: BlockHash,
    checkpoint: Option<&EpochLedger>,
) -> Option<BlockHash> {
    while checkpoint.is_some_and(|state| state.retains(&slot)) {
        hash = any.block_successor(&hash)?;
        slot = AccountSlot::new(slot.account, slot.height + 1);
    }
    Some(hash)
}

fn is_locked(any: &dyn AnySet, state: &EpochLedger, hash: BlockHash) -> bool {
    any.get_block(&hash).is_some_and(|block| {
        state.retained_depth(block.account()) == Some(block.height())
            && state.is_locked(&AccountSlot::new(block.account(), block.height()), &hash)
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

    #[test]
    fn a_child_at_a_retained_depth_cannot_reopen_that_position() {
        let (ledger, parent, child, _) = locked_parent_fixture();
        let mut entries = lock_of(&parent).checkpoint_entries();
        entries.extend(lock_of(&child).checkpoint_entries());
        let checkpoint = EpochLedger::from_checkpoint_entries(&entries).unwrap();
        assert!(!attachable(&ledger, &child, Some(&checkpoint)));
        assert_eq!(
            checkpoint.locks().map(|(_, h)| h).collect::<Vec<_>>(),
            vec![child.hash()]
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

    #[test]
    fn an_unretained_block_is_proposed_as_it_is() {
        let (ledger, parent, _, _) = locked_parent_fixture();

        assert_eq!(
            first_unretained_of(&ledger, &parent, None),
            Some(parent.hash())
        );
    }

    #[test]
    fn a_retained_position_is_stepped_over_to_the_child() {
        let (ledger, parent, child, _) = locked_parent_fixture();

        assert_eq!(
            first_unretained_of(&ledger, &parent, Some(&lock_of(&parent))),
            Some(child.hash())
        );
    }

    #[test]
    fn a_retained_position_without_a_child_proposes_nothing() {
        let (ledger, _, child, _) = locked_parent_fixture();

        assert_eq!(
            first_unretained_of(&ledger, &child, Some(&lock_of(&child))),
            None
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
        EpochLedger::from_checkpoint_entries(&[rsnano_messages::CertifiedEntry {
            account: block.account(),
            height: block.height(),
            hash: block.hash(),
            previous: block.previous(),
            status: 2,
        }])
        .unwrap()
    }

    fn first_unretained_of(
        ledger: &Ledger,
        block: &SavedBlock,
        checkpoint: Option<&EpochLedger>,
    ) -> Option<BlockHash> {
        let slot = AccountSlot::new(block.account(), block.height());
        first_unretained(&ledger.any(), slot, block.hash(), checkpoint)
    }

    fn attachable(ledger: &Ledger, block: &SavedBlock, checkpoint: Option<&EpochLedger>) -> bool {
        dependencies_attachable(&ledger.any(), block, checkpoint)
    }
}
