use std::{
    collections::HashSet,
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
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::info;

#[cfg(feature = "rai_protocol")]
use rsnano_messages::MessageDeserializer;
use rsnano_messages::{Message, MessageSerializer, Publish};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_tcp::{TcpStream, TcpStreamFactory};
use rsnano_nullable_tracing_subscriber::TracingInitializer;
use rsnano_rpc_client::NanoRpcClient;
use rsnano_types::{BlockHash, NetworkType, PrivateKey, ProtocolInfo, RawKey, WalletId};
use rsnano_websocket_messages::{BlockConfirmed, MessageEnvelope, Topic};

#[cfg(feature = "rai_protocol")]
use crate::rai_logging;
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
        if !self.args.summary_only {
            self.tracing_init.init();
        }

        let protocol = ProtocolInfo::default_for(NetworkType::NanoTestNetwork);
        let genesis_hash = get_genesis_hash();

        let mut data_dir = dirs::home_dir().ok_or_else(|| anyhow!("No home dir found"))?;
        data_dir.push("NanoSpam");

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
        let closed_epochs = Arc::new(Mutex::new(HashSet::new()));

        let (tx_ws_msg, rx_ws_msg) = std::sync::mpsc::channel::<(MessageEnvelope, Timestamp)>();

        info!("Connecting to websocket...");
        let mut conf_receiver = ConfirmationReceiver::connect().await?;

        info!("Starting with {} BPS", logic.lock().unwrap().current_bps);

        let started = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                enqueue_blocks(&logic, tx_blocks, &self.clock);
                cancel_block_creation2.cancel();
            });

            s.spawn(|| track_confirmations(rx_ws_msg, &logic));

            tokio_scoped::scope(|scope| {
                scope.spawn(log_status(
                    &logic,
                    &self.clock,
                    cancel_nanospam.clone(),
                    Duration::from_millis(self.args.finalized_blocks_print_interval_ms.get()),
                    self.args.summary_only,
                ));

                if self.args.high_prio_check() {
                    scope.spawn(high_prio_check.run(cancel_block_creation, tx_forks_clone.clone()));
                }

                scope.spawn(conf_receiver.run(cancel_nanospam.clone(), tx_ws_msg, &self.clock));
                scope.spawn(receive_messages(
                    tcp_readers,
                    protocol,
                    cancel_nanospam.clone(),
                    closed_epochs.clone(),
                ));
                scope.spawn(publish_blocks(
                    rx_blocks,
                    tcp_writers,
                    protocol,
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
        let elapsed = started.elapsed();
        let logic = logic.lock().unwrap();
        let created_blocks = logic.block_factory.created();
        println!(
            "{}",
            completion_summary(
                created_blocks,
                logic.confirmed_total,
                logic.current_bps,
                logic.sum_conf_time_total,
                elapsed,
                closed_epochs.lock().unwrap().len(),
            )
        );

        Ok(())
    }
}

fn completion_summary(
    created_blocks: usize,
    confirmed_blocks: usize,
    target_blocks_per_second: usize,
    total_confirmation_time: Duration,
    elapsed: Duration,
    closed_epochs: usize,
) -> String {
    let status = if created_blocks == confirmed_blocks {
        "PASS"
    } else {
        "FAIL"
    };
    let throughput = rate_per_second_milli(confirmed_blocks, elapsed);
    let average_confirmation_time_ms = if confirmed_blocks == 0 {
        0
    } else {
        total_confirmation_time.as_millis() / confirmed_blocks as u128
    };

    format!(
        "event=NANOSPAM_COMPLETE\nstatus={status}\nfinalized_requests={confirmed_blocks}/{created_blocks}\ntarget_blocks_per_second={target_blocks_per_second}\nthroughput_blocks_per_second={}\navg_slot_finalization_latency_ms={average_confirmation_time_ms}\nclosed_epochs={closed_epochs}\nelapsed_ms={}",
        fixed_milli(throughput),
        elapsed.as_millis(),
    )
}

fn rate_per_second_milli(count: usize, elapsed: Duration) -> u64 {
    let elapsed_nanos = elapsed.as_nanos();
    if elapsed_nanos == 0 {
        return 0;
    }

    let rate = (count as u128)
        .saturating_mul(1_000_000_000_000)
        .checked_div(elapsed_nanos)
        .unwrap_or(0);
    rate.min(u64::MAX as u128) as u64
}

fn fixed_milli(value: u64) -> String {
    format!("{}.{:03}", value / 1_000, value % 1_000)
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

        if let Some(fork) = forks.fork {
            let publish_fork = Message::Publish(Publish::new_from_originator(fork));
            fork_buffer = Some(fork_serializer.serialize(&publish_fork));
        }

        // Register the publication before writing to local nodes. A node can
        // confirm the block and deliver its websocket notification while the
        // remaining socket writes are still in progress. Registering
        // afterwards loses that confirmation because it has no publish
        // timestamp and leaves finite runs permanently one block short.
        let was_high_prio = {
            let mut logic = logic.lock().unwrap();
            logic.published(&hash, clock.now())
        };

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
                    stream[writer_index].write_all(buf).await.unwrap();
                });

                counter += 1;
            }
        });

        writer_index += 1;
        if writer_index >= CONNECTIONS_PER_NODE {
            writer_index = 0;
        }

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
    closed_epochs: Arc<Mutex<HashSet<u64>>>,
) {
    #[cfg(feature = "rai_protocol")]
    let protocol = _protocol;
    #[cfg(not(feature = "rai_protocol"))]
    let _ = &closed_epochs;

    select! {
        _ = cancel_token.cancelled() => {},
        _ = async {
            let mut set = JoinSet::new();
            for mut reader in readers.drain(..).flatten() {
                #[cfg(feature = "rai_protocol")]
                let protocol = protocol;
                #[cfg(feature = "rai_protocol")]
                let closed_epochs = closed_epochs.clone();
                set.spawn(async move {
                    #[cfg(feature = "rai_protocol")]
                    let mut deserializer = MessageDeserializer::new(protocol);

                    let mut recv_buffer = vec![0; 1024 * 4];
                    #[cfg(not(feature = "rai_protocol"))]
                    loop {
                        let _ = reader.read(&mut recv_buffer).await.unwrap();
                    }

                    #[cfg(feature = "rai_protocol")]
                    'read_loop: loop {
                        let size = match reader.read(&mut recv_buffer).await {
                            Ok(0) => break,
                            Ok(size) => size,
                            Err(error) => {
                                tracing::debug!(?error, "Could not read message from node");
                                break;
                            }
                        };

                        deserializer.push(&recv_buffer[..size]);
                        while let Some(result) = deserializer.try_deserialize() {
                            match result {
                                Ok(deserialized) => {
                                    if let Some(epoch) =
                                        rai_logging::closed_epoch(&deserialized.message)
                                    {
                                        closed_epochs.lock().unwrap().insert(epoch);
                                    }
                                    rai_logging::log_received_message(&deserialized.message);
                                }
                                Err(error) => {
                                    tracing::debug!(?error, "Could not deserialize message from node");
                                    break 'read_loop;
                                }
                            }
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
    interval: Duration,
    summary_only: bool,
) {
    while timeout(interval, cancel_token.cancelled()).await.is_err() {
        let now = clock.now();

        let stats = {
            let mut l = logic.lock().unwrap();
            let stats = l.stats(now);
            l.reset_cps_counter(now);
            stats
        };

        if summary_only {
            println!("{}", finalized_blocks_progress(stats.total_confirmed));
        } else {
            info!(
                "Confirmed {} blocks | {} bps | {} cps | avg conf time: {} ms",
                stats.total_confirmed.to_formatted_string(&Locale::en),
                stats.target_bps.to_formatted_string(&Locale::en),
                stats.current_cps.to_formatted_string(&Locale::en),
                stats.average_conf_time.as_millis()
            );
        }
    }
}

fn finalized_blocks_progress(total_confirmed: usize) -> String {
    format!("finalized_blocks={total_confirmed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_summary_reports_simulation_style_metrics() {
        let summary = completion_summary(
            1_000,
            900,
            250,
            Duration::from_secs(9),
            Duration::from_secs(5),
            3,
        );

        assert_eq!(
            summary,
            "event=NANOSPAM_COMPLETE\nstatus=FAIL\nfinalized_requests=900/1000\ntarget_blocks_per_second=250\nthroughput_blocks_per_second=180.000\navg_slot_finalization_latency_ms=10\nclosed_epochs=3\nelapsed_ms=5000"
        );
    }

    #[test]
    fn completion_summary_handles_an_empty_instant_run() {
        let summary = completion_summary(0, 0, 1, Duration::ZERO, Duration::ZERO, 0);

        assert!(summary.contains("status=PASS"));
        assert!(summary.contains("throughput_blocks_per_second=0.000"));
        assert!(summary.contains("avg_slot_finalization_latency_ms=0"));
        assert!(summary.contains("closed_epochs=0"));
    }

    #[test]
    fn finalized_blocks_progress_is_machine_readable() {
        assert_eq!(finalized_blocks_progress(1_234), "finalized_blocks=1234");
    }
}
