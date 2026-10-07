use std::{sync::Arc, time::Duration};

use rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder;
use rsnano_node::{
    Node,
    config::NodeConfig,
    consensus::{ReceivedVote, election::KudzuThresholds},
};
#[cfg(feature = "rai_protocol")]
use rsnano_types::ConsensusEpoch;
use rsnano_types::{Amount, DEV_GENESIS_KEY, PrivateKey, Vote, VoteDelivery};
use test_helpers::{System, assert_timely2};
#[cfg(feature = "rai_protocol")]
use test_helpers::{assert_timely, establish_tcp};

/// The weight a final vote needs to confirm a block: the legacy quorum, or the
/// Kudzu finalization certificate
fn confirmation_threshold(node: &Node) -> Amount {
    let quorum = node.rep_tracker.quorum_snapshot();
    if cfg!(feature = "rai_protocol") {
        KudzuThresholds::from_quorum(&quorum).certificate
    } else {
        quorum.quorum_delta
    }
}

// checks that block cannot be confirmed if there is no enough votes to reach quorum
#[test]
fn quorum_minimum_confirm_fail() {
    let mut system = System::new();
    let config = NodeConfig {
        online_weight_minimum: Amount::MAX,
        ..System::default_config_without_backlog_scan()
    };
    let node1 = system.build_node().config(config).finish();
    let wallet_id = node1.wallets.wallet_ids()[0];
    node1
        .wallets
        .insert_adhoc2(&wallet_id, &DEV_GENESIS_KEY.raw_key(), true)
        .unwrap();

    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key = PrivateKey::new();
    let send1 = lattice.genesis().send(
        &key,
        Amount::MAX - (confirmation_threshold(&node1) - Amount::raw(1)),
    );

    node1.process(send1.clone());
    assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

    let vote = ReceivedVote::new(
        Arc::new(Vote::new_final(&DEV_GENESIS_KEY, vec![send1.hash()])),
        VoteDelivery::Direct,
        None,
    );
    let _ = node1.vote_processor.vote_blocking(&vote.into());

    // Give the election a chance to confirm
    std::thread::sleep(Duration::from_secs(1));

    // It should not confirm because there should not be enough quorum
    assert_eq!(node1.block_confirmed(&send1.hash()), false);
}

// This test ensures blocks can be confirmed precisely at the quorum minimum
#[test]
fn quorum_minimum_confirm_success() {
    let mut system = System::new();
    let config = NodeConfig {
        online_weight_minimum: Amount::MAX,
        ..System::default_config_without_backlog_scan()
    };
    let node1 = system.build_node().config(config).finish();
    let wallet_id = node1.wallets.wallet_ids()[0];
    node1
        .wallets
        .insert_adhoc2(&wallet_id, &DEV_GENESIS_KEY.raw_key(), true)
        .unwrap();

    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key1 = PrivateKey::new();

    // Only minimum quorum remains
    let send1 = lattice
        .genesis()
        .send(&key1, Amount::MAX - confirmation_threshold(&node1));

    node1.process(send1.clone());
    assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

    let vote = ReceivedVote::new(
        Arc::new(Vote::new_final(&DEV_GENESIS_KEY, vec![send1.hash()])),
        VoteDelivery::Direct,
        None,
    );
    let _ = node1.vote_processor.vote_blocking(&vote.into());

    assert_timely2(|| node1.block_confirmed(&send1.hash()));
}

#[test]
fn quorum_minimum_flip_fail() {
    let mut system = System::new();
    let config = NodeConfig {
        online_weight_minimum: Amount::MAX,
        ..System::default_config_without_backlog_scan()
    };
    let node1 = system.build_node().config(config).finish();

    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key1 = PrivateKey::new();
    let send1 = lattice.genesis().send(
        &key1,
        Amount::MAX - (confirmation_threshold(&node1) - Amount::raw(1)),
    );

    let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
    let key2 = PrivateKey::new();
    let send2 = fork_lattice.genesis().send(
        &key2,
        Amount::MAX - (confirmation_threshold(&node1) - Amount::raw(1)),
    );

    // Process send1 and wait until its election appears
    node1.process_active(send1.clone());
    assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

    // Process send2 and wait until it is added to the existing election
    node1.process_active(send2.clone());
    assert_timely2(|| node1.is_active_hash(&send2.hash()));

    // Genesis generates a final vote for send2 but it should not be enough to reach quorum
    // due to the online_weight_minimum being so high
    let vote = ReceivedVote::new(
        Arc::new(Vote::new_final(&DEV_GENESIS_KEY, vec![send2.hash()])),
        VoteDelivery::Direct,
        None,
    );
    let _ = node1.vote_processor.vote_blocking(&vote.into());

    // Give the election some time before asserting it is not confirmed
    std::thread::sleep(Duration::from_secs(1));

    assert_eq!(node1.block_confirmed(&send2.hash()), false);
}

#[test]
fn quorum_minimum_flip_success() {
    let mut system = System::new();
    let config = NodeConfig {
        online_weight_minimum: Amount::MAX,
        ..System::default_config_without_backlog_scan()
    };
    let node1 = system.build_node().config(config).finish();

    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key1 = PrivateKey::new();
    let send1 = lattice
        .genesis()
        .send(&key1, Amount::MAX - confirmation_threshold(&node1));

    let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
    let key2 = PrivateKey::new();
    let send2 = fork_lattice
        .genesis()
        .send(&key2, Amount::MAX - confirmation_threshold(&node1));

    // Process send1 and wait until its election appears
    node1.process_active(send1.clone());
    assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

    // Process send2 and wait until it is added to the existing election
    node1.process_active(send2.clone());
    assert_timely2(|| node1.is_active_hash(&send2.hash()));

    // Genesis generates a final vote for send2
    let vote = ReceivedVote::new(
        Arc::new(Vote::new_final(&DEV_GENESIS_KEY, vec![send2.hash()])),
        VoteDelivery::Direct,
        None,
    );
    let _ = node1.vote_processor.vote_blocking(&vote.into());

    // Wait for the election to be confirmed
    assert_timely2(|| node1.block_confirmed(&send2.hash()));
}

/// Kudzu: a replica whose election missed the final votes of a block the
/// representatives have already cemented gets them on request (legacy final
/// vote generation for confirmed blocks)
#[cfg(feature = "rai_protocol")]
#[test]
fn kudzu_final_votes_for_a_cemented_block_are_handed_out_on_request() {
    use rsnano_node::config::NodeFlags;

    let mut system = System::new();
    let flags = NodeFlags {
        disable_rep_crawler: true,
        ..Default::default()
    };
    let node1 = system
        .build_node()
        .config(System::default_config_without_backlog_scan())
        .flags(flags.clone())
        .finish();
    node1
        .wallets
        .insert_adhoc2(
            &node1.wallets.wallet_ids()[0],
            &DEV_GENESIS_KEY.raw_key(),
            true,
        )
        .unwrap();

    let key = PrivateKey::from(42);
    let send1 = UnsavedBlockLatticeBuilder::new()
        .genesis()
        .send(&key, Amount::MAX / 10 * 3);
    node1.process(send1.clone());
    assert_timely2(|| node1.block_confirmed(&send1.hash()));

    // node2 joins afterwards, so it saw none of node1's votes
    let node2 = system
        .build_node()
        .config(NodeConfig {
            online_weight_minimum: Amount::MAX,
            ..System::default_config_without_backlog_scan()
        })
        .flags(flags)
        .finish();
    node2.process_active(send1.clone());
    // node2 solicits, node1 answers with its final vote for the cemented block
    assert_timely2(|| node2.block_confirmed(&send1.hash()));
}

/// Kudzu: statements are immutable. When the notarized fork replaces our ledger
/// block, our own first vote for the losing block stays in the election (legacy
/// withdraws its votes to vote again)
#[cfg(feature = "rai_protocol")]
#[test]
fn kudzu_own_votes_survive_a_winner_change() {
    use rsnano_node::config::NodeFlags;

    let mut system = System::new();
    // neither node re-publishes the other's block, so each keeps its own fork
    let flags = NodeFlags {
        disable_rep_crawler: true,
        disable_block_processor_republishing: true,
        ..Default::default()
    };
    // n = MAX: node1's genesis representative (70 %) makes a certificate, node2's
    // 30 % representative does not
    let config = || NodeConfig {
        online_weight_minimum: Amount::MAX,
        ..System::default_config_without_backlog_scan()
    };
    let node1 = system
        .build_node()
        .config(config())
        .flags(flags.clone())
        .finish();
    let node2 = system.build_node().config(config()).flags(flags).finish();
    node1
        .wallets
        .insert_adhoc2(
            &node1.wallets.wallet_ids()[0],
            &DEV_GENESIS_KEY.raw_key(),
            true,
        )
        .unwrap();
    let key = PrivateKey::from(42);
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let send0 = lattice.genesis().send(&key, Amount::MAX / 10 * 3);
    let open0 = lattice.account(&key).receive(&send0);
    node1.process(send0.clone());
    node1.process(open0.clone());
    assert_timely2(|| node2.block_confirmed(&open0.hash()));
    node2
        .wallets
        .insert_adhoc2(&node2.wallets.wallet_ids()[0], &key.raw_key(), true)
        .unwrap();
    // the key becomes a voting representative with the periodic wallet
    // representative computation (10 s)
    assert_timely(Duration::from_secs(20), || {
        let mut keys = Vec::new();
        node2.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        !keys.is_empty()
    });

    // Hold back the majority's final vote so the changed winner remains
    // observable. Inject its first vote only after node2 voted for its fork.
    node1
        .wallets
        .remove_key(
            &node1.wallets.wallet_ids()[0],
            &DEV_GENESIS_KEY.public_key(),
        )
        .unwrap();

    // node2's ledger block is the fork; the other block gets the certificate
    let other = PrivateKey::from(43);
    let mut fork_lattice = lattice.clone();
    // Keep the majority above the 62% certificate threshold after this send.
    let send1 = lattice.genesis().send(&other, Amount::MAX / 100);
    let fork1 = fork_lattice
        .genesis()
        .send(&other, Amount::MAX / 100 + Amount::raw(1));
    assert_eq!(send1.root(), fork1.root());
    assert_ne!(send1.hash(), fork1.hash());
    node2.process(fork1.clone());
    assert_timely2(|| {
        node2
            .aec
            .election_for_block(&fork1.hash())
            .is_some_and(|e| e.kudzu_votes().rep(&key.public_key()).is_some())
    });
    assert!(node2.aec.try_add_fork(&send1, Amount::ZERO));
    let vote = ReceivedVote::new(
        Arc::new(Vote::new_of_kind(
            &DEV_GENESIS_KEY,
            rsnano_types::VoteKind::First,
            vec![send1.hash()],
        )),
        VoteDelivery::Direct,
        None,
    );
    node2.vote_processor.vote_blocking(&vote.into()).unwrap();
    assert_timely2(|| {
        node2
            .aec
            .election_for_block(&send1.hash())
            .is_some_and(|e| e.winner().hash() == send1.hash())
    });

    let election = node2.aec.election_for_block(&send1.hash()).unwrap();
    let own = election.kudzu_votes().rep(&key.public_key()).unwrap();
    assert_eq!(
        own.first,
        Some(fork1.hash()),
        "own votes: {own:?}, candidates: {:?}, fork1 {}",
        election.candidate_blocks().keys().collect::<Vec<_>>(),
        fork1.hash()
    );
}

/// Kudzu: a request for a block we do not hold, for a root where our ledger
/// has a different successor, tells us that the requester lacks our fork
/// candidate; it is published to the requester so that it can take its
/// second look
#[cfg(feature = "rai_protocol")]
#[test]
fn kudzu_fork_candidate_is_handed_to_a_replica_that_holds_the_other_fork() {
    use rsnano_node::{config::NodeFlags, consensus::AggregatorRequest};
    use rsnano_utils::stats::Direction;

    let mut system = System::new();
    let config = || NodeConfig {
        online_weight_minimum: Amount::MAX,
        ..System::default_config_without_backlog_scan()
    };
    let flags = NodeFlags {
        disable_rep_crawler: true,
        ..Default::default()
    };
    let node1 = system
        .build_node()
        .config(config())
        .flags(flags.clone())
        .finish();
    // Each node gets its own fork before they are connected, so that neither
    // block can reach the other node first
    let node2 = system
        .build_node()
        .config(config())
        .flags(flags)
        .disconnected()
        .finish();

    let key = PrivateKey::from(42);
    let send1 = UnsavedBlockLatticeBuilder::new()
        .genesis()
        .send(&key, Amount::MAX / 10 * 3);
    let fork1 = UnsavedBlockLatticeBuilder::new()
        .genesis()
        .send(&key, Amount::MAX / 10 * 4);
    assert_eq!(send1.root(), fork1.root());
    node1.process(send1.clone());
    node2.process(fork1.clone());
    let channel_to_node2 = establish_tcp(&node1, &node2);
    assert_timely2(|| node1.is_active_root(&send1.qualified_root()));
    assert_timely2(|| node2.is_active_root(&fork1.qualified_root()));
    assert!(node1.block(&fork1.hash()).is_none());

    // node2 asks node1 about its own block; node1 does not hold it but has
    // send1 for the same root, so it answers with send1
    node1.request_aggregator.request(AggregatorRequest {
        channel: channel_to_node2,
        roots_hashes: vec![(fork1.hash(), fork1.root())],
        epoch: ConsensusEpoch::ZERO,
    });
    assert_timely2(|| {
        node1.get_stat(
            "request_aggregator_replies",
            "fork_candidate",
            Direction::In,
        ) >= 1
    });
    assert_timely2(|| {
        node2
            .aec
            .election_for_block(&send1.hash())
            .is_some_and(|e| e.candidate_blocks().contains_key(&fork1.hash()))
    });
}

/// RAI: an epoch closes end to end on one node. The genesis representative
/// is the whole genesis committee: at the timed boundary it signs its report,
/// derives the epoch value from it, proposes the value as the leader of the
/// close's round 0, and its own first vote finalizes the close, which
/// installs the decided state.
#[cfg(feature = "rai_protocol")]
#[test]
fn an_epoch_closes_on_the_report_of_its_committee() {
    use rsnano_node::consensus::{ActiveElectionsConfig, election::AccountFrontier};

    let mut system = System::new();
    let config = NodeConfig {
        active_elections: ActiveElectionsConfig {
            epoch_duration: Duration::from_secs(1),
            ..Default::default()
        },
        ..System::default_config_without_backlog_scan()
    };
    let node = system.build_node().config(config).finish();
    node.wallets
        .insert_adhoc2(
            &node.wallets.wallet_ids()[0],
            &DEV_GENESIS_KEY.raw_key(),
            true,
        )
        .unwrap();
    // What the epoch_start RPC does: the ledger as it stands is the
    // genesis committee, and the epochs start now
    let frontiers: Vec<AccountFrontier> = node
        .ledger
        .any()
        .iter_accounts()
        .map(|(account, info)| AccountFrontier {
            account,
            height: info.block_count,
            hash: info.head,
            representative: info.representative,
            balance: info.balance,
        })
        .collect();
    node.aec.set_genesis_committee(frontiers);
    node.aec.start_epochs();

    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let send1 = lattice
        .genesis()
        .send(&PrivateKey::from(42), Amount::raw(1));
    node.process_active(send1.clone());
    assert_timely2(|| node.block_confirmed(&send1.hash()));

    assert_timely(Duration::from_secs(10), || {
        node.aec
            .epoch_closes()
            .first()
            .is_some_and(|close| close.value.is_some())
    });
    let close = node.aec.epoch_closes().remove(0);
    assert_eq!(close.epoch, ConsensusEpoch::ZERO);
    assert!(close.ready);
    assert!(close.started);
    assert_eq!(close.round, 0);
    assert_eq!(close.closed.map(|(round, _)| round), Some(0));
    assert!(node.aec.current_epoch() >= ConsensusEpoch::new(1));
}
