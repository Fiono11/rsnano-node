use rsnano_messages::MessageType;
use rsnano_node::{Node, config::NodeConfig};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Amount, DEV_GENESIS_KEY};
use rsnano_utils::stats::{Direction, StatType};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use test_helpers::{System, assert_always_eq, assert_timely_eq2, assert_timely2, setup_rep};

fn ledger_snapshot_test_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[test]
fn ledger_snapshot_integration_test() {
    let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
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
    let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
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

fn snapshot_disabled_config() -> NodeConfig {
    let mut config = System::default_config();
    config.rai_epoch_duration = Duration::ZERO;
    config
}

fn snapshot_disabled_config_without_backlog_scan() -> NodeConfig {
    let mut config = System::default_config_without_backlog_scan();
    config.rai_epoch_duration = Duration::ZERO;
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

mod active_elections {
    use super::{snapshot_disabled_config, snapshot_disabled_config_without_backlog_scan};
    use std::{sync::Arc, thread::sleep, time::Duration};

    use rsnano_ledger::{
        AnySet, BlockError, DEV_GENESIS_PUB_KEY, LedgerSet,
        test_helpers::UnsavedBlockLatticeBuilder,
    };
    use rsnano_node::config::{NodeConfig, NodeFlags};
    use rsnano_nullable_tcp::get_available_port;
    use rsnano_types::{Account, Amount, DEV_GENESIS_KEY, PrivateKey, UnixMillisTimestamp, Vote, VoteSource};
    use rsnano_utils::stats::{DetailType, Direction, StatType};
    use test_helpers::{System, assert_always_eq, assert_timely_eq2, assert_timely2};

    #[test]
    fn confirm_election_by_request() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system
            .build_node()
            .config(NodeConfig {
                // Disable vote rebroadcasting to prevent node1 from actively sending votes to node2
                enable_vote_rebroadcast: false,
                ..snapshot_disabled_config()
            })
            .finish();
        let mut lattice = UnsavedBlockLatticeBuilder::new();

        let send1 = lattice.genesis().send(Account::from(1), 100);

        // Process send1 locally on node1
        node1.process(send1.clone());

        // Add rep key to node1
        let wallet_id = node1.wallets.wallet_ids()[0];
        node1
            .wallets
            .insert_adhoc2(&wallet_id, &DEV_GENESIS_KEY.raw_key(), true)
            .unwrap();

        // Ensure election on node1 is already confirmed before connecting with node2
        assert_timely2(|| node1.block_confirmed(&send1.hash()));

        // Wait for the election to be removed and give time for any in-flight vote broadcasts to settle
        assert_timely2(|| node1.aec.len() == 0);
        sleep(Duration::from_secs(1));

        // At this point node1 should not generate votes for send1 block unless it receives a request

        // Create a second node
        let flags = NodeFlags {
            disable_rep_crawler: true,
            ..Default::default()
        };
        let node2 = system
            .build_node()
            .config(snapshot_disabled_config())
            .flags(flags)
            .finish();

        // Process send1 block as live block on node2, this should start an election
        node2.process_active(send1.clone());

        // Ensure election is started on node2
        assert_timely2(|| node2.is_active_root(&send1.qualified_root()));

        // Ensure election on node2 did not get confirmed without us requesting votes
        sleep(Duration::from_secs(1));

        assert_eq!(
            node2
                .aec
                .election_for_root(&send1.qualified_root())
                .unwrap()
                .is_confirmed(),
            false
        );

        // Get random peer list from node2 -- so basically just node2
        let peers = node2.network.read().unwrap().sorted_channels();
        assert_eq!(peers.is_empty(), false);

        // Add representative (node1) to disabled rep crawler of node2
        node2.online_reps.lock().unwrap().vote_observed_directly(
            *DEV_GENESIS_PUB_KEY,
            peers[0].clone(),
            node2.steady_clock.now(),
        );

        // Expect a vote to come back
        // There needs to be at least one request to get the election confirmed,
        // Rep has this block already confirmed so should reply with final vote only

        // Expect election was confirmed
        assert_timely2(|| node1.block_confirmed(&send1.hash()));
        assert_timely2(|| node2.block_confirmed(&send1.hash()));
    }

    #[test]
    fn confirm_new() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system.build_node().config(snapshot_disabled_config()).finish();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let send = lattice.genesis().send(Account::from(1), 100);
        node1.process_active(send.clone());
        assert_timely_eq2(|| node1.aec.len(), 1);
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();
        // Add key to node2
        node2.insert_into_wallet(&DEV_GENESIS_KEY);
        // Let node2 know about the block
        assert_timely2(|| node2.block_exists(&send.hash()));
        // Wait confirmation
        assert_timely_eq2(|| node1.ledger.confirmed_count(), 2);
        assert_timely_eq2(|| node2.ledger.confirmed_count(), 2);
    }

    #[test]
    fn confirmation_consistency() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let config = snapshot_disabled_config_without_backlog_scan();
        let node = system.build_node().config(config).finish();
        let wallet_id = node.wallets.wallet_ids()[0];
        node.wallets
            .insert_adhoc2(&wallet_id, &DEV_GENESIS_KEY.raw_key(), true)
            .unwrap();

        for _ in 0..10 {
            let block = node
                .wallets
                .send(
                    wallet_id,
                    *rsnano_ledger::DEV_GENESIS_ACCOUNT,
                    Account::from(0),
                    node.config.receive_minimum,
                    0.into(),
                    true,
                    None,
                )
                .wait()
                .unwrap();

            assert_timely2(|| node.block_confirmed(&block.hash()));
            assert_timely2(|| node.aec.was_recently_confirmed(&block.hash()));
        }
    }

    #[test]
    fn conflicting_block_vote_existing_election() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let config = snapshot_disabled_config_without_backlog_scan();
        let flags = NodeFlags {
            disable_request_loop: true,
            ..Default::default()
        };
        let node = system.build_node().config(config).flags(flags).finish();

        let key = PrivateKey::new();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let send = lattice.genesis().send(&key, 100);

        let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
        let fork = fork_lattice.genesis().send(&key, 200);

        let vote_fork = Arc::new(Vote::new_final_for_test(
            &DEV_GENESIS_KEY,
            vec![fork.hash()],
        ));

        node.process_local(send.clone()).unwrap();
        assert_timely_eq2(|| node.aec.len(), 1);

        // Vote for conflicting block, but the block does not yet exist in the ledger
        node.vote_processor_queue
            .enqueue(vote_fork, None, VoteSource::Live, None);

        // Block now gets processed
        assert_eq!(node.process_local(fork.clone()), Err(BlockError::Fork));

        // Snapshot mode tears the election down and marks the root as forked
        assert_timely2(|| node.ledger.any().is_forked(&send.qualified_root()));
        assert_timely2(|| node.aec.election_for_root(&send.qualified_root()).is_none());
        assert_eq!(node.block_exists(&send.hash()), true);
        assert_eq!(node.block_exists(&fork.hash()), false);
        assert_eq!(node.block_confirmed(&send.hash()), false);
        assert_eq!(node.block_confirmed(&fork.hash()), false);
        assert_timely_eq2(
            || node.vote_cache.lock().unwrap().find(&fork.hash()).len(),
            1,
        );
    }

    #[test]
    fn fork_filter_cleanup() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system
            .build_node()
            .config(snapshot_disabled_config_without_backlog_scan())
            .finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key = PrivateKey::new();
        let send1 = lattice.genesis().send(&key, 1);
        let mut send_block_bytes = Vec::new();
        send1.serialize(&mut send_block_bytes).unwrap();

        node1.process_active(send1.clone());
        assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

        let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
        let fork = fork_lattice.genesis().send(&key, 2);
        node1.process_active(fork);

        assert_timely2(|| node1.ledger.any().is_forked(&send1.qualified_root()));
        assert_timely2(|| node1.aec.election_for_root(&send1.qualified_root()).is_none());

        // Fork cleanup should also let the original publish leave the duplicate filter.
        assert_timely2(|| !node1.network_filter.apply(&send_block_bytes).1);
    }

    #[test]
    fn fork_replacement_tally() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system
            .build_node()
            .config(snapshot_disabled_config_without_backlog_scan())
            .finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key = PrivateKey::new();
        let send1 = lattice.genesis().send(&key, Amount::nano(1000));
        let mut fork_lattice = lattice.clone();
        let send2 = fork_lattice.genesis().send(&key, Amount::nano(2000));

        node1.process_active(send1.clone());
        assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

        // A higher-tally fork is cached first...
        let vote = Arc::new(Vote::new_final_for_test(
            &DEV_GENESIS_KEY,
            vec![send2.hash()],
        ));
        node1
            .vote_processor_queue
            .enqueue(vote, None, VoteSource::Live, None);
        assert_timely_eq2(
            || node1.vote_cache.lock().unwrap().find(&send2.hash()).len(),
            1,
        );

        // ...but snapshot mode still refuses to replace the original candidate once the fork arrives.
        node1.process_active(send2.clone());
        assert_timely2(|| !node1.block_exists(&send2.hash()));
        assert_timely2(|| {
            node1
                .aec
                .election_for_root(&send1.qualified_root())
                .map(|e| !e.contains_block(&send2.hash()))
                .unwrap_or(true)
        });
        assert_eq!(node1.block_exists(&send1.hash()), true);
    }

    #[test]
    fn inactive_votes_cache_basic() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node = system.build_node().config(snapshot_disabled_config()).finish();
        let key = PrivateKey::new();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let send = lattice.genesis().send(&key, Amount::raw(100));
        let vote = Arc::new(Vote::new_final_for_test(
            &DEV_GENESIS_KEY,
            vec![send.hash()],
        ));
        node.vote_processor_queue
            .enqueue(vote, None, VoteSource::Live, None);
        assert_timely_eq2(|| node.vote_cache.lock().unwrap().size(), 1);
        node.process_active(send.clone());
        assert_timely2(|| node.block_confirmed(&send.hash()));
        assert_timely_eq2(|| node.get_stat("election_vote", "cache", Direction::In), 1);
    }

    #[test]
    fn inactive_votes_cache_election_start() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let mut config = snapshot_disabled_config_without_backlog_scan();
        config.enable_optimistic_scheduler = false;
        config.enable_priority_scheduler = false;
        let node = system.build_node().config(config).finish();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key1 = PrivateKey::new();
        let key2 = PrivateKey::new();

        // Enough weight to trigger election hinting but not enough to confirm block on its own
        let amount = ((node.online_reps.lock().unwrap().trended_or_minimum_weight() / 100)
            * node.config.hinted_scheduler.hinting_threshold_percent as u128)
            / 2
            + Amount::nano(1_000_000);

        let send1 = lattice.genesis().send(&key1, amount);
        let send2 = lattice.genesis().send(&key2, amount);
        let open1 = lattice.account(&key1).receive(&send1);
        let open2 = lattice.account(&key2).receive(&send2);

        node.process(send1.clone());
        let send2 = node.process(send2.clone());
        node.process(open1.clone());
        node.process(open2.clone());

        // These blocks will be processed later
        let send3 = lattice.genesis().send(Account::from(2), 1);
        let send4 = lattice.genesis().send(Account::from(3), 1);

        // Inactive votes
        let vote1 = Arc::new(Vote::new_for_test(
            &key1,
            UnixMillisTimestamp::ZERO,
            0,
            vec![open1.hash(), open2.hash(), send4.hash()],
        ));
        node.vote_processor_queue
            .enqueue(vote1, None, VoteSource::Live, None);
        assert_timely_eq2(|| node.vote_cache.lock().unwrap().size(), 3);
        assert_eq!(node.aec.len(), 0);
        assert_eq!(1, node.ledger.confirmed_count());

        // 2 votes are required to start election (dev network)
        let vote2 = Arc::new(Vote::new_for_test(
            &key2,
            UnixMillisTimestamp::ZERO,
            0,
            vec![open1.hash(), open2.hash(), send4.hash()],
        ));
        node.vote_processor_queue
            .enqueue(vote2, None, VoteSource::Live, None);
        // Only election for send1 should start, other blocks are missing dependencies and don't have enough final weight
        assert_timely_eq2(|| node.aec.len(), 1);
        assert!(node.is_active_hash(&send1.hash()));

        // Confirm elections with weight quorum
        let vote0 = Arc::new(Vote::new_final_for_test(
            &DEV_GENESIS_KEY,
            vec![open1.hash(), open2.hash(), send4.hash()],
        ));
        node.vote_processor_queue
            .enqueue(vote0, None, VoteSource::Live, None);
        assert_timely_eq2(|| node.aec.len(), 0);
        assert_timely_eq2(|| node.ledger.confirmed_count(), 5);
        // Confirmation on disk may lag behind cemented_count cache
        assert_timely2(|| {
            node.block_hashes_confirmed(&[send1.hash(), send2.hash(), open1.hash(), open2.hash()])
        });

        // A late block arrival also checks the inactive votes cache
        assert_eq!(node.aec.len(), 0);
        let send4_cache = node.vote_cache.lock().unwrap().find(&send4.hash());
        assert_eq!(3, send4_cache.len());
        node.process_active(send3.clone());
        // An election is started for send6 but does not
        assert_eq!(node.ledger.confirmed().block_exists(&send3.hash()), false);
        assert_eq!(node.confirming_set.contains(&send3.hash()), false);
        // send7 cannot be voted on but an election should be started from inactive votes
        node.process_active(send4);
        assert_timely_eq2(|| node.ledger.confirmed_count(), 7);
    }

    #[test]
    fn inactive_votes_cache_fork() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node = system.build_node().config(snapshot_disabled_config()).finish();
        let mut lattice1 = UnsavedBlockLatticeBuilder::new();
        let mut lattice2 = UnsavedBlockLatticeBuilder::new();
        let key = PrivateKey::new();

        let send1 = lattice1.genesis().send(&key, 100);
        let send2 = lattice2.genesis().send(&key, 200);

        let vote = Arc::new(Vote::new_final_for_test(
            &DEV_GENESIS_KEY,
            vec![send1.hash()],
        ));
        node.vote_processor_queue
            .enqueue(vote, None, VoteSource::Live, None);

        assert_timely_eq2(|| node.vote_cache.lock().unwrap().size(), 1);

        node.process_active(send2.clone());
        assert_timely2(|| node.is_active_root(&send1.qualified_root()));

        node.process_active(send1.clone());

        assert_timely2(|| node.ledger.any().is_forked(&send1.qualified_root()));
        assert_timely2(|| node.aec.election_for_root(&send1.qualified_root()).is_none());
        assert_eq!(node.block_confirmed(&send1.hash()), false);
        assert_eq!(node.block_confirmed(&send2.hash()), false);
        assert_eq!(node.block_exists(&send2.hash()), true);
        assert_always_eq(
            Duration::from_secs(1),
            || node.get_stat("election_vote", "cache", Direction::In),
            0,
        );
    }

    #[test]
    fn republish_winner() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let mut config = snapshot_disabled_config_without_backlog_scan();
        let node1 = system.build_node().config(config.clone()).finish();
        config.network.listening_port = get_available_port();
        let node2 = system.build_node().config(config).finish();
        let mut lattice = UnsavedBlockLatticeBuilder::new();

        let key = PrivateKey::new();
        let send1 = lattice.genesis().send(&key, Amount::nano(1000));

        node1.process_active(send1.clone());
        assert_timely2(|| node1.block_exists(&send1.hash()));

        assert_timely_eq2(
            || {
                node2
                    .stats
                    .count(StatType::Message, DetailType::Publish, Direction::In)
            },
            1,
        );

        let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
        let fork = fork_lattice.genesis().send(&key, Amount::nano(2000));
        node1.process_active(fork.clone());
        assert_timely2(|| node1.ledger.any().is_forked(&send1.qualified_root()));

        let vote = Arc::new(Vote::new_final_for_test(
            &DEV_GENESIS_KEY,
            vec![fork.hash()],
        ));

        node1
            .vote_processor_queue
            .enqueue(vote, None, VoteSource::Live, None);

        assert_eq!(node2.block_confirmed(&fork.hash()), false);
        assert_always_eq(
            Duration::from_secs(1),
            || {
                node2
                    .stats
                    .count(StatType::Message, DetailType::Publish, Direction::In)
            },
            1,
        );
    }
}

mod election {
    use super::snapshot_disabled_config_without_backlog_scan;
    use std::{sync::Arc, time::Duration};

    use rsnano_ledger::{AnySet, test_helpers::UnsavedBlockLatticeBuilder};
    use rsnano_node::{config::NodeConfig, consensus::ReceivedVote};
    use rsnano_types::{Amount, DEV_GENESIS_KEY, PrivateKey, Vote, VoteSource};
    use test_helpers::{System, assert_timely2};

    #[test]
    fn quorum_minimum_flip_fail() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let config = NodeConfig {
            online_weight_minimum: Amount::MAX,
            ..snapshot_disabled_config_without_backlog_scan()
        };
        let node1 = system.build_node().config(config).finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key1 = PrivateKey::new();
        let send1 = lattice.genesis().send(
            &key1,
            Amount::MAX - (node1.online_reps.lock().unwrap().quorum_delta() - Amount::raw(1)),
        );

        let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
        let key2 = PrivateKey::new();
        let send2 = fork_lattice.genesis().send(
            &key2,
            Amount::MAX - (node1.online_reps.lock().unwrap().quorum_delta() - Amount::raw(1)),
        );

        // Process send1 and wait until its election appears
        node1.process_active(send1.clone());
        assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

        // Process send2. Snapshot mode marks the root as forked instead of creating a multi-block election.
        node1.process_active(send2.clone());
        assert_timely2(|| node1.ledger.any().is_forked(&send1.qualified_root()));

        // Genesis generates a final vote for send2 but it should not be enough to reach quorum
        // due to the online_weight_minimum being so high
        let vote = ReceivedVote::new(
            Arc::new(Vote::new_final_for_test(
                &DEV_GENESIS_KEY,
                vec![send2.hash()],
            )),
            VoteSource::Live,
            None,
        );
        let _ = node1.vote_processor.vote_blocking(&vote.into());

        // Give the election some time before asserting it is not confirmed
        std::thread::sleep(Duration::from_secs(1));

        assert_eq!(node1.block_confirmed(&send2.hash()), false);
        assert_eq!(node1.aec.election_for_root(&send1.qualified_root()).is_none(), true);
    }

    #[test]
    fn quorum_minimum_flip_success() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let config = NodeConfig {
            online_weight_minimum: Amount::MAX,
            ..snapshot_disabled_config_without_backlog_scan()
        };
        let node1 = system.build_node().config(config).finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key1 = PrivateKey::new();
        let send1 = lattice.genesis().send(
            &key1,
            Amount::MAX - node1.online_reps.lock().unwrap().quorum_delta(),
        );

        let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
        let key2 = PrivateKey::new();
        let send2 = fork_lattice.genesis().send(
            &key2,
            Amount::MAX - node1.online_reps.lock().unwrap().quorum_delta(),
        );

        // Process send1 and wait until its election appears
        node1.process_active(send1.clone());
        assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

        // Process send2. Snapshot mode does not let a fork flip the winner even with quorum.
        node1.process_active(send2.clone());
        assert_timely2(|| node1.ledger.any().is_forked(&send1.qualified_root()));

        // Genesis generates a final vote for send2
        let vote = ReceivedVote::new(
            Arc::new(Vote::new_final_for_test(
                &DEV_GENESIS_KEY,
                vec![send2.hash()],
            )),
            VoteSource::Live,
            None,
        );
        let _ = node1.vote_processor.vote_blocking(&vote.into());

        std::thread::sleep(Duration::from_secs(1));

        assert_eq!(node1.block_confirmed(&send2.hash()), false);
        assert_eq!(node1.block_exists(&send1.hash()), true);
        assert_eq!(node1.block_exists(&send2.hash()), false);
    }
}

mod node {
    use super::{snapshot_disabled_config, snapshot_disabled_config_without_backlog_scan};
    use std::{sync::Arc, time::Duration};

    use rsnano_ledger::{
        AnySet, BlockSource, ConfirmedSet, DEV_GENESIS_ACCOUNT, DEV_GENESIS_PUB_KEY, LedgerSet,
        test_helpers::UnsavedBlockLatticeBuilder,
    };
    use rsnano_messages::{ConfirmAck, Message, Publish};
    use rsnano_network::{ChannelId, TrafficType};
    use rsnano_node::{
        block_processing::BlockContext,
        config::NodeFlags,
    };
    use rsnano_nullable_tcp::get_available_port;
    use rsnano_types::{
        Account, Amount, Block, DEV_GENESIS_KEY, PrivateKey, PublicKey, Signature, StateBlockArgs,
        UnixMillisTimestamp, Vote, VoteSource,
    };
    use rsnano_utils::stats::{DetailType, Direction, StatType};
    use test_helpers::{
        System, activate_hashes, assert_timely, assert_timely_eq, assert_timely_eq2,
        assert_timely_msg, assert_timely2, make_fake_channel, start_election,
    };

    #[test]
    fn block_confirm() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system.build_node().config(snapshot_disabled_config()).finish();
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();
        let wallet_id2 = node2.wallets.wallet_ids()[0];
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key = PrivateKey::new();

        let send1 = lattice.genesis().send(&key, Amount::nano(1000));
        let hash1 = send1.hash();

        assert_eq!(
            node1.block_processor_queue.push(BlockContext::new(
                send1.clone().into(),
                BlockSource::Live,
                ChannelId::LOOPBACK
            )),
            true
        );
        assert_eq!(
            node2.block_processor_queue.push(BlockContext::new(
                send1.clone().into(),
                BlockSource::Live,
                ChannelId::LOOPBACK,
            )),
            true
        );

        assert_timely2(|| {
            node1.ledger.any().block_exists(&hash1) && node2.ledger.any().block_exists(&hash1)
        });

        assert!(node1.ledger.any().block_exists(&hash1));
        assert!(node2.ledger.any().block_exists(&hash1));

        // Confirm send1 on node2 so it can vote for send2
        start_election(&node2, &hash1);

        assert_timely2(|| node2.is_active_root(&send1.qualified_root()));

        // Make node2 genesis representative so it can vote
        node2
            .wallets
            .insert_adhoc2(&wallet_id2, &DEV_GENESIS_KEY.raw_key(), true)
            .unwrap();

        assert_timely_eq(
            Duration::from_secs(10),
            || node1.recently_cemented.lock().unwrap().len(),
            1,
        );
    }

    #[test]
    fn bootstrap_fork_open() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let mut node_config = snapshot_disabled_config();
        // Reduce cooldown to speed up fork resolution
        node_config.bootstrap.bootstrap_queue.account_cooldown = Duration::from_millis(100);
        // Make sure we can process the full account number range
        node_config.bootstrap.frontier_scan.parallelism = 3;
        // Disable rate limiting to speed up the scan
        node_config.bootstrap.frontier_rate_limit = 0;
        // Disable automatic election activation
        node_config.backlog_scan.enabled = false;
        node_config.enable_priority_scheduler = false;
        node_config.enable_hinted_scheduler = false;
        node_config.enable_optimistic_scheduler = false;

        let node0 = system.build_node().config(node_config.clone()).finish();
        node_config.network.listening_port = get_available_port();
        let node1 = system.build_node().config(node_config).finish();
        node0.local_block_broadcaster.stop();
        node1.local_block_broadcaster.stop();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key0 = PrivateKey::new();

        let send0 = lattice.genesis().send(&key0, 500);
        let mut fork_lattice = lattice.clone();

        let open0 = lattice
            .account(&key0)
            .receive_and_change(&send0, PublicKey::from_bytes([1; 32]));

        let open1 = fork_lattice
            .account(&key0)
            .receive_and_change(&send0, PublicKey::from_bytes([2; 32]));

        // Both know about send0
        node0.process(send0.clone());
        node1.process(send0.clone());

        // Confirm send0 to allow starting and voting on the following blocks
        node0.confirm(send0.hash());
        node1.confirm(send0.hash());

        // They disagree about open0/open1
        node0.process(open0.clone());
        node1.process(open1.clone());
        assert_timely2(|| node0.block_exists(&open0.hash()));
        assert_timely2(|| node1.block_exists(&open1.hash()));

        // Simulate bootstrap delivering the peer's conflicting open block.
        node1.process_active(open0.clone().into());

        assert_timely2(|| node1.ledger.any().is_forked(&open1.qualified_root()));
        assert_eq!(node1.block_exists(&open1.hash()), true);
        assert_eq!(node1.block_exists(&open0.hash()), false);
    }

    #[test]
    fn confirm_back() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node = system.build_node().config(snapshot_disabled_config()).finish();
        let key = PrivateKey::new();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let send1 = lattice.genesis().send(&key, 1);
        let open = lattice.account(&key).receive(&send1);
        let send2 = lattice.account(&key).send(&*DEV_GENESIS_KEY, 1);

        node.process(send1.clone());
        node.process(open.clone());
        node.process(send2.clone());

        start_election(&node, &send1.hash());
        start_election(&node, &open.hash());
        start_election(&node, &send2.hash());
        assert_eq!(node.aec.len(), 3);
        let vote = Arc::new(Vote::new_final_for_test(
            &DEV_GENESIS_KEY,
            vec![send2.hash()],
        ));

        node.vote_processor_queue
            .enqueue(vote, None, VoteSource::Live, None);

        assert_timely_eq2(|| node.aec.len(), 0);
    }

    #[test]
    fn dependency_graph() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node = system
            .build_node()
            .config(snapshot_disabled_config_without_backlog_scan())
            .finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key1 = PrivateKey::new();
        let key2 = PrivateKey::new();
        let key3 = PrivateKey::new();

        // Send to key1
        let gen_send1 = lattice.genesis().send(&key1, 1);

        // Receive from genesis
        let key1_open = lattice.account(&key1).receive(&gen_send1);
        // Send to genesis
        let key1_send1 = lattice.account(&key1).send(&*DEV_GENESIS_KEY, 1);
        // Receive from key1
        let gen_receive = lattice.genesis().receive(&key1_send1);
        // Send to key2
        let gen_send2 = lattice.genesis().send(&key2, 2);
        // Receive from genesis
        let key2_open = lattice.account(&key2).receive(&gen_send2);
        // Send to key3
        let key2_send1 = lattice.account(&key2).send(&key3, 1);
        // Receive from key2
        let key3_open = lattice.account(&key3).receive(&key2_send1);
        // Send to key1
        let key2_send2 = lattice.account(&key2).send_max(&key1);
        // Receive from key2
        let key1_receive = lattice.account(&key1).receive(&key2_send2);
        // Send to key3
        let key1_send2 = lattice.account(&key1).send_max(&key3);
        // Receive from key1
        let key3_receive = lattice.account(&key3).receive(&key1_send2);
        // Upgrade key3
        let key3_epoch = lattice.account(&key3).epoch1();

        for node in &system.nodes {
            node.process_multi(&[
                gen_send1.clone(),
                key1_open.clone(),
                key1_send1.clone(),
                gen_receive.clone(),
                gen_send2.clone(),
                key2_open.clone(),
                key2_send1.clone(),
                key3_open.clone(),
                key2_send2.clone(),
                key1_receive.clone(),
                key1_send2.clone(),
                key3_receive.clone(),
                key3_epoch.clone(),
            ]);
        }

        // Hash -> Ancestors
        let dependency_graph: std::collections::HashMap<_, _> = [
            (key1_open.hash(), vec![gen_send1.hash()]),
            (key1_send1.hash(), vec![key1_open.hash()]),
            (gen_receive.hash(), vec![gen_send1.hash(), key1_open.hash()]),
            (gen_send2.hash(), vec![gen_receive.hash()]),
            (key2_open.hash(), vec![gen_send2.hash()]),
            (key2_send1.hash(), vec![key2_open.hash()]),
            (key3_open.hash(), vec![key2_send1.hash()]),
            (key2_send2.hash(), vec![key2_send1.hash()]),
            (
                key1_receive.hash(),
                vec![key1_send1.hash(), key2_send2.hash()],
            ),
            (key1_send2.hash(), vec![key1_send1.hash()]),
            (
                key3_receive.hash(),
                vec![key3_open.hash(), key1_send2.hash()],
            ),
            (key3_epoch.hash(), vec![key3_receive.hash()]),
        ]
        .into();
        assert_eq!(node.ledger.block_count() - 2, dependency_graph.len() as u64);

        // Start an election for the first block of the dependency graph, and ensure all blocks are eventually confirmed
        node.insert_into_wallet(&DEV_GENESIS_KEY);
        start_election(&node, &gen_send1.hash());
        assert_timely(Duration::from_secs(30), || {
            // Not many blocks should be active simultaneously
            assert!(node.aec.len() < 6);

            // Ensure that active blocks have their ancestors confirmed
            let error = dependency_graph.iter().any(|entry| {
                if node.is_active_hash(entry.0) {
                    for ancestor in entry.1 {
                        if !node.block_confirmed(ancestor) {
                            return true;
                        }
                    }
                }
                false
            });
            assert!(!error);
            error || node.ledger.confirmed_count() == node.ledger.block_count()
        });
        assert_eq!(node.ledger.confirmed_count(), node.ledger.block_count());
        assert_timely2(|| node.aec.len() == 0);
    }

    #[test]
    fn dependency_graph_frontier() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system
            .build_node()
            .config(snapshot_disabled_config_without_backlog_scan())
            .finish();
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key1 = PrivateKey::new();
        let key2 = PrivateKey::new();
        let key3 = PrivateKey::new();

        // Send to key1
        let gen_send1 = lattice.genesis().send(&key1, 1);

        // Receive from genesis
        let key1_open = lattice.account(&key1).receive(&gen_send1);
        // Send to genesis
        let key1_send1 = lattice.account(&key1).send(&*DEV_GENESIS_KEY, 1);
        // Receive from key1
        let gen_receive = lattice.genesis().receive(&key1_send1);
        // Send to key2
        let gen_send2 = lattice.genesis().send(&key2, 2);
        // Receive from genesis
        let key2_open = lattice.account(&key2).receive(&gen_send2);
        // Send to key3
        let key2_send1 = lattice.account(&key2).send(&key3, 1);
        // Receive from key2
        let key3_open = lattice.account(&key3).receive(&key2_send1);
        // Send to key1
        let key2_send2 = lattice.account(&key2).send(&key1, 1);
        // Receive from key2
        let key1_receive = lattice.account(&key1).receive(&key2_send2);
        // Send to key3
        let key1_send2 = lattice.account(&key1).send_max(&key3);
        // Receive from key1
        let key3_receive = lattice.account(&key3).receive(&key1_send2);
        // Upgrade key3
        let key3_epoch = lattice.account(&key3).epoch1();

        for node in &system.nodes {
            node.process_multi(&[
                gen_send1.clone(),
                key1_open.clone(),
                key1_send1.clone(),
                gen_receive.clone(),
                gen_send2.clone(),
                key2_open.clone(),
                key2_send1.clone(),
                key3_open.clone(),
                key2_send2.clone(),
                key1_receive.clone(),
                key1_send2.clone(),
                key3_receive.clone(),
                key3_epoch.clone(),
            ]);
        }

        // node1 can vote, but only on the first block
        node1.insert_into_wallet(&DEV_GENESIS_KEY);
        assert_timely(Duration::from_secs(10), || {
            node2.is_active_root(&gen_send1.qualified_root())
        });
        start_election(&node1, &gen_send1.hash());
        assert_timely_eq(
            Duration::from_secs(15),
            || node1.ledger.confirmed_count(),
            node1.ledger.block_count(),
        );
        assert_timely_eq(
            Duration::from_secs(15),
            || node2.ledger.confirmed_count(),
            node2.ledger.block_count(),
        );
    }

    #[test]
    fn epoch_conflict_confirm() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let config0 = snapshot_disabled_config_without_backlog_scan();
        let node0 = system.build_node().config(config0).finish();

        let config1 = snapshot_disabled_config_without_backlog_scan();
        let node1 = system.build_node().config(config1).finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key = PrivateKey::new();

        let send = lattice.genesis().send(&key, 1);
        let open = lattice.account(&key).receive(&send);

        let change = lattice.account(&key).change(&key);
        let conflict_account = Account::from_bytes(*open.hash().as_bytes());
        let send2 = lattice.genesis().send(conflict_account, 1);
        let epoch_open = lattice.epoch_open(conflict_account);

        // Process initial blocks
        node0.process_multi(&[send.clone(), send2.clone(), open.clone()]);
        node1.process_multi(&[send.clone(), send2.clone(), open.clone()]);

        // Process conflicting blocks on nodes as blocks coming from live network
        node0.process_active(change.clone());
        node0.process_active(epoch_open.clone());
        node1.process_active(change.clone());
        node1.process_active(epoch_open.clone());

        // Ensure blocks were propagated to both nodes
        assert_timely2(|| node0.blocks_exist(&[change.clone(), epoch_open.clone()]));
        assert_timely2(|| node1.blocks_exist(&[change.clone(), epoch_open.clone()]));

        // Confirm initial blocks in node1 to allow generating votes later
        node1.confirm_multi(&[change.clone(), epoch_open.clone(), send2.clone()]);

        // Start elections on node0 for conflicting change and epoch_open blocks (those two blocks have the same root)
        activate_hashes(&node0, &[change.hash(), epoch_open.hash()]);
        assert_timely2(|| {
            node0.is_active_hash(&change.hash()) && node0.is_active_hash(&epoch_open.hash())
        });

        // Make node1 a representative so it can vote for both blocks
        node1.insert_into_wallet(&DEV_GENESIS_KEY);

        // Ensure both conflicting blocks were successfully processed and confirmed
        assert_timely2(|| node0.blocks_confirmed(&[change.clone(), epoch_open.clone()]));
    }

    #[test]
    fn fork_bootstrap_flip() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let config1 = snapshot_disabled_config_without_backlog_scan();

        let node1 = system.build_node().config(config1).finish();
        let wallet_id1 = node1.wallets.wallet_ids()[0];
        node1
            .wallets
            .insert_adhoc2(&wallet_id1, &DEV_GENESIS_KEY.raw_key(), true)
            .unwrap();

        let mut config2 = snapshot_disabled_config();
        // Reduce cooldown to speed up fork resolution
        config2.bootstrap.bootstrap_queue.account_cooldown = Duration::from_millis(100);
        let node2 = system.build_node().config(config2).disconnected().finish();
        node1
            .wallets
            .insert_adhoc2(&wallet_id1, &DEV_GENESIS_KEY.raw_key(), true)
            .unwrap();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let mut fork_lattice = lattice.clone();
        let key1 = PrivateKey::new();
        let send1 = lattice.genesis().legacy_send(&key1, Amount::raw(1_000_000));

        let key2 = PrivateKey::new();
        let send2 = fork_lattice
            .genesis()
            .legacy_send(&key2, Amount::raw(1_000_000));

        // Insert but don't rebroadcast, simulating settled blocks
        node1.process_local(send1.clone()).unwrap();
        node2.process_local(send2.clone()).unwrap();

        node1.confirm(send1.hash());
        assert_timely2(|| node1.block_exists(&send1.hash()));
        assert_timely2(|| node2.block_exists(&send2.hash()));

        // Snapshot mode keeps the existing bootstrap winner and just marks the root as forked
        node2.process_active(send1.clone());

        assert_timely2(|| node2.ledger.any().is_forked(&send1.qualified_root()));
        assert_eq!(node2.block_exists(&send2.hash()), true);
        assert_eq!(node2.block_exists(&send1.hash()), false);
    }

    #[test]
    fn fork_election_invalid_block_signature() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system.build_node().config(snapshot_disabled_config()).finish();

        // send1 and send2 are forks of each other
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
        let send1 = lattice
            .genesis()
            .send(&*DEV_GENESIS_KEY, Amount::nano(1000));
        let send2 = fork_lattice
            .genesis()
            .send(&*DEV_GENESIS_KEY, Amount::nano(2000));
        let mut send3 = send2.clone();
        send3.set_signature(Signature::new()); // Invalid signature

        let channel = make_fake_channel(&node1);
        node1.inbound_message_queue.put(
            Message::Publish(Publish::new_forward(send1.clone())),
            channel.clone(),
        );
        assert_timely2(|| node1.is_active_root(&send1.qualified_root()));

        node1.inbound_message_queue.put(
            Message::Publish(Publish::new_forward(send3)),
            channel.clone(),
        );
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(node1.ledger.any().is_forked(&send1.qualified_root()), false);

        node1.inbound_message_queue.put(
            Message::Publish(Publish::new_forward(send2.clone())),
            channel.clone(),
        );
        assert_timely2(|| node1.ledger.any().is_forked(&send1.qualified_root()));
        assert_eq!(node1.block_exists(&send1.hash()), true);
        assert_eq!(node1.block_exists(&send2.hash()), false);
        assert_eq!(node1.aec.election_for_root(&send1.qualified_root()).is_none(), true);
    }

    #[test]
    fn fork_keep() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system.build_node().config(snapshot_disabled_config()).finish();
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
        let key1 = PrivateKey::new();
        let key2 = PrivateKey::new();
        // send1 and send2 fork to different accounts
        let send1 = lattice.genesis().send(&key1, 100);
        let send2 = fork_lattice.genesis().send(&key2, 100);
        node1.process_active(send1.clone());
        node2.process_active(send1.clone());
        assert_timely_eq2(|| node1.aec.len(), 1);
        assert_timely_eq2(|| node2.aec.len(), 1);
        node1.insert_into_wallet(&DEV_GENESIS_KEY);
        // Fill node with forked blocks
        node1.process_active(send2.clone());
        node2.process_active(send2.clone());

        assert_timely2(|| node1.ledger.any().is_forked(&send1.qualified_root()));
        assert_timely2(|| node2.ledger.any().is_forked(&send1.qualified_root()));
        assert_eq!(node1.block_exists(&send1.hash()), true);
        assert_eq!(node2.block_exists(&send1.hash()), true);
        assert_eq!(node1.block_exists(&send2.hash()), false);
        assert_eq!(node2.block_exists(&send2.hash()), false);
        assert_eq!(node1.block_confirmed(&send1.hash()), false);
        assert_eq!(node2.block_confirmed(&send1.hash()), false);
    }

    #[test]
    fn fork_multi_flip() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let mut config = snapshot_disabled_config_without_backlog_scan();
        let mut flags = NodeFlags::default();
        flags.disable_block_processor_republishing = true;
        let node1 = system
            .build_node()
            .config(config.clone())
            .flags(flags.clone())
            .finish();
        node1.local_block_broadcaster.stop();

        config.network.listening_port = get_available_port();
        // Reduce cooldown to speed up fork resolution
        config.bootstrap.bootstrap_queue.account_cooldown = Duration::from_millis(100);
        let node2 = system
            .build_node()
            .config(config)
            .flags(flags)
            .disconnected()
            .finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let mut fork_lattice = lattice.clone();
        let key1 = PrivateKey::new();
        let send1 = lattice.genesis().legacy_send(&key1, 100);

        let key2 = PrivateKey::new();
        let send2 = fork_lattice.genesis().legacy_send(&key2, 100);
        let send3 = fork_lattice.genesis().legacy_send(&key2, 0);

        node1.process(send1.clone());
        // Node2 has two blocks that will be rolled back by node1's vote
        node2.process(send2.clone());
        node2.process(send3.clone());

        // Deliver the alternative fork from node1 to node2.
        node2.process_active(send1.clone());

        assert_timely2(|| node2.ledger.any().is_forked(&send1.qualified_root()));
        assert_eq!(node2.ledger.any().block_exists(&send1.hash()), false);
        assert_eq!(node2.ledger.any().block_exists(&send2.hash()), true);
        assert_eq!(node2.ledger.any().block_exists(&send3.hash()), true);
    }

    #[test]
    fn fork_no_vote_quorum() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system.build_node().config(snapshot_disabled_config()).finish();
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();
        let node3 = system.build_node().config(snapshot_disabled_config()).finish();
        node1.local_block_broadcaster.stop();
        node2.local_block_broadcaster.stop();
        node3.local_block_broadcaster.stop();
        let wallet_id1 = node1.wallets.wallet_ids()[0];
        let wallet_id2 = node2.wallets.wallet_ids()[0];
        let wallet_id3 = node3.wallets.wallet_ids()[0];

        node1.insert_into_wallet(&DEV_GENESIS_KEY);

        let key4 = node1
            .wallets
            .deterministic_insert2(&wallet_id1, true)
            .unwrap();

        node1
            .wallets
            .send(
                wallet_id1,
                *DEV_GENESIS_ACCOUNT,
                key4.into(),
                Amount::MAX / 4,
                0.into(),
                true,
                None,
            )
            .wait()
            .unwrap();

        let key1 = node2
            .wallets
            .deterministic_insert2(&wallet_id2, true)
            .unwrap();

        node2
            .wallets
            .set_representative(wallet_id2, key1, false)
            .wait()
            .unwrap();

        let block = node1
            .wallets
            .send(
                wallet_id1,
                *DEV_GENESIS_ACCOUNT,
                key1.into(),
                node1.config.receive_minimum,
                0.into(),
                true,
                None,
            )
            .wait()
            .unwrap();

        assert_timely_msg(
            Duration::from_secs(30),
            || {
                node3.balance(&key1.into()) == node1.config.receive_minimum
                    && node2.balance(&key1.into()) == node1.config.receive_minimum
                    && node1.balance(&key1.into()) == node1.config.receive_minimum
            },
            "balances are wrong",
        );
        assert_eq!(node1.config.receive_minimum, node1.ledger.weight(&key1));
        assert_eq!(node1.config.receive_minimum, node2.ledger.weight(&key1));
        assert_eq!(node1.config.receive_minimum, node3.ledger.weight(&key1));

        let send1: Block = StateBlockArgs {
            key: &DEV_GENESIS_KEY,
            previous: block.hash(),
            representative: *DEV_GENESIS_PUB_KEY,
            balance: (Amount::MAX / 4) - (node1.config.receive_minimum * 2),
            link: Account::from(key1).into(),
            work: node1.work_generate_dev(block.hash()),
        }
        .into();

        node1.process(send1.clone());
        node2.process(send1.clone());
        node3.process(send1.clone());

        let key2 = node3
            .wallets
            .deterministic_insert2(&wallet_id3, true)
            .unwrap();

        let send2: Block = StateBlockArgs {
            key: &DEV_GENESIS_KEY,
            previous: block.hash(),
            representative: *DEV_GENESIS_PUB_KEY,
            balance: (Amount::MAX / 4) - (node1.config.receive_minimum * 2),
            link: Account::from(key2).into(),
            work: node1.work_generate_dev(block.hash()),
        }
        .into();

        let vote = Vote::new_for_test(
            &PrivateKey::new(),
            UnixMillisTimestamp::ZERO,
            0,
            vec![send2.hash()],
        );
        let confirm = Message::ConfirmAck(ConfirmAck::new_with_own_vote(vote));
        let channel = node2
            .network
            .read()
            .unwrap()
            .find_node_id(&node3.node_id())
            .unwrap()
            .clone();
        node2
            .message_sender
            .lock()
            .unwrap()
            .try_send(&channel, &confirm, TrafficType::Generic);

        assert_timely_msg(
            Duration::from_secs(10),
            || {
                node3
                    .stats
                    .count(StatType::Message, DetailType::ConfirmAck, Direction::In)
                    >= 3
            },
            "no confirm ack",
        );
        assert_eq!(node1.latest(&DEV_GENESIS_ACCOUNT), send1.hash());
        assert_eq!(node2.latest(&DEV_GENESIS_ACCOUNT), send1.hash());
        assert_eq!(node3.latest(&DEV_GENESIS_ACCOUNT), send1.hash());
    }

    #[test]
    fn fork_open() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node = system.build_node().config(snapshot_disabled_config()).finish();

        // create block send1, to send all the balance from genesis to key1
        // this is done to ensure that the open block(s) cannot be voted on and confirmed
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key1 = PrivateKey::new();
        let send1 = lattice.genesis().send(&key1, Amount::MAX);
        let mut fork_lattice = lattice.clone();

        node.process(send1.clone());
        node.confirm(send1.hash());

        // create the 1st open block to receive send1, which should be regarded as the winner just because it is first
        let open1 = lattice.account(&key1).receive_and_change(&send1, 1);
        let channel = make_fake_channel(&node);
        node.inbound_message_queue.put(
            Message::Publish(Publish::new_forward(open1.clone())),
            channel.clone(),
        );
        assert_timely_eq2(|| node.aec.len(), 1);

        // create 2nd open block, which is a fork of open1 block
        let open2 = fork_lattice.account(&key1).receive_and_change(&send1, 2);
        node.inbound_message_queue.put(
            Message::Publish(Publish::new_forward(open2.clone())),
            channel.clone(),
        );
        assert_timely2(|| node.ledger.any().is_forked(&open2.qualified_root()));
        assert_timely2(|| node.aec.election_for_root(&open2.qualified_root()).is_none());

        // Snapshot mode keeps the first saved block and tracks the root as forked.
        assert_timely2(|| node.block_exists(&open1.hash()));
        assert_eq!(node.block_exists(&open2.hash()), false);
    }

    #[test]
    fn fork_open_flip() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system.build_node().config(snapshot_disabled_config()).finish();
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let key1 = PrivateKey::new();
        let rep1 = PrivateKey::new();
        let rep2 = PrivateKey::new();

        // send 1 raw from genesis to key1 on both node1 and node2
        let send1 = lattice.genesis().legacy_send(&key1, 1);
        node1.process(send1.clone());

        let mut fork_lattice = lattice.clone();
        // We should be keeping this block
        let open1 = lattice.account(&key1).legacy_open_with_rep(&send1, &rep1);

        // create a fork of block open1, this block will lose the election
        let open2 = fork_lattice
            .account(&key1)
            .legacy_open_with_rep(&send1, &rep2);
        assert_ne!(open1.hash(), open2.hash());

        let open1 = node1.process(open1);
        let open2 = node2.process(open2);
        assert_timely2(|| node1.block_exists(&open1.hash()));
        assert_timely2(|| node2.block_exists(&open2.hash()));

        // Notify both nodes of both blocks, both nodes will become aware that a fork exists
        node1.process_active(open2.clone().into());
        node2.process_active(open1.clone().into());

        assert_timely2(|| node1.ledger.any().is_forked(&open1.qualified_root()));
        assert_timely2(|| node2.ledger.any().is_forked(&open1.qualified_root()));
        assert!(node1.block_exists(&open1.hash()));
        assert!(!node1.block_exists(&open2.hash()));
        assert!(node2.block_exists(&open2.hash()));
        assert!(!node2.block_exists(&open1.hash()));
    }

    #[test]
    fn rep_self_vote() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node0 = system
            .build_node()
            .flags(NodeFlags {
                // Prevent automatic election cleanup
                disable_request_loop: true,
                ..Default::default()
            })
            .config(rsnano_node::config::NodeConfig {
                online_weight_minimum: Amount::MAX,
                // Disable automatic election activation
                enable_priority_scheduler: false,
                enable_hinted_scheduler: false,
                enable_optimistic_scheduler: false,
                ..snapshot_disabled_config_without_backlog_scan()
            })
            .finish();

        let rep_big = PrivateKey::new();

        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let fund_big = lattice.genesis().send_all_except(
            &rep_big,
            Amount::raw(0xb000_0000_0000_0000_0000_0000_0000_0000),
        );

        let open_big = lattice.account(&rep_big).receive(&fund_big);

        node0.process_local(fund_big.clone()).unwrap();
        node0.process_local(open_big.clone()).unwrap();

        // Confirm both blocks, allowing voting on the upcoming block
        start_election(&node0, &open_big.hash());

        assert_timely2(|| node0.is_active_root(&open_big.qualified_root()));
        node0.force_confirm(&open_big.hash());

        // Insert representatives into the node to allow voting
        node0.insert_into_wallet(&rep_big);
        node0.insert_into_wallet(&DEV_GENESIS_KEY);
        assert_timely_eq2(|| node0.wallet_reps.lock().unwrap().voting_reps(), 2);

        let block0 = lattice.genesis().send_all_except(
            &rep_big,
            Amount::raw(0x6000_0000_0000_0000_0000_0000_0000_0000),
        );

        node0.process_local(block0.clone()).unwrap();

        start_election(&node0, &block0.hash());

        // Snapshot mode still records both self-votes even though fork handling differs.
        assert_timely2(|| {
            let votes = node0.history.votes(&block0.root(), &block0.hash(), false);
            votes.iter().any(|v| v.voter == rep_big.public_key())
                && votes.iter().any(|v| v.voter == *DEV_GENESIS_PUB_KEY)
        });
    }

    #[test]
    fn search_receivable_multiple() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node = system.build_node().config(snapshot_disabled_config()).finish();
        let wallet_id = node.wallets.wallet_ids()[0];
        let key2 = PrivateKey::new();
        let key3 = PrivateKey::new();
        node.insert_into_wallet(&DEV_GENESIS_KEY);
        node.insert_into_wallet(&key3);

        node.wallets
            .send(
                wallet_id,
                *DEV_GENESIS_ACCOUNT,
                key3.account(),
                node.config.receive_minimum,
                0.into(),
                true,
                None,
            )
            .wait_timeout(Duration::from_secs(5))
            .unwrap();

        assert_timely2(|| !node.balance(&key3.account()).is_zero());

        node.wallets
            .send(
                wallet_id,
                *DEV_GENESIS_ACCOUNT,
                key2.account(),
                node.config.receive_minimum,
                0.into(),
                true,
                None,
            )
            .wait_timeout(Duration::from_secs(5))
            .unwrap();

        node.wallets
            .send(
                wallet_id,
                key3.account(),
                key2.account(),
                node.config.receive_minimum,
                0.into(),
                true,
                None,
            )
            .wait_timeout(Duration::from_secs(5))
            .unwrap();

        node.wallets
            .insert_adhoc2(&wallet_id, &key2.raw_key(), true)
            .unwrap();

        node.wallets
            .search_receivable(&wallet_id)
            .wait_timeout(Duration::from_secs(5))
            .unwrap();

        assert_timely2(|| node.balance(&key2.account()) == node.config.receive_minimum * 2);
    }

    #[test]
    fn send_self() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let key2 = PrivateKey::new();
        let node = system.build_node().config(snapshot_disabled_config()).finish();
        let wallet_id = node.wallets.wallet_ids()[0];
        node.wallets
            .insert_adhoc2(&wallet_id, &DEV_GENESIS_KEY.raw_key(), true)
            .unwrap();
        node.wallets
            .insert_adhoc2(&wallet_id, &key2.raw_key(), true)
            .unwrap();

        node.wallets
            .send(
                wallet_id,
                *DEV_GENESIS_ACCOUNT,
                key2.account(),
                node.config.receive_minimum,
                0.into(),
                true,
                None,
            )
            .wait()
            .unwrap();

        assert_timely_msg(
            Duration::from_secs(10),
            || !node.balance(&key2.account()).is_zero(),
            "balance is still zero",
        );

        assert_eq!(
            Amount::MAX - node.config.receive_minimum,
            node.balance(&DEV_GENESIS_ACCOUNT)
        );
    }

    #[test]
    fn send_single() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let key2 = PrivateKey::new();
        let node1 = system.build_node().config(snapshot_disabled_config()).finish();
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();
        let wallet_id1 = node1.wallets.wallet_ids()[0];
        let wallet_id2 = node2.wallets.wallet_ids()[0];

        node1
            .wallets
            .insert_adhoc2(&wallet_id1, &DEV_GENESIS_KEY.raw_key(), true)
            .unwrap();
        node2
            .wallets
            .insert_adhoc2(&wallet_id2, &key2.raw_key(), true)
            .unwrap();

        node1
            .wallets
            .send(
                wallet_id1,
                *DEV_GENESIS_ACCOUNT,
                key2.account(),
                node1.config.receive_minimum,
                0.into(),
                true,
                None,
            )
            .wait()
            .unwrap();

        assert_eq!(
            Amount::MAX - node1.config.receive_minimum,
            node1.balance(&DEV_GENESIS_ACCOUNT)
        );

        assert!(node1.balance(&key2.account()).is_zero());

        assert_timely_msg(
            Duration::from_secs(10),
            || !node1.balance(&key2.account()).is_zero(),
            "balance is still zero",
        );
    }

    #[test]
    fn unconfirmed_send() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();

        let node1 = system.build_node().config(snapshot_disabled_config()).finish();
        let wallet_id1 = node1.wallets.wallet_ids()[0];
        node1
            .wallets
            .insert_adhoc2(&wallet_id1, &DEV_GENESIS_KEY.raw_key(), true)
            .unwrap();

        let key2 = PrivateKey::new();
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();
        let wallet_id2 = node2.wallets.wallet_ids()[0];
        node2
            .wallets
            .insert_adhoc2(&wallet_id2, &key2.raw_key(), true)
            .unwrap();

        let send1 = node1
            .wallets
            .send(
                wallet_id1,
                *DEV_GENESIS_ACCOUNT,
                key2.account(),
                Amount::nano(2),
                0.into(),
                true,
                None,
            )
            .wait()
            .unwrap();

        assert_timely2(|| node1.block_confirmed(&send1.hash()));
        assert_timely2(|| node2.block_confirmed(&send1.hash()));

        // wait until receive1 (auto-receive created by wallet) is cemented
        assert_timely_eq2(
            || {
                node2
                    .ledger
                    .confirmed()
                    .get_conf_info(&key2.account())
                    .unwrap_or_default()
                    .height
            },
            1,
        );

        assert_eq!(node2.balance(&key2.account()), Amount::nano(2));

        let recv1 = node2
            .ledger
            .any()
            .find_receive_block_by_send_hash(&key2.account(), &send1.hash())
            .unwrap();

        // create send2 to send from node2 to node1 and save it to node2's ledger without triggering an election (node1 does not hear about it)
        let send2: Block = StateBlockArgs {
            key: &key2,
            previous: recv1.hash(),
            representative: *DEV_GENESIS_PUB_KEY,
            balance: Amount::nano(1),
            link: (*DEV_GENESIS_ACCOUNT).into(),
            work: node2.work_generate_dev(recv1.hash()),
        }
        .into();

        node2.process_local(send2.clone()).unwrap();

        let send3 = node2
            .wallets
            .send(
                wallet_id2,
                key2.account(),
                *DEV_GENESIS_ACCOUNT,
                Amount::nano(1),
                0.into(),
                true,
                None,
            )
            .wait()
            .unwrap();
        assert_timely2(|| node2.block_confirmed(&send2.hash()));
        assert_timely2(|| node1.block_confirmed(&send2.hash()));
        assert_timely2(|| node2.block_confirmed(&send3.hash()));
        assert_timely2(|| node1.block_confirmed(&send3.hash()));
        assert_timely_eq2(|| node2.ledger.confirmed_count(), 5);
        assert_timely_eq2(|| node1.balance(&DEV_GENESIS_ACCOUNT), Amount::MAX);
    }

    #[test]
    fn unlock_search() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node = system.build_node().config(snapshot_disabled_config()).finish();
        let wallet_id = node.wallets.wallet_ids()[0];
        let key2 = PrivateKey::new();
        let balance = node.balance(&DEV_GENESIS_ACCOUNT);

        node.wallets.rekey(&wallet_id, "").unwrap();
        node.wallets
            .insert_adhoc2(&wallet_id, &DEV_GENESIS_KEY.raw_key(), true)
            .unwrap();

        node.wallets
            .send(
                wallet_id,
                *DEV_GENESIS_ACCOUNT,
                key2.account(),
                node.config.receive_minimum,
                0.into(),
                true,
                None,
            )
            .wait()
            .unwrap();

        assert_timely2(|| node.balance(&DEV_GENESIS_ACCOUNT) != balance);

        assert_timely_eq(Duration::from_secs(10), || node.aec.len(), 0);

        node.wallets
            .insert_adhoc2(&wallet_id, &key2.raw_key(), true)
            .unwrap();
        node.wallets.enter_password(wallet_id, "").unwrap();

        assert_timely_msg(
            Duration::from_secs(10),
            || !node.balance(&key2.account()).is_zero(),
            "balance is still zero",
        );
    }

    #[test]
    fn vote_by_hash_republish() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system.build_node().config(snapshot_disabled_config()).finish();
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();
        let key = PrivateKey::new();

        // send1 and send2 are forks of each other
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
        let send1 = lattice.genesis().send(&key, Amount::nano(1000));
        let send2 = fork_lattice.genesis().send(&key, Amount::nano(2000));

        // give block send1 to node1 and check that an election for send1 starts on both nodes
        node1.process_active(send1.clone());
        assert_timely2(|| node1.is_active_root(&send1.qualified_root()));
        assert_timely2(|| node2.is_active_root(&send1.qualified_root()));

        // give block send2 to node1 and wait until the block is received and processed by node1
        node1.network_filter.clear_all();
        node1.process_active(send2.clone());
        assert_timely2(|| node1.is_active_root(&send2.qualified_root()));

        // construct a vote for send2 in order to overturn send1
        let vote = Arc::new(Vote::new_final_for_test(
            &DEV_GENESIS_KEY,
            vec![send2.hash()],
        ));
        node1
            .vote_processor_queue
            .enqueue(vote, None, VoteSource::Live, None);

        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(node1.block_confirmed(&send2.hash()), false);
        assert_eq!(node2.block_confirmed(&send2.hash()), false);
        assert_eq!(node1.block_exists(&send1.hash()), true);
        assert_eq!(node2.block_exists(&send1.hash()), true);
        assert_eq!(node1.block_exists(&send2.hash()), false);
        assert_eq!(node2.block_exists(&send2.hash()), false);
    }

    #[test]
    fn vote_republish() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node1 = system.build_node().config(snapshot_disabled_config()).finish();
        let node2 = system.build_node().config(snapshot_disabled_config()).finish();
        let key2 = PrivateKey::new();
        // by not setting a private key on node1's wallet for genesis account, it is stopped from voting
        node2.insert_into_wallet(&key2);

        // send1 and send2 are forks of each other
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let mut fork_lattice = UnsavedBlockLatticeBuilder::new();
        let send1 = lattice.genesis().send(&key2, Amount::nano(1000));
        let send2 = fork_lattice.genesis().send(&key2, Amount::nano(2000));

        // process send1 first, this will make sure send1 goes into the ledger and an election is started
        node1.process_active(send1.clone());
        assert_timely2(|| node2.block_exists(&send1.hash()));
        assert_timely2(|| node1.is_active_root(&send1.qualified_root()));
        assert_timely2(|| node2.is_active_root(&send1.qualified_root()));

        // now process send2, send2 will not go in the ledger because only the first block of a fork goes in the ledger
        node1.process_active(send2.clone());
        assert_timely2(|| node1.is_active_root(&send2.qualified_root()));

        // send2 cannot be synced because it is not in the ledger of node1, it is only in the election object in RAM on node1
        assert_eq!(node1.block_exists(&send2.hash()), false);

        // the vote causes the election to reach quorum and for the vote (and block?) to be published from node1 to node2
        let vote = Arc::new(Vote::new_final_for_test(
            &DEV_GENESIS_KEY,
            vec![send2.hash()],
        ));
        node1
            .vote_processor_queue
            .enqueue(vote, None, VoteSource::Live, None);

        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(node1.block_confirmed(&send2.hash()), false);
        assert_eq!(node2.block_confirmed(&send2.hash()), false);
        assert_eq!(node1.block_exists(&send1.hash()), true);
        assert_eq!(node2.block_exists(&send1.hash()), true);
        assert_eq!(node1.block_exists(&send2.hash()), false);
        assert_eq!(node2.block_exists(&send2.hash()), false);
    }
}

mod request_aggregator {
    use super::snapshot_disabled_config;
    use std::time::{Duration, Instant};

    use rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder;
    use rsnano_node::consensus::{AggregatorRequest, VoteGenerationEvent};
    use rsnano_output_tracker::OutputTrackerMt;
    use rsnano_types::{DEV_GENESIS_KEY, PrivateKey};
    use test_helpers::{System, assert_timely2, make_fake_channel};

    #[test]
    fn forked_open() {
        let _serial_guard = crate::tests::ledger_snapshots::ledger_snapshot_test_guard();
        let mut system = System::new();
        let node = system.build_node().config(snapshot_disabled_config()).finish();

        // Voting needs a rep key set up on the node
        node.insert_into_wallet(&DEV_GENESIS_KEY);

        // Setup two forks of the open block
        let key = PrivateKey::new();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let send0 = lattice.genesis().send(&key, 500);
        let mut fork_lattice = lattice.clone();
        let open0 = lattice.account(&key).receive_and_change(&send0, 1);
        let open1 = fork_lattice.account(&key).receive_and_change(&send0, 2);

        node.process(send0);
        node.process(open0.clone());
        node.confirm(open0.hash());

        assert_timely2(|| node.aec.is_empty());

        let vote_tracker = node.vote_generators.track();

        let channel = make_fake_channel(&node);

        // Request vote for the wrong fork
        let request = AggregatorRequest {
            channel: channel.clone(),
            roots_hashes: vec![(open1.hash(), open1.root())],
        };
        node.request_aggregator.request(request);

        let vote_event = wait_vote_event(&vote_tracker);

        assert_eq!(vote_event.blocks.len(), 1);
        // Vote for the correct fork alternative
        assert_eq!(vote_event.blocks[0].hash(), open0.hash());
    }

    fn wait_vote_event(tracker: &OutputTrackerMt<VoteGenerationEvent>) -> VoteGenerationEvent {
        let start = Instant::now();
        loop {
            let output = tracker.output();
            if !output.is_empty() {
                return output[0].clone();
            }

            if start.elapsed() > Duration::from_secs(5) {
                panic!("timeout!");
            }

            std::thread::yield_now();
        }
    }
}
