use std::{
    net::{Ipv6Addr, SocketAddrV6},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::yield_now,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use num_format::{Locale, ToFormattedString};
use rand::{RngExt, rng};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
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
use rsnano_types::{BlockHash, NetworkType, PrivateKey, ProtocolInfo, RawKey, WalletId};
use rsnano_websocket_messages::{BlockConfirmed, MessageEnvelope, Topic};

use crate::{
    byzantine::{RecentBlocks, byzantine_keys, run_byzantine},
    cli_args::CliArgs,
    confirmation_receiver::ConfirmationReceiver,
    domain::{BlockResult, Forks, spam_logic::SpamLogic},
    frontiers_sync::sync_frontiers,
    handshake::perform_handshake,
    high_prio_check::HighPrioCheck,
    node_lifetime::NodeLifetime,
    restart::Restarter,
    rotation::run_rotations,
    setup::{
        configure_nodes, create_account_map, genesis_key, get_genesis_hash, peering_port, rpc_port,
        start_nodes,
    },
    wallets_factory::create_wallets,
    weight_shift::{WeightShifts, run_weight_shifts},
};

const MAX_BUFFERED_BLOCKS: usize = 1024;
const CONNECTIONS_PER_NODE: usize = 4;

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

        let mut account_map = create_account_map(&data_dir, self.args.accounts);

        if self.args.set_up_new_nodes() {
            configure_nodes(&self.args, &data_dir);
        }

        // Only the representatives that run a node have an RPC; the offline and
        // Byzantine ones exist in the ledger only
        for i in 0..self.args.honest_prs() {
            let rpc_client =
                NanoRpcClient::new(format!("http://[::1]:{}", rpc_port(i)).parse().unwrap());
            self.rpc_clients.push(rpc_client);
        }

        let genesis_rpc = &self.rpc_clients[0];

        if !self.args.attach {
            let node_handles = start_nodes(&self.args, &data_dir, &self.rpc_clients).await;
            self.node_lifetime = NodeLifetime::new(node_handles, self.args.kill_nodes());
        }

        let representatives = self.args.representatives();
        let genesis_wallet_id = if self.args.set_up_new_nodes() {
            create_wallets(
                &self.rpc_clients,
                genesis_rpc,
                &mut account_map,
                &representatives,
                &(0..self.args.prs)
                    .map(|i| self.args.pr_balance(i))
                    .collect::<Vec<_>>(),
            )
            .await
        } else {
            WalletId::ZERO
        };

        if self.args.sync {
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

        if self.args.setup_only {
            return Ok(());
        }

        if self.args.sync {
            high_prio_check.sync_accounts().await?;
        }

        // With representatives that never vote, the weight a node can see is
        // only the honest share of the configured minimum
        wait_for_full_quorum(
            &self.rpc_clients,
            self.args.honest_prs() as u128,
            self.args.prs as u128,
        )
        .await?;

        // RAI: the setup is over and every PR holds its share of the weight:
        // epoch 0 starts now on every PR. Its genesis committee is the
        // ledger as it stands, so every PR must hold the same ledger first.
        if self.args.epoch_duration_ms > 0 {
            wait_for_equal_ledgers(&self.rpc_clients).await?;
            for rpc_client in &self.rpc_clients {
                rpc_client.epoch_start().await?;
            }
            info!("Started the epochs on every PR");
        }
        let epochs_started = Instant::now();

        let mut tcp_writers = Vec::new();
        let mut tcp_readers = Vec::new();

        for node_index in 0..self.args.honest_prs() {
            let (node_readers, node_writers) =
                connect_node(&self.tcp_stream_factory, node_index).await?;
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
        let mut conf_receiver = ConfirmationReceiver::connect().await?;

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
                        self.args.honest_prs(),
                        protocol,
                        genesis_hash,
                        recent_blocks.clone(),
                        cancel_nanospam.clone(),
                        &self.tcp_stream_factory,
                    ));
                }

                if let Some(schedule) = &self.args.rotation {
                    scope.spawn(run_rotations(
                        genesis_rpc,
                        schedule.clone(),
                        epochs_started,
                        Duration::from_millis(self.args.epoch_duration_ms),
                    ));
                }

                if self.args.weight_shift_percent > 0 {
                    scope.spawn(run_weight_shifts(
                        genesis_rpc,
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
            let is_fork = rng().random_bool(l.fork_propability());
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
    mut tcp_streams: Vec<Option<Vec<WriteHalf<TcpStream>>>>,
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
    let mut writer_index = 0;
    while let Some(forks) = rx_blocks.recv().await {
        // A restarted node is published to again once nanospam reconnected
        while let Ok((node_index, writers)) = reconnected.try_recv() {
            tcp_streams[node_index] = Some(writers);
        }
        let block = forks.block.clone();
        let hash = block.hash();
        // What the Byzantine representatives vote about
        recent_blocks.push(hash);
        let publish = Message::Publish(Publish::new_from_originator(block));
        let buffer = serializer.serialize(&publish);
        let mut fork_buffer = None;

        if let Some(fork) = forks.fork {
            let publish_fork = Message::Publish(Publish::new_from_originator(fork));
            fork_buffer = Some(fork_serializer.serialize(&publish_fork));
        }

        let mut counter = 0;
        let failed: Vec<AtomicBool> = tcp_streams.iter().map(|_| AtomicBool::new(false)).collect();
        tokio_scoped::scope(|s| {
            for (stream, failed) in tcp_streams.iter_mut().zip(&failed) {
                if rng().random_bool(drop_probability) {
                    // drop this transmission
                    continue;
                }

                let buf = if let Some(fbuf) = fork_buffer
                    && counter % 2 == 0
                {
                    // send fork to every second node
                    fbuf
                } else {
                    buffer
                };
                // A node that is down still takes its turn, so the nodes
                // receiving the forks stay the same
                counter += 1;

                let Some(stream) = stream else {
                    continue;
                };
                s.spawn(async {
                    if stream[writer_index].write_all(buf).await.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                });
            }
        });
        for (node_index, failed) in failed.iter().enumerate() {
            if failed.load(Ordering::Relaxed) && tcp_streams[node_index].take().is_some() {
                info!("RAI_PUBLISH_DISCONNECTED pr={node_index}");
            }
        }

        let now = clock.now();

        writer_index += 1;
        if writer_index >= CONNECTIONS_PER_NODE {
            writer_index = 0;
        }

        let was_high_prio = {
            let mut l = logic.lock().unwrap();
            // TODO support delayed forks
            let prio = l.published(&hash, now);
            if l.is_finished() {
                break;
            }
            prio
        };

        if was_high_prio {
            tracing::info!("High prio block published: {hash}");
        }
    }
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

/// Hands the recent blocks of the genesis account, as the first PR holds them,
/// to every other PR; a block they hold already is ignored as old
async fn republish_genesis_chain(rpc_clients: &[NanoRpcClient]) {
    let genesis = genesis_key().account();
    let Ok(history) = rpc_clients[0]
        .account_history(AccountHistoryArgs::new(genesis, 40))
        .await
    else {
        return;
    };
    for entry in history.history.iter().rev() {
        let Ok(info) = rpc_clients[0].block_info(entry.hash).await else {
            continue;
        };
        for client in &rpc_clients[1..] {
            let _ = client
                .process(ProcessArgs::build(info.contents.clone()).finish())
                .await;
        }
    }
    info!("Republished the genesis chain to all PRs");
}

/// The spam only starts once every PR has seen every representative vote and
/// is connected to it: the quorum is then the same on all PRs, and no PR starts
/// with thresholds derived from a partial view of the network.
/// RAI: every PR holds the same, fully cemented ledger: the genesis
/// committee each derives from it at the start of the epochs is the same
async fn wait_for_equal_ledgers(rpc_clients: &[NanoRpcClient]) -> anyhow::Result<()> {
    info!("Waiting for all PRs to hold the same cemented ledger...");
    let started = Instant::now();
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
        // A setup block lost on one node: the genesis chain, whose sends
        // fund every account of the setup, is handed to every node again
        if started.elapsed() > Duration::from_secs(10) && started.elapsed().as_millis() % 5000 < 200
        {
            republish_genesis_chain(rpc_clients).await;
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Err(anyhow!("the PRs never held the same ledger: {counts:?}"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_full_quorum(
    rpc_clients: &[NanoRpcClient],
    honest_prs: u128,
    total_prs: u128,
) -> anyhow::Result<()> {
    info!("Waiting for all PRs to see the full quorum...");
    let started = Instant::now();
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
            // A setup block flooded to the nodes may have been lost on one of
            // them; nothing else delivers it before the spam starts, so the
            // genesis chain, whose sends move the representative weight the
            // gate compares, is handed to every node again
            _ if started.elapsed() > Duration::from_secs(10)
                && started.elapsed().as_millis() % 5000 < 200 =>
            {
                republish_genesis_chain(rpc_clients).await;
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
    node_index: usize,
) -> anyhow::Result<(Vec<ReadHalf<TcpStream>>, Vec<WriteHalf<TcpStream>>)> {
    let protocol = ProtocolInfo::default_for(NetworkType::NanoTestNetwork);
    let peer_addr = SocketAddrV6::new(Ipv6Addr::LOCALHOST, peering_port(node_index), 0, 0);
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
