use rsnano_core::{Account, Amount, BlockEnum, BlockHash, KeyPair, StateBlock};
use rsnano_ledger::{DEV_GENESIS_HASH, DEV_GENESIS_PUB_KEY};
use std::time::Duration;
use test_helpers::{assert_timely, setup_rpc_client_and_server, System};

#[test]
fn unchecked_clear() {
    let mut system = System::new();
    let node = system.make_node();

    let (rpc_client, server) = setup_rpc_client_and_server(node.clone(), true);

    let keypair = KeyPair::new();

    let send1 = BlockEnum::State(StateBlock::new(
        keypair.account(),
        BlockHash::zero(),
        *DEV_GENESIS_PUB_KEY,
        Amount::MAX - Amount::raw(1),
        Account::zero().into(),
        &keypair,
        node.work_generate_dev((*DEV_GENESIS_HASH).into()),
    ));

    node.process_local(send1.clone()).unwrap();

    assert_timely(Duration::from_secs(5), || !node.unchecked.is_empty());

    node.runtime
        .block_on(async { rpc_client.unchecked_clear().await.unwrap() });

    assert!(node.unchecked.is_empty());

    server.abort();
}
