use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread::yield_now,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use num_format::{Locale, ToFormattedString};
use rand::{RngExt, rng};
use tokio::{
    io::{AsyncReadExt, ReadHalf, WriteHalf},
    select,
    sync::mpsc,
    task::JoinSet,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::info;

use rsnano_messages::{Message, MessageSerializer, Publish};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_tcp::{TcpStream, TcpStreamFactory};
use rsnano_nullable_tracing_subscriber::TracingInitializer;
use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::{AccountHistoryArgs, ProcessArgs};
use rsnano_types::{Account, BlockHash, NetworkType, PrivateKey, ProtocolInfo, RawKey, WalletId};
use rsnano_websocket_messages::{BlockConfirmed, MessageEnvelope, Topic};

use crate::{
    block_publisher::BlockPublisher,
    byzantine::{RecentBlocks, byzantine_keys, run_byzantine},
    cli_args::CliArgs,
    confirmation_receiver::ConfirmationReceiver,
    domain::{BlockResult, Forks, RetrySchedule, spam_logic::SpamLogic},
    frontiers_sync::sync_frontiers,
    handshake::perform_handshake,
    high_prio_check::HighPrioCheck,
    node_lifetime::NodeLifetime,
    node_writers::NodeWriters,
    restart::Restarter,
    rotation::run_rotations,
    setup::{
        NodeHosts, configure_nodes, create_account_map, get_genesis_hash, peering_port,
        remote_nodes, restore_account_keys, restore_node_ledgers, rpc_port, start_nodes,
    },
    wallets_factory::{create_wallets, prefund_accounts},
    weight_shift::{WeightShifts, run_weight_shifts},
};

const MAX_BUFFERED_BLOCKS: usize = 1024;
/// A setup wait first republishes and re-confirms after this long, then
/// every SETUP_RECOVERY_EVERY
const SETUP_RECOVERY_FIRST: Duration = Duration::from_secs(10);
const SETUP_RECOVERY_EVERY: Duration = Duration::from_secs(5);
const REPUBLISH_TASKS: usize = 32;
const CONNECTIONS_PER_NODE: usize = 4;
const PUBLISH_DRAIN_LIMIT: Duration = Duration::from_secs(30);

pub(crate) struct NanoSpamApp {
    tracing_init: TracingInitializer,
    tcp_stream_factory: TcpStreamFactory,
    clock: SteadyClock,
    rpc_clients: Vec<NanoRpcClient>,
    node_lifetime: NodeLifetime,
    args: CliArgs,
}

impl NanoSpamApp {
    pub fn new(args: CliArgs) -> Self {
        Self {
            tracing_init: Default::default(),
            tcp_stream_factory: Default::default(),
            clock: Default::default(),
            rpc_clients: Default::default(),
            node_lifetime: Default::default(),
            args,
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        self.tracing_init.init();

        let protocol = ProtocolInfo::default_for(NetworkType::NanoTestNetwork);
        let genesis_hash = get_genesis_hash();

        let data_dir = match &self.args.data_dir {
            Some(path) => path.clone(),
            None => dirs::home_dir()
                .ok_or_else(|| anyhow!("No home dir found"))?
                .join("NanoSpam"),
        };

        if let Some(snapshot) = &self.args.ledger_snapshot {
            restore_account_keys(snapshot, &data_dir)?;
        }
        let mut account_map = create_account_map(&data_dir, self.args.accounts);

        if self.args.set_up_new_nodes() || self.args.ledger_snapshot.is_some() {
            configure_nodes(&self.args, &data_dir);
        }
        if let Some(snapshot) = &self.args.ledger_snapshot {
            restore_node_ledgers(snapshot, &data_dir, self.args.honest_prs())?;
        }

        // Only the representatives that run a node have an RPC; the offline and
        // Byzantine ones exist in the ledger only
        for i in 0..self.args.honest_prs() {
            let rpc_client = NanoRpcClient::new(
                format!("http://{}", self.args.hosts().socket(i, rpc_port(i)))
                    .parse()
                    .unwrap(),
            );
            self.rpc_clients.push(rpc_client);
        }

        let genesis_rpc = &self.rpc_clients[0];

        if !self.args.attach {
            let node_handles = start_nodes(&self.args, &data_dir, &self.rpc_clients).await;
            self.node_lifetime = NodeLifetime::new(
                node_handles,
                remote_nodes(&self.args, &data_dir),
                self.args.kill_nodes(),
            );
        }

        let representatives = self.args.representatives();
        let genesis_wallet_id = if self.args.set_up_new_nodes() {
            let (wallet, seeds) = create_wallets(
                &self.rpc_clients,
                genesis_rpc,
                &mut account_map,
                &representatives,
                &(0..self.args.prs)
                    .map(|i| self.args.pr_balance(i))
                    .collect::<Vec<_>>(),
            )
            .await;
            // Connected only now: a node that has just started may still be
            // shaking hands with its peers, and one address can only have so
            // many handshakes open
            let mut publisher = BlockPublisher::connect(
                &self.tcp_stream_factory,
                self.args.hosts(),
                self.args.honest_prs(),
            )
            .await?;
            prefund_accounts(
                genesis_rpc,
                &mut publisher,
                &mut account_map,
                &representatives,
                &seeds,
                self.args.prefund_count()?,
            )
            .await;
            publisher.close().await;
            wallet
        } else {
            WalletId::ZERO
        };

        if self.args.sync || self.args.ledger_snapshot.is_some() {
            sync_frontiers(&self.rpc_clients, &mut account_map).await;
        }

        let logic = Mutex::new(SpamLogic::new(account_map, self.args.spam_spec()?));

        let (tx_blocks, rx_blocks) = mpsc::channel::<Forks>(MAX_BUFFERED_BLOCKS);
        let mut high_prio_check = HighPrioCheck::new(genesis_rpc, &logic, representatives);

        if self.args.set_up_new_nodes() {
            high_prio_check
                .create_prio_accounts(genesis_wallet_id)
                .await?;
        }

        // A saved setup must hold the same ledger on every node
        if self.args.setup_only {
            return wait_until_nodes_agree(&self.args, &self.rpc_clients).await;
        }

        if self.args.sync {
            high_prio_check.sync_accounts().await?;
        }

        wait_until_nodes_agree(&self.args, &self.rpc_clients).await?;

        // RAI: the setup is over and every PR holds its share of the weight:
        // epoch 0 starts now on every PR.
        // One origin for all: a PR that gets the request later starts where
        // the others are. Started one PR after the other on their own clocks,
        // the last PR's boundaries lagged the first's by the sum of the
        // round trips, and every close waited for the latest report.
        if self.args.epoch_duration_ms > 0 {
            let origin_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis() as u64)
                .unwrap_or_default();
            for rpc_client in &self.rpc_clients {
                rpc_client.epoch_start_at(origin_unix_ms).await?;
            }
            info!("Started the epochs on every PR at unix ms {origin_unix_ms}");
        }
        let epochs_started = Instant::now();

        let mut tcp_writers = Vec::new();
        let mut tcp_readers = Vec::new();

        for node_index in 0..self.args.honest_prs() {
            let (node_readers, node_writers) =
                connect_node(&self.tcp_stream_factory, self.args.hosts(), node_index).await?;
            tcp_writers.push(Some(node_writers));
            tcp_readers.push(node_readers);
        }
        let (tx_reconnected, rx_reconnected) = mpsc::unbounded_channel();

        let recent_blocks = RecentBlocks::default();
        let tx_forks_clone = tx_blocks.clone();
        let cancel_block_creation = CancellationToken::new();
        let cancel_block_creation2 = cancel_block_creation.clone();
        let cancel_nanospam = CancellationToken::new();

        let (tx_ws_msg, rx_ws_msg) = std::sync::mpsc::channel::<(MessageEnvelope, Timestamp)>();

        info!("Connecting to websocket...");
        let mut conf_receiver = ConfirmationReceiver::connect(self.args.hosts()).await?;

        info!("Starting with {} BPS", logic.lock().unwrap().current_bps);

        let started = Instant::now();
        let restarter = Restarter {
            args: &self.args,
            data_dir: &data_dir,
            rpc_clients: &self.rpc_clients,
            node_lifetime: &self.node_lifetime,
            tcp_stream_factory: &self.tcp_stream_factory,
            reconnected: tx_reconnected,
            spam_started: started,
            down: Duration::from_millis(self.args.restart_down_ms),
        };
        std::thread::scope(|s| {
            s.spawn(|| {
                enqueue_blocks(&logic, tx_blocks, &self.clock);
                cancel_block_creation2.cancel();
            });

            s.spawn(|| track_confirmations(rx_ws_msg, &logic));

            tokio_scoped::scope(|scope| {
                scope.spawn(log_status(&logic, &self.clock, cancel_nanospam.clone()));

                if self.args.high_prio_check() {
                    scope.spawn(high_prio_check.run(cancel_block_creation, tx_forks_clone.clone()));
                }

                scope.spawn(conf_receiver.run(cancel_nanospam.clone(), tx_ws_msg, &self.clock));
                scope.spawn(receive_messages(
                    tcp_readers,
                    protocol,
                    cancel_nanospam.clone(),
                ));
                if self.args.byzantine > 0 {
                    scope.spawn(run_byzantine(
                        byzantine_keys(self.args.prs, self.args.byzantine),
                        (self.args.epoch_duration_ms > 0).then(|| {
                            (
                                epochs_started,
                                Duration::from_millis(self.args.epoch_duration_ms),
                            )
                        }),
                        self.args.honest_prs(),
                        self.args.hosts(),
                        protocol,
                        genesis_hash,
                        recent_blocks.clone(),
                        cancel_nanospam.clone(),
                        &self.tcp_stream_factory,
                    ));
                }

                if let Some(schedule) = &self.args.rotation {
                    scope.spawn(run_rotations(
                        &self.rpc_clients,
                        schedule.clone(),
                        epochs_started,
                        Duration::from_millis(self.args.rotation_period_ms()),
                    ));
                }

                if self.args.weight_shift_percent > 0 {
                    scope.spawn(run_weight_shifts(
                        &self.rpc_clients,
                        WeightShifts::new(self.args.prs, self.args.weight_shift_percent),
                        self.args.weight_shift_period(),
                        cancel_nanospam.clone(),
                    ));
                }

                for spec in &self.args.restart {
                    scope.spawn(restarter.run(*spec, cancel_nanospam.clone()));
                }

                scope.spawn(publish_blocks(
                    rx_blocks,
                    tcp_writers,
                    rx_reconnected,
                    protocol,
                    &logic,
                    cancel_nanospam,
                    self.args.drop_probability(),
                    &self.clock,
                    recent_blocks.clone(),
                ));

                if !self.args.no_republish {
                    scope.spawn(republish_delayed_blocks(
                        tx_forks_clone,
                        &logic,
                        &self.clock,
                    ));
                }
            });
        });
        let duration_secs = started.elapsed().as_secs_f64();
        let logic = logic.lock().unwrap();
        let created_blocks = logic.block_factory.created();
        let cps = (created_blocks as f64 / duration_secs) as i32;
        info!("Confirming {created_blocks} blocks took {duration_secs:.2}s");
        info!("Confirmation rate: {cps} cps");
        let conf_time = logic.sum_conf_time_total.as_millis() / created_blocks as u128;
        info!("Average conf time: {conf_time} ms");
        // The blocks published without a fork alternative: the primary
        // throughput and latency, comparable across fork variants. Forks are
        // counted apart and do not hold the run open.
        let nonfork_cps = (logic.nonfork_confirmed as f64 / duration_secs) as i32;
        info!(
            "Non-fork confirmation rate: {nonfork_cps} cps ({} of {} non-fork blocks, {} of {} forks)",
            logic.nonfork_confirmed,
            logic.nonfork_created,
            logic.fork_confirmed,
            logic.fork_created
        );
        let metrics = serde_json::json!({
            "created": created_blocks,
            "confirmed": logic.confirmed_total,
            "duration_secs": duration_secs,
            "confirmation_unit": "primary publication or its confirmed fork alternative",
            "alternative_confirmed": logic.alternative_confirmed,
            "confirmation_histogram_ms": logic.confirmation_histogram_ms,
            "nonfork_created": logic.nonfork_created,
            "nonfork_confirmed": logic.nonfork_confirmed,
            "nonfork_histogram_ms": logic.nonfork_histogram_ms,
            "fork_created": logic.fork_created,
            "fork_confirmed": logic.fork_confirmed,
            "fork_unresolved_at_end": logic.fork_created - logic.fork_confirmed,
            "series_start_unix_ms": logic.series_start_unix_ms,
            "series_columns": ["second", "published", "nonfork_confirmed", "nonfork_p50_ms", "nonfork_p95_ms"],
            "nonfork_series": logic.series_json(),
            "nonfork_by_publish": logic.publish_histograms_json(),
        });
        info!("RAI_BENCH_METRICS {metrics}");

        Ok(())
    }
}

fn enqueue_blocks(logic: &Mutex<SpamLogic>, tx_blocks: mpsc::Sender<Forks>, clock: &SteadyClock) {
    loop {
        let now = clock.now();

        let result = {
            let mut l = logic.lock().unwrap();
            let is_fork = crate::workload_rng::with_rng(|r| r.random_bool(l.fork_propability()));
            l.next_block(is_fork, now)
        };

        match result {
            Some(BlockResult::Block(forks)) => {
                tx_blocks.blocking_send(forks).unwrap();
            }
            Some(BlockResult::Waiting) => {
                yield_now();
                continue;
            }
            None => {
                break;
            }
        };
    }
}

async fn publish_blocks(
    mut rx_blocks: mpsc::Receiver<Forks>,
    tcp_streams: Vec<Option<Vec<WriteHalf<TcpStream>>>>,
    mut reconnected: mpsc::UnboundedReceiver<(usize, Vec<WriteHalf<TcpStream>>)>,
    protocol: ProtocolInfo,
    logic: &Mutex<SpamLogic>,
    cancel_token: CancellationToken,
    drop_probability: f64,
    clock: &SteadyClock,
    recent_blocks: RecentBlocks,
) {
    let mut serializer = MessageSerializer::new(protocol);
    let mut fork_serializer = MessageSerializer::new(protocol);
    let writers = NodeWriters::start(tcp_streams);
    while let Some(forks) = rx_blocks.recv().await {
        // A restarted node is published to again once nanospam reconnected
        while let Ok((node_index, new_writers)) = reconnected.try_recv() {
            writers.reconnected(node_index, new_writers);
        }
        let block = forks.block.clone();
        let hash = block.hash();
        // What the Byzantine representatives vote about
        recent_blocks.push(hash);
        let publish = Message::Publish(Publish::new_from_originator(block));
        let buffer = Arc::new(serializer.serialize(&publish).to_vec());
        let fork_buffer = forks.fork.map(|fork| {
            let publish_fork = Message::Publish(Publish::new_from_originator(fork));
            Arc::new(fork_serializer.serialize(&publish_fork).to_vec())
        });

        // Marked published before it is sent: a node next to the client can
        // confirm the block before a distant node has received it, and a
        // confirmation that arrived before this mark was never counted
        // TODO support delayed forks
        let was_high_prio = logic.lock().unwrap().published(&hash, clock.now());

        let mut counter = 0;
        for node_index in 0..writers.len() {
            if rng().random_bool(drop_probability) {
                // drop this transmission
                continue;
            }

            let message = match &fork_buffer {
                // send fork to every second node
                Some(fork_buffer) if counter % 2 == 0 => fork_buffer,
                _ => &buffer,
            };
            // A node that is down still takes its turn, so the nodes
            // receiving the forks stay the same
            counter += 1;
            writers.send(node_index, message.clone());
        }

        if was_high_prio {
            tracing::info!("High prio block published: {hash}");
        }
        if logic.lock().unwrap().is_finished() {
            break;
        }
    }
    // A run's end does not wait long for a node that stopped reading
    writers.finish(PUBLISH_DRAIN_LIMIT).await;
    cancel_token.cancel();
}

async fn republish_delayed_blocks(
    tx_forks: mpsc::Sender<Forks>,
    logic: &Mutex<SpamLogic>,
    clock: &SteadyClock,
) {
    loop {
        while let Some(block) = {
            let now = clock.now();
            let mut l = logic.lock().unwrap();
            if l.is_finished() {
                return;
            }
            l.next_delayed(now)
        } {
            tx_forks.send(Forks::new(block)).await.unwrap();
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn receive_messages(
    mut readers: Vec<Vec<ReadHalf<TcpStream>>>,
    _protocol: ProtocolInfo,
    cancel_token: CancellationToken,
) {
    select! {
        _ = cancel_token.cancelled() => {},
        _ = async {
            let mut set = JoinSet::new();
            for reader in readers.drain(..).flatten() {
                set.spawn(drain_reader(reader));
            }
            set.join_all().await;
        } => {}
    }
}

fn track_confirmations(
    rx_ws_msg: std::sync::mpsc::Receiver<(MessageEnvelope, Timestamp)>,
    logic: &Mutex<SpamLogic>,
) {
    while let Ok((msg, timestamp)) = rx_ws_msg.recv() {
        if msg.topic == Some(Topic::Confirmation) {
            let data: BlockConfirmed = serde_json::from_value(msg.message.unwrap()).unwrap();
            let block_hash = BlockHash::decode_hex(data.hash).unwrap();

            let high_prio_conf_time = logic.lock().unwrap().confirmed(&block_hash, timestamp);

            if let Some(time) = high_prio_conf_time {
                tracing::info!(
                    "High prio block confirmed: {block_hash}. Conf time: {} ms",
                    time.as_millis()
                );
            }
        }
    }
}

async fn log_status(
    logic: &Mutex<SpamLogic>,
    clock: &SteadyClock,
    cancel_token: CancellationToken,
) {
    while timeout(Duration::from_secs(1), cancel_token.cancelled())
        .await
        .is_err()
    {
        let now = clock.now();

        let stats = {
            let mut l = logic.lock().unwrap();
            let stats = l.stats(now);
            l.reset_cps_counter(now);
            stats
        };

        info!(
            "Confirmed {} blocks | {} bps | {} cps | avg conf time: {} ms",
            stats.total_confirmed.to_formatted_string(&Locale::en),
            stats.target_bps.to_formatted_string(&Locale::en),
            stats.current_cps.to_formatted_string(&Locale::en),
            stats.average_conf_time.as_millis()
        );
    }
}

/// The accounts whose frontier on a node differs from the reference node's
fn differing_accounts(
    reference: &HashMap<Account, BlockHash>,
    held: &HashMap<Account, BlockHash>,
) -> Vec<Account> {
    reference
        .iter()
        .filter(|(account, frontier)| held.get(*account) != Some(*frontier))
        .map(|(account, _)| *account)
        .collect()
}

/// A setup block flooded to the nodes may have been lost on one of them, and
/// nothing else delivers it before the spam starts. Hands every account chain
/// a PR lacks to it, as the first PR holds it; a block it holds already is
/// ignored as old. A receive ahead of its send arrives on the next pass.
async fn republish_differing_chains(rpc_clients: &[NanoRpcClient]) {
    let Ok(reference) = rpc_clients[0].frontiers(Account::ZERO, 100_000).await else {
        return;
    };
    let reference = reference.frontiers.unwrap_or_default();
    for client in &rpc_clients[1..] {
        let Ok(held) = client.frontiers(Account::ZERO, 100_000).await else {
            continue;
        };
        let accounts = differing_accounts(&reference, &held.frontiers.unwrap_or_default());
        // A chain takes a round trip per block: at once, a distant node
        // does not hold up the setup for minutes
        let per_task = accounts.len().div_ceil(REPUBLISH_TASKS).max(1);
        tokio_scoped::scope(|s| {
            for chunk in accounts.chunks(per_task) {
                s.spawn(async move {
                    for account in chunk {
                        republish_chain(&rpc_clients[0], client, *account).await;
                    }
                });
            }
        });
        info!("Republished {} account chains to a PR", accounts.len());
    }
}

async fn republish_chain(source: &NanoRpcClient, target: &NanoRpcClient, account: Account) {
    let Ok(history) = source
        .account_history(AccountHistoryArgs::new(account, 40))
        .await
    else {
        return;
    };
    for entry in history.history.iter().rev() {
        let Ok(info) = source.block_info(entry.hash).await else {
            continue;
        };
        let _ = target
            .process(ProcessArgs::build(info.contents.clone()).finish())
            .await;
    }
}

/// The spam only starts once every PR has seen every representative vote and
/// is connected to it: the quorum is then the same on all PRs, and no PR starts
/// with thresholds derived from a partial view of the network.
/// RAI: every PR holds the same, fully cemented ledger: the genesis
/// committee each derives from it at the start of the epochs is the same
async fn wait_for_equal_ledgers(rpc_clients: &[NanoRpcClient]) -> anyhow::Result<()> {
    info!("Waiting for all PRs to hold the same cemented ledger...");
    let started = Instant::now();
    let mut recovery = RetrySchedule::new(SETUP_RECOVERY_FIRST, SETUP_RECOVERY_EVERY);
    loop {
        let mut counts = Vec::new();
        for rpc_client in rpc_clients {
            let count = rpc_client.block_count().await?;
            counts.push((count.count.inner(), count.cemented.inner()));
        }
        let equal = counts.windows(2).all(|w| w[0] == w[1]);
        let cemented = counts.iter().all(|(count, cemented)| count == cemented);
        if equal && cemented {
            info!(
                "All PRs hold the same ledger of {} blocks after {:?}",
                counts[0].0,
                started.elapsed()
            );
            return Ok(());
        }
        if recovery.due(started.elapsed()) {
            republish_differing_chains(rpc_clients).await;
            // A node holding every block but never cementing one of them:
            // its election there missed the votes, which nothing sends again
            for (rpc_client, (count, cemented)) in rpc_clients.iter().zip(&counts) {
                if count != cemented {
                    confirm_uncemented_frontiers(rpc_client).await;
                }
            }
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Err(anyhow!("the PRs never held the same ledger: {counts:?}"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Starts the election of every frontier a node holds unconfirmed again, so
/// that it solicits the votes it missed
async fn confirm_uncemented_frontiers(rpc_client: &NanoRpcClient) {
    let Ok(response) = rpc_client.frontiers(Account::ZERO, 100_000).await else {
        return;
    };
    let frontiers: Vec<BlockHash> = response
        .frontiers
        .unwrap_or_default()
        .into_values()
        .collect();
    // In batches: one request per block takes a round trip each, which to a
    // distant node is minutes for the setup's thousands of frontiers, far
    // past the time the setup waits for equal ledgers
    for batch in frontiers.chunks(FRONTIER_BATCH) {
        let Ok(info) = rpc_client.blocks_info(batch.to_vec()).await else {
            continue;
        };
        for (hash, block) in info.blocks {
            if !block.confirmed.inner() {
                info!("Confirming the uncemented setup block {hash} again");
                let _ = rpc_client.block_confirm(hash).await;
            }
        }
    }
}

/// Frontiers whose confirmation one request asks for
const FRONTIER_BATCH: usize = 1000;

/// Every PR sees the same quorum and, under RAI, holds the same ledger, which
/// the genesis committee of epoch 0 is derived from
async fn wait_until_nodes_agree(
    args: &CliArgs,
    rpc_clients: &[NanoRpcClient],
) -> anyhow::Result<()> {
    // With representatives that never vote, the weight a node can see is
    // only the honest share of the configured minimum
    wait_for_full_quorum(rpc_clients, args.honest_prs() as u128, args.prs as u128).await?;
    if args.epoch_duration_ms > 0 {
        wait_for_equal_ledgers(rpc_clients).await?;
    }
    Ok(())
}

async fn wait_for_full_quorum(
    rpc_clients: &[NanoRpcClient],
    honest_prs: u128,
    total_prs: u128,
) -> anyhow::Result<()> {
    info!("Waiting for all PRs to see the full quorum...");
    let started = Instant::now();
    let mut recovery = RetrySchedule::new(SETUP_RECOVERY_FIRST, SETUP_RECOVERY_EVERY);
    loop {
        let mut online = Vec::new();
        let mut missing = None;
        for (i, rpc_client) in rpc_clients.iter().enumerate() {
            let quorum = rpc_client.confirmation_quorum().await?;
            // The configured minimum is the whole voting weight; the funds moved
            // to the spam accounts during setup are below one representative's
            // share, so a missing representative shows as a clearly lower stake
            let enough = quorum.online_weight_minimum / 100 * 90 / total_prs * honest_prs;
            let full = quorum.online_stake_total >= enough && quorum.peers_stake_total >= enough;
            if !full {
                missing = Some((i, quorum));
                break;
            }
            online.push(quorum.online_stake_total);
        }
        let agree = online.windows(2).all(|w| w[0] == w[1]);
        match missing {
            None if agree => {
                info!("All PRs see the full quorum after {:?}", started.elapsed());
                return Ok(());
            }
            // The sends and receives of a lost setup block move the
            // representative weight this gate compares
            _ if recovery.due(started.elapsed()) => {
                republish_differing_chains(rpc_clients).await;
            }
            _ if started.elapsed() > Duration::from_secs(120) => {
                return Err(anyhow!(
                    "the PRs never saw the full quorum: {:?} / online {:?}",
                    missing.map(|(i, q)| {
                        format!(
                            "PR{i} online {:?} peered {:?} of {:?}",
                            q.online_stake_total, q.peers_stake_total, q.online_weight_minimum
                        )
                    }),
                    online
                ));
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Opens nanospam's publishing connections to node PR`node_index`
pub(crate) async fn connect_node(
    tcp_stream_factory: &TcpStreamFactory,
    hosts: &NodeHosts,
    node_index: usize,
) -> anyhow::Result<(Vec<ReadHalf<TcpStream>>, Vec<WriteHalf<TcpStream>>)> {
    let protocol = ProtocolInfo::default_for(NetworkType::NanoTestNetwork);
    let peer_addr = hosts.socket(node_index, peering_port(node_index));
    info!(?peer_addr, "Connecting to node PR{node_index}...");
    let mut readers = Vec::with_capacity(CONNECTIONS_PER_NODE);
    let mut writers = Vec::with_capacity(CONNECTIONS_PER_NODE);
    for i in 0..CONNECTIONS_PER_NODE {
        let mut tcp_stream = tcp_stream_factory.connect(peer_addr).await?;
        info!("Performing handshake...");
        let node_id_key: PrivateKey = RawKey::from(42 + i as u64).into();
        perform_handshake(protocol, get_genesis_hash(), node_id_key, &mut tcp_stream).await?;
        let (tcp_read, tcp_write) = tokio::io::split(tcp_stream);
        readers.push(tcp_read);
        writers.push(tcp_write);
    }
    Ok((readers, writers))
}

/// Reads and discards what a node sends on a publishing connection, until
/// the node closes it or goes away
pub(crate) async fn drain_reader(mut reader: ReadHalf<TcpStream>) {
    let mut recv_buffer = vec![0; 1024 * 4];
    while let Ok(read) = reader.read(&mut recv_buffer).await {
        if read == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_older_frontier_differs() {
        let reference = HashMap::from([
            (Account::from(1), BlockHash::from(10)),
            (Account::from(2), BlockHash::from(20)),
            (Account::from(3), BlockHash::from(30)),
        ]);
        let held = HashMap::from([
            (Account::from(1), BlockHash::from(10)),
            (Account::from(2), BlockHash::from(19)),
        ]);

        let mut differing = differing_accounts(&reference, &held);
        differing.sort();

        assert_eq!(differing, [Account::from(2), Account::from(3)]);
    }

    #[test]
    fn equal_frontiers_do_not_differ() {
        let frontiers = HashMap::from([(Account::from(1), BlockHash::from(10))]);

        assert!(differing_accounts(&frontiers, &frontiers).is_empty());
    }
}
