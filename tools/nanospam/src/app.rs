use std::{
    collections::HashSet,
    net::{Ipv6Addr, SocketAddrV6},
    sync::Mutex,
    thread::yield_now,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

#[cfg(feature = "rai_protocol")]
use rsnano_messages::EpochStart;
use rsnano_messages::{Message, MessageSerializer, Publish};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_tcp::{TcpStream, TcpStreamFactory};
use rsnano_nullable_tracing_subscriber::TracingInitializer;
use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::PeersDto;
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

#[cfg(feature = "rai_protocol")]
fn log_epoch_stats(epoch: &crate::domain::spam_logic::EpochStats, stdout: bool) {
    let lines = [
        format!(
            "Epoch {} cut elections: {} total, {} finalized ({:.2}%), completion {} ms, average latency {} ms",
            epoch.epoch,
            epoch.cut.total,
            epoch.cut.finalized,
            epoch.cut.confirmation_percent,
            epoch.cut.completion_time.as_millis(),
            epoch.cut.average_latency.as_millis()
        ),
        format!(
            "Epoch {} non-cut elections: {} total, {} finalized ({:.2}%), completion {} ms, average latency {} ms",
            epoch.epoch,
            epoch.non_cut.total,
            epoch.non_cut.finalized,
            epoch.non_cut.confirmation_percent,
            epoch.non_cut.completion_time.as_millis(),
            epoch.non_cut.average_latency.as_millis()
        ),
        format!(
            "Epoch {} cut hash: {} | convergence {} ms | reports {}/{} | rounds 1 | agree {}",
            epoch.epoch,
            epoch.cut_hash,
            epoch.cut_hash_convergence.as_millis(),
            epoch.cut_reports,
            epoch.expected_reports,
            epoch.cut_hashes_agree
        ),
        format!(
            "Epoch {} finalized hash: {} | cut-to-close {} ms | convergence {} ms | reports {}/{} | rounds {}",
            epoch.epoch,
            epoch.epoch_hash,
            epoch.epoch_completion_time.as_millis(),
            epoch.epoch_hash_convergence.as_millis(),
            epoch.epoch_reports,
            epoch.expected_reports,
            epoch.convergence_rounds
        ),
    ];
    for line in lines {
        if stdout {
            println!("{line}");
        } else {
            info!("{line}");
        }
    }
}

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
            let node_handles = start_nodes(&self.args, data_dir, &self.rpc_clients).await;
            if self.args.kill_nodes() {
                self.node_lifetime = NodeLifetime::new(node_handles);
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

        #[cfg(feature = "rai_protocol")]
        wait_for_full_mesh(&self.rpc_clients, self.args.prs).await?;

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

        #[cfg(feature = "rai_protocol")]
        {
            let starts_at_unix_ms =
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64 + 3_000;
            let mut serializer = MessageSerializer::new(protocol);
            let epoch_duration_ms = self.args.epoch_duration * 1_000;
            for epoch in 1..=self.args.epochs as u64 {
                let epoch_starts_at = starts_at_unix_ms + (epoch - 1) * epoch_duration_ms;
                let start = Message::EpochStart(EpochStart {
                    epoch,
                    starts_at_unix_ms: epoch_starts_at,
                    closes_at_unix_ms: epoch_starts_at + epoch_duration_ms,
                });
                let bytes = serializer.serialize(&start).to_vec();
                for writers in &mut tcp_writers {
                    writers[0].write_all(&bytes).await?;
                }
            }
            info!(
                starts_at_unix_ms,
                epoch_duration_ms,
                epochs = self.args.epochs,
                "Scheduled common RAI epochs on all PRs"
            );
        }

        let tx_forks_clone = tx_blocks.clone();
        let cancel_block_creation = CancellationToken::new();
        let cancel_block_creation2 = cancel_block_creation.clone();
        let cancel_nanospam = CancellationToken::new();

        let (tx_ws_msg, rx_ws_msg) =
            std::sync::mpsc::channel::<(usize, MessageEnvelope, Timestamp)>();

        info!("Connecting to websocket...");
        let mut conf_receivers = Vec::with_capacity(self.args.prs);
        for node_index in 0..self.args.prs {
            conf_receivers.push(ConfirmationReceiver::connect(node_index, node_index == 0).await?);
        }

        info!("Starting with {} BPS", logic.lock().unwrap().current_bps);

        let started = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                enqueue_blocks(&logic, tx_blocks, &self.clock);
                cancel_block_creation2.cancel();
            });

            let confirmation_cancel = cancel_nanospam.clone();
            s.spawn(|| track_confirmations(rx_ws_msg, &logic, confirmation_cancel, self.args.prs));

            tokio_scoped::scope(|scope| {
                if self.args.timeout > 0 {
                    let timeout_cancel = cancel_nanospam.clone();
                    let wait = Duration::from_secs(self.args.timeout);
                    scope.spawn(async move {
                        select! {
                            _ = tokio::time::sleep(wait) => timeout_cancel.cancel(),
                            _ = timeout_cancel.cancelled() => {}
                        }
                    });
                }
                scope.spawn(log_status(&logic, &self.clock, cancel_nanospam.clone()));
                scope.spawn(reconcile_confirmations(
                    genesis_rpc,
                    &logic,
                    &self.clock,
                    cancel_nanospam.clone(),
                ));

                if self.args.high_prio_check() {
                    scope.spawn(high_prio_check.run(cancel_block_creation, tx_forks_clone.clone()));
                }

                for (node_index, receiver) in conf_receivers.iter_mut().enumerate() {
                    scope.spawn(receiver.run(
                        node_index,
                        cancel_nanospam.clone(),
                        tx_ws_msg.clone(),
                        &self.clock,
                    ));
                }
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
        let logic = logic.lock().unwrap();
        let created_blocks = logic.block_factory.created();
        let (published_blocks, publication_duration) = logic.publication_stats();
        let publication_duration_secs = publication_duration.as_secs_f64();
        let publication_rate = if publication_duration_secs > 0.0 {
            published_blocks as f64 / publication_duration_secs
        } else {
            0.0
        };
        let finalized_blocks = logic.confirmed_total;
        let terminated_elections = logic.terminated_total;
        let termination_rate = (created_blocks as f64 / duration_secs) as i32;
        let finalization_rate = (finalized_blocks as f64 / duration_secs) as i32;
        let finalization_time = if finalized_blocks > 0 {
            logic.sum_conf_time_total.as_millis() / finalized_blocks as u128
        } else {
            0
        };
        let termination_time = if terminated_elections > 0 {
            logic.sum_termination_time_total.as_millis() / terminated_elections as u128
        } else {
            0
        };
        info!(
            "Terminated {terminated_elections} of {created_blocks} elections in {duration_secs:.2}s"
        );
        info!(
            "Published {published_blocks} blocks in {publication_duration_secs:.2}s ({publication_rate:.2} blocks/s)"
        );
        info!("Election termination rate: {termination_rate} elections/s");
        info!("Finalized {finalized_blocks} of {created_blocks} elections");
        #[cfg(feature = "rai_protocol")]
        info!(
            "Fast finalizations: {} | Final-vote finalizations: {}",
            logic.fast_finalized_total, logic.final_finalized_total
        );
        #[cfg(feature = "rai_protocol")]
        for epoch in logic.epoch_stats(self.args.prs) {
            log_epoch_stats(&epoch, false);
        }
        info!("Finalization rate: {finalization_rate} elections/s");
        if finalized_blocks > 0 {
            info!("Average finalization time: {finalization_time} ms");
        }
        if terminated_elections > 0 {
            info!("Average election termination time: {termination_time} ms");
        }
        if self.args.final_results_only {
            println!(
                "Terminated {terminated_elections} of {created_blocks} elections in {duration_secs:.2}s"
            );
            println!(
                "Published {published_blocks} blocks in {publication_duration_secs:.2}s ({publication_rate:.2} blocks/s)"
            );
            println!("Election termination rate: {termination_rate} elections/s");
            println!("Finalized {finalized_blocks} of {created_blocks} elections");
            #[cfg(feature = "rai_protocol")]
            println!(
                "Fast finalizations: {} | Final-vote finalizations: {}",
                logic.fast_finalized_total, logic.final_finalized_total
            );
            #[cfg(feature = "rai_protocol")]
            if self.args.epochs > 0 {
                for epoch in logic.epoch_stats(self.args.prs) {
                    log_epoch_stats(&epoch, true);
                }
            }
            println!("Finalization rate: {finalization_rate} elections/s");
            println!("Average finalization time: {finalization_time} ms");
            println!("Average election termination time: {termination_time} ms");
        }
        #[cfg(not(feature = "rai_protocol"))]
        if self.args.timeout > 0 && terminated_elections < created_blocks {
            return Err(anyhow!(
                "only {terminated_elections} of {created_blocks} elections terminated within {} seconds",
                self.args.timeout
            ));
        }
        #[cfg(feature = "rai_protocol")]
        if let Some(error) = logic.epoch_error() {
            return Err(anyhow!(error.to_owned()));
        }
        #[cfg(feature = "rai_protocol")]
        if logic.epochs_completed() != self.args.epochs {
            return Err(anyhow!(
                "only {} of {} RAI epochs finalized within {} seconds",
                logic.epochs_completed(),
                self.args.epochs,
                self.args.timeout
            ));
        }

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
                if tx_blocks.blocking_send(forks).is_err() {
                    break;
                }
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
            if l.is_finished() {
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

fn track_confirmations(
    rx_ws_msg: std::sync::mpsc::Receiver<(usize, MessageEnvelope, Timestamp)>,
    logic: &Mutex<SpamLogic>,
    cancel_token: CancellationToken,
    prs: usize,
) {
    loop {
        let (node_index, msg, timestamp) = match rx_ws_msg.recv_timeout(Duration::from_millis(100))
        {
            Ok(message) => message,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !cancel_token.is_cancelled() => {
                continue;
            }
            Err(_) => break,
        };
        if node_index != 0
            && msg.topic != Some(Topic::EpochComplete)
            && msg.topic != Some(Topic::EpochCut)
        {
            continue;
        }
        if msg.topic == Some(Topic::Confirmation) {
            let data: BlockConfirmed = serde_json::from_value(msg.message.unwrap()).unwrap();
            let block_hash = BlockHash::decode_hex(data.hash).unwrap();

            let (high_prio_conf_time, finished) = {
                let mut logic = logic.lock().unwrap();
                #[cfg(feature = "rai_protocol")]
                if logic.delayed.primary_hash(&block_hash).is_some()
                    && let Some(election_info) = &data.election_info
                {
                    let final_tally = rsnano_types::Amount::decode_dec(&election_info.final_tally)
                        .expect("invalid final tally in confirmation message");
                    logic.record_finalization_type(final_tally);
                }
                let conf_time = logic.confirmed(&block_hash, timestamp);
                (conf_time, logic.is_finished())
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
        } else if msg.topic == Some(Topic::ElectionTerminated)
            && let Some(ref message) = msg.message
            && let Some(hash) = message.get("hash").and_then(|value| value.as_str())
            && let Some(hash) = BlockHash::decode_hex(hash)
        {
            let timeout = message
                .get("timeout")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let mut logic = logic.lock().unwrap();
            logic.terminated(&hash, timeout, timestamp);
        } else {
            #[cfg(feature = "rai_protocol")]
            if msg.topic == Some(Topic::EpochCut) {
                if let Some(message) = msg.message {
                    let Some(epoch) = message.get("epoch").and_then(|value| value.as_u64()) else {
                        continue;
                    };
                    let Some(cut_hash) = message
                        .get("cut_hash")
                        .and_then(|value| value.as_str())
                        .and_then(rsnano_types::Blake2Hash::decode_hex)
                    else {
                        continue;
                    };
                    let decode = |name: &str| -> HashSet<BlockHash> {
                        message
                            .get(name)
                            .and_then(|v| v.as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v.as_str())
                            .filter_map(BlockHash::decode_hex)
                            .collect()
                    };
                    let cut = decode("cut");
                    let non_cut = decode("non_cut");
                    info!(
                        cut = cut.len(),
                        non_cut = non_cut.len(),
                        "Received RAI epoch cut"
                    );
                    logic
                        .lock()
                        .unwrap()
                        .cut_reported(node_index, epoch, cut_hash, cut, non_cut, timestamp);
                }
            } else if msg.topic == Some(Topic::EpochComplete) {
                let Some(message) = msg.message else {
                    continue;
                };
                let Some(epoch) = message.get("epoch").and_then(|value| value.as_u64()) else {
                    continue;
                };
                let Some(non_cut_count) = message
                    .get("non_cut_count")
                    .and_then(|value| value.as_u64())
                else {
                    continue;
                };
                let Some(round) = message.get("round").and_then(|value| value.as_u64()) else {
                    continue;
                };
                let Some(finalized_hash) = message
                    .get("finalized_hash")
                    .and_then(|value| value.as_str())
                else {
                    continue;
                };
                let (completed, error) = {
                    let mut logic = logic.lock().unwrap();
                    logic.epoch_reported(
                        node_index,
                        epoch,
                        round as u32,
                        non_cut_count,
                        finalized_hash.to_owned(),
                        timestamp,
                        prs,
                    );
                    (logic.epochs_completed(), logic.epoch_error().is_some())
                };
                info!(node_index, epoch, completed, "RAI epoch finalized by PR");
                if logic.lock().unwrap().is_finished() || error {
                    cancel_token.cancel();
                }
            }
        }
    }
}

#[cfg(feature = "rai_protocol")]
async fn wait_for_full_mesh(clients: &[NanoRpcClient], prs: usize) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let mut ready = true;
        for client in clients {
            let count = match client.peers(None).await? {
                PeersDto::Simple(peers) => peers.peers.len(),
                PeersDto::Detailed(peers) => peers.peers.len(),
            };
            if count < prs.saturating_sub(1) {
                ready = false;
                break;
            }
        }
        if ready {
            info!(prs, "All PRs are fully interconnected before epoch start");
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!("PR network did not become fully interconnected"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
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
            if cancel_token.is_cancelled() {
                return;
            }
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
