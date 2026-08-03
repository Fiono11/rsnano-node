use std::{
    net::{Ipv6Addr, SocketAddrV6},
    sync::{Arc, Mutex},
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
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use tracing::info;

use rsnano_messages::{Message, MessageSerializer, Publish};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_tcp::{TcpStream, TcpStreamFactory};
use rsnano_nullable_tracing_subscriber::TracingInitializer;
use rsnano_rpc_client::NanoRpcClient;
use rsnano_types::{BlockHash, JsonBlock, NetworkType, PrivateKey, ProtocolInfo, RawKey, WalletId};
use rsnano_websocket_messages::{BlockConfirmed, MessageEnvelope, Topic};

use crate::{
    cli_args::CliArgs,
    confirmation_receiver::ConfirmationReceiver,
    confirmation_tracker::reconcile_confirmations,
    domain::{BlockResult, Forks, spam_logic::SpamLogic},
    frontiers_sync::sync_frontiers,
    handshake::perform_handshake,
    high_prio_check::HighPrioCheck,
    node_lifetime::NodeLifetime,
    setup::{
        configure_nodes, configure_run_nodes, create_account_map, get_genesis_hash, peering_port,
        rpc_port, start_nodes, validate_prepared_network, write_prepared_network,
    },
    wallets_factory::create_wallets,
};

const MAX_BUFFERED_BLOCKS: usize = 1024;
const CONNECTIONS_PER_NODE: usize = 4;
const GLOBAL_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) struct NanoSpamApp {
    tracing_init: TracingInitializer,
    tcp_stream_factory: TcpStreamFactory,
    clock: SteadyClock,
    rpc_clients: Vec<NanoRpcClient>,
    node_lifetime: NodeLifetime,
    args: CliArgs,
    last_rai_phase: Arc<Mutex<String>>,
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
            last_rai_phase: Arc::new(Mutex::new("starting".to_string())),
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let last_phase = self.last_rai_phase.clone();
        match timeout(GLOBAL_TIMEOUT, self.run_inner()).await {
            Ok(result) => result.map_err(|error| {
                anyhow!(
                    "{error:#}; last known RAI phase: {}",
                    last_phase.lock().unwrap(),
                )
            }),
            Err(_) => Err(anyhow!(
                "nanospam exceeded the {GLOBAL_TIMEOUT:?} global timeout; last known RAI phase: {}",
                last_phase.lock().unwrap()
            )),
        }
    }

    async fn run_inner(mut self) -> anyhow::Result<()> {
        self.tracing_init.init();

        let protocol = ProtocolInfo::default_for(NetworkType::NanoTestNetwork);
        let genesis_hash = get_genesis_hash();

        let data_dir = if let Some(path) = self.args.data_dir.clone() {
            path
        } else {
            let mut path = dirs::home_dir().ok_or_else(|| anyhow!("No home dir found"))?;
            path.push("NanoSpam");
            path
        };
        std::fs::create_dir_all(&data_dir)?;

        if !self.args.set_up_new_nodes() {
            validate_prepared_network(&data_dir, &self.args)?;
        }

        let mut account_map = create_account_map(&data_dir, self.args.accounts);

        if self.args.set_up_new_nodes() {
            *self.last_rai_phase.lock().unwrap() = "configuring PR nodes".to_string();
            configure_nodes(&self.args, &data_dir);
        } else {
            configure_run_nodes(&self.args, &data_dir);
        }

        for i in 0..self.args.prs {
            let rpc_client =
                NanoRpcClient::new(format!("http://[::1]:{}", rpc_port(i)).parse().unwrap());
            self.rpc_clients.push(rpc_client);
        }

        let genesis_rpc = &self.rpc_clients[0];

        {
            *self.last_rai_phase.lock().unwrap() =
                "starting PR nodes and waiting for RPC".to_string();
            self.node_lifetime = NodeLifetime::new(self.args.kill_nodes());
            start_nodes(
                &self.args,
                data_dir.clone(),
                &self.rpc_clients,
                &mut self.node_lifetime,
            )
            .await?;
        }

        let genesis_wallet_id = if self.args.set_up_new_nodes() {
            *self.last_rai_phase.lock().unwrap() = "creating test wallets".to_string();
            create_wallets(&self.rpc_clients, genesis_rpc, &mut account_map).await
        } else {
            WalletId::ZERO
        };

        if self.args.sync() {
            sync_frontiers(&self.rpc_clients, &mut account_map).await;
        }

        let logic = Mutex::new(SpamLogic::new(account_map, self.args.spam_spec()?));

        let (tx_blocks, rx_blocks) = mpsc::channel::<Forks>(MAX_BUFFERED_BLOCKS);
        let mut high_prio_check = HighPrioCheck::new(genesis_rpc, &logic);

        if self.args.set_up_new_nodes() {
            high_prio_check
                .create_prio_accounts(genesis_wallet_id)
                .await?;
        }

        if self.args.setup_only() {
            write_prepared_network(&data_dir, &self.args)?;
            return Ok(());
        }

        if self.args.sync() {
            high_prio_check.sync_accounts().await?;
        }

        let mut tcp_writers = Vec::new();
        let mut tcp_readers = Vec::new();

        for node_index in 0..self.args.prs {
            let peer_addr = SocketAddrV6::new(Ipv6Addr::LOCALHOST, peering_port(node_index), 0, 0);
            info!(?peer_addr, "Connecting to node PR{node_index}...");
            let mut node_writers = Vec::with_capacity(CONNECTIONS_PER_NODE);
            let mut node_readers = Vec::with_capacity(CONNECTIONS_PER_NODE);
            for i in 0..CONNECTIONS_PER_NODE {
                let mut tcp_stream = self.tcp_stream_factory.connect(peer_addr).await?;
                info!("Performing handshake...");
                let node_id_key: PrivateKey = RawKey::from(42 + i as u64).into();
                perform_handshake(protocol, genesis_hash, node_id_key, &mut tcp_stream).await?;
                let (tcp_read, tcp_write) = tokio::io::split(tcp_stream);
                node_writers.push(tcp_write);
                node_readers.push(tcp_read);
            }
            tcp_writers.push(node_writers);
            tcp_readers.push(node_readers);
        }

        let tx_forks_clone = tx_blocks.clone();
        let cancel_block_creation = CancellationToken::new();
        let cancel_block_creation2 = cancel_block_creation.clone();
        let cancel_nanospam = CancellationToken::new();

        let (tx_ws_msg, rx_ws_msg) = std::sync::mpsc::channel::<(MessageEnvelope, Timestamp)>();

        #[cfg(feature = "rai_protocol")]
        let transition = Mutex::new(RaiTransitionObservation::default());

        info!("Connecting to websocket...");
        let mut conf_receiver = ConfirmationReceiver::connect().await?;
        let initial_cemented = genesis_rpc.block_count().await?.cemented.inner();
        let expected_cemented = self
            .args
            .blocks
            .map(|blocks| initial_cemented + blocks as u64);

        #[cfg(feature = "rai_protocol")]
        let initial_rai_statuses = if self.args.rai_epoch_duration_ms.is_some() {
            let statuses = fetch_rai_statuses(&self.rpc_clients).await?;
            if !statuses
                .iter()
                .all(|status| status.open_epoch.inner() == 0 && status.closing_epoch.is_none())
            {
                anyhow::bail!(
                    "prepared PRs did not all start in open epoch 0; restart the prepared network"
                );
            }
            // C1's 200 blocks at 50 BPS take roughly four seconds. Starting
            // halfway through a five-second epoch intentionally places
            // finalized workload on both sides of the first boundary.
            sleep(Duration::from_millis(
                self.args.rai_epoch_duration_ms.unwrap() / 2,
            ))
            .await;
            statuses
        } else {
            Vec::new()
        };

        info!("Starting with {} BPS", logic.lock().unwrap().current_bps);
        *self.last_rai_phase.lock().unwrap() =
            "running workload; waiting for epoch 0 to begin closing".to_string();

        let started = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                enqueue_blocks(&logic, tx_blocks, &self.clock);
                cancel_block_creation2.cancel();
            });

            s.spawn(|| track_confirmations(rx_ws_msg, &logic));

            tokio_scoped::scope(|scope| {
                scope.spawn(log_status(&logic, &self.clock, cancel_nanospam.clone()));

                #[cfg(feature = "rai_protocol")]
                if self.args.rai_epoch_duration_ms.is_some() {
                    scope.spawn(observe_rai_transition(
                        &self.rpc_clients,
                        &transition,
                        self.last_rai_phase.clone(),
                        cancel_nanospam.clone(),
                    ));
                }

                if self.args.high_prio_check() {
                    scope.spawn(high_prio_check.run(cancel_block_creation, tx_forks_clone.clone()));
                }

                scope.spawn(conf_receiver.run(cancel_nanospam.clone(), tx_ws_msg, &self.clock));
                scope.spawn(reconcile_confirmations(
                    genesis_rpc,
                    &logic,
                    &self.clock,
                    cancel_nanospam.clone(),
                    expected_cemented,
                ));
                scope.spawn(receive_messages(
                    tcp_readers,
                    protocol,
                    cancel_nanospam.clone(),
                ));
                scope.spawn(publish_blocks(
                    rx_blocks,
                    tcp_writers,
                    protocol,
                    genesis_rpc,
                    &logic,
                    cancel_nanospam,
                    self.args.drop_probability(),
                    &self.clock,
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

        #[cfg(feature = "rai_protocol")]
        {
            if self.args.rai_epoch_duration_ms.is_some() {
                wait_for_rai_close(
                    &self.rpc_clients,
                    &transition,
                    &self.last_rai_phase,
                    &initial_rai_statuses,
                    Duration::from_secs(240),
                )
                .await?;
            }
            let statuses = fetch_rai_statuses(&self.rpc_clients).await?;
            print_finalized_blocks_by_epoch(&statuses);
            if self.args.rai_epoch_duration_ms.is_some() {
                record_rai_statuses(&statuses, &transition, &self.last_rai_phase);
                validate_epoch_transition(
                    &statuses,
                    &initial_rai_statuses,
                    self.args.blocks.unwrap_or(0),
                    &transition.into_inner().unwrap(),
                )?;
            }
        }

        Ok(())
    }
}

#[cfg(feature = "rai_protocol")]
#[derive(Default)]
struct RaiTransitionObservation {
    saw_epoch_zero_closing: bool,
    saw_open_epoch_one_overlap: bool,
    saw_matching_cut: bool,
    saw_obligations_finalized: bool,
    validation_error: Option<String>,
}

#[cfg(feature = "rai_protocol")]
async fn observe_rai_transition(
    clients: &[NanoRpcClient],
    observation: &Mutex<RaiTransitionObservation>,
    last_phase: Arc<Mutex<String>>,
    cancel: CancellationToken,
) {
    loop {
        select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }

        let Ok(statuses) = fetch_rai_statuses(clients).await else {
            continue;
        };
        record_rai_statuses(&statuses, observation, &last_phase);
    }
}

#[cfg(feature = "rai_protocol")]
fn record_rai_statuses(
    statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    observation: &Mutex<RaiTransitionObservation>,
    last_phase: &Arc<Mutex<String>>,
) {
    let phase = statuses
        .iter()
        .enumerate()
        .map(|(pr, status)| {
            format!(
                "PR{pr}=open {}, closing {} ({})",
                status.open_epoch.inner(),
                status
                    .closing_epoch
                    .as_ref()
                    .map(|epoch| epoch.inner().to_string())
                    .unwrap_or_else(|| "none".to_string()),
                status.closing_phase.as_deref().unwrap_or("closed"),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    *last_phase.lock().unwrap() = phase;

    let mut observed = observation.lock().unwrap();
    observed.saw_epoch_zero_closing |= statuses
        .iter()
        .any(|status| status.closing_epoch.as_ref().map(|epoch| epoch.inner()) == Some(0));
    observed.saw_open_epoch_one_overlap |= statuses.iter().any(|status| {
        status.open_epoch.inner() == 1
            && status.closing_epoch.as_ref().map(|epoch| epoch.inner()) == Some(0)
    });

    let cuts = statuses
        .iter()
        .filter_map(|status| status.cut_hashes.get("0"))
        .collect::<Vec<_>>();
    if cuts.len() == statuses.len() {
        if cuts.windows(2).all(|pair| pair[0] == pair[1]) {
            observed.saw_matching_cut = true;
        } else {
            observed.validation_error =
                Some("the PRs decided different epoch-0 close cuts".to_string());
        }
    }

    observed.saw_obligations_finalized |= statuses.iter().all(|status| {
        let obligations = status
            .drain_obligations
            .get("0")
            .map(|count| count.inner())
            .unwrap_or(0);
        let finalized = status
            .drain_finalized
            .get("0")
            .map(|count| count.inner())
            .unwrap_or(0);
        finalized == obligations
    });
}

#[cfg(feature = "rai_protocol")]
async fn wait_for_rai_close(
    clients: &[NanoRpcClient],
    observation: &Mutex<RaiTransitionObservation>,
    last_phase: &Arc<Mutex<String>>,
    initial_statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    timeout_after: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout_after;
    loop {
        let statuses = fetch_rai_statuses(clients).await?;
        record_rai_statuses(&statuses, observation, last_phase);
        let target_epoch = statuses
            .iter()
            .zip(initial_statuses)
            .filter_map(|(status, initial)| workload_counts(status, initial).ok())
            .flat_map(|counts| counts.into_keys())
            .filter_map(|epoch| epoch.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        if statuses.iter().all(|status| {
            status
                .closed_through
                .as_ref()
                .is_some_and(|epoch| epoch.inner() >= target_epoch)
        }) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "RAI workload epochs did not finish closing within {timeout_after:?}; statuses: {statuses:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(feature = "rai_protocol")]
fn print_finalized_blocks_by_epoch(statuses: &[rsnano_rpc_messages::RaiStatusResponse]) {
    println!("Finalized blocks by RAI epoch:");
    for (pr, status) in statuses.iter().enumerate() {
        let formatted = status
            .finalized_by_epoch
            .iter()
            .map(|(epoch, count)| format!("epoch {epoch}: {}", count.inner()))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  PR{pr}: {formatted}");
    }
}

#[cfg(feature = "rai_protocol")]
async fn fetch_rai_statuses(
    clients: &[NanoRpcClient],
) -> anyhow::Result<Vec<rsnano_rpc_messages::RaiStatusResponse>> {
    let mut statuses = Vec::with_capacity(clients.len());
    for (pr, client) in clients.iter().enumerate() {
        statuses.push(
            client
                .rai_status()
                .await
                .map_err(|error| anyhow!("could not query RAI status from PR{pr}: {error}"))?,
        );
    }
    Ok(statuses)
}

#[cfg(feature = "rai_protocol")]
fn validate_epoch_transition(
    statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    initial_statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    requested_workload: usize,
    observed: &RaiTransitionObservation,
) -> anyhow::Result<()> {
    if let Some(error) = &observed.validation_error {
        anyhow::bail!("RAI validation error: {error}");
    }
    let first = statuses
        .first()
        .ok_or_else(|| anyhow!("no PRs reported RAI status"))?;
    let initial_first = initial_statuses
        .first()
        .ok_or_else(|| anyhow!("no initial RAI status was recorded"))?;
    if initial_statuses.len() != statuses.len() {
        anyhow::bail!("initial and final RAI status counts differ");
    }
    let expected_counts = workload_counts(first, initial_first)?;
    for (pr, (status, initial)) in statuses.iter().zip(initial_statuses).enumerate().skip(1) {
        let finalized: u64 = workload_counts(status, initial)?.values().sum();
        if finalized != requested_workload as u64 {
            anyhow::bail!(
                "PR{pr} finalized {finalized} workload blocks, requested {requested_workload}"
            );
        }
    }

    let workload_epochs = expected_counts
        .keys()
        .filter_map(|epoch| epoch.parse::<u64>().ok())
        .collect::<Vec<_>>();
    let first_workload_epoch = workload_epochs.iter().min().copied().unwrap_or(0);
    let last_workload_epoch = workload_epochs.iter().max().copied().unwrap_or(0);
    if workload_epochs.len() < 2 {
        anyhow::bail!("workload did not span at least two RAI epochs");
    }
    for epoch in first_workload_epoch..=last_workload_epoch {
        let epoch = epoch.to_string();
        let expected_cut = first.cut_hashes.get(&epoch);
        let expected_close = first.close_hashes.get(&epoch);
        if expected_cut.is_none() || expected_close.is_none() {
            anyhow::bail!("PR0 has no installed cut/close hash for workload epoch {epoch}");
        }
        for (pr, status) in statuses.iter().enumerate().skip(1) {
            if status.cut_hashes.get(&epoch) != expected_cut {
                anyhow::bail!("PR{pr} installed a different cut hash for epoch {epoch}");
            }
            if status.close_hashes.get(&epoch) != expected_close {
                anyhow::bail!("PR{pr} installed a different close hash for epoch {epoch}");
            }
        }
    }

    let finalized: u64 = expected_counts.values().sum();
    if finalized != requested_workload as u64 {
        anyhow::bail!(
            "finalized workload is {finalized}, requested workload is {requested_workload}"
        );
    }
    Ok(())
}

#[cfg(feature = "rai_protocol")]
fn workload_counts(
    status: &rsnano_rpc_messages::RaiStatusResponse,
    initial: &rsnano_rpc_messages::RaiStatusResponse,
) -> anyhow::Result<std::collections::BTreeMap<String, u64>> {
    let mut counts = std::collections::BTreeMap::new();
    for (epoch, final_count) in &status.finalized_by_epoch {
        let initial_count = initial
            .finalized_by_epoch
            .get(epoch)
            .map(|count| count.inner())
            .unwrap_or(0);
        let count = final_count
            .inner()
            .checked_sub(initial_count)
            .ok_or_else(|| {
                anyhow!("finalized count for epoch {epoch} decreased during the workload")
            })?;
        if count > 0 {
            counts.insert(epoch.clone(), count);
        }
    }
    Ok(counts)
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
    mut tcp_streams: Vec<Vec<WriteHalf<TcpStream>>>,
    protocol: ProtocolInfo,
    genesis_rpc: &NanoRpcClient,
    logic: &Mutex<SpamLogic>,
    cancel_token: CancellationToken,
    drop_probability: f64,
    clock: &SteadyClock,
) {
    let mut serializer = MessageSerializer::new(protocol);
    let mut fork_serializer = MessageSerializer::new(protocol);
    let mut writer_index = 0;
    while let Some(forks) = rx_blocks.recv().await {
        let block = forks.block.clone();
        let hash = block.hash();
        let publish = Message::Publish(Publish::new_from_originator(block));
        let buffer = serializer.serialize(&publish);
        let mut fork_buffer = None;
        let fork_block = forks.fork.clone();
        let fork_hash = fork_block.as_ref().map(|fork| fork.hash());

        if let Some(fork) = forks.fork {
            let publish_fork = Message::Publish(Publish::new_from_originator(fork));
            fork_buffer = Some(fork_serializer.serialize(&publish_fork));
        }

        let mut counter = 0;
        tokio_scoped::scope(|s| {
            for stream in &mut tcp_streams {
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

                s.spawn(async {
                    let _ = stream[writer_index].write_all(buf).await;
                });

                counter += 1;
            }
        });

        // A publish alone does not necessarily activate an election when the
        // priority scheduler is disabled. Fork-only RAI tests still need the
        // conflicting root to become a visible slot obligation, so explicitly
        // activate the side delivered to PR0 once its publish is processed.
        if let Some(fork_hash) = fork_hash {
            if let Some(fork) = fork_block {
                let _ = genesis_rpc.process(JsonBlock::from(fork)).await;
            }
            for _ in 0..100 {
                if genesis_rpc
                    .block_confirm(fork_hash)
                    .await
                    .is_ok_and(|response| bool::from(response.started))
                {
                    break;
                }
                sleep(Duration::from_millis(50)).await;
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
            if tx_forks.send(Forks::new(block)).await.is_err() {
                return;
            }
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
            for mut reader in readers.drain(..).flatten() {
                set.spawn(async move {
                    let mut recv_buffer = vec![0; 1024 * 4];
                    loop {
                        match reader.read(&mut recv_buffer).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                });
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

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use rsnano_rpc_messages::RaiStatusResponse;
    use std::collections::BTreeMap;

    fn status(epoch_0: u64, epoch_1: u64, close_hash: &str) -> RaiStatusResponse {
        RaiStatusResponse {
            open_epoch: 1.into(),
            closing_epoch: None,
            closing_phase: None,
            closed_through: Some(0.into()),
            cut_hashes: BTreeMap::from([
                ("0".into(), "cut-0".into()),
                ("1".into(), "cut-1".into()),
            ]),
            close_hashes: BTreeMap::from([
                ("0".into(), format!("{close_hash}-0")),
                ("1".into(), format!("{close_hash}-1")),
            ]),
            drain_obligations: BTreeMap::from([("0".into(), 1.into())]),
            drain_finalized: BTreeMap::from([("0".into(), 1.into())]),
            finalized_by_epoch: BTreeMap::from([
                ("0".into(), epoch_0.into()),
                ("1".into(), epoch_1.into()),
            ]),
        }
    }

    #[test]
    fn epoch_zero_only_fails() {
        assert!(
            validate_epoch_transition(
                &[status(10, 0, "A")],
                &[status(0, 0, "A")],
                10,
                &observed(),
            )
            .is_err()
        );
    }

    #[test]
    fn different_local_boundary_split_passes_when_close_hashes_and_totals_match() {
        validate_epoch_transition(
            &[status(4, 6, "A"), status(5, 5, "A")],
            &[status(0, 0, "A"), status(0, 0, "A")],
            10,
            &observed(),
        )
        .unwrap();
    }

    #[test]
    fn different_pr_totals_fail() {
        assert!(validate_epoch_transition(
            &[status(4, 6, "A"), status(5, 4, "A")],
            &[status(0, 0, "A"), status(0, 0, "A")],
            10,
            &observed(),
        )
        .is_err());
    }

    #[test]
    fn different_close_hashes_fail() {
        assert!(
            validate_epoch_transition(
                &[status(4, 6, "A"), status(4, 6, "B")],
                &[status(0, 0, "A"), status(0, 0, "A")],
                10,
                &observed(),
            )
            .is_err()
        );
    }

    #[test]
    fn matching_epoch_transition_passes() {
        validate_epoch_transition(
            &[
                status(4, 6, "A"),
                status(4, 6, "A"),
                status(4, 6, "A"),
                status(4, 6, "A"),
            ],
            &[
                status(0, 0, "A"),
                status(0, 0, "A"),
                status(0, 0, "A"),
                status(0, 0, "A"),
            ],
            10,
            &observed(),
        )
        .unwrap();
    }

    fn observed() -> RaiTransitionObservation {
        RaiTransitionObservation {
            saw_epoch_zero_closing: true,
            saw_open_epoch_one_overlap: true,
            saw_matching_cut: true,
            saw_obligations_finalized: true,
            validation_error: None,
        }
    }
}
