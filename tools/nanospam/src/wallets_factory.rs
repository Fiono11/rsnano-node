use std::time::Duration;

use anyhow::Context;
use tokio::time::sleep;
#[cfg(not(feature = "rai_protocol"))]
use tracing::debug;
use tracing::info;

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
) -> anyhow::Result<WalletId> {
    let mut genesis_wallet = WalletId::ZERO;
    #[cfg(feature = "rai_protocol")]
    let mut setup_wallets = Vec::with_capacity(rpc_clients.len());
    let genesis_key = genesis_key();
    let pr_count = rpc_clients.len();
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        info!("Creating wallet...");
        let resp = rpc_client
            .wallet_create(None)
            .await
            .with_context(|| format!("failed to create wallet on PR{i}"))?;
        #[cfg(feature = "rai_protocol")]
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
            .with_context(|| format!("failed to add representative key on PR{i}"))?;

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
                .with_context(|| format!("failed to add setup genesis key on PR{i}"))?;
        }

        info!("Setting default representative...");
        rpc_client
            .wallet_representative_set(WalletRepresentativeSetArgs {
                wallet: resp.wallet,
                representative: pr_key.account(),
                update_existing_accounts: Some(false.into()),
            })
            .await
            .with_context(|| format!("failed to set default representative on PR{i}"))?;

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
                .with_context(|| format!("failed to fund PR{i}"))?
                .block;
            #[cfg(not(feature = "rai_protocol"))]
            wait_until_confirmed(rpc_client, send_hash).await;
            #[cfg(feature = "rai_protocol")]
            wait_until_confirmed_on_all(rpc_clients, send_hash)
                .await
                .with_context(|| format!("failed to confirm PR{i} funding send {send_hash}"))?;

            info!("Receiving...");
            // trigger wallet receive to speed things up
            let recv_hash = match rpc_client
                .receive(ReceiveArgs {
                    wallet: resp.wallet,
                    account: pr_key.account(),
                    block: send_hash,
                    work: Some(WorkNonce::new(0)),
                })
                .await
            {
                Ok(response) => response.block,
                Err(error) => {
                    // The wallet may have auto-received the send before this
                    // explicit request reaches it. In that case the RPC reports
                    // "Block is not receivable", but the account frontier is
                    // the receive block we need to confirm.
                    tracing::debug!(pr = i, %send_hash, ?error, "explicit receive did not create a block; checking account frontier");
                    rpc_client
                        .account_info(pr_key.account())
                        .await
                        .with_context(|| {
                            format!(
                                "failed to receive PR{i} funding send {send_hash}: {error}"
                            )
                        })?
                        .frontier
                }
            };
            #[cfg(not(feature = "rai_protocol"))]
            wait_until_confirmed(rpc_client, recv_hash).await;
            #[cfg(feature = "rai_protocol")]
            wait_until_confirmed_on_all(rpc_clients, recv_hash)
                .await
                .with_context(|| format!("failed to confirm PR{i} funding receive {recv_hash}"))?;

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
        .context("failed to send initial spam amount")?
        .block;
    #[cfg(not(feature = "rai_protocol"))]
    wait_until_confirmed(genesis_rpc, genesis_send).await;
    #[cfg(feature = "rai_protocol")]
    wait_until_confirmed_on_all(rpc_clients, genesis_send)
        .await
        .with_context(|| format!("failed to confirm initial spam send {genesis_send}"))?;
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
        .context("failed to process initial spam receive")?;

    #[cfg(not(feature = "rai_protocol"))]
    wait_until_confirmed(genesis_rpc, recv.hash).await;
    #[cfg(feature = "rai_protocol")]
    wait_until_confirmed_on_all(rpc_clients, recv.hash)
        .await
        .with_context(|| format!("failed to confirm initial spam receive {}", recv.hash))?;

    account_map.set_account_state(
        initial_key.account(),
        INITIAL_AMOUNT,
        genesis_receive.hash(),
    );

    #[cfg(feature = "rai_protocol")]
    for i in 1..pr_count {
        rpc_clients[i]
            .account_remove(setup_wallets[i], genesis_key.account())
            .await
            .with_context(|| format!("failed to remove setup genesis key from PR{i}"))?;
    }

    Ok(genesis_wallet)
}

#[cfg(not(feature = "rai_protocol"))]
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

pub(crate) async fn wait_until_confirmed_on_all(
    rpc_clients: &[NanoRpcClient],
    hash: BlockHash,
) -> anyhow::Result<()> {
    const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(60);
    let started = std::time::Instant::now();
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
        if started.elapsed() >= CONFIRMATION_TIMEOUT {
            return Err(anyhow::anyhow!(
                "setup block {hash} was not observed by any PR within {} seconds",
                CONFIRMATION_TIMEOUT.as_secs()
            ));
        }
        sleep(Duration::from_millis(100)).await;
    };
    let mut last_confirm_request = None;
    loop {
        let mut unconfirmed = Vec::new();
        for (index, client) in rpc_clients.iter().enumerate() {
            match client.block_info(hash).await {
                Ok(info) if info.confirmed.inner() => {}
                Ok(_) => unconfirmed.push(index),
                Err(_) => {
                    // Setup traffic can arrive after the originating election
                    // has ended. Repair the missing block before requesting a
                    // fresh committee-wide election.
                    if let Err(error) = client.process(block.clone()).await
                        && !error.to_string().contains("Old block")
                    {
                        tracing::warn!(pr = index, %hash, ?error, "failed to repair missing setup block");
                    }
                    unconfirmed.push(index);
                }
            }
        }
        if unconfirmed.is_empty() {
            return Ok(());
        }

        if started.elapsed() >= CONFIRMATION_TIMEOUT {
            return Err(anyhow::anyhow!(
                "setup block {hash} was not confirmed within {} seconds on PRs {unconfirmed:?}",
                CONFIRMATION_TIMEOUT.as_secs()
            ));
        }

        if last_confirm_request
            .is_none_or(|last: std::time::Instant| last.elapsed() >= Duration::from_secs(1))
        {
            // Restart the election on the whole committee. A lagging replica may
            // receive a setup block after peers have already dropped its election.
            // Re-process on every lagging PR first so the restarted election has
            // its block and dependency context available locally.
            for index in &unconfirmed {
                if let Err(error) = rpc_clients[*index].process(block.clone()).await
                    && !error.to_string().contains("Old block")
                {
                    tracing::warn!(pr = *index, %hash, ?error, "failed to reprocess setup block");
                }
            }
            for (index, client) in rpc_clients.iter().enumerate() {
                if let Err(error) = client.block_confirm(hash).await {
                    tracing::warn!(pr = index, %hash, ?error, "failed to request setup block confirmation");
                }
            }
            last_confirm_request = Some(std::time::Instant::now());
        }

        sleep(Duration::from_millis(100)).await;
    }
}
