use std::{sync::Mutex, time::Duration};

use rsnano_nullable_clock::SteadyClock;
use rsnano_rpc_client::NanoRpcClient;
use tokio::{select, time::interval};
use tokio_util::sync::CancellationToken;

use crate::domain::spam_logic::SpamLogic;

const RECONCILE_INTERVAL: Duration = Duration::from_millis(250);
const RPC_BATCH_SIZE: usize = 128;

/// Reconciles websocket confirmation tracking with the node's durable ledger.
/// Websocket delivery is intentionally the low-latency path, but it is not a
/// durable completion signal: a disconnect or an implicitly cemented dependency
/// can otherwise leave nanospam waiting after every requested block is confirmed.
pub(crate) async fn reconcile_confirmations(
    rpc_client: &NanoRpcClient,
    logic: &Mutex<SpamLogic>,
    clock: &SteadyClock,
    cancel_token: CancellationToken,
    expected_cemented: Option<u64>,
) {
    let mut ticker = interval(RECONCILE_INTERVAL);
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

        if let Some(expected) = expected_cemented
            && let Ok(count) = rpc_client.block_count().await
            && count.count.inner() == count.cemented.inner()
            && count.cemented.inner() >= expected
        {
            logic.lock().unwrap().mark_workload_cemented(clock.now());
            // Wake the publisher if it is waiting on an empty channel. Sender
            // clones are retained by the optional high-priority and delayed
            // republish paths, so channel closure is not a reliable workload
            // completion signal.
            cancel_token.cancel();
            return;
        }

        let outstanding = logic.lock().unwrap().delayed.hashes();
        for hashes in outstanding.chunks(RPC_BATCH_SIZE) {
            let Ok(response) = rpc_client.blocks_info(hashes.to_vec()).await else {
                continue;
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
