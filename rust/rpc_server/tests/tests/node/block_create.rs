use rsnano_core::{Amount, BlockEnum, BlockType, KeyPair, Link, Root, WalletId, DEV_GENESIS_KEY};
use rsnano_ledger::{DEV_GENESIS_ACCOUNT, DEV_GENESIS_HASH};
use rsnano_node::wallets::WalletsExt;
use rsnano_rpc_messages::{AccountIdentifier, BlockCreateArgs, BlockTypeDto, TransactionInfo};
use test_helpers::{setup_rpc_client_and_server, System};

#[test]
fn block_create_send_with_private_key() {
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
fn block_create_send_with_wallet_account() {
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
                    AccountIdentifier::WalletAccount {
                        wallet: wallet_id,
                        account: *DEV_GENESIS_ACCOUNT,
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
fn block_create_receive_with_private_key() {
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
                    send.hash(), 
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
        Some(*DEV_GENESIS_ACCOUNT)
    );
}

#[test]
fn block_create_receive_with_wallet_account() {
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
                    AccountIdentifier::WalletAccount {
                        wallet: wallet_id,
                        account: *DEV_GENESIS_ACCOUNT,
                    },
                    TransactionInfo::Receive {
                        source: send.hash(),
                    },
                    send.hash(), 
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
        Some(*DEV_GENESIS_ACCOUNT)
    );
}

#[test]
fn block_create_send_with_link() {
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
                    TransactionInfo::Link {
                        link: key1.account().into(),
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
fn block_create_receive_with_link() {
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
                    Amount::MAX,
                    AccountIdentifier::WalletAccount {
                        wallet: wallet_id,
                        account: *DEV_GENESIS_ACCOUNT,
                    },
                    TransactionInfo::Link {
                        link: send.hash().into(),
                    },
                    send.hash(), 
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
        Some(*DEV_GENESIS_ACCOUNT)
    );
}

#[test]
fn block_create_change_with_private_key() {
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
                    Amount::MAX, // is valid with any amount?
                    AccountIdentifier::PrivateKey {
                        key: DEV_GENESIS_KEY.private_key(),
                    },
                    TransactionInfo::Link {
                        link: Link::zero(),
                    },
                    *DEV_GENESIS_HASH,
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
        Some(*DEV_GENESIS_ACCOUNT)
    );
}