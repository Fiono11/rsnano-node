use std::time::Duration;

use tokio::time::sleep;
use tracing::{debug, info};

use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::{SendArgs, WalletAddArgs, WalletRepresentativeSetArgs};
use rsnano_types::{Amount, Block, BlockHash, JsonBlock, StateBlockArgs, WalletId, WorkNonce};

use crate::{
    domain::AccountMap,
    setup::{genesis_key, pr_key},
};

// Keep a small setup reserve on genesis for the optional priority accounts.
// The remaining supply is split evenly across one spam account per PR.
const SETUP_RESERVE: Amount = Amount::nano(100_000_000);

pub(crate) async fn create_wallets(
    rpc_clients: &[NanoRpcClient],
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
) -> WalletId {
    let mut genesis_wallet = WalletId::ZERO;
    let genesis_key = genesis_key();
    let pr_count = rpc_clients.len();
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
    }

    // During stake redistribution the genesis representative loses almost all
    // of its weight. Temporarily make every destination representative
    // available on PR0 so the setup chain cannot strand itself between weight
    // changes and the periodic local-representative refresh. These extra keys
    // are removed again before the prepared ledgers are handed to `run`.
    for i in 1..pr_count {
        genesis_rpc
            .wallet_add(WalletAddArgs {
                wallet: genesis_wallet,
                key: pr_key(i).raw_key(),
                work: None,
            })
            .await
            .unwrap();
    }
    if pr_count > 1 {
        sleep(Duration::from_secs(11)).await;
    }

    let distributable = Amount::MAX - SETUP_RESERVE;
    let share = distributable / pr_count as u128;
    let remainder = distributable - share * pr_count as u128;
    for i in 0..pr_count {
        let spam_key = account_map
            .state(&account_map.accounts()[i])
            .unwrap()
            .key
            .clone();
        let amount = share
            + if i == pr_count - 1 {
                remainder
            } else {
                Amount::ZERO
            };
        let representative = pr_key(i).public_key();
        info!("Funding spam account {i} and delegating it to PR{i}...");
        let send_hash = genesis_rpc
            .send(SendArgs {
                wallet: genesis_wallet,
                source: genesis_key.account(),
                destination: spam_key.account(),
                amount,
                work: Some(WorkNonce::new(0)),
                id: None,
            })
            .await
            .unwrap()
            .block;
        wait_until_confirmed_on_all(rpc_clients, send_hash).await;
        let receive: Block = StateBlockArgs {
            key: &spam_key,
            previous: BlockHash::ZERO,
            representative,
            balance: amount,
            link: send_hash.into(),
            work: 0.into(),
        }
        .into();
        let receive_hash = genesis_rpc
            .process(JsonBlock::from(receive.clone()))
            .await
            .unwrap()
            .hash;
        wait_until_confirmed_on_all(rpc_clients, receive_hash).await;
        account_map.set_account_state(spam_key.account(), amount, receive.hash());
        account_map.set_representative(spam_key.account(), representative);
    }

    for i in 1..pr_count {
        genesis_rpc
            .account_remove(genesis_wallet, pr_key(i).account())
            .await
            .unwrap();
    }

    genesis_wallet
}

async fn wait_until_confirmed_on_all(rpc_clients: &[NanoRpcClient], hash: BlockHash) {
    for rpc_client in rpc_clients {
        wait_until_confirmed(rpc_client, hash).await;
    }
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
