use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use futures::future::join_all;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::{SendArgs, WalletAddArgs, WalletRepresentativeSetArgs};
use rsnano_types::{Amount, Block, BlockHash, JsonBlock, StateBlockArgs, WalletId, WorkNonce};

use crate::{
    domain::AccountMap,
    setup::{genesis_key, pr_key},
};

// Keep a small setup reserve on genesis for the optional priority accounts.
// Genesis delegates to PR0, so PR0's spam account receives a correspondingly
// smaller amount and the final representative weights remain balanced.
const SETUP_RESERVE: Amount = Amount::nano(100);

fn representative_target(pr_index: usize, pr_count: usize) -> Amount {
    let share = Amount::MAX / pr_count as u128;
    let remainder = Amount::MAX - share * pr_count as u128;
    let target_weight = share
        + if pr_index == pr_count - 1 {
            remainder
        } else {
            Amount::ZERO
        };

    if pr_index == 0 {
        target_weight - SETUP_RESERVE
    } else {
        target_weight
    }
}

fn spam_account_amount(account_index: usize, account_count: usize, pr_count: usize) -> Amount {
    let pr_index = account_index % pr_count;
    let accounts_for_pr = 1 + (account_count - 1 - pr_index) / pr_count;
    let target = representative_target(pr_index, pr_count);
    let base = target / accounts_for_pr as u128;
    let remainder = target - base * accounts_for_pr as u128;
    let position_for_pr = account_index / pr_count;

    base + if position_for_pr == accounts_for_pr - 1 {
        remainder
    } else {
        Amount::ZERO
    }
}

pub(crate) async fn create_wallets(
    rpc_clients: &[NanoRpcClient],
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
    fund_all_accounts: bool,
) -> anyhow::Result<WalletId> {
    let mut genesis_wallet = WalletId::ZERO;
    #[cfg(feature = "rai_protocol")]
    let mut setup_wallets = Vec::with_capacity(rpc_clients.len());
    let genesis_key = genesis_key();
    let pr_count = rpc_clients.len();
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        debug!("Creating wallet for PR{i}");
        let resp = rpc_client.wallet_create(None).await.unwrap();
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
            .unwrap();

        // RAI setup runs in epoch zero, whose fixed committee contains only
        // the genesis representative. Give every setup node that key so a
        // missed PR0 vote cannot strand a locally restarted election. Remove
        // these temporary copies before preserving the prepared wallets.
        #[cfg(feature = "rai_protocol")]
        if i != 0 {
            rpc_client
                .wallet_add(WalletAddArgs {
                    wallet: resp.wallet,
                    key: genesis_key.raw_key(),
                    work: None,
                })
                .await
                .unwrap();
        }

        debug!("Setting default representative for PR{i}");
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

    let funded_account_count = if fund_all_accounts {
        account_map.accounts().len()
    } else {
        pr_count
    };
    info!("Funding {funded_account_count} spam accounts...");
    let funded_accounts = Arc::new(AtomicUsize::new(0));
    let progress_cancel = CancellationToken::new();
    let progress_task = tokio::spawn(log_funding_progress(
        funded_accounts.clone(),
        funded_account_count,
        progress_cancel.clone(),
    ));
    for i in 0..funded_account_count {
        let spam_key = account_map
            .state(&account_map.accounts()[i])
            .unwrap()
            .key
            .clone();
        let pr_index = i % pr_count;
        let amount = spam_account_amount(i, funded_account_count, pr_count);
        let representative = pr_key(pr_index).public_key();
        debug!("Funding spam account {i} and delegating it to PR{pr_index}");
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
        wait_until_confirmed_on_all(rpc_clients, send_hash).await?;
        let receive: Block = StateBlockArgs {
            key: &spam_key,
            previous: BlockHash::ZERO,
            // Opening receives the funding send and delegates its balance in
            // the same block. All PR keys are temporarily present on PR0 while
            // setup redistributes the genesis voting weight.
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
        wait_until_confirmed_on_all(rpc_clients, receive_hash).await?;

        account_map.set_account_state(spam_key.account(), amount, receive_hash);
        account_map.set_representative(spam_key.account(), representative);
        funded_accounts.store(i + 1, Ordering::Relaxed);
    }
    progress_cancel.cancel();
    let _ = progress_task.await;
    info!("Funded all {funded_account_count} spam accounts");

    for i in 1..pr_count {
        genesis_rpc
            .account_remove(genesis_wallet, pr_key(i).account())
            .await
            .unwrap();
        #[cfg(feature = "rai_protocol")]
        rpc_clients[i]
            .account_remove(setup_wallets[i], genesis_key.account())
            .await
            .unwrap();
    }

    Ok(genesis_wallet)
}

async fn log_funding_progress(
    funded_accounts: Arc<AtomicUsize>,
    total: usize,
    cancel: CancellationToken,
) {
    let started = Instant::now();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = sleep(Duration::from_secs(1)) => {
                let funded = funded_accounts.load(Ordering::Relaxed);
                info!(
                    "Funded {funded}/{total} spam accounts | setup elapsed: {:.1}s",
                    started.elapsed().as_secs_f64()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entire_supply_is_balanced_between_prs() {
        let pr_count = 6;
        let account_count = 100;
        let amounts = (0..account_count)
            .map(|i| spam_account_amount(i, account_count, pr_count))
            .collect::<Vec<_>>();

        assert_eq!(
            amounts.iter().copied().sum::<Amount>(),
            Amount::MAX - SETUP_RESERVE
        );

        let mut weights = vec![Amount::ZERO; pr_count];
        weights[0] += SETUP_RESERVE;
        for (i, amount) in amounts.into_iter().enumerate() {
            weights[i % pr_count] += amount;
        }
        for weight in &weights[..pr_count - 1] {
            assert_eq!(*weight, weights[0]);
        }
        assert_eq!(weights[pr_count - 1] - weights[0], Amount::raw(3));
    }

    #[test]
    fn rotating_all_funded_accounts_keeps_six_pr_weights_nearly_equal() {
        let pr_count = 6;
        let account_count = 100;
        let mut weights = vec![Amount::ZERO; pr_count];
        // The genesis account is not part of the one-change-per-account
        // workload and continues to delegate the setup reserve to PR0.
        weights[0] = SETUP_RESERVE;
        for account_index in 0..account_count {
            let source_pr = account_index % pr_count;
            let destination_pr = (source_pr + 1) % pr_count;
            weights[destination_pr] += spam_account_amount(account_index, account_count, pr_count);
        }

        let share = Amount::MAX / pr_count as u128;
        assert_eq!(weights[0], share + SETUP_RESERVE + Amount::raw(3));
        assert_eq!(weights[1], share - SETUP_RESERVE);
        assert_eq!(&weights[2..], vec![share; 4]);
        let minimum = *weights.iter().min().unwrap();
        let maximum = *weights.iter().max().unwrap();
        assert_eq!(maximum - minimum, SETUP_RESERVE * 2 + Amount::raw(3));
    }
}

const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) async fn wait_until_confirmed_on_all(
    rpc_clients: &[NanoRpcClient],
    hash: BlockHash,
) -> anyhow::Result<()> {
    let block = rpc_clients[0]
        .block_info(hash)
        .await
        .with_context(|| format!("source node lost setup block {hash}"))?
        .contents;
    let results = join_all(
        rpc_clients
            .iter()
            .enumerate()
            .map(|(index, rpc_client)| wait_until_confirmed(rpc_client, index, hash, &block)),
    )
    .await;
    for result in results {
        result?;
    }
    Ok(())
}

async fn wait_until_confirmed(
    rpc_client: &NanoRpcClient,
    node_index: usize,
    hash: BlockHash,
    block: &JsonBlock,
) -> anyhow::Result<()> {
    debug!("Waiting for confirmation for {hash} on PR{node_index}");
    let started = Instant::now();
    let mut last_confirm_request = None;
    let mut last_process_request = None;
    loop {
        if started.elapsed() >= CONFIRMATION_TIMEOUT {
            bail!(
                "setup block {hash} was not confirmed on PR{node_index} within {CONFIRMATION_TIMEOUT:?}"
            );
        }
        match rpc_client.block_info(hash).await {
            Ok(info) => {
                if info.confirmed.inner() {
                    return Ok(());
                }
                if last_confirm_request
                    .is_none_or(|last: Instant| last.elapsed() >= Duration::from_secs(1))
                {
                    // A block can arrive after its original election has already
                    // finished on another PR. Start a local election so this
                    // node does not wait forever with an uncemented block.
                    let _ = rpc_client.block_confirm(hash).await;
                    last_confirm_request = Some(Instant::now());
                }
            }
            Err(e) => {
                debug!("PR{node_index} does not have setup block {hash}: {e:?}");
                if last_process_request
                    .is_none_or(|last: Instant| last.elapsed() >= Duration::from_secs(1))
                {
                    // Setup blocks originate on PR0. Epidemic publish is not a
                    // delivery guarantee, so explicitly repair a peer which
                    // missed the original broadcast before asking it to
                    // confirm the block.
                    if let Err(error) = rpc_client.process(block.clone()).await {
                        warn!("Could not repair setup block {hash} on PR{node_index}: {error:?}");
                    }
                    last_process_request = Some(Instant::now());
                }
            }
        }

        sleep(Duration::from_millis(100)).await;
    }
}
