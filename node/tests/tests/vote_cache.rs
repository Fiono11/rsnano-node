use std::{collections::HashMap, sync::Arc};

use rsnano_ledger::{LedgerSet, test_helpers::UnsavedBlockLatticeBuilder};
use rsnano_node::consensus::ReceivedVote;
use rsnano_types::{
    Account, Amount, DEV_GENESIS_KEY, PrivateKey, UnixMillisTimestamp, Vote, VoteDelivery,
};
use rsnano_utils::stats::Direction;
use test_helpers::{System, assert_timely_eq2, assert_timely2, start_election};

#[test]
fn vote_cache_basic() {
    let mut system = System::new();
    let node = system.make_node();
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let send = lattice.genesis().send(42, Amount::raw(100));
    let vote = Arc::new(Vote::new_final(&DEV_GENESIS_KEY, vec![send.hash()]));

    // Process vote so that it will be cached
    node.vote_processor_queue
        .enqueue(vote, None, VoteDelivery::Direct, None);
    assert_timely2(|| node.vote_cache.contains(&send.hash()));

    // Now process the block. The cached vote should be applied.
    node.process_active(send.clone());

    assert_timely2(|| node.block_confirmed(&send.hash()));
}

#[test]
fn vote_cache_fork() {
    let mut system = System::new();
    let node = system.make_node();
    let mut lattice1 = UnsavedBlockLatticeBuilder::new();
    let mut lattice2 = UnsavedBlockLatticeBuilder::new();
    let key = PrivateKey::new();

    let send1 = lattice1.genesis().send(&key, 100);
    let send2 = lattice2.genesis().send(&key, 200);

    let vote = Arc::new(Vote::new_final(&DEV_GENESIS_KEY, vec![send1.hash()]));
    node.vote_processor_queue
        .enqueue(vote, None, VoteDelivery::Direct, None);

    assert_timely_eq2(|| node.vote_cache.len(), 1);

    node.process_active(send2.clone());

    assert_timely2(|| node.is_active_root(&send1.qualified_root()));

    node.process_active(send1.clone());

    assert_timely_eq2(|| node.block_confirmed(&send1.hash()), true);
}

#[test]
fn vote_cache_existing_vote() {
    let mut system = System::new();
    let config = System::default_config_without_backlog_scan();
    let node = system.build_node().config(config).finish();
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key = PrivateKey::new();
    let rep_weight = Amount::nano(100_000);

    let send = lattice.genesis().send(&key, rep_weight);
    let open = lattice.account(&key).receive(&send);

    node.process(send.clone());
    node.process(open.clone());

    assert_timely2(|| node.is_active_hash(&send.hash()));
    assert!(
        node.ledger.weight(&key.public_key())
            > node.rep_tracker.quorum_snapshot().minimum_principal_weight
    );

    // Insert vote
    let vote1 = Arc::new(Vote::new(
        &key,
        UnixMillisTimestamp::ZERO,
        0,
        vec![send.hash()],
    ));
    node.vote_processor_queue
        .enqueue(vote1.clone(), None, VoteDelivery::Direct, None);

    assert_timely_eq2(
        || {
            node.aec
                .election_for_block(&send.hash())
                .unwrap()
                .vote_count()
        },
        1,
    );

    assert_timely_eq2(|| node.get_stat("election", "vote", Direction::In), 1);

    let last_vote1 = node
        .aec
        .election_for_block(&send.hash())
        .unwrap()
        .votes()
        .get(&key.public_key())
        .unwrap()
        .clone();

    assert_eq!(send.hash(), last_vote1.hash);

    // Attempt to change vote with vote_cache
    node.vote_cache.process(vote1, rep_weight, &HashMap::new());

    let mut cached = Vec::new();
    node.vote_cache.collect_votes(&mut cached, &send.hash());
    assert_eq!(cached.len(), 1);
    let _ = node
        .vote_processor
        .vote_blocking(&ReceivedVote::new(cached[0].clone(), VoteDelivery::Direct, None).into());

    // Check that election data is not changed
    let election = node.aec.election_for_block(&send.hash()).unwrap();
    assert_eq!(election.vote_count(), 1);
    let last_vote2 = election.votes().get(&key.public_key()).unwrap().clone();
    assert_eq!(send.hash(), last_vote2.hash);
    assert_eq!(0, node.get_stat("election_vote", "replayed", Direction::In));
}

#[test]
fn vote_cache_multiple_votes() {
    let mut system = System::new();
    let config = System::default_config_without_backlog_scan();
    let node = system.build_node().config(config).finish();
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key = PrivateKey::new();

    let send1 = lattice.genesis().send(&key, Amount::nano(100_000));
    let send2 = lattice.genesis().send(&key, Amount::nano(100_000));
    let open = lattice.account(&key).receive(&send1);

    // put the blocks in the ledger witout triggering an election
    node.process(send1.clone());
    node.process(send2.clone());
    node.process(open.clone());

    assert_timely2(|| node.is_active_hash(&send1.hash()));
    node.aec.cancel(&send1.qualified_root());
    assert_timely2(|| !node.is_active_hash(&send1.hash()));

    // Process votes
    let vote1 = Arc::new(Vote::new(
        &key,
        UnixMillisTimestamp::ZERO,
        0,
        vec![send1.hash()],
    ));
    node.vote_processor_queue
        .enqueue(vote1, None, VoteDelivery::Direct, None);

    let vote2 = Arc::new(Vote::new(
        &DEV_GENESIS_KEY,
        UnixMillisTimestamp::ZERO,
        0,
        vec![send1.hash()],
    ));
    node.vote_processor_queue
        .enqueue(vote2, None, VoteDelivery::Direct, None);

    assert_timely_eq2(|| node.vote_cache.vote_count(&send1.hash()), 2);
    assert_eq!(1, node.vote_cache.len());
    start_election(&node, &send1.hash());
    assert_timely_eq2(
        || {
            node.aec
                .election_for_block(&send1.hash())
                .unwrap()
                .vote_count()
        },
        2,
    );
    assert_timely_eq2(
        || node.get_stat("election_vote", "replayed", Direction::In),
        2,
    );
}

#[test]
fn vote_cache_election_start() {
    let mut system = System::new();
    let mut config = System::default_config_without_backlog_scan();
    config.enable_optimistic_scheduler = false;
    config.enable_priority_scheduler = false;
    let node = system.build_node().config(config).finish();
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let key1 = PrivateKey::new();
    let key2 = PrivateKey::new();

    // Enough weight to trigger election hinting but not enough to confirm block on its own
    let amount = ((node.rep_tracker.quorum_snapshot().trended_or_min_weight / 100)
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
    let vote1 = Arc::new(Vote::new(
        &key1,
        UnixMillisTimestamp::ZERO,
        0,
        vec![open1.hash(), open2.hash(), send4.hash()],
    ));
    node.vote_processor_queue
        .enqueue(vote1, None, VoteDelivery::Direct, None);
    assert_timely_eq2(|| node.vote_cache.len(), 3);
    assert_eq!(node.aec.len(), 0);
    assert_eq!(1, node.ledger.confirmed_count());

    // 2 votes are required to start election (dev network)
    let vote2 = Arc::new(Vote::new(
        &key2,
        UnixMillisTimestamp::ZERO,
        0,
        vec![open1.hash(), open2.hash(), send4.hash()],
    ));
    node.vote_processor_queue
        .enqueue(vote2, None, VoteDelivery::Direct, None);
    // Only election for send1 should start, other blocks are missing dependencies and don't have enough final weight
    assert_timely_eq2(|| node.aec.len(), 1);
    assert!(node.is_active_hash(&send1.hash()));

    // Confirm elections with weight quorum
    let vote0 = Arc::new(Vote::new_final(
        &DEV_GENESIS_KEY,
        vec![open1.hash(), open2.hash(), send4.hash()],
    ));
    node.vote_processor_queue
        .enqueue(vote0, None, VoteDelivery::Direct, None);
    assert_timely_eq2(|| node.aec.len(), 0);
    assert_timely_eq2(|| node.ledger.confirmed_count(), 5);
    // Confirmation on disk may lag behind cemented_count cache
    assert_timely2(|| {
        node.block_hashes_confirmed(&[send1.hash(), send2.hash(), open1.hash(), open2.hash()])
    });

    // A late block arrival also checks the inactive votes cache
    assert_eq!(node.aec.len(), 0);
    let send4_cache = node.vote_cache.vote_count(&send4.hash());
    assert_eq!(3, send4_cache);
    node.process_active(send3.clone());
    // An election is started for send6 but does not
    assert_eq!(node.ledger.confirmed().block_exists(&send3.hash()), false);
    assert_eq!(node.confirming_set.contains(&send3.hash()), false);
    // send7 cannot be voted on but an election should be started from inactive votes
    node.process_active(send4);
    assert_timely_eq2(|| node.ledger.confirmed_count(), 7);
}
