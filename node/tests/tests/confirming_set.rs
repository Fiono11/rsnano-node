use std::time::Duration;

use rsnano_ledger::{LedgerSet, test_helpers::UnsavedBlockLatticeBuilder};
use rsnano_types::{Amount, PrivateKey};
use rsnano_utils::stats::{DetailType, Direction, StatType};
use test_helpers::{System, assert_always_eq, assert_timely_eq2, assert_timely2, start_election};

#[cfg(feature = "rai_protocol")]
use {
    rsnano_ledger::{ConfirmedSet, DEV_GENESIS_ACCOUNT},
    rsnano_node::consensus::{ApplyVoteArgs, FilteredVote, ReceivedVote},
    rsnano_types::{
        Block, DEV_GENESIS_KEY, RaiVoteMetadata, UnixMillisTimestamp, Vote, VoteDelivery,
    },
    std::sync::Arc,
};

#[cfg(feature = "rai_protocol")]
fn certify_rai_tip(node: &rsnano_node::Node, tip: &Block) {
    start_election(node, &tip.hash());
    assert_timely2(|| node.is_active_hash(&tip.hash()));
    let vote: FilteredVote = ReceivedVote::new(
        Arc::new(Vote::new_rai(
            &DEV_GENESIS_KEY,
            UnixMillisTimestamp::new(1),
            0,
            vec![tip.hash()],
            RaiVoteMetadata::default(),
        )),
        VoteDelivery::Direct,
        None,
    )
    .into();

    let rep_weights = node.ledger.rep_weights.read();
    let result = node.aec.apply_vote(ApplyVoteArgs {
        vote: &vote,
        rep_weights: &rep_weights,
        quorum_snapshot: &node.rep_tracker.quorum_snapshot(),
        now: node.steady_clock.now(),
    });
    assert_eq!(result.get(&tip.hash()), Some(&Ok(())));
}

/// RAI only replaces the election certificate. Its certified tip is deliberately
/// handed to the legacy confirming set, whose dependency walk and atomic ledger
/// transaction differ from the specification's strictly account-local segment rule.
#[cfg(feature = "rai_protocol")]
#[test]
fn rai_certified_tip_uses_legacy_cementation() {
    let mut system = System::new();
    let mut config = System::default_config_without_backlog_scan();
    config.enable_voting = false;
    let node = system.build_node().config(config).finish();
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let destination = PrivateKey::new();
    let b1 = lattice.genesis().send(&destination, Amount::raw(1));
    let b2 = lattice.genesis().send(&destination, Amount::raw(1));
    let b3 = lattice.genesis().send(&destination, Amount::raw(1));
    node.process_multi(&[b1.clone(), b2.clone(), b3.clone()]);

    node.confirming_set.set_cooldown(true);
    certify_rai_tip(&node, &b3);
    assert_timely2(|| node.confirming_set.contains(&b3.hash()));

    // Certification itself performs no partial ledger writes.
    assert_eq!(
        node.ledger
            .confirmed()
            .get_conf_info(&DEV_GENESIS_ACCOUNT)
            .unwrap()
            .height,
        1
    );
    assert!(!node.block_confirmed(&b1.hash()));
    assert!(!node.block_confirmed(&b2.hash()));
    assert!(!node.block_confirmed(&b3.hash()));

    node.confirming_set.set_cooldown(false);
    assert_timely2(|| !node.confirming_set.contains(&b3.hash()));
    assert!(node.blocks_confirmed(&[b1.clone(), b2.clone(), b3.clone()]));

    let confirmation_height = node
        .ledger
        .confirmed()
        .get_conf_info(&DEV_GENESIS_ACCOUNT)
        .unwrap();
    assert_eq!(confirmation_height.height, 4);
    assert_eq!(confirmation_height.frontier, b3.hash());
    assert_timely_eq2(
        || {
            node.stats.count(
                StatType::ConfirmationHeight,
                DetailType::BlocksConfirmed,
                Direction::In,
            )
        },
        3,
    );
    assert_timely_eq2(|| node.recently_cemented.lock().unwrap().len(), 3);
    assert_timely_eq2(
        || node.stats().get("confirmation_observer", "active_quorum"),
        1,
    );
}

#[cfg(feature = "rai_protocol")]
#[test]
fn rai_certified_receive_uses_legacy_dependency_closure() {
    let mut system = System::new();
    let mut config = System::default_config_without_backlog_scan();
    config.enable_voting = false;
    let node = system.build_node().config(config).finish();
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let account_b = PrivateKey::new();
    let account_a = PrivateKey::new();
    let fund_b = lattice.genesis().send(&account_b, Amount::raw(10));
    let open_b = lattice.account(&account_b).receive(&fund_b);
    node.process_and_confirm_multi(&[fund_b, open_b]);

    let send = lattice.account(&account_b).send(&account_a, Amount::raw(5));
    let receive = lattice.account(&account_a).receive(&send);
    node.process_multi(&[send.clone(), receive.clone()]);

    node.confirming_set.set_cooldown(true);
    certify_rai_tip(&node, &receive);
    assert_timely2(|| node.confirming_set.contains(&receive.hash()));
    assert!(!node.block_confirmed(&send.hash()));
    assert!(!node.block_confirmed(&receive.hash()));

    node.confirming_set.set_cooldown(false);
    assert_timely2(|| !node.confirming_set.contains(&receive.hash()));
    assert!(node.blocks_confirmed(&[send.clone(), receive.clone()]));

    let confirmed = node.ledger.confirmed();
    let source_height = confirmed.get_conf_info(&account_b.account()).unwrap();
    assert_eq!(source_height.height, 2);
    assert_eq!(source_height.frontier, send.hash());
    let receive_height = confirmed.get_conf_info(&account_a.account()).unwrap();
    assert_eq!(receive_height.height, 1);
    assert_eq!(receive_height.frontier, receive.hash());
    assert_timely_eq2(
        || node.stats().get("confirmation_observer", "active_quorum"),
        1,
    );
    assert_timely_eq2(
        || {
            node.stats.count(
                StatType::ConfirmationHeight,
                DetailType::BlocksConfirmed,
                Direction::In,
            )
        },
        4,
    );
    assert_timely_eq2(|| node.recently_cemented.lock().unwrap().len(), 2);
}

// The callback and confirmation history should only be updated after confirmation height is set (and not just after voting)
#[test]
fn confirmed_history() {
    let mut system = System::new();
    let mut config = System::default_config_without_backlog_scan();
    config.bootstrap.enable = false;
    let node = system.build_node().config(config).finish();

    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key1 = PrivateKey::new();
    let send1 = lattice.genesis().send(&key1, Amount::nano(1000));
    let send2 = lattice.genesis().send(&key1, Amount::nano(1000));

    node.process_multi(&[send1.clone(), send2.clone()]);
    assert_timely2(|| node.aec.is_active_hash(&send1.hash()));
    node.aec.cancel(&send1.qualified_root());
    start_election(&node, &send2.hash());
    {
        // Prevent the confirming set doing any writes
        node.confirming_set.set_cooldown(true);

        // Confirm send1
        node.force_confirm(&send2.hash());
        assert_timely2(|| !node.is_active_hash(&send2.hash()));
        assert_eq!(node.recently_cemented.lock().unwrap().len(), 0);
        assert_eq!(node.ledger.confirmed().block_exists(&send1.hash()), false);

        // Confirm that no inactive callbacks have been called when the
        // confirmation height processor has already iterated over it, waiting to write
        assert_always_eq(
            Duration::from_millis(50),
            || {
                node.stats.count(
                    StatType::ConfirmationObserver,
                    DetailType::InactiveConfHeight,
                    Direction::Out,
                )
            },
            0,
        );
        node.confirming_set.set_cooldown(false);
    }

    assert_timely2(|| node.ledger.confirmed().block_exists(&send1.hash()));

    assert_timely_eq2(|| node.aec.len(), 0);
    assert_timely_eq2(
        || node.stats().get("confirmation_observer", "active_quorum"),
        1,
    );

    // Each block that's confirmed is in the recently_cemented history
    assert_timely_eq2(|| node.recently_cemented.lock().unwrap().len(), 2);
    assert_eq!(node.aec.len(), 0);

    // Confirm the callback is not called under this circumstance
    assert_timely_eq2(
        || node.stats().get("confirmation_observer", "active_quorum"),
        1,
    );
    assert_timely_eq2(|| node.stats().get("confirmation_observer", "inactive"), 1);
    assert_timely_eq2(
        || {
            node.stats.count(
                StatType::ConfirmationHeight,
                DetailType::BlocksConfirmed,
                Direction::In,
            )
        },
        2,
    );
    assert_eq!(node.ledger.confirmed_count(), 3);
}

#[test]
fn dependent_election() {
    let mut system = System::new();
    let config = System::default_config_without_backlog_scan();
    let node = system.build_node().config(config).finish();

    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key1 = PrivateKey::new();
    let send1 = lattice.genesis().send(&key1, Amount::nano(1000));
    let send2 = lattice.genesis().send(&key1, Amount::nano(1000));
    let send3 = lattice.genesis().send(&key1, Amount::nano(1000));
    node.process_multi(&[send1.clone(), send2.clone(), send3.clone()]);

    assert_timely2(|| node.aec.is_active_hash(&send1.hash()));
    node.aec.cancel(&send1.qualified_root());
    assert_timely2(|| !node.aec.is_active_hash(&send1.hash()));

    // This election should be confirmed as active_conf_height
    start_election(&node, &send2.hash());
    // Start an election and confirm it
    start_election(&node, &send3.hash());
    node.force_confirm(&send3.hash());

    // Wait for blocks to be confirmed in ledger, callbacks will happen after
    assert_timely_eq2(
        || {
            node.stats.count(
                StatType::ConfirmationHeight,
                DetailType::BlocksConfirmed,
                Direction::In,
            )
        },
        3,
    );
    // Once the item added to the confirming set no longer exists, callbacks have completed
    assert_timely2(|| !node.confirming_set.contains(&send3.hash()));

    assert_timely_eq2(
        || node.stats().get("confirmation_observer", "active_quorum"),
        1,
    );
    assert_timely_eq2(
        || {
            node.stats()
                .get("confirmation_observer", "active_confirmation_height")
        },
        1,
    );
    assert_timely_eq2(|| node.stats().get("confirmation_observer", "inactive"), 1);
    assert_eq!(node.ledger.confirmed_count(), 4);
}
