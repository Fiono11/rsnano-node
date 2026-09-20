use std::time::Duration;

use tokio::time::sleep;
use tracing::{debug, info};

use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::{ReceiveArgs, SendArgs, WalletAddArgs, WalletRepresentativeSetArgs};
use rsnano_types::{
    Account, Amount, Block, BlockHash, JsonBlock, PublicKey, StateBlockArgs, WalletId, WorkNonce,
};

use crate::{
    domain::{AccountMap, Representatives},
    setup::{genesis_key, pr_key},
};

const INITIAL_AMOUNT: Amount = Amount::nano(100_000_000);

/// The weight shared equally by the principal representatives; the rest funds the spam
pub(crate) fn voting_weight() -> Amount {
    Amount::MAX - INITIAL_AMOUNT
}

/// `total_prs` counts every principal representative, the offline and
/// Byzantine ones included: each holds an equal share of the voting weight in
/// the ledger, whether or not it runs a node. `rpc_clients` are the nodes that
/// do run, which get a wallet.
pub(crate) async fn create_wallets(
    rpc_clients: &[NanoRpcClient],
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
    representatives: &Representatives,
    total_prs: usize,
) -> WalletId {
    let mut genesis_wallet = WalletId::ZERO;
    let genesis_key = genesis_key();
    let pr_count = total_prs;
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        info!("Creating wallet...");
        let resp = rpc_client.wallet_create(None).await.unwrap();
        if i == 0 {
            genesis_wallet = resp.wallet;
        }
        let pr_key = pr_key(i);
        rpc_client
            .wallet_add(WalletAddArgs {
                wallet: resp.wallet,
                key: pr_key.raw_key(),
                work: None,
            })
            .await
            .unwrap();

        info!("Setting default representative...");
        rpc_client
            .wallet_representative_set(WalletRepresentativeSetArgs {
                wallet: resp.wallet,
                representative: pr_key.account(),
                update_existing_accounts: Some(false.into()),
            })
            .await
            .unwrap();

        // the first rpc client is the genesis client
        if i > 0 {
            let pr_balance = voting_weight() / pr_count as u128;
            info!(
                "Sending Ӿ{} to PR{i} wallet {} ...",
                pr_balance.format_balance(0),
                pr_key.account().encode_account()
            );
            let send_hash = genesis_rpc
                .send(SendArgs {
                    wallet: genesis_wallet,
                    source: genesis_key.account(),
                    destination: pr_key.account(),
                    amount: pr_balance,
                    work: Some(WorkNonce::new(0)),
                    id: None,
                })
                .await
                .unwrap()
                .block;
            wait_until_confirmed(rpc_client, send_hash).await;

            info!("Receiving...");
            // trigger wallet receive to speed things up
            let _ = rpc_client
                .receive(ReceiveArgs {
                    wallet: resp.wallet,
                    account: pr_key.account(),
                    block: send_hash,
                    work: Some(WorkNonce::new(0)),
                })
                .await;
            let recv_hash = rpc_client
                .account_info(pr_key.account())
                .await
                .unwrap()
                .frontier;
            wait_until_confirmed(rpc_client, recv_hash).await;
            info!("DONE");
            info!(
                "********************************************************************************"
            );
        }
    }

    // The representatives without a node: funded the same way, but nanospam
    // has to create their receive block, since no wallet will
    for i in rpc_clients.len()..total_prs {
        let pr_key = pr_key(i);
        let pr_balance = voting_weight() / pr_count as u128;
        info!(
            "Sending \u{04FE}{} to PR{i} (no node) {} ...",
            pr_balance.format_balance(0),
            pr_key.account().encode_account()
        );
        let send_hash = genesis_rpc
            .send(SendArgs {
                wallet: genesis_wallet,
                source: genesis_key.account(),
                destination: pr_key.account(),
                amount: pr_balance,
                work: Some(WorkNonce::new(0)),
                id: None,
            })
            .await
            .unwrap()
            .block;
        wait_until_confirmed(genesis_rpc, send_hash).await;

        let receive: Block = StateBlockArgs {
            key: &pr_key,
            previous: BlockHash::ZERO,
            // A representative votes with its own weight
            representative: pr_key.public_key(),
            balance: pr_balance,
            link: send_hash.into(),
            work: 0.into(),
        }
        .into();
        let receive_hash = receive.hash();
        genesis_rpc.process(JsonBlock::from(receive)).await.unwrap();
        wait_until_confirmed(genesis_rpc, receive_hash).await;
        info!("DONE");
    }

    info!("Sending initial spam amount...");
    let initial_key = account_map.initial_key().clone();
    // Send total spam amount
    let genesis_send = genesis_rpc
        .send(SendArgs {
            wallet: genesis_wallet,
            source: genesis_key.account(),
            destination: initial_key.account(),
            amount: INITIAL_AMOUNT,
            work: Some(0.into()),
            id: None,
        })
        .await
        .unwrap()
        .block;
    wait_until_confirmed(genesis_rpc, genesis_send).await;
    info!("Receiving initial spam amount...");
    let initial_account = initial_key.account();
    let genesis_receive: Block = StateBlockArgs {
        key: &initial_key,
        previous: BlockHash::ZERO,
        representative: representative_of(&initial_account, representatives),
        balance: INITIAL_AMOUNT,
        link: genesis_send.into(),
        work: 0.into(),
    }
    .into();

    let recv = genesis_rpc
        .process(JsonBlock::from(genesis_receive.clone()))
        .await
        .unwrap();

    wait_until_confirmed(genesis_rpc, recv.hash).await;

    account_map.set_account_state(initial_account, INITIAL_AMOUNT, genesis_receive.hash());

    seed_representatives(genesis_rpc, account_map, representatives).await;

    genesis_wallet
}

/// RAI: the representative an account delegates to; the account itself
/// in a run without representatives
fn representative_of(account: &Account, representatives: &Representatives) -> PublicKey {
    representatives
        .of(account)
        .unwrap_or_else(|| account.as_key())
}

/// RAI: the spam amount sits in one account, delegated to one
/// representative, which would hold it all in the committee derived from
/// epoch 0. Spread over one seed account per representative, every
/// representative starts with an equal share of it and the shares drift
/// from there with the spam.
async fn seed_representatives(
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
    representatives: &Representatives,
) {
    if representatives.len() < 2 {
        return;
    }
    let initial_key = account_map.initial_key().clone();
    let initial_account = initial_key.account();
    let share = INITIAL_AMOUNT / (representatives.len() as u128 + 1);
    // The first account delegating to each representative, the initial
    // account's representative included: it keeps a share of its own
    let mut seeds: Vec<Account> = Vec::new();
    for rep in representatives.iter() {
        let seed = account_map
            .accounts()
            .iter()
            .skip(1)
            .find(|account| representatives.of(account) == Some(*rep) && !seeds.contains(account))
            .copied();
        if let Some(seed) = seed {
            seeds.push(seed);
        }
    }
    let mut frontier = account_map
        .state(&initial_account)
        .unwrap()
        .confirmed_frontier;
    let mut balance = INITIAL_AMOUNT;
    for seed in seeds {
        balance -= share;
        let send: Block = StateBlockArgs {
            key: &initial_key,
            previous: frontier,
            representative: representative_of(&initial_account, representatives),
            balance,
            link: seed.into(),
            work: 0.into(),
        }
        .into();
        frontier = send.hash();
        info!(
            "Seeding Ӿ{} to {} for representative {}",
            share.format_balance(0),
            seed.encode_account(),
            representative_of(&seed, representatives)
        );
        genesis_rpc.process(JsonBlock::from(send)).await.unwrap();
        wait_until_confirmed(genesis_rpc, frontier).await;

        let seed_key = account_map.state(&seed).unwrap().key.clone();
        let receive: Block = StateBlockArgs {
            key: &seed_key,
            previous: BlockHash::ZERO,
            representative: representative_of(&seed, representatives),
            balance: share,
            link: frontier.into(),
            work: 0.into(),
        }
        .into();
        let receive_hash = receive.hash();
        genesis_rpc.process(JsonBlock::from(receive)).await.unwrap();
        wait_until_confirmed(genesis_rpc, receive_hash).await;
        account_map.set_account_state(seed, share, receive_hash);
    }
    account_map.set_account_state(initial_account, balance, frontier);
}

async fn wait_until_confirmed(rpc_client: &NanoRpcClient, hash: BlockHash) {
    info!("Waiting for confirmation for {hash}");
    loop {
        match rpc_client.block_info(hash).await {
            Ok(info) => {
                if info.confirmed.inner() {
                    break;
                }
            }
            Err(e) => {
                debug!("Got error: {e:?}")
            }
        }

        sleep(Duration::from_millis(100)).await;
    }
}
