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

/// Kudzu: a replica that missed the votes of a terminated election obtains
/// them from the representatives themselves, which re-sign their statements
/// for exactly that election (Section 4.2)
#[cfg(feature = "rai_protocol")]
#[test]
fn kudzu_certificates_are_handed_to_a_replica_that_missed_the_votes() {
    use rsnano_node::{
        config::NodeFlags,
        consensus::{AggregatorRequest, ApplyVoteArgs, FilteredVote},
    };
    use rsnano_types::VoteKind;
    use rsnano_utils::stats::Direction;

    let mut system = System::new();
    // n = MAX: the genesis representative with 70% of the supply is a
    // notarization certificate (62%) but not a fast finalization (81%)
    let config = || NodeConfig {
        online_weight_minimum: Amount::MAX,
        ..System::default_config_without_backlog_scan()
    };
    let flags = NodeFlags {
        disable_rep_crawler: true,
        ..Default::default()
    };
    // node1 is the genesis representative and votes
    let node1 = system
        .build_node()
        .config(config())
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

    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key = PrivateKey::from(42);
    let send1 = lattice.genesis().send(&key, Amount::MAX / 10 * 3);
    node1.process(send1.clone());
    assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

    // A timeout certificate rules out explicit finalization, so node1's own
    // first vote terminates the election without finalizing it
    let timeout_vote: FilteredVote = ReceivedVote::new(
        Arc::new(Vote::new_of_kind(
            &DEV_GENESIS_KEY,
            VoteKind::Timeout,
            vec![send1.hash()],
        )),
        VoteDelivery::Direct,
        None,
    )
    .into();
    node1.aec.apply_vote(ApplyVoteArgs {
        vote: &timeout_vote,
        rep_weights: &node1.ledger.rep_weights.read(),
        quorum_snapshot: &node1.rep_tracker.quorum_snapshot(),
        now: node1.steady_clock.now(),
    });
    assert_timely2(|| {
        node1
            .aec
            .election_for_block(&send1.hash())
            .is_some_and(|e| e.certificates().is_notarized(&send1.hash()))
    });
    assert!(!node1.block_confirmed(&send1.hash()));

    // node2 joins afterwards, so it never saw node1's first vote broadcast
    let node2 = system.build_node().config(config()).flags(flags).finish();
    node2.process_active(send1.clone());
    assert_timely2(|| node2.is_active_root(&send1.qualified_root()));

    // node1 hands its re-signed statement for that election to node2 on request
    let channel_to_node2 = node1
        .network
        .read()
        .unwrap()
        .find_node_id(&node2.node_id.public_key().into())
        .unwrap()
        .clone();
    node1.request_aggregator.request(AggregatorRequest {
        channel: channel_to_node2,
        roots_hashes: vec![(send1.hash(), send1.root())],
        epoch: ConsensusEpoch::ZERO,
    });
    assert_timely2(|| {
        node1.get_stat(
            "request_aggregator_replies",
            "certificate_votes",
            Direction::In,
        ) >= 1
    });
    // (on the dev network node1's periodic re-broadcast may deliver the same
    // statement first; either way node2 ends up with the certificate)
    assert_timely2(|| {
        node2
            .aec
            .election_for_block(&send1.hash())
            .is_some_and(|e| e.certificates().is_notarized(&send1.hash()))
    });
    assert!(!node2.block_confirmed(&send1.hash()));
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

    // node2's ledger block is the fork; node1's block gets the certificate
    let other = PrivateKey::from(43);
    let mut fork_lattice = lattice.clone();
    let send1 = lattice.genesis().send(&other, Amount::MAX / 10);
    let fork1 = fork_lattice
        .genesis()
        .send(&other, Amount::MAX / 10 + Amount::raw(1));
    assert_eq!(send1.root(), fork1.root());
    assert_ne!(send1.hash(), fork1.hash());
    node1.process(send1.clone());
    node2.process(fork1.clone());
    assert_timely2(|| {
        node2
            .aec
            .election_for_block(&fork1.hash())
            .is_some_and(|e| e.kudzu_votes().rep(&key.public_key()).is_some())
    });
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

/// RAI: once a node has left an epoch, the epoch's close election runs on
/// the epoch's final state. The genesis representative is the only principal
/// representative: it leads round 0, its first vote proposes the state it
/// attests, and that vote fast finalizes the value.
#[cfg(feature = "rai_protocol")]
#[test]
fn epoch_close_election_finalizes_the_epoch_state() {
    use rsnano_node::consensus::ActiveElectionsConfig;

    let mut system = System::new();
    let config = || NodeConfig {
        online_weight_minimum: Amount::MAX,
        active_elections: ActiveElectionsConfig {
            epoch_terminated_elections: 1,
            ..Default::default()
        },
        ..System::default_config_without_backlog_scan()
    };
    let node1 = system.build_node().config(config()).finish();
    node1
        .wallets
        .insert_adhoc2(
            &node1.wallets.wallet_ids()[0],
            &DEV_GENESIS_KEY.raw_key(),
            true,
        )
        .unwrap();
    // node2 has no representative: it only collects the certificates
    let node2 = system.build_node().config(config()).finish();
    node1.aec.start_epochs();
    node2.aec.start_epochs();
    assert!(node1.aec.epoch_closes().is_empty());

    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let send1 = lattice
        .genesis()
        .send(&PrivateKey::from(42), Amount::raw(1));
    node1.process(send1.clone());
    node2.process(send1.clone());
    assert_timely2(|| node1.block_confirmed(&send1.hash()));

    // The decided election ends epoch 0; its close election finalizes the
    // state with the one finalized block
    assert_timely2(|| node1.aec.current_epoch() == ConsensusEpoch::new(1));
    assert_timely(Duration::from_secs(10), || {
        node1
            .aec
            .epoch_closes()
            .first()
            .is_some_and(|close| close.closed.is_some())
    });
    let close = node1.aec.epoch_closes().remove(0);
    assert_eq!(close.epoch, ConsensusEpoch::ZERO);
    assert!(close.ready);
    let state = node1.aec.epoch_state(ConsensusEpoch::ZERO);
    assert_eq!(state.finalized, 1);
    assert_eq!(close.value, Some(state.close_value(ConsensusEpoch::ZERO)));
    assert_eq!(close.closed, Some((0, close.value.unwrap())));
    assert_eq!(close.round, 0);
    // The current epoch is not closed
    assert_eq!(node1.aec.epoch_closes().len(), 1);

    // node2 attests the same state and holds the certificate node1's vote formed
    assert_timely(Duration::from_secs(10), || {
        node2
            .aec
            .epoch_closes()
            .first()
            .is_some_and(|close| close.closed.is_some())
    });
    let close2 = node2.aec.epoch_closes().remove(0);
    assert_eq!(close2.closed, close.closed);
    assert_eq!(close2.value, close.value);
}

/// RAI: the only discard: a block notarized in an epoch after the epoch was
/// closed is not in the value finalized, it is rolled back. This node has no
/// representative; the genesis representative's votes decide everything.
#[cfg(feature = "rai_protocol")]
#[test]
fn late_notarized_blocks_of_a_closed_epoch_are_rolled_back() {
    use rsnano_node::consensus::{ActiveElectionsConfig, ApplyVoteArgs, FilteredVote};
    use rsnano_types::VoteKind;

    let mut system = System::new();
    let config = NodeConfig {
        online_weight_minimum: Amount::MAX,
        active_elections: ActiveElectionsConfig {
            epoch_terminated_elections: 1,
            ..Default::default()
        },
        ..System::default_config_without_backlog_scan()
    };
    let node = system.build_node().config(config).finish();
    node.aec.start_epochs();
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let send1 = lattice
        .genesis()
        .send(&PrivateKey::from(42), Amount::raw(1));
    let send2 = lattice
        .genesis()
        .send(&PrivateKey::from(43), Amount::raw(1));
    let apply = |vote: Vote| {
        node.aec.apply_vote(ApplyVoteArgs {
            vote: &FilteredVote::from(ReceivedVote::new(
                Arc::new(vote),
                VoteDelivery::Direct,
                None,
            )),
            rep_weights: &node.ledger.rep_weights.read(),
            quorum_snapshot: &node.rep_tracker.quorum_snapshot(),
            now: node.steady_clock.now(),
        });
    };

    // send1 is finalized in epoch 0, which ends and is closed
    node.process_active(send1.clone());
    assert_timely2(|| node.is_active_root(&send1.qualified_root()));
    apply(Vote::new_final(&DEV_GENESIS_KEY, vec![send1.hash()]));
    assert_timely2(|| node.aec.current_epoch() == ConsensusEpoch::new(1));
    let value = node
        .aec
        .epoch_state(ConsensusEpoch::ZERO)
        .close_value(ConsensusEpoch::ZERO);
    apply(Vote::new_in_epoch(
        &DEV_GENESIS_KEY,
        VoteKind::First,
        ConsensusEpoch::close_round(ConsensusEpoch::ZERO, 0),
        vec![value],
    ));
    assert_timely2(|| node.aec.epoch_closes()[0].closed == Some((0, value)));

    // send2 arrives from the network afterwards, with a vote of epoch 0 from
    // a peer still in it: an instance of epoch 0 opened after the close
    node.process_active(send2.clone());
    assert_timely2(|| node.block(&send2.hash()).is_some());
    apply(Vote::new_in_epoch(
        &DEV_GENESIS_KEY,
        VoteKind::Notar,
        ConsensusEpoch::ZERO,
        vec![send2.hash()],
    ));
    node.vote_processor
        .vote_blocking(&FilteredVote::from(ReceivedVote::new(
            Arc::new(Vote::new_in_epoch(
                &DEV_GENESIS_KEY,
                VoteKind::First,
                ConsensusEpoch::ZERO,
                vec![send2.hash()],
            )),
            VoteDelivery::Direct,
            None,
        )));
    // Notarized late: rolled back and discarded, the epoch's value stays
    assert_timely2(|| node.block(&send2.hash()).is_none());
    assert!(!node.is_active_root(&send2.qualified_root()));
    assert_eq!(
        node.aec
            .epoch_state(ConsensusEpoch::ZERO)
            .close_value(ConsensusEpoch::ZERO),
        value
    );
    assert!(node.block(&send1.hash()).is_some());
}
