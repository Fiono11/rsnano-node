use rsnano_messages::MessageType;
use rsnano_node::{Node, config::NodeConfig};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Amount, DEV_GENESIS_KEY};
use rsnano_utils::stats::{Direction, StatType};
use std::sync::Arc;
use std::time::Duration;
use test_helpers::{System, assert_always_eq, assert_timely_eq2, assert_timely2, setup_rep};

#[test]
fn ledger_snapshot_integration_test() {
    let mut system = System::new();
    let node1 = system
        .build_node()
        .config(voting_snapshot_config(Duration::ZERO))
        .finish();
    node1.insert_into_wallet(&DEV_GENESIS_KEY);

    let node2 = system
        .build_node()
        .config(voting_snapshot_config(Duration::ZERO))
        .finish();
    let amount_pr = Amount::nano(2_000_000);
    let rep2_key = setup_rep(&node2, amount_pr, &DEV_GENESIS_KEY);
    node2.insert_into_wallet(&rep2_key);

    assert_peered_principal_reps(&node1, 2);
    assert_peered_principal_reps(&node2, 2);

    node1.ledger_snapshots.start_ledger_snapshot();

    assert_message_received(&node1, MessageType::Preproposal, 1);
    assert_message_received(&node2, MessageType::Preproposal, 1);

    assert_message_received(&node1, MessageType::Proposal, 2);
    assert_message_received(&node2, MessageType::Proposal, 2);

    assert_message_received(&node1, MessageType::ProposalVote, 2);
    assert_message_received(&node2, MessageType::ProposalVote, 2);
}

#[test]
fn ledger_snapshot_timer_integration_test() {
    let mut system = System::new();
    let clock = Arc::new(SteadyClock::new_null());
    let epoch_duration = Duration::from_millis(250);

    let node1 = system
        .build_node()
        .steady_clock(clock.clone())
        .config(voting_snapshot_config(epoch_duration))
        .finish();
    node1.insert_into_wallet(&DEV_GENESIS_KEY);

    let node2 = system
        .build_node()
        .steady_clock(clock.clone())
        .config(voting_snapshot_config(epoch_duration))
        .finish();
    let amount_pr = Amount::nano(2_000_000);
    let rep2_key = setup_rep(&node2, amount_pr, &DEV_GENESIS_KEY);
    node2.insert_into_wallet(&rep2_key);

    assert_peered_principal_reps(&node1, 2);
    assert_peered_principal_reps(&node2, 2);

    // Freeze time first to show that the automatic path is timer-driven.
    assert_no_message_received_for(&node1, MessageType::Preproposal, Duration::from_millis(250));
    assert_no_message_received_for(&node2, MessageType::Preproposal, Duration::from_millis(250));

    // The snapshot ticker itself runs on a 1-second interval, so advance beyond
    // both the epoch duration and the ticker interval.
    clock.advance(Duration::from_secs(2));

    assert_message_received(&node1, MessageType::Preproposal, 1);
    assert_message_received(&node2, MessageType::Preproposal, 1);

    assert_message_received(&node1, MessageType::Proposal, 2);
    assert_message_received(&node2, MessageType::Proposal, 2);

    assert_message_received(&node1, MessageType::ProposalVote, 2);
    assert_message_received(&node2, MessageType::ProposalVote, 2);
}

// Helper functions:
// -----------------------------------------------------------------------------

fn voting_snapshot_config(epoch_duration: Duration) -> NodeConfig {
    let mut config = System::default_config();
    config.enable_voting = true;
    config.rai_epoch_duration = epoch_duration;
    config
}

fn assert_peered_principal_reps(node: &Node, expected_rep_count: usize) {
    assert_timely2(|| {
        node.online_reps
            .lock()
            .unwrap()
            .peered_principal_reps()
            .len()
            == expected_rep_count
    });
}

fn assert_message_received(node: &Node, message_type: MessageType, count: usize) {
    assert_timely_eq2(
        || {
            node.stats
                .count(StatType::Message, message_type.into(), Direction::In) as usize
        },
        count,
    );
}

fn assert_no_message_received_for(node: &Node, message_type: MessageType, duration: Duration) {
    assert_always_eq(
        duration,
        || {
            node.stats
                .count(StatType::Message, message_type.into(), Direction::In) as usize
        },
        0,
    );
}
