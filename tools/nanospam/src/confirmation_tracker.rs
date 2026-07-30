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

        if let Some(expected) = expected_cemented
            && let Ok(count) = rpc_client.block_count().await
            && count.count.inner() == count.cemented.inner()
            && count.cemented.inner() >= expected
        {
            logic.lock().unwrap().mark_workload_cemented(clock.now());
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
        }
    }
}
