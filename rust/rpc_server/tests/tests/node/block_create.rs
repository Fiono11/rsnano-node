use rsnano_core::{Amount, BlockEnum, BlockType, KeyPair, Root, WalletId, DEV_GENESIS_KEY};
use rsnano_ledger::{DEV_GENESIS_ACCOUNT, DEV_GENESIS_HASH};
use rsnano_node::wallets::WalletsExt;
use rsnano_rpc_messages::{AccountIdentifier, BlockCreateArgs, BlockTypeDto, TransactionInfo};
use test_helpers::{confirm_block, process_block_local, send_block, send_block_to, setup_rpc_client_and_server, System};

#[test]
fn block_create_send() {
    let mut system = System::new();
    let mut config = System::default_config();
    config.online_weight_minimum = Amount::MAX;
    let node = system.build_node().config(config).finish();

    let wallet_id = WalletId::zero();
    node.wallets.create(wallet_id);
    node.wallets
        .insert_adhoc2(&wallet_id, &DEV_GENESIS_KEY.private_key(), false)
        .unwrap();
    let key1 = KeyPair::new();

    let (rpc_client, _server) = setup_rpc_client_and_server(node.clone(), true);

    let result = node.runtime.block_on(async {
        rpc_client
            .block_create(
                BlockCreateArgs::builder(
                    BlockTypeDto::State,
                    Amount::MAX - Amount::raw(100),
                    AccountIdentifier::PrivateKey {
                        key: DEV_GENESIS_KEY.private_key(),
                    },
                    TransactionInfo::Send {
                        destination: key1.account(),
                    },
                    *DEV_GENESIS_HASH,
                    *DEV_GENESIS_ACCOUNT,
                )
                .build()
                .unwrap(),
            )
            .await
            .unwrap()
    });

    let block_hash = result.hash;
    let block: BlockEnum = result.block.into();

    assert_eq!(block.block_type(), BlockType::State);
    assert_eq!(block.hash(), block_hash);

    node.process(block.clone()).unwrap();

    let tx = node.ledger.read_txn();
    assert_eq!(
        node.ledger.any().block_account(&tx, &block.hash()),
        Some(*DEV_GENESIS_ACCOUNT)
    );
}

#[test]
fn block_create_receive() {
    let mut system = System::new();
    let mut config = System::default_config();
    config.online_weight_minimum = Amount::MAX;
    let node = system.build_node().config(config).finish();

    let wallet_id = WalletId::zero();
    node.wallets.create(wallet_id);
    node.wallets
        .insert_adhoc2(&wallet_id, &DEV_GENESIS_KEY.private_key(), false)
        .unwrap();
    let key1 = KeyPair::new();

    let (rpc_client, _server) = setup_rpc_client_and_server(node.clone(), true);

    let send = node
        .wallets
        .send_action2(
            &wallet_id,
            *DEV_GENESIS_ACCOUNT,
            *DEV_GENESIS_ACCOUNT,
            node.config.receive_minimum,
            node.work_generate_dev(Root::from(*DEV_GENESIS_HASH)),
            false,
            None,
        )
        .unwrap();

    // Create a receive block
    let result = node.runtime.block_on(async {
        rpc_client
            .block_create(
                BlockCreateArgs::builder(
                    BlockTypeDto::State,
                    node.config.receive_minimum,
                    AccountIdentifier::PrivateKey {
                        key: DEV_GENESIS_KEY.private_key(),
                    },
                    TransactionInfo::Receive {
                        source: send.hash(),
                    },
                    *DEV_GENESIS_HASH, // Use key1's account as previous
                    key1.account(),
                )
                .build()
                .unwrap(),
            )
            .await
            .unwrap()
    });

    let block_hash = result.hash;
    let block: BlockEnum = result.block.into();

    assert_eq!(block.block_type(), BlockType::State);
    assert_eq!(block.hash(), block_hash);

    node.process(block.clone()).unwrap();

    let tx = node.ledger.read_txn();
    assert_eq!(
        node.ledger.any().block_account(&tx, &block.hash()),
        Some(key1.account())
    );
}
