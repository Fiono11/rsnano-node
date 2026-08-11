use rsnano_ledger::DEV_GENESIS_HASH;
use test_helpers::{System, setup_rpc_client_and_server};

#[test]
fn blocks_info() {
    let mut system = System::new();
    let node = system.make_node();

    let server = setup_rpc_client_and_server(node.clone(), false);

    node.runtime.block_on(async {
        server
            .client
            .blocks_info(vec![*DEV_GENESIS_HASH])
            .await
            .unwrap()
    });
}

#[cfg(feature = "rai_protocol")]
#[test]
fn blocks_info_returns_rai_finalization_epoch() {
    use std::sync::atomic::AtomicBool;

    use rsnano_ledger::{CementingObserver, DEV_GENESIS_PUB_KEY};
    use rsnano_types::{
        Account, Amount, Block, BlockHash, DEV_GENESIS_KEY, RaiEpoch, StateBlockArgs,
    };

    struct Observer;
    impl CementingObserver for Observer {
        fn already_confirmed(&mut self, _hash: &BlockHash) {}
        fn cementing_failed(&mut self, hash: &BlockHash) {
            panic!("failed to cement {hash}")
        }
    }

    let mut system = System::new();
    let node = system.make_node();
    let block: Block = StateBlockArgs {
        key: &DEV_GENESIS_KEY,
        previous: *DEV_GENESIS_HASH,
        representative: *DEV_GENESIS_PUB_KEY,
        balance: Amount::MAX - Amount::raw(1),
        link: Account::from(1).into(),
        work: node.work_generate_dev(*DEV_GENESIS_HASH),
    }
    .into();
    node.ledger.process_one(&block).unwrap();

    let epoch = RaiEpoch::new(7);
    node.ledger.confirm_batch_rai(
        [(&block.hash(), Some(epoch))],
        &AtomicBool::new(false),
        1024,
        &mut Observer,
    );

    let server = setup_rpc_client_and_server(node.clone(), false);
    let response = node
        .runtime
        .block_on(async { server.client.blocks_info(vec![block.hash()]).await.unwrap() });

    assert_eq!(
        response.blocks[&block.hash()].rai_finalization_epoch,
        Some(epoch.number().into())
    );
}
