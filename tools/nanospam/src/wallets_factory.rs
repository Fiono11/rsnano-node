use std::time::Duration;

use tokio::time::sleep;
use tracing::{debug, info};

use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::{ReceiveArgs, SendArgs, WalletAddArgs, WalletRepresentativeSetArgs};
use rsnano_types::{Amount, Block, BlockHash, JsonBlock, StateBlockArgs, WalletId, WorkNonce};

use crate::{
    domain::AccountMap,
    setup::{genesis_key, pr_key},
};

const INITIAL_AMOUNT: Amount = Amount::nano(100_000_000);

pub(crate) async fn create_wallets(
    rpc_clients: &[NanoRpcClient],
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
) -> WalletId {
    let mut genesis_wallet = WalletId::ZERO;
    let genesis_key = genesis_key();
    let pr_count = rpc_clients.len();
    let spam_representatives: Vec<_> = (0..pr_count).map(|i| pr_key(i).public_key()).collect();
    account_map.assign_representatives_round_robin(&spam_representatives);
    let initial_representative = spam_representatives[0];
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
            let pr_balance = (Amount::MAX - INITIAL_AMOUNT) / pr_count as u128;
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
            // The recipient must locally confirm the send before its wallet can
            // create the receive block.
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
            // PR0 is the confirmation source used by the spam run's websocket
            // tracker. RAI followers can apply a receive block before locally
            // cementing it, so waiting on the recipient can stall forever even
            // though PR0 has already finalized the block.
            wait_until_confirmed(genesis_rpc, recv_hash).await;
            info!("DONE");
            info!(
                "********************************************************************************"
            );
        }
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
    let genesis_receive: Block = StateBlockArgs {
        key: &initial_key,
        previous: BlockHash::ZERO,
        representative: initial_representative,
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

    account_map.set_account_state_with_representative(
        initial_key.account(),
        INITIAL_AMOUNT,
        genesis_receive.hash(),
        initial_representative,
    );

    genesis_wallet
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
