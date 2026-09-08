use rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder;
use rsnano_types::{DEV_GENESIS_KEY, VoteKind};
use test_helpers::{System, assert_timely2};

#[test]
fn kudzu_slow_confirmation_generates_first_then_final() {
    use rsnano_node::consensus::election::KudzuThresholds;
    use rsnano_types::Amount;
    let mut system = System::new();
    let mut config = System::default_config();
    config.online_weight_minimum = Amount::MAX;
    let voter = system.build_node().config(config).finish();
    let block = UnsavedBlockLatticeBuilder::new().genesis().send(100, 1);
    voter.process(block.clone());
    // Exactly a slow quorum, insufficient for an 81% fast certificate.
    voter.ledger.rep_weights.put(
        DEV_GENESIS_KEY.public_key(),
        KudzuThresholds::new(Amount::MAX).certificate,
    );
    let wallet = voter.wallets.wallet_ids()[0];
    voter
        .wallets
        .insert_adhoc2(&wallet, &DEV_GENESIS_KEY.raw_key(), true)
        .unwrap();
    assert_timely2(|| voter.block_confirmed(&block.hash()));
    let votes = voter.history.votes(&block.root(), &block.hash(), false);
    assert!(votes.iter().any(|v| v.kind() == VoteKind::First));
    assert!(votes.iter().any(|v| v.kind() == VoteKind::Final));
    assert!(votes.iter().all(|v| v.kind() != VoteKind::Notarize));
}

#[test]
fn kudzu_second_look_generates_only_a_notarization_for_the_other_fork() {
    use rsnano_node::consensus::ReceivedVote;
    use rsnano_types::{Amount, PrivateKey, Vote, VoteDelivery};
    use std::sync::Arc;
    let mut system = System::new();
    let mut config = System::default_config();
    config.online_weight_minimum = Amount::MAX;
    let voter = system.build_node().config(config).finish();
    let peer = PrivateKey::from(2);
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let fund_peer = lattice.genesis().send(&peer, Amount::MAX / 100 * 43);
    let open_peer = lattice.account(&peer).receive(&fund_peer);
    let reserve = lattice.genesis().send(999, Amount::MAX / 100 * 37);
    voter.process_and_confirm_multi(&[fund_peer, open_peer, reserve]);
    let mut fork_lattice = lattice.clone();
    let a = lattice.genesis().send(100, 1);
    let b = fork_lattice.genesis().send(200, 1);
    voter.process(a.clone());
    let wallet = voter.wallets.wallet_ids()[0];
    voter
        .wallets
        .insert_adhoc2(&wallet, &DEV_GENESIS_KEY.raw_key(), true)
        .unwrap();
    assert_timely2(|| {
        voter
            .history
            .votes(&a.root(), &a.hash(), false)
            .iter()
            .any(|v| v.kind() == VoteKind::First)
    });
    voter.process_active(b.clone());
    assert_timely2(|| voter.aec.election_for_block(&b.hash()).is_some());
    let vote = Vote::new_with_kind(&peer, vec![b.hash()], 0, VoteKind::First);
    voter
        .vote_processor
        .vote_blocking(&ReceivedVote::new(Arc::new(vote), VoteDelivery::Direct, None).into())
        .unwrap();
    assert_timely2(|| {
        voter
            .history
            .votes(&b.root(), &b.hash(), false)
            .iter()
            .any(|v| v.kind() == VoteKind::Notarize)
    });
    let votes = voter.history.votes(&b.root(), &b.hash(), false);
    assert!(votes.iter().all(|v| v.kind() == VoteKind::Notarize));
}

#[test]
fn kudzu_fast_confirmation_over_network() {
    let mut system = System::new();
    let voter = system.make_node();
    let observer = system.make_node();
    let wallet = voter.wallets.wallet_ids()[0];
    voter
        .wallets
        .insert_adhoc2(&wallet, &DEV_GENESIS_KEY.raw_key(), true)
        .unwrap();
    let block = UnsavedBlockLatticeBuilder::new().genesis().send(100, 1);
    // Both nodes have the block, but only one owns voting keys. This exercises
    // generation, the new confirm_ack encoding, receipt, and cementation.
    observer.process(block.clone());
    voter.process(block.clone());
    assert_timely2(|| {
        voter.block_confirmed(&block.hash()) && observer.block_confirmed(&block.hash())
    });
    let votes = voter.history.votes(&block.root(), &block.hash(), false);
    assert!(votes.iter().any(|v| v.kind() == VoteKind::First));
    assert!(votes.iter().all(|v| v.kind() != VoteKind::Notarize));
}
