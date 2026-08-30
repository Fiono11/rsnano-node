use std::{
    net::{Ipv6Addr, SocketAddrV6},
    sync::Mutex,
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
use rsnano_types::{BlockHash, NetworkType, PrivateKey, ProtocolInfo, RawKey, WalletId};
use rsnano_websocket_messages::{BlockConfirmed, MessageEnvelope, Topic};

use crate::{
    cli_args::CliArgs,
    confirmation_receiver::ConfirmationReceiver,
    domain::{BlockResult, Forks, spam_logic::SpamLogic},
    frontiers_sync::sync_frontiers,
    handshake::perform_handshake,
    high_prio_check::HighPrioCheck,
    node_lifetime::NodeLifetime,
    setup::{
        configure_nodes, create_account_map, get_genesis_hash, peering_port, rpc_port, start_nodes,
    },
    wallets_factory::create_wallets,
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

        let data_dir = if let Some(path) = &self.args.data_dir {
            path.clone()
        } else {
            let mut path = dirs::home_dir().ok_or_else(|| anyhow!("No home dir found"))?;
            path.push("NanoSpam");
            path
        };
        std::fs::create_dir_all(&data_dir)?;
        #[cfg(feature = "rai_protocol")]
        let epoch_start_file = data_dir.join("rai_epoch_start");
        #[cfg(feature = "rai_protocol")]
        if epoch_start_file.exists() {
            std::fs::remove_file(&epoch_start_file)?;
        }

        let mut account_map = create_account_map(&data_dir, self.args.accounts);

        if self.args.set_up_new_nodes() {
            configure_nodes(&self.args, &data_dir);
        }

        for i in 0..self.args.prs {
            let rpc_client =
                NanoRpcClient::new(format!("http://[::1]:{}", rpc_port(i)).parse().unwrap());
            self.rpc_clients.push(rpc_client);
        }

        let genesis_rpc = &self.rpc_clients[0];

        if !self.args.attach {
            let nodes = start_nodes(&self.args, data_dir, &self.rpc_clients).await?;
            if self.args.kill_nodes() {
                self.node_lifetime = nodes;
            } else {
                // Explicitly relinquish ownership for --no-kill.
                let _ = nodes.release();
            }
        }

        let genesis_wallet_id = if self.args.set_up_new_nodes() {
            create_wallets(&self.rpc_clients, genesis_rpc, &mut account_map).await
        } else {
            WalletId::ZERO
        };

        if self.args.sync {
            sync_frontiers(&self.rpc_clients, &mut account_map).await;
        }

        let logic = Mutex::new(SpamLogic::new(account_map, self.args.spam_spec()?));

        let (tx_blocks, rx_blocks) = mpsc::channel::<Forks>(MAX_BUFFERED_BLOCKS);
        let mut high_prio_check = HighPrioCheck::new(genesis_rpc, &logic);

        if self.args.set_up_new_nodes() && self.args.high_prio_check() {
            high_prio_check
                .create_prio_accounts(genesis_wallet_id, &self.rpc_clients)
                .await?;
        }

        if self.args.setup_only {
            return Ok(());
        }

        if self.args.sync {
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

        info!("Connecting to websocket...");
        let mut conf_receivers = vec![ConfirmationReceiver::connect().await?];

        info!("Starting with {} BPS", logic.lock().unwrap().current_bps);

        #[cfg(feature = "rai_protocol")]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            let start = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64 + 1_000;
            std::fs::write(&epoch_start_file, start.to_string())?;
            info!(
                start_unix_millis = start,
                duration_ms = 5_000,
                "All PRs ready; arming synchronized RAI epoch 1"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        let started = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                enqueue_blocks(&logic, tx_blocks, &self.clock);
                cancel_block_creation2.cancel();
            });

            #[cfg(not(feature = "rai_protocol"))]
            {
                let confirmation_cancel = cancel_nanospam.clone();
                let confirmation_logic = &logic;
                s.spawn(move || {
                    track_confirmations(rx_ws_msg, confirmation_logic, confirmation_cancel)
                });
            }
            #[cfg(feature = "rai_protocol")]
            {
                let confirmation_cancel = cancel_nanospam.clone();
                let confirmation_logic = &logic;
                s.spawn(move || {
                    track_rai_confirmation_feedback(
                        rx_ws_msg,
                        confirmation_logic,
                        confirmation_cancel,
                    )
                });
            }

            tokio_scoped::scope(|scope| {
                if self.args.timeout > 0 {
                    let timeout_cancel = cancel_nanospam.clone();
                    let completed = cancel_nanospam.clone();
                    let wait = Duration::from_secs(self.args.timeout);
                    scope.spawn(async move {
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => timeout_cancel.cancel(),
                            _ = completed.cancelled() => {}
                        }
                    });
                }
                scope.spawn(log_status(&logic, &self.clock, cancel_nanospam.clone()));
                #[cfg(not(feature = "rai_protocol"))]
                scope.spawn(reconcile_confirmations(
                    genesis_rpc,
                    &logic,
                    &self.clock,
                    cancel_nanospam.clone(),
                ));
                if self.args.high_prio_check() {
                    scope.spawn(high_prio_check.run(cancel_block_creation, tx_forks_clone.clone()));
                }

                for receiver in &mut conf_receivers {
                    scope.spawn(receiver.run(
                        cancel_nanospam.clone(),
                        tx_ws_msg.clone(),
                        &self.clock,
                    ));
                }
                drop(tx_ws_msg);
                scope.spawn(receive_messages(
                    tcp_readers,
                    protocol,
                    cancel_nanospam.clone(),
                ));
                scope.spawn(publish_blocks(
                    rx_blocks,
                    tcp_writers,
                    protocol,
                    &logic,
                    cancel_nanospam.clone(),
                    self.args.drop_probability(),
                    self.args.fork_recipients,
                    &self.clock,
                ));

                if !self.args.no_republish {
                    scope.spawn(republish_delayed_blocks(
                        tx_forks_clone,
                        &logic,
                        &self.clock,
                        cancel_nanospam.clone(),
                    ));
                }
            });
        });
        let duration_secs = started.elapsed().as_secs_f64();
        #[cfg(feature = "rai_protocol")]
        let node_converged = {
            let metrics_wait = if self.args.timeout == 0 {
                Duration::from_secs(60)
            } else {
                Duration::from_secs(self.args.timeout).saturating_sub(started.elapsed())
            };
            collect_node_metrics(&self.rpc_clients, &logic, metrics_wait).await?
        };
        let logic = logic.lock().unwrap();
        let created_blocks = logic.block_factory.created();
        let finalized_blocks = logic.confirmed_total;
        let terminated_elections = logic.terminated_total;
        let termination_rate = (created_blocks as f64 / duration_secs) as i32;
        let finalization_rate = (finalized_blocks as f64 / duration_secs) as i32;
        info!(
            "Terminated {terminated_elections} of {created_blocks} elections in {duration_secs:.2}s"
        );
        info!("Election termination rate: {termination_rate} elections/s");
        info!("Finalized {finalized_blocks} of {created_blocks} elections");
        #[cfg(feature = "rai_protocol")]
        info!(
            "Fast finalizations: {} | Final-vote finalizations: {}",
            logic.fast_finalized_total, logic.final_finalized_total
        );
        info!("Finalization rate: {finalization_rate} elections/s");
        #[cfg(feature = "rai_protocol")]
        for (epoch, stats) in &logic.epoch_stats {
            let average_finalization_ms = if stats.finalized == 0 {
                0
            } else {
                stats.sum_confirmation_time.as_millis() / stats.finalized as u128
            };
            let average_termination_ms = if stats.terminated == 0 {
                0
            } else {
                stats.sum_termination_time.as_millis() / stats.terminated as u128
            };
            info!(
                epoch,
                published = stats.published,
                terminated = stats.terminated,
                finalized = stats.finalized,
                fast_finalized = stats.fast_finalized,
                final_vote_finalized = stats.final_vote_finalized,
                average_finalization_ms,
                average_termination_ms,
                "RAI epoch results"
            );
        }
        #[cfg(feature = "rai_protocol")]
        info!(
            unclassified = logic.unclassified_finalizations,
            "Finalizations without an election epoch"
        );
        #[cfg(feature = "rai_protocol")]
        {
            let expected_by_epoch: std::collections::BTreeMap<_, _> = logic
                .per_pr_epoch_stats
                .iter()
                .filter(|((pr, _), _)| *pr == 0)
                .map(|((_, epoch), _)| (*epoch, logic.canonical_epoch_hashes(0, *epoch)))
                .collect();
            let all_epochs: std::collections::BTreeSet<_> = logic
                .per_pr_epoch_stats
                .keys()
                .map(|(_, epoch)| *epoch)
                .collect();
            for pr_index in 0..self.args.prs {
                let all_finalized = logic.pr_finalized_hashes(pr_index);
                for epoch in &all_epochs {
                    let expected_hashes = expected_by_epoch.get(epoch).cloned().unwrap_or_default();
                    let mut stats = logic
                        .per_pr_epoch_stats
                        .get(&(pr_index, *epoch))
                        .cloned()
                        .unwrap_or_default();
                    stats.finalized_hashes = logic.canonical_epoch_hashes(pr_index, *epoch);
                    let missing = expected_hashes.difference(&stats.finalized_hashes).count();
                    let extra = stats.finalized_hashes.difference(&expected_hashes).count();
                    let finalized = stats.finalized_hashes.len();
                    let average_finalization_ms = if finalized == 0 {
                        0
                    } else {
                        stats.sum_confirmation_time.as_millis() / finalized as u128
                    };
                    info!(
                        pr = pr_index,
                        epoch,
                        expected = expected_hashes.len(),
                        finalized,
                        missing,
                        extra,
                        fast_finalized = stats.fast_finalized,
                        final_vote_finalized = stats.final_vote_finalized,
                        average_finalization_ms,
                        "RAI per-PR epoch results"
                    );
                }
                let missing_total = logic.expected_hashes.difference(&all_finalized).count();
                let unclassified = logic
                    .per_pr_unclassified
                    .get(&pr_index)
                    .map(|hashes| hashes.len())
                    .unwrap_or_default();
                info!(
                    pr = pr_index,
                    expected = logic.expected_hashes.len(),
                    finalized = all_finalized.intersection(&logic.expected_hashes).count(),
                    missing = missing_total,
                    unclassified,
                    "RAI per-PR convergence"
                );
            }
        }
        if finalized_blocks > 0 {
            let finalization_time =
                logic.sum_conf_time_total.as_millis() / finalized_blocks as u128;
            info!("Average finalization time: {finalization_time} ms");
        }
        if terminated_elections > 0 {
            let termination_time =
                logic.sum_termination_time_total.as_millis() / terminated_elections as u128;
            info!("Average election termination time: {termination_time} ms");
        }
        #[cfg(not(feature = "rai_protocol"))]
        if self.args.timeout > 0 && terminated_elections < created_blocks {
            return Err(anyhow!(
                "only {terminated_elections} of {created_blocks} elections terminated within {} seconds",
                self.args.timeout
            ));
        }
        #[cfg(feature = "rai_protocol")]
        if !node_converged {
            return Err(anyhow!(
                "not every PR finalized all {} published blocks",
                logic.expected_hashes.len()
            ));
        }

        Ok(())
    }
}

#[cfg(feature = "rai_protocol")]
async fn collect_node_metrics(
    rpc_clients: &[NanoRpcClient],
    logic: &Mutex<SpamLogic>,
    wait: Duration,
) -> anyhow::Result<bool> {
    logic.lock().unwrap().clear_pr_finalizations();
    let deadline = Instant::now() + wait;

    loop {
        for (pr_index, client) in rpc_clients.iter().enumerate() {
            let response = client.confirmation_history().await?;
            let mut logic = logic.lock().unwrap();
            for entry in response.confirmations {
                logic.record_node_finalization(
                    pr_index,
                    entry.hash,
                    entry.epoch.inner(),
                    entry.final_tally,
                    Duration::from_millis(entry.duration.inner()),
                );
            }
        }

        if logic
            .lock()
            .unwrap()
            .all_prs_same_canonical_epochs(rpc_clients.len())
            || Instant::now() >= deadline
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let all_nodes_reported = logic
        .lock()
        .unwrap()
        .all_prs_same_canonical_epochs(rpc_clients.len());

    let expected = logic.lock().unwrap().expected_hashes.clone();
    let hashes = expected.iter().copied().collect::<Vec<_>>();
    let mut all_confirmed = true;
    for (pr_index, client) in rpc_clients.iter().enumerate() {
        let ledger = client.blocks_info(hashes.clone()).await?;
        let ledger_confirmed = ledger
            .blocks
            .iter()
            .filter(|(hash, info)| expected.contains(hash) && info.confirmed.inner())
            .count();
        let absent = expected.len().saturating_sub(ledger.blocks.len());
        let history_hashes = logic.lock().unwrap().pr_finalized_hashes(pr_index);
        let confirmed_without_history = ledger
            .blocks
            .iter()
            .filter(|(hash, info)| {
                expected.contains(hash) && info.confirmed.inner() && !history_hashes.contains(hash)
            })
            .count();
        let unconfirmed = ledger
            .blocks
            .iter()
            .filter(|(hash, info)| expected.contains(hash) && !info.confirmed.inner())
            .count();
        if unconfirmed > 0 {
            let logic = logic.lock().unwrap();
            for (hash, _) in ledger
                .blocks
                .iter()
                .filter(|(hash, info)| expected.contains(hash) && !info.confirmed.inner())
            {
                info!(
                    pr = pr_index,
                    block_hash = %hash,
                    reference_canonical_epoch = ?logic.canonical_epoch(0, hash),
                    pr_canonical_epoch = ?logic.canonical_epoch(pr_index, hash),
                    pr_finalized_epochs = ?logic.finalized_epochs(pr_index, hash),
                    "RAI stalled block diagnosis"
                );
            }
        }
        info!(
            pr = pr_index,
            expected = expected.len(),
            ledger_confirmed,
            unconfirmed,
            absent,
            history_finalized = history_hashes.len(),
            confirmed_without_history,
            "RAI node finality diagnosis"
        );
        if pr_index > 0 {
            let logic = logic.lock().unwrap();
            for hash in expected.iter().filter(|hash| {
                logic.canonical_epoch(0, hash) != logic.canonical_epoch(pr_index, hash)
            }) {
                info!(
                    pr = pr_index,
                    block_hash = %hash,
                    reference_canonical_epoch = ?logic.canonical_epoch(0, hash),
                    pr_canonical_epoch = ?logic.canonical_epoch(pr_index, hash),
                    reference_finalized_epochs = ?logic.finalized_epochs(0, hash),
                    pr_finalized_epochs = ?logic.finalized_epochs(pr_index, hash),
                    "RAI canonical epoch mismatch diagnosis"
                );
            }
        }
        all_confirmed &= ledger_confirmed == expected.len();
    }

    Ok(all_nodes_reported && all_confirmed)
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
    logic: &Mutex<SpamLogic>,
    cancel_token: CancellationToken,
    drop_probability: f64,
    fork_recipients: usize,
    clock: &SteadyClock,
) {
    let mut serializer = MessageSerializer::new(protocol);
    let mut fork_serializer = MessageSerializer::new(protocol);
    let mut writer_index = 0;
    loop {
        let forks = select! {
            _ = cancel_token.cancelled() => break,
            value = rx_blocks.recv() => match value {
                Some(value) => value,
                None => break,
            }
        };
        let block = forks.block.clone();
        let hash = block.hash();
        let publish = Message::Publish(Publish::new_from_originator(block));
        let buffer = serializer.serialize(&publish);
        let mut fork_buffer = None;

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

                let send_fork = fork_recipients > 0 && counter < fork_recipients
                    || fork_recipients == 0 && counter % 2 == 0;
                let buf = fork_buffer
                    .as_ref()
                    .filter(|_| send_fork)
                    .unwrap_or(&buffer);

                s.spawn(async {
                    stream[writer_index].write_all(buf).await.unwrap();
                });

                counter += 1;
            }
        });

        let now = clock.now();

        writer_index += 1;
        if writer_index >= CONNECTIONS_PER_NODE {
            writer_index = 0;
        }

        let was_high_prio = {
            let mut l = logic.lock().unwrap();
            // TODO support delayed forks
            let prio = l.published(&hash, now);
            #[cfg(feature = "rai_protocol")]
            let finished = l.all_blocks_published();
            #[cfg(not(feature = "rai_protocol"))]
            let finished = l.is_finished();
            if finished {
                break;
            }
            prio
        };

        if was_high_prio {
            tracing::info!("High prio block published: {hash}");
        }
    }
    if !cancel_token.is_cancelled() {
        cancel_token.cancel();
    }
}

async fn republish_delayed_blocks(
    tx_forks: mpsc::Sender<Forks>,
    logic: &Mutex<SpamLogic>,
    clock: &SteadyClock,
    cancel_token: CancellationToken,
) {
    while !cancel_token.is_cancelled() {
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

        select! {
            _ = cancel_token.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
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
                    loop{
                        let _ = reader.read(&mut recv_buffer).await.unwrap();
                    }
                });
            }
            set.join_all().await;
        } => {}
    }
}

#[cfg(not(feature = "rai_protocol"))]
fn track_confirmations(
    rx_ws_msg: std::sync::mpsc::Receiver<(MessageEnvelope, Timestamp)>,
    logic: &Mutex<SpamLogic>,
    cancel_token: CancellationToken,
) {
    while let Ok((msg, timestamp)) = rx_ws_msg.recv() {
        let pr_index = 0;
        if msg.topic == Some(Topic::Confirmation) {
            let data: BlockConfirmed = serde_json::from_value(msg.message.unwrap()).unwrap();
            let block_hash = BlockHash::decode_hex(data.hash).unwrap();

            let (high_prio_conf_time, finished) = {
                let mut logic = logic.lock().unwrap();
                #[cfg(feature = "rai_protocol")]
                if let Some(election_info) = &data.election_info {
                    let final_tally = rsnano_types::Amount::decode_dec(&election_info.final_tally)
                        .expect("invalid final tally in confirmation message");
                    let epoch = election_info
                        .epoch
                        .as_deref()
                        .and_then(|value| value.parse::<u64>().ok());
                    logic.record_pr_finalization(
                        pr_index,
                        block_hash,
                        epoch,
                        final_tally,
                        timestamp,
                    );
                    if pr_index == 0
                        && logic.delayed.primary_hash(&block_hash).is_some()
                        && let Some(epoch) = epoch
                    {
                        logic.record_finalization_type(epoch, final_tally);
                    }
                }
                #[cfg(feature = "rai_protocol")]
                let epoch = data
                    .election_info
                    .as_ref()
                    .and_then(|info| info.epoch.as_deref())
                    .and_then(|value| value.parse::<u64>().ok());
                let conf_time = if pr_index == 0 {
                    logic.confirmed(
                        &block_hash,
                        timestamp,
                        #[cfg(feature = "rai_protocol")]
                        epoch,
                    )
                } else {
                    None
                };
                #[cfg(feature = "rai_protocol")]
                let finished = logic.is_finished();
                #[cfg(not(feature = "rai_protocol"))]
                let finished = logic.is_finished();
                (conf_time, finished)
            };

            if finished {
                cancel_token.cancel();
            }

            if let Some(time) = high_prio_conf_time {
                tracing::info!(
                    "High prio block confirmed: {block_hash}. Conf time: {} ms",
                    time.as_millis()
                );
            }
        } else if pr_index == 0
            && msg.topic == Some(Topic::ElectionTerminated)
            && let Some(message) = msg.message
            && let Some(hash) = message.get("hash").and_then(|value| value.as_str())
            && let Some(hash) = BlockHash::decode_hex(hash)
        {
            let timeout = message
                .get("timeout")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let mut logic = logic.lock().unwrap();
            if logic.terminated(&hash, timeout, timestamp) {
                info!(
                    "Terminated {} of {} elections",
                    logic.terminated_total,
                    logic.block_factory.created()
                );
            }
            #[cfg(feature = "rai_protocol")]
            let finished = logic.block_factory.created() >= logic.block_factory.max_blocks()
                && logic.terminated_total >= logic.block_factory.max_blocks();
            #[cfg(feature = "rai_protocol")]
            if finished {
                cancel_token.cancel();
            }
        }
    }
}

#[cfg(feature = "rai_protocol")]
fn track_rai_confirmation_feedback(
    rx_ws_msg: std::sync::mpsc::Receiver<(MessageEnvelope, Timestamp)>,
    logic: &Mutex<SpamLogic>,
    cancel_token: CancellationToken,
) {
    while let Ok((msg, timestamp)) = rx_ws_msg.recv() {
        if msg.topic != Some(Topic::Confirmation) {
            continue;
        }
        let data: BlockConfirmed = serde_json::from_value(msg.message.unwrap()).unwrap();
        let block_hash = BlockHash::decode_hex(data.hash).unwrap();
        let finished = {
            let mut logic = logic.lock().unwrap();
            logic.confirmed(&block_hash, timestamp, None);
            logic.is_finished()
        };
        if finished {
            cancel_token.cancel();
            return;
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

#[cfg(not(feature = "rai_protocol"))]
async fn reconcile_confirmations(
    rpc: &NanoRpcClient,
    logic: &Mutex<SpamLogic>,
    clock: &SteadyClock,
    cancel_token: CancellationToken,
) {
    while !cancel_token.is_cancelled() {
        let hashes = logic.lock().unwrap().unconfirmed_hashes();
        if hashes.is_empty() && logic.lock().unwrap().is_finished() {
            cancel_token.cancel();
            return;
        }
        for hash in hashes {
            if rpc
                .block_info(hash)
                .await
                .is_ok_and(|info| info.confirmed.inner())
            {
                let finished = {
                    let mut logic = logic.lock().unwrap();
                    logic.confirmed(&hash, clock.now());
                    logic.is_finished()
                };
                if finished {
                    cancel_token.cancel();
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
