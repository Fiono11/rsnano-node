use rsnano_core::{
    Account, Amount, BlockBuilder, BlockDetails, BlockEnum, BlockHash, Epoch, KeyPair, PendingKey,
    PublicKey, RawKey,
};
use rsnano_node::Node;
use rsnano_rpc_messages::{
    AccountIdentifier, BlockCreateArgs, BlockCreateDto, BlockTypeDto, ErrorDto, TransactionInfo, WorkVersionDto
};
use serde_json::to_string_pretty;
use std::sync::Arc;

pub async fn block_create(node: Arc<Node>, enable_control: bool, args: BlockCreateArgs) -> String {
    if !enable_control {
        return to_string_pretty(&ErrorDto::new("RPC control is disabled".to_string())).unwrap();
    }

    let work_version = args.version.unwrap_or(WorkVersionDto::Work1).into();
    let mut difficulty = args
        .difficulty
        .unwrap_or_else(|| node.network_params.work.threshold_base(work_version).into());

    let mut previous = args.previous;
    let mut balance = args.balance;
    let mut prv_key = RawKey::default();
    let mut account = Account::zero();

    let work = args.work;

    if work.is_none() && !node.distributed_work.work_generation_enabled() {
        return to_string_pretty(&ErrorDto::new("Work generation is disabled".to_string()))
            .unwrap();
    }

    match args.account_identifier {
        AccountIdentifier::WalletAccount { wallet, account: acc } => {
            account = acc;
            prv_key = node.wallets.fetch(&wallet, &account.into()).unwrap();
        }
        AccountIdentifier::PrivateKey { key } => {
            prv_key = key;
            let pub_key: PublicKey = (&prv_key).try_into().unwrap();
            account = pub_key.into();
        }
    }

    if prv_key.is_zero() {
        return to_string_pretty(&ErrorDto::new("Block create key required".to_string())).unwrap();
    }

    let pub_key: PublicKey = (&prv_key).try_into().unwrap();
    let pub_account: Account = pub_key.into();

    if account != Account::zero() && account != pub_account {
        return to_string_pretty(&ErrorDto::new(
            "Block create public key mismatch".to_string(),
        ))
        .unwrap();
    }

    let key_pair = KeyPair::from(prv_key);

    // Build the block
    let mut block = match args.block_type {
        BlockTypeDto::State => {
            let builder = BlockBuilder::state();
            builder
                .account(pub_account)
                .previous(previous)
                .representative(args.representative)
                .balance(balance)
                .link(match args.transaction_info {
                    TransactionInfo::Send { destination } => destination.into(),
                    TransactionInfo::Receive { source } => source.into(),
                    TransactionInfo::Link { link } => link,
                })
                .sign(&key_pair)
                .build()
        }
        BlockTypeDto::Open => {
            if let TransactionInfo::Receive { source } = args.transaction_info {
                let builder = BlockBuilder::legacy_open();
                builder
                    .account(pub_account)
                    .source(source.into())
                    .representative(args.representative.into())
                    .sign(&key_pair)
                    .build()
            } else {
                return to_string_pretty(&ErrorDto::new(
                    "Invalid transaction info for open block".to_string(),
                ))
                .unwrap();
            }
        }
        BlockTypeDto::Receive => {
            if let TransactionInfo::Receive { source } = args.transaction_info {
                let builder = BlockBuilder::legacy_receive();
                builder
                    .previous(previous)
                    .source(source.into())
                    .sign(&key_pair)
                    .build()
            } else {
                return to_string_pretty(&ErrorDto::new(
                    "Invalid transaction info for receive block".to_string(),
                ))
                .unwrap();
            }
        }
        BlockTypeDto::Change => {
            let builder = BlockBuilder::legacy_change();
            builder
                .previous(previous)
                .representative(args.representative.into())
                .sign(&key_pair)
                .build()
        }
        BlockTypeDto::Send => {
            if let TransactionInfo::Send { destination } = args.transaction_info {
                let amount = args.balance; // Adjusted: assuming balance field represents the amount to send
                if balance >= amount {
                    let builder = BlockBuilder::legacy_send();
                    builder
                        .previous(previous)
                        .destination(destination)
                        .balance(balance - amount)
                        .sign(key_pair)
                        .build()
                } else {
                    return to_string_pretty(&ErrorDto::new("Insufficient balance".to_string()))
                        .unwrap();
                }
            } else {
                return to_string_pretty(&ErrorDto::new(
                    "Invalid transaction info for send block".to_string(),
                ))
                .unwrap();
            }
        }
    };

    let root = if !previous.is_zero() {
        previous
    } else {
        pub_account.into()
    };

    if work.is_none() {
        difficulty = if args.difficulty.is_none() {
            difficulty_ledger(node.clone(), &block).into()
        } else {
            difficulty
        };

        let work = match node
            .distributed_work
            .make(root.into(), difficulty.into(), Some(pub_account))
            .await
        {
            Some(work) => work,
            None => {
                return to_string_pretty(&ErrorDto::new("Failed to generate work".to_string()))
                    .unwrap()
            }
        };
        block.set_work(work);
    } else {
        block.set_work(work.unwrap().into());
    }

    let hash = block.hash();
    let difficulty = block.work();
    let json_block = block.json_representation();

    to_string_pretty(&BlockCreateDto::new(hash, difficulty.into(), json_block)).unwrap()
}

pub fn difficulty_ledger(node: Arc<Node>, block: &BlockEnum) -> u64 {
    let mut details = BlockDetails::new(Epoch::Epoch0, false, false, false);
    let mut details_found = false;

    let transaction = node.store.tx_begin_read();

    // Previous block find
    let mut block_previous: Option<BlockEnum> = None;
    let previous = block.previous();
    if !previous.is_zero() {
        block_previous = node.ledger.any().get_block(&transaction, &previous);
    }

    // Send check
    if let Some(_prev_block) = &block_previous {
        let is_send =
            node.ledger.any().block_balance(&transaction, &previous) > block.balance_field();
        details = BlockDetails::new(Epoch::Epoch0, is_send, false, false);
        details_found = true;
    }

    // Epoch check
    if let Some(prev_block) = &block_previous {
        let epoch = prev_block.sideband().unwrap().details.epoch;
        details = BlockDetails::new(epoch, details.is_send, details.is_receive, details.is_epoch);
    }

    // Link check
    if let Some(link) = block.link_field() {
        if !details.is_send {
            if let Some(block_link) = node.ledger.any().get_block(&transaction, &link.into()) {
                let account = block.account_field().unwrap();
                if node
                    .ledger
                    .any()
                    .get_pending(&transaction, &PendingKey::new(account, link.into()))
                    .is_some()
                {
                    let epoch =
                        std::cmp::max(details.epoch, block_link.sideband().unwrap().details.epoch);
                    details = BlockDetails::new(epoch, details.is_send, true, details.is_epoch);
                    details_found = true;
                }
            }
        }
    }

    if details_found {
        node.network_params.work.threshold(&details)
    } else {
        node.network_params
            .work
            .threshold_base(block.work_version())
    }
}