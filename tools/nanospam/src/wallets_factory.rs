use std::time::Duration;

use tokio::time::sleep;
use tracing::info;

use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::{ReceiveArgs, SendArgs, WalletAddArgs, WalletRepresentativeSetArgs};
use rsnano_types::{Amount, Block, BlockHash, JsonBlock, StateBlockArgs, WalletId, WorkNonce};

use crate::{
    domain::AccountMap,
    setup::{genesis_key, pr_balance_weights, pr_key},
};

const INITIAL_AMOUNT: Amount = Amount::nano(100_000_000);

pub(crate) async fn create_wallets(
    rpc_clients: &[NanoRpcClient],
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
    fork_recipients: usize,
) -> WalletId {
    let mut genesis_wallet = WalletId::ZERO;
    let mut setup_wallets = Vec::with_capacity(rpc_clients.len());
    let genesis_key = genesis_key();
    let pr_count = rpc_clients.len();
    let balance_total = Amount::MAX - INITIAL_AMOUNT;
    let balances = pr_balance_weights(balance_total, pr_count, fork_recipients);
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        info!("Creating wallet...");
        let resp = rpc_client.wallet_create(None).await.unwrap();
        setup_wallets.push(resp.wallet);
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

        // RAI starts in a fixed epoch-zero committee containing the genesis
        // representative. Keep that signer locally available while setup
        // redistributes stake and peers are still learning the new committee.
        #[cfg(feature = "rai_protocol")]
        if i > 0 {
            rpc_client
                .wallet_add(WalletAddArgs {
                    wallet: resp.wallet,
                    key: genesis_key.raw_key(),
                    work: None,
                })
                .await
                .unwrap();

            // PR0 creates the redistribution chain. Let it sign for each new
            // representative until the periodic local-representative refresh
            // observes the transferred voting weight.
            genesis_rpc
                .wallet_add(WalletAddArgs {
                    wallet: genesis_wallet,
                    key: pr_key.raw_key(),
                    work: None,
                })
                .await
                .unwrap();
        }

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

    // All fixed-committee signing keys now exist. Bootstrap ledger balances only after
    // every PR can participate in the elections that confirm these transfers.
    for (i, rpc_client) in rpc_clients.iter().enumerate().skip(1) {
        let pr_balance = balances[i];
        let pr_key = pr_key(i);
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
        wait_until_confirmed_on_all(rpc_clients, send_hash)
            .await
            .unwrap();

        info!("Receiving...");
        // trigger wallet receive to speed things up
        let _ = rpc_client
            .receive(ReceiveArgs {
                wallet: setup_wallets[i],
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
        wait_until_confirmed_on_all(rpc_clients, recv_hash)
            .await
            .unwrap();

        info!("DONE");
        info!("********************************************************************************");
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
    wait_until_confirmed_on_all(rpc_clients, genesis_send)
        .await
        .unwrap();
    info!("Receiving initial spam amount...");
    let initial_representative = initial_key.public_key();
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

    wait_until_confirmed_on_all(rpc_clients, recv.hash)
        .await
        .unwrap();

    account_map.set_account_state(
        initial_key.account(),
        INITIAL_AMOUNT,
        genesis_receive.hash(),
    );

    #[cfg(feature = "rai_protocol")]
    for i in 1..pr_count {
        genesis_rpc
            .account_remove(genesis_wallet, pr_key(i).account())
            .await
            .unwrap();
        rpc_clients[i]
            .account_remove(setup_wallets[i], genesis_key.account())
            .await
            .unwrap();
    }

    genesis_wallet
}

pub(crate) fn expected_pr_weights(
    prs: usize,
    fork_recipients: usize,
) -> Vec<(rsnano_types::Account, Amount)> {
    pr_balance_weights(Amount::MAX - INITIAL_AMOUNT, prs, fork_recipients)
        .into_iter()
        .enumerate()
        .map(|(i, weight)| (pr_key(i).account(), weight))
        .collect()
}

pub(crate) async fn wait_until_confirmed_on_all(
    rpc_clients: &[NanoRpcClient],
    hash: BlockHash,
) -> anyhow::Result<()> {
    let block = loop {
        let mut found = None;
        for client in rpc_clients {
            if let Ok(info) = client.block_info(hash).await {
                found = Some(info.contents);
                break;
            }
        }
        if let Some(block) = found {
            break block;
        }
        sleep(Duration::from_millis(100)).await;
    };
    let mut last_confirm_request = None;
    loop {
        let mut all_confirmed = true;
        for client in rpc_clients {
            match client.block_info(hash).await {
                Ok(info) if info.confirmed.inner() => {}
                Ok(_) => all_confirmed = false,
                Err(_) => {
                    // Setup traffic can arrive after the originating election
                    // has ended. Repair the missing block before requesting a
                    // fresh committee-wide election.
                    let _ = client.process(block.clone()).await;
                    all_confirmed = false;
                }
            }
        }
        if all_confirmed {
            return Ok(());
        }

        if last_confirm_request
            .is_none_or(|last: std::time::Instant| last.elapsed() >= Duration::from_secs(1))
        {
            // Restart the election on the whole committee. A lagging replica may
            // receive a setup block after peers have already dropped its election.
            for client in rpc_clients {
                let _ = client.block_confirm(hash).await;
            }
            last_confirm_request = Some(std::time::Instant::now());
        }

        sleep(Duration::from_millis(100)).await;
    }
}
