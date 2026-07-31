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
    confirmation_tracker::reconcile_confirmations,
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
            let node_handles = start_nodes(&self.args, data_dir.clone(), &self.rpc_clients).await;
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

        let (tx_ws_msg, rx_ws_msg) = std::sync::mpsc::channel::<(MessageEnvelope, Timestamp)>();

        info!("Connecting to websocket...");
        let mut conf_receiver = ConfirmationReceiver::connect().await?;
        let initial_cemented = genesis_rpc.block_count().await?.cemented.inner();
        let expected_cemented = self
            .args
            .blocks
            .map(|blocks| initial_cemented + blocks as u64);

        info!("Starting with {} BPS", logic.lock().unwrap().current_bps);

        let started = Instant::now();
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
            let statuses = fetch_rai_statuses(&self.rpc_clients).await?;
            print_finalized_blocks_by_epoch(&statuses);
            if self.args.rai_epoch_duration_ms.is_some() {
                validate_epoch_transition(&statuses, self.args.blocks.unwrap_or(0))?;
            }
        }

        Ok(())
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
    requested_workload: usize,
) -> anyhow::Result<()> {
    let first = statuses
        .first()
        .ok_or_else(|| anyhow!("no PRs reported RAI status"))?;
    for (pr, status) in statuses.iter().enumerate().skip(1) {
        if status.finalized_by_epoch != first.finalized_by_epoch {
            anyhow::bail!("PR{pr} reported different per-epoch finalized counts");
        }
        if status.close_hashes.get("0") != first.close_hashes.get("0") {
            anyhow::bail!("PR{pr} installed a different epoch-0 close hash");
        }
    }

    let epoch_0 = first
        .finalized_by_epoch
        .get("0")
        .map(|value| value.inner())
        .unwrap_or(0);
    let epoch_1 = first
        .finalized_by_epoch
        .get("1")
        .map(|value| value.inner())
        .unwrap_or(0);
    if epoch_0 == 0 {
        anyhow::bail!("epoch 0 has no finalized workload blocks");
    }
    if epoch_1 == 0 {
        anyhow::bail!("epoch 1 has no finalized workload blocks");
    }

    for (pr, status) in statuses.iter().enumerate() {
        if status.close_hashes.get("0").is_none() {
            anyhow::bail!("PR{pr} has no installed epoch-0 close hash");
        }
        if status.closed_through.as_ref().map(|value| value.inner()) != Some(0) {
            anyhow::bail!("PR{pr} has not closed exactly through epoch 0");
        }
        if status.open_epoch.inner() != 1 || status.closing_epoch.is_some() {
            anyhow::bail!("PR{pr} does not have epoch 1 open");
        }
    }

    let finalized: u64 = first
        .finalized_by_epoch
        .values()
        .map(|value| value.inner())
        .sum();
    if finalized != requested_workload as u64 {
        anyhow::bail!(
            "finalized workload is {finalized}, requested workload is {requested_workload}"
        );
    }
    Ok(())
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
            closed_through: Some(0.into()),
            close_hashes: BTreeMap::from([("0".into(), close_hash.into())]),
            finalized_by_epoch: BTreeMap::from([
                ("0".into(), epoch_0.into()),
                ("1".into(), epoch_1.into()),
            ]),
        }
    }

    #[test]
    fn epoch_zero_only_fails() {
        assert!(validate_epoch_transition(&[status(10, 0, "A")], 10).is_err());
    }

    #[test]
    fn different_pr_counts_fail() {
        assert!(validate_epoch_transition(&[status(4, 6, "A"), status(5, 5, "A")], 10).is_err());
    }

    #[test]
    fn different_close_hashes_fail() {
        assert!(validate_epoch_transition(&[status(4, 6, "A"), status(4, 6, "B")], 10).is_err());
    }

    #[test]
    fn matching_epoch_transition_passes() {
        validate_epoch_transition(&[status(4, 6, "A"), status(4, 6, "A")], 10).unwrap();
    }
}
