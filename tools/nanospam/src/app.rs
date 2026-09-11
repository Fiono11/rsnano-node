use std::{
    net::{Ipv6Addr, SocketAddrV6},
    sync::Mutex,
    thread::yield_now,
    time::{Duration, Instant},
};

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

        let data_dir = self.args.data_dir.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .expect("No home dir found")
                .join("NanoSpam")
        });

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
            let node_handles = start_nodes(&self.args, data_dir.clone(), &self.rpc_clients).await;
            if self.args.kill_nodes() {
                self.node_lifetime = NodeLifetime::new(node_handles);
            }
        }

        #[cfg(feature = "rai_protocol")]
        verify_fixed_committee(&self.rpc_clients, "startup").await?;

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

        #[cfg(feature = "rai_protocol")]
        verify_fixed_committee(&self.rpc_clients, "workload_start").await?;

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

        #[cfg(feature = "rai_protocol")]
        if self.args.epoch_length > 0 && !self.args.attach {
            let start = std::time::SystemTime::now() + Duration::from_secs(1);
            let millis = start.duration_since(std::time::UNIX_EPOCH)?.as_millis();
            let temporary = data_dir.join("epoch-start-ms.tmp");
            std::fs::write(&temporary, millis.to_string())?;
            std::fs::rename(temporary, data_dir.join("epoch-start-ms"))?;
            info!(
                "EPOCH_SCHEDULE {}",
                serde_json::json!({"start_unix_ms":millis,"duration_seconds":self.args.epoch_length})
            );
            tokio::time::sleep(
                start
                    .duration_since(std::time::SystemTime::now())
                    .unwrap_or_default(),
            )
            .await;
        }

        info!("Starting with {} BPS", logic.lock().unwrap().current_bps);

        let started = Instant::now();
        logic.lock().unwrap().deadline = Some(started + Duration::from_secs(60));
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
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let duration_secs = started.elapsed().as_secs_f64();
        let mut logic = logic.lock().unwrap();
        let created_blocks = logic.block_factory.created();
        let confirmed_blocks = logic.confirmed_total;
        let cps = confirmed_blocks as f64 / duration_secs;
        info!("Confirming {created_blocks} blocks took {duration_secs:.2}s");
        info!("Confirmation rate: {cps} cps");
        let conf_time = if confirmed_blocks == 0 {
            0.0
        } else {
            logic.sum_conf_time_total.as_secs_f64() * 1000.0 / confirmed_blocks as f64
        };
        info!("Average conf time: {conf_time} ms");
        let confirmed_nonforks = confirmed_blocks - logic.confirmed_forks;
        let mean_ms = |duration: Duration, count: usize| {
            (count > 0).then(|| duration.as_secs_f64() * 1000.0 / count as f64)
        };
        let summary = serde_json::json!({ "confirmed_forks":logic.confirmed_forks,"confirmed_nonforks":confirmed_nonforks,"average_fork_confirmation_ms":mean_ms(logic.sum_fork_time, logic.confirmed_forks),"average_nonfork_confirmation_ms":mean_ms(logic.sum_nonfork_time, confirmed_nonforks), "rai_protocol": cfg!(feature = "rai_protocol"), "epoch_length": self.args.epoch_length, "prs": self.args.prs, "accounts": self.args.accounts, "created_blocks": created_blocks, "published_blocks": logic.published_blocks, "confirmed_blocks": confirmed_blocks, "duration_seconds": duration_secs, "confirmation_rate_cps": cps, "average_confirmation_ms": conf_time });
        let workload_records = logic.workload_records.clone();
        let published_hashes = logic.published_hashes.clone();
        let published_workload_blocks = logic.published_blocks;
        let workload_roots = logic.workload_roots.clone();
        let logic_metrics = Mutex::new(std::mem::take(&mut logic.epoch_performance));
        drop(logic);
        info!("BENCHMARK_RESULT {summary}");
        #[cfg(feature = "rai_protocol")]
        if self.args.epoch_length > 0 {
            anyhow::ensure!(
                self.args.blocks == Some(workload_roots.len())
                    && published_workload_blocks == workload_roots.len(),
                "Requested workload was not completely generated"
            );
            let closure = verify_epoch_closures(
                &self.rpc_clients,
                self.args.epoch_length,
                self.args.closed_epochs,
            );
            tokio::pin!(closure);
            let closed_epochs = loop {
                tokio::select! {
                    result = &mut closure => break result?,
                    message = conf_receiver.next() => {
                        record_outcome(message?, self.clock.now(), &logic_metrics);
                    }
                }
            };
            // Consume outcome messages already queued while the final close RPC completed.
            // Reporting and JSON aggregation happen only after the measured workload.
            let drain_until = Instant::now() + Duration::from_secs(1);
            while Instant::now() < drain_until {
                match timeout(Duration::from_millis(100), conf_receiver.next()).await {
                    Ok(message) => record_outcome(message?, self.clock.now(), &logic_metrics),
                    Err(_) => break,
                }
            }
            let metrics = logic_metrics.lock().unwrap().summarize(closed_epochs);
            info!("EPOCH_PERFORMANCE_RESULT {metrics}");
            return Ok(());
        }
        let performance_cutoff = cutoff;
        let mut cutoff = cutoff;
        let mut observations = vec![Vec::new(); self.rpc_clients.len()];
        let mut diagnostics = vec![serde_json::Value::Null; self.rpc_clients.len()];
        if !cfg!(feature = "rai_protocol") || self.args.audit_output.is_some() {
            collect_audits(&self.rpc_clients, &mut observations, &mut diagnostics).await?;
        }
        let mut agreement = crate::termination_check::check(
            &workload_roots,
            &observations,
            cutoff,
            cfg!(feature = "rai_protocol"),
        );
        if !cfg!(feature = "rai_protocol") || self.args.audit_output.is_some() {
            info!("PERFORMANCE_WINDOW_TERMINATION_RESULT {agreement}");
        }
        if cfg!(feature = "rai_protocol") {
            agreement = check_block_trees(&self.rpc_clients, &workload_roots).await?;
        }
        let mut stable = if agreement["success"] == true { 3 } else { 0 };
        let observation_started = Instant::now();
        // Passive observation only: the nodes' existing vote/request mechanisms
        // remain responsible for recovery. Compare a common cutoff across PRs.
        while cfg!(feature = "rai_protocol")
            && stable < 3
            && observation_started.elapsed() < Duration::from_secs(120)
            && self.args.blocks == Some(workload_roots.len())
        {
            tokio::time::sleep(Duration::from_secs(1)).await;
            cutoff = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            if !cfg!(feature = "rai_protocol") || self.args.audit_output.is_some() {
                collect_audits(&self.rpc_clients, &mut observations, &mut diagnostics).await?;
            }
            agreement = check_block_trees(&self.rpc_clients, &workload_roots).await?;
            stable = if agreement["success"] == true {
                stable + 1
            } else {
                0
            };
            info!(
                "Canonical recovery: {} unmatched roots, {} pending everywhere",
                agreement["failed_roots"], agreement["pending_roots"]
            );
        }
        info!(
            "CANONICAL_AGREEMENT_RESULT {}",
            serde_json::json!({
                "success":agreement["success"], "additional_observation_seconds":(cutoff - performance_cutoff) as f64 / 1e9,
                "performance_cutoff":performance_cutoff, "agreement_cutoff":cutoff
            })
        );
        #[cfg(feature = "rai_protocol")]
        verify_fixed_committee(&self.rpc_clients, "end").await?;

        if let Some(path) = &self.args.audit_output {
            std::fs::write(
                path,
                serde_json::to_vec(
                    &serde_json::json!({"performance_cutoff":performance_cutoff,"cutoff":cutoff,"workload":workload_records,"published_hashes":published_hashes,"observations":observations,"diagnostics":diagnostics} ),
                )?,
            )?;
        }
        info!("TERMINATION_RESULT {agreement}");
        for (pr, events) in observations
            .iter()
            .enumerate()
            .filter(|(_, events)| !events.is_empty())
        {
            let ids = |kinds: &[u8]| {
                events
                    .iter()
                    .filter(|e| {
                        e.4 <= cutoff && workload_roots.contains(&e.1) && kinds.contains(&e.0)
                    })
                    .map(|e| (e.1.clone(), e.3))
                    .collect::<std::collections::HashSet<_>>()
            };
            let notarized = ids(&[1]);
            let finalized = ids(&[2, 4]);
            info!(
                "ELECTION_RESULT {}",
                serde_json::json!({"pr":pr,"terminated":ids(&[1,2,4,7]).len(),"timeout_notarized":ids(&[7]).len(),"notarized":notarized.len(),"notarized_not_finalized":notarized.difference(&finalized).count(),"finalized":finalized.len(),"fast":ids(&[5]).len(),"nonfast_explicit":ids(&[6]).len(),"implicit":ids(&[4]).len()})
            );
        }
        anyhow::ensure!(
            self.args
                .blocks
                .is_some_and(|n| workload_roots.len() == n && published_workload_blocks == n),
            "Requested workload was not completely generated"
        );
        anyhow::ensure!(
            agreement["success"] == true,
            "Termination/certificate agreement failed: {agreement}"
        );
        #[cfg(feature = "rai_protocol")]
        if self.args.epoch_length > 0 {
            verify_epoch_closures(
                &self.rpc_clients,
                self.args.epoch_length,
                self.args.closed_epochs,
            )
            .await?;
        }
        for (index, client) in self.rpc_clients.iter().enumerate() {
            if let Ok(count) = client.block_count().await {
                info!("PR{index} ledger: {}", serde_json::to_string(&count)?);
            }
        }

        Ok(())
    }
}

#[cfg(feature = "rai_protocol")]
async fn verify_fixed_committee(clients: &[NanoRpcClient], phase: &str) -> anyhow::Result<()> {
    use rsnano_types::Amount;
    let weight = Amount::MAX / clients.len() as u128;
    let total = weight * clients.len() as u128;
    let members: std::collections::HashSet<_> = (0..clients.len())
        .map(|i| crate::setup::pr_key(i).account())
        .collect();
    for (pr, client) in clients.iter().enumerate() {
        let q = client.confirmation_quorum_with_details().await?;
        anyhow::ensure!(
            q.online_weight_minimum == total
                && q.trended_stake_total == total
                && q.online_stake_total <= total,
            "PR{pr} does not use the fixed committee quorum base; rebuild the rsnano executable with rai_protocol"
        );
        anyhow::ensure!(
            q.peers.as_ref().is_some_and(|peers| peers
                .iter()
                .all(|p| p.weight == weight && members.contains(&p.account))),
            "PR{pr} has a peer outside the fixed equal-weight committee"
        );
        info!(
            "RAI_QUORUM_SNAPSHOT {}",
            serde_json::json!({"phase":phase,"pr":pr,"quorum":q})
        );
    }
    Ok(())
}

async fn check_block_trees(
    clients: &[NanoRpcClient],
    roots: &std::collections::HashSet<rsnano_types::QualifiedRoot>,
) -> anyhow::Result<serde_json::Value> {
    let mut downloads = JoinSet::new();
    for client in clients {
        let client = client.clone();
        downloads.spawn(async move {
            let response = client.block_tree().await?;
            let events: Vec<crate::termination_check::Event> = serde_json::from_value(
                response
                    .block_tree
                    .ok_or_else(|| anyhow::anyhow!("Missing block tree"))?,
            )?;
            let ledger = client.block_count().await?;
            Ok::<_, anyhow::Error>((events, ledger))
        });
    }
    let mut trees = Vec::new();
    let mut ledgers = Vec::new();
    while let Some(result) = downloads.join_next().await {
        let (tree, ledger) = result??;
        trees.push(tree);
        ledgers.push(ledger);
    }
    let equal = ledgers.first().is_some_and(|first| {
        ledgers.iter().all(|ledger| {
            ledger.cemented == first.cemented
                && ledger.confirmation_epochs == first.confirmation_epochs
        })
    });
    let mut result = crate::termination_check::check(roots, &trees, u64::MAX, true);
    result["finalized_ledgers_equal"] = equal.into();
    if !equal {
        result["success"] = false.into();
    }
    Ok(result)
}

async fn collect_audits(
    clients: &[NanoRpcClient],
    observations: &mut [Vec<crate::termination_check::Event>],
    diagnostics: &mut [serde_json::Value],
) -> anyhow::Result<()> {
    let mut downloads = JoinSet::new();
    for (pr, client) in clients.iter().enumerate() {
        let client = client.clone();
        let mut events = std::mem::take(&mut observations[pr]);
        let mut diagnostic = std::mem::take(&mut diagnostics[pr]);
        downloads.spawn(async move {
            collect_audit(&client, &mut events, &mut diagnostic).await?;
            Ok::<_, anyhow::Error>((pr, events, diagnostic))
        });
    }
    while let Some(result) = downloads.join_next().await {
        let (pr, events, diagnostic) = result??;
        observations[pr] = events;
        diagnostics[pr] = diagnostic;
    }
    Ok(())
}

async fn collect_audit(
    client: &NanoRpcClient,
    events: &mut Vec<crate::termination_check::Event>,
    diagnostic: &mut serde_json::Value,
) -> anyhow::Result<()> {
    let mut offset = events.len() as u64;
    loop {
        let response = client.termination_audit(offset).await?;
        let page = response
            .termination_audit
            .ok_or_else(|| anyhow::anyhow!("Node has no termination audit support"))?;
        anyhow::ensure!(
            page["enabled"] == true && page["overflow"] == false,
            "Termination audit disabled or overflowed"
        );
        if offset == 0 {
            *diagnostic = page.get("active").cloned().unwrap_or_default();
        }
        let batch: Vec<crate::termination_check::Event> =
            serde_json::from_value(page["events"].clone())?;
        let count = batch.len();
        events.extend(batch);
        offset += count as u64;
        if offset >= page["total"].as_u64().unwrap() {
            break;
        }
        anyhow::ensure!(count > 0, "Incomplete audit page");
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
        logic
            .lock()
            .unwrap()
            .epoch_performance
            .published(&block.qualified_root(), clock.now());
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
        if msg.topic == Some(Topic::ElectionOutcome) {
            let event = serde_json::from_value(msg.message.unwrap()).unwrap();
            logic
                .lock()
                .unwrap()
                .epoch_performance
                .observe(event, timestamp);
            continue;
        }
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

#[cfg(feature = "rai_protocol")]
async fn verify_epoch_closures(
    clients: &[NanoRpcClient],
    seconds: u64,
    requested: Option<u64>,
) -> anyhow::Result<u64> {
    let mut target = 1;
    for client in clients {
        let count = client.block_count().await?;
        target = target.max(count.current_epoch.map(u64::from).unwrap_or(0) + 1);
    }
    if let Some(requested) = requested {
        anyhow::ensure!(requested > 0, "closed epochs must be positive");
        target = requested;
    }
    let deadline =
        Instant::now() + Duration::from_secs(seconds.saturating_mul(2).saturating_add(300));
    let mut last_progress = Instant::now();
    loop {
        let mut closed = Vec::new();
        let mut states = Vec::new();
        for client in clients {
            let count = client.block_count().await?;
            states.push(serde_json::json!({"voting_epoch":count.current_epoch,"draining_epoch":count.draining_epoch}));
            closed.push(count.closed_epochs.unwrap_or_default());
        }
        let complete = !closed.is_empty()
            && (0..target).all(|e| {
                let key = e.to_string();
                closed[0]
                    .get(&key)
                    .is_some_and(|expected| closed.iter().all(|c| c.get(&key) == Some(expected)))
            });
        if complete {
            if requested.is_some() {
                anyhow::ensure!(
                    closed.iter().all(|c| c.len() == target as usize),
                    "unexpected extra closed epochs"
                );
            }
            info!(
                "EPOCH_CLOSE_RESULT {}",
                serde_json::json!({"success":true,"epochs_checked":target,"closed_epochs":closed[0]})
            );
            return Ok(target);
        }
        if last_progress.elapsed() >= Duration::from_secs(5) {
            info!(
                "EPOCH_CLOSE_WAIT {}",
                serde_json::json!({"target_epoch":target - 1,"states":states,"closed_epochs":closed})
            );
            last_progress = Instant::now();
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "Epoch close hashes did not converge through epoch {}: {:?}",
            target - 1,
            closed
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn record_outcome(
    msg: MessageEnvelope,
    received: Timestamp,
    metrics: &Mutex<crate::epoch_performance::EpochPerformance>,
) {
    if msg.topic == Some(Topic::ElectionOutcome) {
        metrics.lock().unwrap().observe(
            serde_json::from_value(msg.message.unwrap()).unwrap(),
            received,
        );
    }
}
