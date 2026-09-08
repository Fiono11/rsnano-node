#![cfg(feature = "rai_protocol")]
use rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder;
use rsnano_node::{
    config::NodeFlags,
    consensus::{
        ConfirmationSolicitor,
        election::{Election, ElectionBehavior},
    },
    representatives::PeeredRepInfo,
};
use rsnano_types::{Amount, DEV_GENESIS_KEY, Vote, VoteKind};
use std::{sync::Arc, time::Duration};
use test_helpers::System;

// A later phase cannot substitute for a missing notarization statement.
#[test]
fn final_before_first_requests_missing_vote_and_preserves_final_summary() {
    let mut system = System::new();
    let mut flags = NodeFlags::default();
    flags.disable_request_loop = true;
    flags.disable_rep_crawler = true;
    let peer = system.build_node().flags(flags.clone()).finish();
    let node = system.build_node().flags(flags).finish();
    let channel = node
        .network
        .read()
        .unwrap()
        .find_node_id(&peer.node_id.public_key().into())
        .unwrap()
        .clone();
    let mut solicitor = ConfirmationSolicitor::new(node.message_flooder.lock().unwrap().clone());
    solicitor.prepare(&[PeeredRepInfo {
        rep_key: DEV_GENESIS_KEY.public_key(),
        channel_id: channel.channel_id(),
        weight: Amount::nano(100_000),
    }]);
    let block = node.process(UnsavedBlockLatticeBuilder::new().genesis().send(123, 1));
    let hash = block.hash();
    let mut election = Election::new(
        block,
        ElectionBehavior::Priority,
        Duration::from_secs(1),
        node.steady_clock.now(),
    );
    assert!(solicitor.add(&election), "Initially requests missing vote");
    election
        .add_kudzu_vote(
            Arc::new(Vote::new_with_kind(
                &DEV_GENESIS_KEY,
                vec![hash],
                0,
                VoteKind::Final,
            )),
            hash,
            node.steady_clock.now(),
        )
        .unwrap();
    assert!(!election.has_quorum());
    assert!(
        election
            .kudzu_certificate(hash, VoteKind::Notarize)
            .is_none()
    );
    assert!(
        solicitor.add(&election),
        "Final-only still needs First/notarization"
    );
    election
        .add_kudzu_vote(
            Arc::new(Vote::new_with_kind(
                &DEV_GENESIS_KEY,
                vec![hash],
                0,
                VoteKind::First,
            )),
            hash,
            node.steady_clock.now(),
        )
        .unwrap();
    assert!(
        election
            .votes()
            .get(&DEV_GENESIS_KEY.public_key())
            .unwrap()
            .is_final_vote(),
        "Late First preserves Final summary"
    );
}

#[test]
fn queued_final_after_fast_confirmation_recovers_first() {
    use rsnano_node::consensus::election::VoteType;
    use test_helpers::assert_timely2;
    let mut system = System::new();
    let node = system.make_node();
    let wallet = node.wallets.wallet_ids()[0];
    node.wallets
        .insert_adhoc2(&wallet, &DEV_GENESIS_KEY.raw_key(), true)
        .unwrap();
    let block = UnsavedBlockLatticeBuilder::new().genesis().send(456, 1);
    node.process_and_confirm_multi(&[block.clone()]);
    node.vote_generators
        .generate_vote_in_epoch(&block.root(), &block.hash(), VoteType::Final, 0);
    assert_timely2(|| {
        node.history
            .votes(&block.root(), &block.hash(), false)
            .iter()
            .any(|v| v.kind() == VoteKind::First)
    });
    assert!(
        node.history
            .votes(&block.root(), &block.hash(), false)
            .iter()
            .all(|v| v.kind() != VoteKind::Final)
    );
}
