use std::{sync::Mutex, time::Duration};

use rsnano_nullable_clock::SteadyClock;
use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::BlocksInfoArgs;
use tokio::{
    select,
    time::{MissedTickBehavior, interval, timeout},
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::domain::spam_logic::SpamLogic;

const RECONCILE_INTERVAL: Duration = Duration::from_millis(250);
const RPC_BATCH_SIZE: usize = 128;
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Reconciles websocket confirmation tracking with the node's durable ledger.
/// Websocket delivery is intentionally the low-latency path, but it is not a
/// durable completion signal: a disconnect or an implicitly cemented dependency
/// can otherwise leave nanospam waiting after every requested block is confirmed.
pub(crate) async fn reconcile_confirmations(
    rpc_client: &NanoRpcClient,
    logic: &Mutex<SpamLogic>,
    clock: &SteadyClock,
    cancel_token: CancellationToken,
) {
    let mut ticker = interval(RECONCILE_INTERVAL);
    // A full outstanding-hash sweep may take longer than the interval. Do not
    // replay every missed tick in a burst and turn reconciliation into a
    // continuous RPC load precisely while the node is catching up.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;

    loop {
        select! {
            _ = cancel_token.cancelled() => return,
            _ = ticker.tick() => {}
        }

        // The websocket receiver may have observed the final confirmation
        // before this reconciliation tick. In that case there are no newly
        // confirmed outstanding hashes below, but the publisher still needs
        // to be woken from rx_blocks.recv().
        if logic.lock().unwrap().is_finished() {
            cancel_token.cancel();
            return;
        }

        let outstanding = logic.lock().unwrap().delayed.hashes();
        for hashes in outstanding.chunks(RPC_BATCH_SIZE) {
            let args = BlocksInfoArgs {
                receivable: None,
                receive_hash: None,
                source: None,
                include_not_found: Some(true.into()),
                include_linked_account: None,
                hashes: hashes.to_vec(),
            };
            let request = select! {
                _ = cancel_token.cancelled() => return,
                result = timeout(RPC_TIMEOUT, rpc_client.blocks_info(args)) => result,
            };
            let response = match request {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    warn!("RPC error while checking delayed blocks: {error}");
                    continue;
                }
                Err(error) => {
                    warn!("RPC status poll for delayed blocks timed out: {error}");
                    continue;
                }
            };
            let confirmed = response
                .blocks
                .into_iter()
                .filter_map(|(hash, info)| info.confirmed.inner().then_some(hash))
                .collect::<Vec<_>>();
            if confirmed.is_empty() {
                continue;
            }

            let now = clock.now();
            let mut logic = logic.lock().unwrap();
            for hash in confirmed {
                logic.confirmed(&hash, now);
            }
            if logic.is_finished() {
                // Do not leave publish_blocks asleep in rx_blocks.recv() after
                // the last confirmation arrives through reconciliation.
                cancel_token.cancel();
                return;
            }
        }
    }
}
