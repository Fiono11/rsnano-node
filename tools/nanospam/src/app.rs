use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    thread::yield_now,
    time::{Duration, Instant},
};

#[cfg(feature = "rai_protocol")]
use std::collections::BTreeMap;

use anyhow::{Context, anyhow, ensure};
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
use tracing::{info, warn};

use rsnano_messages::{Message, MessageSerializer, Publish};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_tcp::{TcpStream, TcpStreamFactory};
use rsnano_nullable_tracing_subscriber::TracingInitializer;
use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::BlocksInfoArgs;
use rsnano_types::{
    Account, BlockHash, JsonBlock, NetworkType, PrivateKey, ProtocolInfo, RawKey, WalletId,
};
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
const RPC_TIMEOUT: Duration = Duration::from_secs(5);
// Close elections use eventual repair and have no 24-second protocol bound.
// Leave enough room for several report/cut/drain/record repair rounds while
// retaining the global watchdog as the outer harness deadline.
#[cfg(feature = "rai_protocol")]
const RAI_CLOSE_TIMEOUT: Duration = Duration::from_secs(240);

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

    pub async fn run(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let last_phase = self.last_rai_phase.clone();
        let global_timeout = Duration::from_secs(self.args.global_timeout_secs);
        let global_deadline = Instant::now() + global_timeout;
        // `run_inner` contains a synchronous scoped workload. A Tokio timer
        // alone cannot be polled while that scope is blocking this executor
        // thread, so a small OS-thread watchdog must also cancel its workers.
        let timed_shutdown = shutdown.child_token();
        let watchdog_shutdown = timed_shutdown.clone();
        std::thread::spawn(move || {
            std::thread::sleep(global_timeout);
            watchdog_shutdown.cancel();
        });
        match timeout(
            global_timeout,
            self.run_inner(timed_shutdown, global_deadline),
        )
        .await
        {
            Ok(result) => result.map_err(|error| {
                anyhow!(
                    "{error:#}; last known RAI phase: {}",
                    last_phase.lock().unwrap(),
                )
            }),
            Err(_) => Err(anyhow!(
                "nanospam exceeded the {global_timeout:?} global timeout; last known RAI phase: {}",
                last_phase.lock().unwrap()
            )),
        }
    }

    async fn run_inner(
        mut self,
        shutdown: CancellationToken,
        global_deadline: Instant,
    ) -> anyhow::Result<()> {
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
                NanoRpcClient::new(format!("http://127.0.0.1:{}", rpc_port(i)).parse().unwrap());
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

        #[cfg(feature = "rai_protocol")]
        if !self.args.setup_only() {
            let mut expected = (0..self.args.prs)
                .map(|i| crate::setup::pr_key(i).account().encode_account())
                .collect::<Vec<_>>();
            expected.sort();
            for (i, rpc) in self.rpc_clients.iter().enumerate() {
                let actual = rpc.rai_status().await?.genesis_committee;
                ensure!(
                    actual == expected,
                    "PR{i} initialized with the wrong RAI genesis committee: expected {expected:?}, got {actual:?}"
                );
            }
        }

        let genesis_wallet_id = if self.args.set_up_new_nodes() {
            *self.last_rai_phase.lock().unwrap() = "creating test wallets".to_string();
            create_wallets(
                &self.rpc_clients,
                genesis_rpc,
                &mut account_map,
                self.args.fund_all_accounts,
            )
            .await?
        } else {
            WalletId::ZERO
        };

        if self.args.sync() {
            sync_frontiers(&self.rpc_clients, &mut account_map).await;
        }

        let live_representatives = (0..self.args.prs)
            .map(|index| crate::setup::pr_key(index).public_key())
            .collect();
        let logic = Mutex::new(SpamLogic::new(
            account_map,
            self.args.spam_spec()?,
            live_representatives,
        ));

        let (tx_blocks, rx_blocks) = mpsc::channel::<Forks>(MAX_BUFFERED_BLOCKS);
        let mut high_prio_check = HighPrioCheck::new(genesis_rpc, &logic);

        if self.args.set_up_new_nodes() {
            high_prio_check
                .create_prio_accounts(genesis_wallet_id, &self.rpc_clients)
                .await?;
        }

        if self.args.setup_only() {
            write_prepared_network(&data_dir, &self.args)?;
            self.stop_nodes_gracefully().await?;
            return Ok(());
        }

        if self.args.sync() {
            high_prio_check.sync_accounts().await?;
        }

        let mut tcp_writers = Vec::new();
        let mut tcp_readers = Vec::new();

        for node_index in 0..self.args.prs {
            let peer_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, peering_port(node_index)));
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
        let cancel_block_creation = shutdown.child_token();
        let cancel_block_creation2 = cancel_block_creation.clone();
        let cancel_nanospam = shutdown.child_token();

        let (tx_ws_msg, rx_ws_msg) = std::sync::mpsc::channel::<(MessageEnvelope, Timestamp)>();

        #[cfg(feature = "rai_protocol")]
        let transition = Mutex::new(RaiTransitionObservation::default());
        let mut confirmation_latencies = HashMap::new();

        info!("Connecting to websocket...");
        let mut conf_receiver = ConfirmationReceiver::connect().await?;
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
            statuses
        } else {
            Vec::new()
        };

        info!("Starting with {} BPS", logic.lock().unwrap().current_bps);
        *self.last_rai_phase.lock().unwrap() =
            "running workload; waiting for epoch 0 to begin closing".to_string();

        let total_timeout = Duration::from_secs(self.args.global_timeout_secs);
        let started = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                enqueue_blocks(&logic, tx_blocks, &self.clock, &shutdown);
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
                    cancel_nanospam.clone(),
                    self.args.drop_probability(),
                    &self.clock,
                ));

                if !self.args.no_republish {
                    scope.spawn(republish_delayed_blocks(
                        tx_forks_clone,
                        &logic,
                        &self.clock,
                        shutdown.clone(),
                    ));
                }
            });
        });
        ensure!(
            !shutdown.is_cancelled(),
            "nanospam exceeded the {total_timeout:?} global timeout while publishing or confirming the workload"
        );
        let duration_secs = started.elapsed().as_secs_f64();
        let logic = logic.lock().unwrap();
        let created_blocks = logic.block_factory.created();
        let requested_blocks = logic.block_factory.max_blocks();
        let publication_times = logic.publication_times().clone();
        let workload_confirmed = logic.is_finished();
        let average_pr0_confirmation_time = logic.average_confirmation_time_total();
        let cps = (created_blocks as f64 / duration_secs) as i32;
        info!("Confirming {created_blocks} blocks took {duration_secs:.2}s");
        info!("Confirmation rate: {cps} cps");
        drop(logic);
        ensure!(
            created_blocks == requested_blocks,
            "workload stopped after creating {created_blocks} of {requested_blocks} requested blocks"
        );
        ensure!(
            publication_times.len() == created_blocks,
            "workload stopped after publishing {} of {created_blocks} created blocks",
            publication_times.len()
        );
        ensure!(
            workload_confirmed,
            "workload stopped before PR0 confirmed all {created_blocks} blocks"
        );
        wait_for_block_confirmations(
            &self.rpc_clients,
            &publication_times,
            &mut confirmation_latencies,
            &shutdown,
            global_deadline,
        )
        .await?;
        let confirmation_samples = confirmation_latencies.len();
        info!(
            "Average PR0 ledger confirmation time: {}",
            format_duration(average_pr0_confirmation_time)
        );
        println!("Block confirmation report:");
        println!(
            "  {created_blocks} blocks confirmed on {} PRs ({confirmation_samples} samples), average PR0 ledger confirmation time {}",
            self.rpc_clients.len(),
            format_duration(average_pr0_confirmation_time)
        );

        #[cfg(feature = "rai_protocol")]
        {
            if self.args.rai_epoch_duration_ms.is_some() {
                wait_for_rai_close(
                    &self.rpc_clients,
                    &initial_rai_statuses,
                    created_blocks as u64,
                    &transition,
                    &self.last_rai_phase,
                    RAI_CLOSE_TIMEOUT.min(
                        global_deadline
                            .checked_duration_since(Instant::now())
                            .unwrap_or(Duration::ZERO)
                            .max(Duration::from_millis(1)),
                    ),
                )
                .await?;
            }
            let statuses = fetch_rai_statuses(&self.rpc_clients).await?;
            if self.args.rai_epoch_duration_ms.is_some() {
                record_rai_statuses(&statuses, &transition, &self.last_rai_phase);
                let observed = transition.into_inner().unwrap();
                validate_epoch_transition(&statuses, &initial_rai_statuses, &observed)?;
                print_rai_final_report(
                    &statuses,
                    &initial_rai_statuses,
                    average_pr0_confirmation_time,
                )?;
            } else {
                print_finalized_blocks_by_epoch(&statuses);
            }
        }

        Ok(())
    }

    async fn stop_nodes_gracefully(&mut self) -> anyhow::Result<()> {
        *self.last_rai_phase.lock().unwrap() = "stopping setup nodes gracefully".to_string();
        for (i, rpc) in self.rpc_clients.iter().enumerate() {
            rpc.stop()
                .await
                .with_context(|| format!("could not stop setup node PR{i} through RPC"))?;
        }

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut all_stopped = true;
            for i in 0..self.rpc_clients.len() {
                if self.node_lifetime.child_status(i)?.is_none() {
                    all_stopped = false;
                }
            }
            if all_stopped {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "setup nodes did not stop within 10 seconds after the stop RPC"
            );
            sleep(Duration::from_millis(50)).await;
        }
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
    epoch_timings: BTreeMap<u64, RaiEpochTiming>,
}

#[cfg(feature = "rai_protocol")]
#[derive(Default)]
struct RaiEpochTiming {
    close_finished_at: Option<Instant>,
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
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }

        let statuses = select! {
            _ = cancel.cancelled() => break,
            statuses = fetch_rai_statuses(clients) => statuses,
        };
        let Ok(statuses) = statuses else {
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
    let now = Instant::now();
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

    // A fast close can complete while the workload observer is stopped and
    // before the post-workload waiter polls again. These durable fields prove
    // the same transition: start_closing opens epoch 1 atomically with marking
    // epoch 0 closing, and the cut/close hashes plus closed_through are retained
    // only after that close has completed on every PR.
    let epoch_zero_close_completed = !statuses.is_empty()
        && statuses.iter().all(|status| {
            status.open_epoch.inner() >= 1
                && status.closed_through.is_some()
                && status.cut_hashes.contains_key("0")
                && status.close_hashes.contains_key("0")
        });
    observed.saw_epoch_zero_closing |= epoch_zero_close_completed;
    observed.saw_open_epoch_one_overlap |= epoch_zero_close_completed;

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

    let mut epochs = statuses
        .iter()
        .filter_map(|status| status.closing_epoch.as_ref().map(|epoch| epoch.inner()))
        .collect::<Vec<_>>();
    epochs.extend(statuses.iter().flat_map(|status| {
        status
            .cut_hashes
            .keys()
            .chain(status.close_hashes.keys())
            .filter_map(|epoch| epoch.parse::<u64>().ok())
    }));
    epochs.sort_unstable();
    epochs.dedup();

    for epoch in epochs {
        let timing = observed.epoch_timings.entry(epoch).or_default();
        let epoch_key = epoch.to_string();
        let record_installed = !statuses.is_empty()
            && statuses
                .iter()
                .all(|status| status.close_hashes.contains_key(&epoch_key));
        if record_installed && timing.close_finished_at.is_none() {
            timing.close_finished_at = Some(now);
        }
    }
}

#[cfg(feature = "rai_protocol")]
async fn wait_for_rai_close(
    clients: &[NanoRpcClient],
    initial_statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    expected_finalized: u64,
    observation: &Mutex<RaiTransitionObservation>,
    last_phase: &Arc<Mutex<String>>,
    timeout_after: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout_after;
    loop {
        let statuses = fetch_rai_statuses(clients).await?;
        record_rai_statuses(&statuses, observation, last_phase);
        // Derive the close target from epochs that actually contain this
        // workload. Using the currently-open epoch races a fast close: by the
        // time all confirmations are observed, the node may already have
        // advanced and the harness would wait for an unrelated empty epoch.
        if let Some(target_epoch) =
            converged_workload_epoch(&statuses, initial_statuses, expected_finalized)
            && statuses.iter().all(|status| {
                status
                    .closed_through
                    .as_ref()
                    .is_some_and(|epoch| epoch.inner() >= target_epoch)
            })
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "RAI workload epochs did not finish closing within {timeout_after:?}; statuses: {statuses:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(feature = "rai_protocol")]
fn converged_workload_epoch(
    statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    initial_statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    expected_finalized: u64,
) -> Option<u64> {
    let Some((first, initial_first)) = statuses.first().zip(initial_statuses.first()) else {
        return None;
    };
    if statuses.len() != initial_statuses.len() {
        return None;
    }
    let Ok(expected_counts) = workload_counts(first, initial_first) else {
        return None;
    };
    if expected_counts.values().sum::<u64>() != expected_finalized
        || !statuses
            .iter()
            .zip(initial_statuses)
            .skip(1)
            .all(|(status, initial)| {
                workload_counts(status, initial).is_ok_and(|counts| counts == expected_counts)
            })
    {
        return None;
    }

    expected_counts
        .keys()
        .map(|epoch| epoch.parse::<u64>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?
        .into_iter()
        .max()
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
fn print_rai_final_report(
    statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    initial_statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    average_pr0_finalization: Option<Duration>,
) -> anyhow::Result<()> {
    let status = statuses
        .first()
        .ok_or_else(|| anyhow!("no PRs reported final RAI status"))?;
    let initial = initial_statuses
        .first()
        .ok_or_else(|| anyhow!("no initial RAI status was recorded"))?;
    let counts = workload_counts(status, initial)?;

    let finalized: u64 = counts.values().sum();
    println!("RAI finalization report:");
    println!(
        "  {finalized} finalized blocks, average PR0 finalization time {}",
        format_duration(average_pr0_finalization)
    );
    println!("  close protocol timings by epoch:");
    for (epoch, close_hash) in &status.close_hashes {
        let count = counts.get(epoch).copied().unwrap_or(0);
        println!("    epoch {epoch}: {count} finalized blocks");
        println!("      close hash: {close_hash}");
        println!(
            "      close cut election: {}",
            format_duration(average_close_election_duration(statuses, epoch, |status| {
                &status.cut_election_durations_us
            }))
        );
        println!(
            "      close record election: {}",
            format_duration(average_close_election_duration(statuses, epoch, |status| {
                &status.record_election_durations_us
            }))
        );
    }
    Ok(())
}

async fn wait_for_block_confirmations(
    clients: &[NanoRpcClient],
    publication_times: &HashMap<BlockHash, Instant>,
    latencies: &mut HashMap<(usize, BlockHash), Duration>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> anyhow::Result<()> {
    let expected = clients.len() * publication_times.len();
    if expected == 0 {
        return Ok(());
    }
    let mut last_progress_log = Instant::now() - Duration::from_secs(1);
    loop {
        observe_block_confirmations_once(clients, publication_times, latencies, cancel, deadline)
            .await?;
        if all_pr_confirmations_observed(clients.len(), publication_times.len(), latencies.len()) {
            return Ok(());
        }

        if last_progress_log.elapsed() >= Duration::from_secs(1) {
            info!(
                "Observed {}/{} all-block/all-PR ledger confirmations; per PR: {:?}",
                latencies.len(),
                expected,
                confirmation_counts_by_pr(clients.len(), latencies)
            );
            last_progress_log = Instant::now();
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "only observed {}/{} all-block/all-PR ledger confirmations before the global deadline; per PR: {:?}",
                latencies.len(),
                expected,
                confirmation_counts_by_pr(clients.len(), latencies)
            );
        }
        select! {
            _ = cancel.cancelled() => anyhow::bail!(
                "confirmation validation was cancelled at {}/{} all-block/all-PR samples",
                latencies.len(),
                expected
            ),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

async fn observe_block_confirmations_once(
    clients: &[NanoRpcClient],
    publication_times: &HashMap<BlockHash, Instant>,
    latencies: &mut HashMap<(usize, BlockHash), Duration>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> anyhow::Result<()> {
    for (pr, client) in clients.iter().enumerate() {
        let pending = publication_times
            .keys()
            .filter(|hash| !latencies.contains_key(&(pr, **hash)))
            .copied()
            .collect::<Vec<_>>();
        for batch in pending.chunks(128) {
            if cancel.is_cancelled() || Instant::now() >= deadline {
                anyhow::bail!(
                    "global deadline reached while validating confirmations ({}/{} samples)",
                    latencies.len(),
                    clients.len() * publication_times.len()
                );
            }
            let args = BlocksInfoArgs {
                receivable: None,
                receive_hash: None,
                source: None,
                include_not_found: Some(true.into()),
                include_linked_account: None,
                hashes: batch.to_vec(),
            };
            let request_timeout = RPC_TIMEOUT.min(
                deadline
                    .checked_duration_since(Instant::now())
                    .unwrap_or(Duration::ZERO),
            );
            let request = select! {
                _ = cancel.cancelled() => anyhow::bail!(
                    "confirmation validation was cancelled while querying PR{pr}"
                ),
                result = timeout(request_timeout, client.blocks_info(args)) => result,
            };
            let response = match request {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    warn!(
                        "RPC error while checking ledger confirmation state for {pr} batch {}: {error}",
                        batch.len()
                    );
                    continue;
                }
                Err(error) => {
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "global deadline reached while querying confirmation state from PR{pr}"
                        );
                    }
                    warn!(
                        "RPC timeout while checking ledger confirmation state for {pr} batch {}: {error}",
                        batch.len()
                    );
                    continue;
                }
            };
            let observed_at = Instant::now();
            for (hash, info) in response.blocks {
                if info.confirmed.inner()
                    && let Some(published_at) = publication_times.get(&hash)
                {
                    latencies
                        .entry((pr, hash))
                        .or_insert_with(|| observed_at.saturating_duration_since(*published_at));
                }
            }
        }
    }
    Ok(())
}

fn all_pr_confirmations_observed(
    pr_count: usize,
    block_count: usize,
    observed_samples: usize,
) -> bool {
    observed_samples == pr_count * block_count
}

fn confirmation_counts_by_pr(
    pr_count: usize,
    latencies: &HashMap<(usize, BlockHash), Duration>,
) -> Vec<usize> {
    let mut counts = vec![0; pr_count];
    for (pr, _) in latencies.keys() {
        if let Some(count) = counts.get_mut(*pr) {
            *count += 1;
        }
    }
    counts
}

#[cfg(feature = "rai_protocol")]
fn average_close_election_duration<'a>(
    statuses: &'a [rsnano_rpc_messages::RaiStatusResponse],
    epoch: &str,
    durations: impl Fn(
        &'a rsnano_rpc_messages::RaiStatusResponse,
    ) -> &'a BTreeMap<String, rsnano_rpc_messages::RpcU64>,
) -> Option<Duration> {
    if statuses.is_empty() {
        return None;
    }
    let total = statuses.iter().try_fold(0_u128, |total, status| {
        durations(status)
            .get(epoch)
            .map(|duration| total + duration.inner() as u128)
    })?;
    Some(Duration::from_micros(
        (total / statuses.len() as u128) as u64,
    ))
}

fn format_duration(duration: Option<Duration>) -> String {
    duration
        .map(|value| format!("{:.2} ms", value.as_secs_f64() * 1_000.0))
        .unwrap_or_else(|| "not observed".to_string())
}

#[cfg(feature = "rai_protocol")]
async fn fetch_rai_statuses(
    clients: &[NanoRpcClient],
) -> anyhow::Result<Vec<rsnano_rpc_messages::RaiStatusResponse>> {
    let mut statuses = Vec::with_capacity(clients.len());
    for (pr, client) in clients.iter().enumerate() {
        let response = timeout(RPC_TIMEOUT, client.rai_status())
            .await
            .map_err(|error| anyhow!("RAI status query timed out for PR{pr}: {error}"))?
            .map_err(|error| anyhow!("could not query RAI status from PR{pr}: {error}"))?;
        statuses.push(response);
    }
    Ok(statuses)
}

#[cfg(feature = "rai_protocol")]
fn validate_epoch_transition(
    statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    initial_statuses: &[rsnano_rpc_messages::RaiStatusResponse],
    observed: &RaiTransitionObservation,
) -> anyhow::Result<()> {
    if let Some(error) = &observed.validation_error {
        anyhow::bail!("RAI validation error: {error}");
    }
    ensure!(
        observed.saw_epoch_zero_closing,
        "epoch 0 was never observed closing"
    );
    ensure!(
        observed.saw_open_epoch_one_overlap,
        "epoch 1 was not observed open while epoch 0 was closing"
    );
    ensure!(
        observed.saw_matching_cut,
        "no matching epoch-0 cut was observed"
    );
    ensure!(
        observed.saw_obligations_finalized,
        "epoch-0 drain obligations did not all reach terminal outcomes"
    );
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
        let actual_counts = workload_counts(status, initial)?;
        if actual_counts != expected_counts {
            anyhow::bail!(
                "PR{pr} finalized a different per-epoch workload: expected {expected_counts:?}, got {actual_counts:?}"
            );
        }
    }

    let closed_through = first
        .closed_through
        .as_ref()
        .map(|e| e.inner())
        .unwrap_or(0);
    for epoch in 0..=closed_through {
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
        for (pr, status) in statuses.iter().enumerate() {
            ensure!(
                status
                    .cut_election_durations_us
                    .get(&epoch)
                    .is_some_and(|duration| duration.inner() > 0),
                "PR{pr} did not report a positive cut-election duration for epoch {epoch}"
            );
            ensure!(
                status
                    .record_election_durations_us
                    .get(&epoch)
                    .is_some_and(|duration| duration.inner() > 0),
                "PR{pr} did not report a positive record-election duration for epoch {epoch}"
            );
        }
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

fn enqueue_blocks(
    logic: &Mutex<SpamLogic>,
    tx_blocks: mpsc::Sender<Forks>,
    clock: &SteadyClock,
    cancel_token: &CancellationToken,
) {
    loop {
        if cancel_token.is_cancelled() {
            break;
        }

        let now = clock.now();

        let result = {
            let mut l = logic.lock().unwrap();
            let is_fork = rng().random_bool(l.fork_propability());
            l.next_block(is_fork, now)
        };

        match result {
            Some(BlockResult::Block(forks)) => {
                let mut pending = forks;
                loop {
                    if cancel_token.is_cancelled() {
                        return;
                    }
                    match tx_blocks.try_send(pending) {
                        Ok(()) => break,
                        Err(mpsc::error::TrySendError::Full(forks)) => {
                            pending = forks;
                            yield_now();
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                    }
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
    genesis_rpc: &NanoRpcClient,
    logic: &Mutex<SpamLogic>,
    cancel_token: CancellationToken,
    drop_probability: f64,
    clock: &SteadyClock,
) {
    let mut serializer = MessageSerializer::new(protocol);
    let mut fork_serializer = MessageSerializer::new(protocol);
    loop {
        let forks = select! {
            _ = cancel_token.cancelled() => break,
            forks = rx_blocks.recv() => match forks {
                Some(forks) => forks,
                None => break,
            }
        };
        let block = forks.block.clone();
        let writer_index = connection_index(&block);
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

        // Start timing before the socket writes. A fast confirmation can
        // otherwise arrive before DelayedBlocks has a publication timestamp
        // and silently disappear from the average.
        let was_high_prio = {
            let mut l = logic.lock().unwrap();
            l.published(&hash, clock.now())
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
                    select! {
                        _ = cancel_token.cancelled() => {}
                        _ = stream[writer_index].write_all(buf) => {}
                    }
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
                select! {
                    _ = cancel_token.cancelled() => return,
                    _ = genesis_rpc.process(JsonBlock::from(fork)) => {}
                }
            }
            for _ in 0..100 {
                let started = select! {
                    _ = cancel_token.cancelled() => return,
                    response = genesis_rpc.block_confirm(fork_hash) => response
                        .is_ok_and(|response| bool::from(response.started)),
                };
                if started {
                    break;
                }
                select! {
                    _ = cancel_token.cancelled() => return,
                    _ = sleep(Duration::from_millis(50)) => {}
                }
            }
        }

        if was_high_prio {
            tracing::info!("High prio block published: {hash}");
        }
        if logic.lock().unwrap().is_finished() {
            cancel_token.cancel();
            break;
        }
    }
}

fn connection_index(block: &rsnano_types::Block) -> usize {
    block
        .account_field()
        .map(connection_index_for_account)
        .unwrap_or(0)
}

fn connection_index_for_account(account: Account) -> usize {
    account.as_bytes()[31] as usize % CONNECTIONS_PER_NODE
}

async fn republish_delayed_blocks(
    tx_forks: mpsc::Sender<Forks>,
    logic: &Mutex<SpamLogic>,
    clock: &SteadyClock,
    cancel_token: CancellationToken,
) {
    loop {
        if cancel_token.is_cancelled() {
            return;
        }

        while let Some(block) = {
            let now = clock.now();
            let mut l = logic.lock().unwrap();
            if l.is_finished() {
                return;
            }
            l.next_delayed(now)
        } {
            select! {
                _ = cancel_token.cancelled() => return,
                result = tx_forks.send(Forks::new(block)) => {
                    if result.is_err() {
                        return;
                    }
                }
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

    #[test]
    fn confirmation_completion_requires_every_block_on_every_pr() {
        assert!(!all_pr_confirmations_observed(6, 36_000, 36_000));
        assert!(!all_pr_confirmations_observed(6, 36_000, 215_999));
        assert!(all_pr_confirmations_observed(6, 36_000, 216_000));
    }

    #[test]
    fn connection_selection_is_account_affine() {
        let account = PrivateKey::from(42).account();
        let first = connection_index_for_account(account);
        assert_eq!(connection_index_for_account(account), first);
        assert!(first < CONNECTIONS_PER_NODE);
    }

    fn status(epoch_0: u64, epoch_1: u64, close_hash: &str) -> RaiStatusResponse {
        RaiStatusResponse {
            genesis_committee: Vec::new(),
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
            cut_election_durations_us: BTreeMap::from([
                ("0".into(), 1_000.into()),
                ("1".into(), 1_000.into()),
            ]),
            record_election_durations_us: BTreeMap::from([
                ("0".into(), 2_000.into()),
                ("1".into(), 2_000.into()),
            ]),
        }
    }

    #[test]
    fn epoch_zero_only_can_converge() {
        validate_epoch_transition(&[status(10, 0, "A")], &[status(0, 0, "A")], &observed())
            .unwrap();
    }

    #[test]
    fn different_per_epoch_finalization_fails() {
        assert!(
            validate_epoch_transition(
                &[status(4, 6, "A"), status(5, 5, "A")],
                &[status(0, 0, "A"), status(0, 0, "A")],
                &observed(),
            )
            .is_err()
        );
    }

    #[test]
    fn different_pr_totals_fail() {
        assert!(
            validate_epoch_transition(
                &[status(4, 6, "A"), status(5, 4, "A")],
                &[status(0, 0, "A"), status(0, 0, "A")],
                &observed(),
            )
            .is_err()
        );
    }

    #[test]
    fn different_close_hashes_fail() {
        assert!(
            validate_epoch_transition(
                &[status(4, 6, "A"), status(4, 6, "B")],
                &[status(0, 0, "A"), status(0, 0, "A")],
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
            &observed(),
        )
        .unwrap();
    }

    #[test]
    fn completed_close_proves_transition_when_transient_phase_was_missed() {
        let statuses = [status(4, 6, "A"), status(4, 6, "A")];
        let initial = [status(0, 0, "A"), status(0, 0, "A")];
        let observation = Mutex::new(RaiTransitionObservation::default());
        let last_phase = Arc::new(Mutex::new(String::new()));

        record_rai_statuses(&statuses, &observation, &last_phase);

        let observed = observation.into_inner().unwrap();
        assert!(observed.saw_epoch_zero_closing);
        assert!(observed.saw_open_epoch_one_overlap);
        validate_epoch_transition(&statuses, &initial, &observed).unwrap();
    }

    #[test]
    fn incomplete_durable_close_does_not_infer_a_missed_transition() {
        let mut incomplete = status(4, 6, "A");
        incomplete.close_hashes.remove("0");
        let observation = Mutex::new(RaiTransitionObservation::default());
        let last_phase = Arc::new(Mutex::new(String::new()));

        record_rai_statuses(&[incomplete], &observation, &last_phase);

        let observed = observation.into_inner().unwrap();
        assert!(!observed.saw_epoch_zero_closing);
        assert!(!observed.saw_open_epoch_one_overlap);
    }

    #[test]
    fn finalized_tags_must_exactly_cover_workload_and_match_on_every_pr() {
        let converged = status(1, 0, "A");
        let initial = status(0, 0, "A");
        assert_eq!(
            converged_workload_epoch(
                &[converged.clone(), converged.clone()],
                &[initial.clone(), initial.clone()],
                1,
            ),
            Some(0)
        );

        let two_epochs = status(4, 6, "A");
        assert_eq!(
            converged_workload_epoch(
                &[two_epochs.clone(), two_epochs],
                &[initial.clone(), initial.clone()],
                10,
            ),
            Some(1)
        );

        // Extra finalization tags must not let priority or unrelated blocks
        // satisfy the workload oracle.
        assert_eq!(
            converged_workload_epoch(
                &[converged.clone(), converged.clone()],
                &[initial.clone(), initial.clone()],
                0,
            ),
            None
        );

        let mut missing_tag = converged.clone();
        missing_tag.finalized_by_epoch.insert("0".into(), 0.into());
        assert_eq!(
            converged_workload_epoch(
                &[converged.clone(), missing_tag],
                &[initial.clone(), initial.clone()],
                1,
            ),
            None
        );

        assert_eq!(converged_workload_epoch(&[converged], &[initial], 2,), None);
    }

    fn observed() -> RaiTransitionObservation {
        RaiTransitionObservation {
            saw_epoch_zero_closing: true,
            saw_open_epoch_one_overlap: true,
            saw_matching_cut: true,
            saw_obligations_finalized: true,
            validation_error: None,
            epoch_timings: Default::default(),
        }
    }
}
