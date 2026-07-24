use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::block::{Block, GenesisAccount, SignedBlock};
use crate::certificate::GlobalResult;
use crate::close::{CloseCutCandidate, ClosePackage, FinalityEvidence, SignedReport, SlotStatus};
use crate::committee::Committee;
use crate::crypto::{AccountKeyStore, DemoKeyStore};
use crate::engine::{CloseProtocolAction, EpochState, RaiEngine};
use crate::error::{RaiError, Result};
use crate::types::{AccountId, ElectionId, Hash32, ReplicaId, Slot, VoteValue};
use crate::vote::{SignedVote, VoteKind};

const COMMITTEE_ID: u64 = 7;
const NODE_COUNT: usize = 6;
const MAX_PROTOCOL_STEPS: usize = 96;
const GENESIS_REPLICA_WEIGHT: u128 = 1_000;
const DEFAULT_CONFLICTING_BLOCK_PERCENTAGE: f64 = 100.0 / NODE_COUNT as f64;
const DEFAULT_SLOW_REPLICA_DELAY_MS: u64 = 100;
const GENESIS_ACCOUNT_BALANCE: u128 = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum LogLevel {
    Summary,
    Protocol,
    Network,
    Trace,
}

impl LogLevel {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "summary" => Ok(Self::Summary),
            "protocol" => Ok(Self::Protocol),
            "network" => Ok(Self::Network),
            "trace" => Ok(Self::Trace),
            _ => Err(RaiError::InvalidConfiguration(format!(
                "invalid log level {value:?}; expected summary, protocol, network, or trace"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ByzantineBehavior {
    /// Deterministically rotate through omission, equivocation, invalid values,
    /// and ordinary first votes.
    #[default]
    Mixed,
    /// Process inbound traffic but send no messages. This models an offline
    /// Byzantine member, the worst case for liveness at the configured bound.
    Silent,
}

impl ByzantineBehavior {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "mixed" => Ok(Self::Mixed),
            "silent" => Ok(Self::Silent),
            _ => Err(RaiError::InvalidConfiguration(format!(
                "invalid Byzantine behavior {value:?}; expected mixed or silent"
            ))),
        }
    }
}

/// A two-way network partition active over `[start_ms, end_ms)`.
///
/// Replicas listed in `left` can communicate with one another, and replicas not
/// listed can communicate with one another, but cross-group deliveries are
/// discarded while the window is active. Multiple windows may be configured.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionWindow {
    pub start_ms: u64,
    pub end_ms: u64,
    pub left: BTreeSet<ReplicaId>,
}

impl PartitionWindow {
    pub fn new(
        start_ms: u64,
        end_ms: u64,
        left: impl IntoIterator<Item = ReplicaId>,
    ) -> Result<Self> {
        let window = Self {
            start_ms,
            end_ms,
            left: left.into_iter().collect(),
        };
        window.validate()?;
        Ok(window)
    }

    fn validate(&self) -> Result<()> {
        if self.start_ms >= self.end_ms {
            return Err(RaiError::InvalidConfiguration(
                "partition start must be earlier than partition end".into(),
            ));
        }
        if self.left.is_empty() || self.left.len() == NODE_COUNT {
            return Err(RaiError::InvalidConfiguration(
                "partition must place at least one replica on each side".into(),
            ));
        }
        if self
            .left
            .iter()
            .any(|replica| *replica == 0 || *replica > NODE_COUNT as u64)
        {
            return Err(RaiError::InvalidConfiguration(format!(
                "partition replica ids must be in 1..={NODE_COUNT}"
            )));
        }
        Ok(())
    }

    fn blocks(&self, at_ms: u64, from: ReplicaId, to: ReplicaId) -> bool {
        at_ms >= self.start_ms
            && at_ms < self.end_ms
            && self.left.contains(&from) != self.left.contains(&to)
    }
}

#[derive(Clone, Debug)]
pub struct TimedSimulationConfig {
    pub epoch_start_ms: i64,
    pub epoch_close_ms: i64,
    pub next_epoch_delay_ms: i64,
    pub block_interval_ms: i64,
    /// Optional aggregate client offer rate. When set, this takes precedence
    /// over `block_interval_ms`.
    pub blocks_per_second: Option<u64>,
    pub tick_ms: u64,
    pub stop_ms: u64,
    pub clock_offsets_ms: [i64; NODE_COUNT],
    pub log_level: LogLevel,
    pub log_file: Option<PathBuf>,
    /// Print protocol activity while the simulation is running. The final
    /// summary is always printed independently of this setting.
    pub verbose: bool,
    pub print_logs: bool,
    pub realtime: bool,
    pub seed: u64,
    /// Number of consecutive epochs to close in one linked simulation.
    pub epochs: u64,
    pub account_count: u64,
    /// Percentage of initial account/slot opportunities for which the client
    /// creates two distinct, owner-authorized blocks with the same parent.
    pub conflicting_block_percentage: f64,
    /// Exact number of replicas that exhibit arbitrary voting behavior.
    pub byzantine_replicas: usize,
    pub byzantine_behavior: ByzantineBehavior,
    /// Exact number of correct replicas whose outbound messages are delayed.
    pub slow_replicas: usize,
    /// Additional delay applied to every hop sent by a slow replica.
    pub slow_replica_delay_ms: u64,
    pub latency_min_ms: u64,
    pub latency_max_ms: u64,
    pub reorder_window_ms: u64,
    pub drop_rate: f64,
    pub duplicate_rate: f64,
    pub close_round_timeout_ms: i64,
    pub partitions: Vec<PartitionWindow>,
}

impl Default for TimedSimulationConfig {
    fn default() -> Self {
        Self {
            epoch_start_ms: 100,
            epoch_close_ms: 900,
            next_epoch_delay_ms: 100,
            block_interval_ms: 0,
            blocks_per_second: None,
            tick_ms: 10,
            stop_ms: 2_200,
            clock_offsets_ms: [0, 5, -5, 10, -10, 0],
            log_level: LogLevel::Protocol,
            log_file: None,
            verbose: false,
            print_logs: true,
            realtime: false,
            seed: 42,
            epochs: 1,
            account_count: 6,
            conflicting_block_percentage: DEFAULT_CONFLICTING_BLOCK_PERCENTAGE,
            byzantine_replicas: 0,
            byzantine_behavior: ByzantineBehavior::Mixed,
            slow_replicas: 0,
            slow_replica_delay_ms: DEFAULT_SLOW_REPLICA_DELAY_MS,
            latency_min_ms: 5,
            latency_max_ms: 35,
            reorder_window_ms: 20,
            drop_rate: 0.0,
            duplicate_rate: 0.0,
            close_round_timeout_ms: 120,
            partitions: Vec::new(),
        }
    }
}

impl TimedSimulationConfig {
    pub fn from_args(args: &[String]) -> Result<Self> {
        let mut config = Self::default();
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].clone();
            match arg.as_str() {
                "--epoch-start-ms" => {
                    config.epoch_start_ms = parse_i64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--epoch-close-ms" => {
                    config.epoch_close_ms = parse_i64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--next-epoch-delay-ms" => {
                    config.next_epoch_delay_ms =
                        parse_i64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--block-interval-ms" => {
                    config.block_interval_ms =
                        parse_i64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--blocks-per-second" => {
                    config.blocks_per_second =
                        Some(parse_u64(next_value(args, &mut index, &arg)?, &arg)?);
                }
                "--tick-ms" => {
                    config.tick_ms = parse_u64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--stop-ms" => {
                    config.stop_ms = parse_u64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--clock-offsets-ms" => {
                    config.clock_offsets_ms = parse_offsets(next_value(args, &mut index, &arg)?)?;
                }
                "--log-level" => {
                    config.log_level = LogLevel::parse(next_value(args, &mut index, &arg)?)?;
                }
                "--log-file" => {
                    config.log_file = Some(PathBuf::from(next_value(args, &mut index, &arg)?));
                }
                "--verbose" | "-v" => config.verbose = true,
                "--no-stdout" => config.print_logs = false,
                "--realtime" => config.realtime = true,
                "--seed" => {
                    config.seed = parse_u64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--epochs" => {
                    config.epochs = parse_u64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--accounts" => {
                    config.account_count = parse_u64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--conflicting-block-percentage" => {
                    config.conflicting_block_percentage =
                        parse_percentage(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--byzantine-replicas" | "--f" => {
                    config.byzantine_replicas =
                        parse_usize(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--byzantine-behavior" => {
                    config.byzantine_behavior =
                        ByzantineBehavior::parse(next_value(args, &mut index, &arg)?)?;
                }
                "--slow-replicas" | "--p" => {
                    config.slow_replicas = parse_usize(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--slow-delay-ms" => {
                    config.slow_replica_delay_ms =
                        parse_u64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--latency-min-ms" => {
                    config.latency_min_ms = parse_u64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--latency-max-ms" => {
                    config.latency_max_ms = parse_u64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--reorder-window-ms" => {
                    config.reorder_window_ms =
                        parse_u64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--drop-rate" => {
                    config.drop_rate =
                        parse_probability(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--duplicate-rate" => {
                    config.duplicate_rate =
                        parse_probability(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--close-round-timeout-ms" => {
                    config.close_round_timeout_ms =
                        parse_i64(next_value(args, &mut index, &arg)?, &arg)?;
                }
                "--partition" => {
                    config
                        .partitions
                        .push(parse_partition(next_value(args, &mut index, &arg)?)?);
                }
                "--help" | "-h" => {
                    return Err(RaiError::InvalidConfiguration(
                        timed_simulation_help().to_string(),
                    ));
                }
                other => {
                    return Err(RaiError::InvalidConfiguration(format!(
                        "unknown timed simulation option {other:?}\n\n{}",
                        timed_simulation_help()
                    )));
                }
            }
            index += 1;
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.epoch_start_ms < 0 || self.epoch_close_ms <= self.epoch_start_ms {
            return Err(RaiError::InvalidConfiguration(
                "--epoch-close-ms must be greater than a non-negative --epoch-start-ms".into(),
            ));
        }
        if self.next_epoch_delay_ms < 0
            || self.block_interval_ms < 0
            || self.blocks_per_second == Some(0)
            || self.close_round_timeout_ms <= 0
            || self.tick_ms == 0
            || self.epochs == 0
            || self.account_count == 0
            || !(0.0..=100.0).contains(&self.conflicting_block_percentage)
            || self.latency_max_ms < self.latency_min_ms
            || !(0.0..=1.0).contains(&self.drop_rate)
            || !(0.0..=1.0).contains(&self.duplicate_rate)
            || self.stop_ms < self.epoch_close_ms as u64
        {
            return Err(RaiError::InvalidConfiguration(
                "invalid timing, account, stop, or latency configuration".into(),
            ));
        }
        let classified_replicas = self
            .byzantine_replicas
            .checked_add(self.slow_replicas)
            .ok_or_else(|| {
                RaiError::InvalidConfiguration("replica-count arithmetic overflow".into())
            })?;
        if classified_replicas > NODE_COUNT {
            return Err(RaiError::InvalidConfiguration(format!(
                "Byzantine and slow replica sets must be disjoint and total at most {NODE_COUNT}"
            )));
        }
        if self.slow_replicas < self.byzantine_replicas {
            return Err(RaiError::InvalidConfiguration(format!(
                "the RAI committee bound requires p >= f (p={}, f={})",
                self.slow_replicas, self.byzantine_replicas
            )));
        }
        let minimum_replicas = self
            .byzantine_replicas
            .checked_mul(3)
            .and_then(|faults| {
                self.slow_replicas
                    .checked_mul(2)
                    .and_then(|slow| faults.checked_add(slow))
            })
            .and_then(|minimum| minimum.checked_add(1))
            .ok_or_else(|| {
                RaiError::InvalidConfiguration("replica fault-bound arithmetic overflow".into())
            })?;
        if NODE_COUNT < minimum_replicas {
            return Err(RaiError::InvalidConfiguration(format!(
                "six-node simulation violates n >= 3f + 2p + 1 for f={} and p={} (minimum n={minimum_replicas})",
                self.byzantine_replicas, self.slow_replicas
            )));
        }
        if self.slow_replicas > 0 && self.slow_replica_delay_ms == 0 {
            return Err(RaiError::InvalidConfiguration(
                "--slow-delay-ms must be greater than zero when --slow-replicas is nonzero".into(),
            ));
        }
        for partition in &self.partitions {
            partition.validate()?;
        }
        Ok(())
    }

    fn is_byzantine(&self, replica: ReplicaId) -> bool {
        replica > 0 && replica <= self.byzantine_replicas as ReplicaId
    }

    fn is_slow(&self, replica: ReplicaId) -> bool {
        let first = self.byzantine_replicas as ReplicaId + 1;
        let last = self.byzantine_replicas.saturating_add(self.slow_replicas) as ReplicaId;
        replica >= first && replica <= last
    }

    fn fault_weight(&self) -> Result<u128> {
        (self.byzantine_replicas as u128)
            .checked_mul(GENESIS_REPLICA_WEIGHT)
            .ok_or_else(|| RaiError::InvalidConfiguration("Byzantine weight overflow".into()))
    }

    fn participation_weight(&self) -> Result<u128> {
        (self.slow_replicas as u128)
            .checked_mul(GENESIS_REPLICA_WEIGHT)
            .ok_or_else(|| RaiError::InvalidConfiguration("slow-replica weight overflow".into()))
    }
}

pub fn timed_simulation_help() -> &'static str {
    r#"TIMED SIX-NODE ADVERSARIAL CONFORMANCE SIMULATION:
cargo run -- timed-six-nodes [options]

Each replica owns an independent RaiEngine and only its own Ed25519 private key.
An independent multi-account client signs account blocks; Ed25519-signed votes, reports,
close-cut candidates, and close packages travel through a seeded priority queue
with latency, reordering, random drops, duplication, gossip, and optional
time-bounded partitions. Every node starts from the same hardcoded
balance/delegation/owner-key genesis. Client submissions for multiple accounts
enter replicas at the configured block interval throughout the open epoch; zero
submits the whole workload immediately. A configurable percentage of account/slot
opportunities receive two different valid blocks. Byzantine and slow replica counts also
set the equal-weight committee's F and P bounds; replicas 1..=f are Byzantine and the
next p replicas are slow. When at least one client conflict is configured, the seeded
primary slot is included so the scenario can close, release, and retry it.

Client/replica fault options:
  --epochs N
  --accounts N
  --conflicting-block-percentage PERCENT
  --byzantine-replicas N, --f N
  --byzantine-behavior mixed|silent
  --slow-replicas N, --p N
  --slow-delay-ms N
    Byzantine voters use the mixed behavior by default; silent sends nothing.
    The conflict percentage is rounded to an exact slot quota and selected by seed.
    Slow replicas add this delay to every outbound network hop. Replica sets are disjoint.
    Configurations must satisfy p >= f and 6 >= 3f + 2p + 1.

Network options:
  --seed N
  --latency-min-ms N
  --latency-max-ms N
  --reorder-window-ms N
  --drop-rate P
  --duplicate-rate P
  --partition START:END:LEFT_IDS
    Example: --partition 250:900:1,2,3
    This separates replicas 1,2,3 from the remaining replicas during
    [250ms,900ms). Repeat --partition for multiple windows.

Timing/logging options:
  --epoch-start-ms N --epoch-close-ms N --next-epoch-delay-ms N
  --block-interval-ms N --blocks-per-second N --tick-ms N --stop-ms N
    block-interval-ms=0 is an unlimited-rate burst; positive values pace logical
    account-slot requests. blocks-per-second sets a precise aggregate offered
    load and takes precedence over block-interval-ms. Enough deterministic
    client accounts are pre-generated to sustain that rate for the open epoch.
  --clock-offsets-ms a,b,c,d,e,f --close-round-timeout-ms N
  --verbose, -v
    Print execution details. Without this flag, only the final summary is printed.
  --log-level summary|protocol|network|trace --log-file PATH
  --no-stdout --realtime
    --no-stdout suppresses verbose output; the final summary is always printed."#
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SimulationReport {
    pub epochs_requested: usize,
    pub correct_epochs_closed: usize,
    pub epoch_close_hashes: Vec<Hash32>,
    pub client_accounts: usize,
    /// Configured aggregate client offer rate, when rate pacing is enabled.
    pub target_blocks_per_second: Option<u64>,
    /// Logical account-slot requests generated by the client.
    pub client_requests: usize,
    /// Direct client-to-replica ingress copies. A conflicting request can have
    /// multiple ingress copies and therefore exceed `client_requests`.
    pub client_submissions: usize,
    pub client_ingress_replicas: usize,
    pub client_conflicting_slots: usize,
    pub proposals: usize,
    pub byzantine_replicas: usize,
    pub slow_replicas: usize,
    pub byzantine_votes_omitted: usize,
    pub byzantine_double_votes: usize,
    pub byzantine_wrong_votes: usize,
    pub fast_blocks: usize,
    pub notarized_blocks: usize,
    pub unresolved_blocks: usize,
    pub timeout_blocks: usize,
    pub close_excluded_blocks: usize,
    pub epochs_closed: usize,
    pub finalized_slots: usize,
    pub selected: usize,
    pub released: usize,
    pub events_logged: usize,
    pub last_close_hash: Option<Hash32>,
    pub network_scheduled: usize,
    pub network_byzantine_scheduled: usize,
    pub network_delivered: usize,
    pub network_dropped: usize,
    pub network_duplicated: usize,
    pub network_slow_delayed: usize,
    pub network_partition_dropped: usize,
    pub network_accepted: usize,
    pub network_deduplicated: usize,
    pub network_rejected: usize,
    pub network_queue_remaining: usize,
    pub close_cut_rounds: usize,
    pub close_record_rounds: usize,
    /// Individual slot finalization latency, measured from that slot's logical
    /// submission until every correct replica derives the same Fast or Final
    /// result. Timed-out/released slots are not samples.
    pub slot_finalization_latency_samples: usize,
    pub average_slot_finalization_latency_ms: u64,
    /// Epoch commit latency, measured from the first logical submission in an
    /// epoch until the certified close is installed on all correct replicas.
    pub epoch_commit_latency_samples: usize,
    pub average_epoch_commit_latency_ms: u64,
    /// Close-cut latency is measured from its first broadcast until the first
    /// close-record broadcast proves that the cut was finalized.
    pub finalized_close_cut_latency_samples: usize,
    pub average_finalized_close_cut_latency_ms: u64,
    /// Close-record latency is measured from its first broadcast until all
    /// correct replicas have installed the certified close.
    pub finalized_close_record_latency_samples: usize,
    pub average_finalized_close_record_latency_ms: u64,
    /// Finalized logical requests across all completed epochs.
    pub committed_requests: usize,
    /// Logical time from the first request until the last completed epoch.
    pub benchmark_elapsed_ms: u64,
    /// Committed slots per second, rounded down.
    pub throughput_slots_per_second: u64,
    /// Logical requests offered per second, scaled by 1,000.
    pub offered_requests_per_second_milli: u64,
    /// Committed slots per second, scaled by 1,000.
    pub throughput_slots_per_second_milli: u64,
    #[doc(hidden)]
    pub slot_submission_times: BTreeMap<ElectionId, u64>,
    #[doc(hidden)]
    pub consensus_observed: BTreeSet<ElectionId>,
    #[doc(hidden)]
    pub first_submission_ms: Option<u64>,
    #[doc(hidden)]
    pub last_completion_ms: Option<u64>,
    pub epoch_snapshots: Vec<EpochSnapshot>,
    pub nodes_closed: usize,
    pub nodes_finalized: usize,
    pub distinct_close_hashes: usize,
    pub correct_nodes_closed: usize,
    pub correct_nodes_finalized: usize,
    pub correct_distinct_close_hashes: usize,
    pub correct_distinct_finalized_hashes: usize,
    pub safety_faults: usize,
    pub converged: bool,
    pub correct_converged: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpochSnapshot {
    pub epoch: u64,
    pub accounts: BTreeMap<AccountId, crate::block::AccountState>,
    /// Committees that governed elections in this epoch.
    pub committees: Vec<Committee>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ClosePhaseTimings {
    first_close_cut_ms: Option<u64>,
    first_close_record_ms: Option<u64>,
    first_correct_close_ms: Option<u64>,
}

/// A simulation-side client whose account signing keys are kept separate from
/// every replica. One client can own and submit chains for multiple accounts.
#[derive(Clone, Debug)]
pub struct SimulationClient {
    keys: AccountKeyStore,
    accounts: BTreeSet<AccountId>,
}

impl SimulationClient {
    pub fn new(
        keys: AccountKeyStore,
        accounts: impl IntoIterator<Item = AccountId>,
    ) -> Result<Self> {
        let accounts = accounts.into_iter().collect::<BTreeSet<_>>();
        if accounts.is_empty() || accounts.iter().any(|account| !keys.contains(*account)) {
            return Err(RaiError::InvalidConfiguration(
                "simulation client accounts must be non-empty and have private keys".into(),
            ));
        }
        Ok(Self { keys, accounts })
    }

    pub fn deterministic(accounts: impl IntoIterator<Item = AccountId>) -> Self {
        let accounts = accounts.into_iter().collect::<BTreeSet<_>>();
        Self::new(
            AccountKeyStore::deterministic(accounts.iter().copied()),
            accounts,
        )
        .expect("deterministic simulation client has account keys")
    }

    pub fn owns(&self, account: AccountId) -> bool {
        self.accounts.contains(&account)
    }

    pub fn accounts(&self) -> &BTreeSet<AccountId> {
        &self.accounts
    }

    pub fn genesis_account(
        &self,
        account: AccountId,
        balance: u128,
        representative: ReplicaId,
    ) -> Result<GenesisAccount> {
        let owner = self.keys.public_key(account).ok_or_else(|| {
            RaiError::InvalidConfiguration(format!("client does not own account {account}"))
        })?;
        Ok(GenesisAccount::new(account, balance, representative, owner))
    }

    pub fn sign_block(&self, block: Block) -> Result<SignedBlock> {
        if !self.owns(block.account()) {
            return Err(RaiError::InvalidSignature);
        }
        SignedBlock::sign(&self.keys, block)
    }
}

fn generated_genesis(account_count: u64) -> Result<(SimulationClient, Vec<GenesisAccount>)> {
    // Keep at least one genesis delegation per replica even when a test asks
    // for a smaller client workload. Additional load accounts distribute
    // delegation round-robin across the fixed six-replica committee.
    let genesis_count = account_count.max(NODE_COUNT as u64);
    let accounts = 1..=genesis_count;
    let client = SimulationClient::deterministic(accounts.clone());
    let genesis = accounts
        .map(|account| {
            let representative = ((account - 1) % NODE_COUNT as u64) + 1;
            client.genesis_account(account, GENESIS_ACCOUNT_BALANCE, representative)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((client, genesis))
}

struct EventLogger {
    level: LogLevel,
    verbose: bool,
    print_logs: bool,
    file: Option<BufWriter<File>>,
    count: usize,
}

impl EventLogger {
    fn new(config: &TimedSimulationConfig) -> Result<Self> {
        let file = match &config.log_file {
            Some(path) => Some(BufWriter::new(File::create(path).map_err(|error| {
                RaiError::Io(format!(
                    "cannot create log file {}: {error}",
                    path.display()
                ))
            })?)),
            None => None,
        };
        Ok(Self {
            level: config.log_level,
            verbose: config.verbose,
            print_logs: config.print_logs,
            file,
            count: 0,
        })
    }

    fn emit(&mut self, required: LogLevel, event: &str, details: &str) -> Result<()> {
        if self.level < required {
            return Ok(());
        }
        let line = if details.is_empty() {
            format!("event={event}")
        } else {
            format!("event={event} {details}")
        };
        if self.verbose && self.print_logs {
            println!("{line}");
        }
        if let Some(file) = &mut self.file {
            writeln!(file, "{line}")
                .map_err(|error| RaiError::Io(format!("cannot write simulation log: {error}")))?;
        }
        self.count += 1;
        Ok(())
    }

    fn emit_final_summary(&mut self, details: &str) -> Result<()> {
        let line = format!("event=SIMULATION_COMPLETE {details}");
        println!("{line}");
        if let Some(file) = &mut self.file {
            writeln!(file, "{line}")
                .map_err(|error| RaiError::Io(format!("cannot write simulation log: {error}")))?;
        }
        self.count += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()
                .map_err(|error| RaiError::Io(format!("cannot flush simulation log: {error}")))?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum SimMessage {
    Block {
        election: ElectionId,
        signed: SignedBlock,
    },
    Vote(SignedVote),
    Report(SignedReport),
    CloseCut {
        election: ElectionId,
        candidate: CloseCutCandidate,
    },
    CloseRecord {
        election: ElectionId,
        package: ClosePackage,
    },
}

impl SimMessage {
    fn label(&self) -> &'static str {
        match self {
            Self::Block { .. } => "block",
            Self::Vote(_) => "vote",
            Self::Report(_) => "report",
            Self::CloseCut { .. } => "close-cut",
            Self::CloseRecord { .. } => "close-record",
        }
    }

    fn slot_election(&self) -> Option<&ElectionId> {
        match self {
            Self::Block { election, .. } if election.slot().is_some() => Some(election),
            Self::Vote(vote) if vote.election.slot().is_some() => Some(&vote.election),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct ScheduledEnvelope {
    deliver_at_ms: u64,
    order: u64,
    message_id: u64,
    from: ReplicaId,
    to: ReplicaId,
    payload: SimMessage,
}

impl PartialEq for ScheduledEnvelope {
    fn eq(&self, other: &Self) -> bool {
        self.deliver_at_ms == other.deliver_at_ms && self.order == other.order
    }
}

impl Eq for ScheduledEnvelope {}

impl PartialOrd for ScheduledEnvelope {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledEnvelope {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse time and insertion order so BinaryHeap acts as a min-heap.
        other
            .deliver_at_ms
            .cmp(&self.deliver_at_ms)
            .then_with(|| other.order.cmp(&self.order))
    }
}

#[derive(Clone, Debug)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9e37_79b9_7f4a_7c15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn range_inclusive(&mut self, minimum: u64, maximum: u64) -> u64 {
        if minimum >= maximum {
            return minimum;
        }
        minimum + self.next_u64() % (maximum - minimum + 1)
    }

    fn chance(&mut self, probability: f64) -> bool {
        if probability <= 0.0 {
            return false;
        }
        if probability >= 1.0 {
            return true;
        }
        let sample = self.next_u64() as f64 / u64::MAX as f64;
        sample < probability
    }
}

#[derive(Clone, Debug, Default)]
struct NetworkStats {
    scheduled: usize,
    byzantine_scheduled: usize,
    delivered: usize,
    dropped: usize,
    duplicated: usize,
    slow_delayed: usize,
    partition_dropped: usize,
    accepted: usize,
    deduplicated: usize,
    rejected: usize,
}

struct AdversarialNetwork {
    queue: BinaryHeap<ScheduledEnvelope>,
    catalog: BTreeMap<u64, (ReplicaId, SimMessage)>,
    rng: DeterministicRng,
    next_order: u64,
    next_message_id: u64,
    now_ms: u64,
    stats: NetworkStats,
}

impl AdversarialNetwork {
    fn new(seed: u64, now_ms: u64) -> Self {
        Self {
            queue: BinaryHeap::new(),
            catalog: BTreeMap::new(),
            rng: DeterministicRng::new(seed),
            next_order: 0,
            next_message_id: 1,
            now_ms,
            stats: NetworkStats::default(),
        }
    }

    fn register_message(&mut self, origin: ReplicaId, payload: SimMessage) -> u64 {
        let message_id = self.next_message_id;
        self.next_message_id = self.next_message_id.wrapping_add(1);
        self.catalog.insert(message_id, (origin, payload));
        message_id
    }

    fn broadcast(
        &mut self,
        message_id: u64,
        from: ReplicaId,
        previous_hop: Option<ReplicaId>,
        config: &TimedSimulationConfig,
        logger: &mut EventLogger,
    ) -> Result<()> {
        let payload = self
            .catalog
            .get(&message_id)
            .map(|(_, payload)| payload.clone())
            .ok_or_else(|| RaiError::InvalidConfiguration("unknown network message id".into()))?;
        for to in 1..=NODE_COUNT as u64 {
            if to == from || previous_hop == Some(to) {
                continue;
            }
            self.schedule_one(message_id, from, to, payload.clone(), config, logger)?;
        }
        Ok(())
    }

    fn schedule_one(
        &mut self,
        message_id: u64,
        from: ReplicaId,
        to: ReplicaId,
        payload: SimMessage,
        config: &TimedSimulationConfig,
        logger: &mut EventLogger,
    ) -> Result<()> {
        self.stats.scheduled += 1;
        if config.is_byzantine(from) {
            self.stats.byzantine_scheduled += 1;
        }
        if self.rng.chance(config.drop_rate) {
            self.stats.dropped += 1;
            logger.emit(
                LogLevel::Network,
                "NETWORK_DROP",
                &format!(
                    "time={} id={} from={} to={} kind={} reason=random",
                    self.now_ms,
                    message_id,
                    from,
                    to,
                    payload.label()
                ),
            )?;
            return Ok(());
        }

        let base = self
            .rng
            .range_inclusive(config.latency_min_ms, config.latency_max_ms);
        let reorder = self.rng.range_inclusive(0, config.reorder_window_ms);
        let slow_delay = if config.is_slow(from) {
            self.stats.slow_delayed += 1;
            config.slow_replica_delay_ms
        } else {
            0
        };
        let deliver_at_ms = round_up_to_tick(
            self.now_ms
                .saturating_add(base)
                .saturating_add(reorder)
                .saturating_add(slow_delay),
            config.tick_ms,
        );
        self.push_envelope(ScheduledEnvelope {
            deliver_at_ms,
            order: 0,
            message_id,
            from,
            to,
            payload: payload.clone(),
        });
        logger.emit(
            LogLevel::Trace,
            "NETWORK_SCHEDULE",
            &format!(
                "time={} deliver={} id={} from={} to={} kind={} slow_delay_ms={}",
                self.now_ms,
                deliver_at_ms,
                message_id,
                from,
                to,
                payload.label(),
                slow_delay
            ),
        )?;

        if self.rng.chance(config.duplicate_rate) {
            self.stats.duplicated += 1;
            self.stats.scheduled += 1;
            let duplicate_delay = self.rng.range_inclusive(1, config.reorder_window_ms.max(1));
            let duplicate_at = round_up_to_tick(
                deliver_at_ms.saturating_add(duplicate_delay),
                config.tick_ms,
            );
            self.push_envelope(ScheduledEnvelope {
                deliver_at_ms: duplicate_at,
                order: 0,
                message_id,
                from,
                to,
                payload,
            });
            logger.emit(
                LogLevel::Trace,
                "NETWORK_DUPLICATE",
                &format!(
                    "time={} deliver={} id={} from={} to={}",
                    self.now_ms, duplicate_at, message_id, from, to
                ),
            )?;
        }
        Ok(())
    }

    fn push_envelope(&mut self, mut envelope: ScheduledEnvelope) {
        envelope.order = self.next_order;
        self.next_order = self.next_order.wrapping_add(1);
        self.queue.push(envelope);
    }

    fn pop_due(&mut self, until_ms: u64) -> Option<ScheduledEnvelope> {
        let due = self
            .queue
            .peek()
            .map(|envelope| envelope.deliver_at_ms <= until_ms)
            .unwrap_or(false);
        due.then(|| self.queue.pop().expect("peeked network envelope"))
    }

    fn next_delivery_ms(&self) -> Option<u64> {
        self.queue.peek().map(|envelope| envelope.deliver_at_ms)
    }

    fn partition_blocks(
        &self,
        config: &TimedSimulationConfig,
        envelope: &ScheduledEnvelope,
    ) -> bool {
        config
            .partitions
            .iter()
            .any(|window| window.blocks(envelope.deliver_at_ms, envelope.from, envelope.to))
    }

    fn anti_entropy(
        &mut self,
        nodes: &BTreeMap<ReplicaId, SimNode>,
        config: &TimedSimulationConfig,
        logger: &mut EventLogger,
    ) -> Result<usize> {
        let known = self.catalog.keys().copied().collect::<Vec<_>>();
        let mut transmissions = 0;
        for message_id in known {
            let Some(sender) = nodes.iter().find_map(|(replica, node)| {
                (!node.is_byzantine() && node.seen.contains(&message_id)).then_some(*replica)
            }) else {
                continue;
            };
            let payload = self
                .catalog
                .get(&message_id)
                .map(|(_, payload)| payload.clone())
                .ok_or_else(|| {
                    RaiError::InvalidConfiguration("unknown anti-entropy message id".into())
                })?;
            for (receiver, node) in nodes {
                if *receiver == sender || node.seen.contains(&message_id) {
                    continue;
                }
                self.schedule_one(
                    message_id,
                    sender,
                    *receiver,
                    payload.clone(),
                    config,
                    logger,
                )?;
                transmissions += 1;
            }
        }
        if transmissions > 0 {
            logger.emit(
                LogLevel::Network,
                "ANTI_ENTROPY",
                &format!("time={} transmissions={transmissions}", self.now_ms),
            )?;
        }
        Ok(transmissions)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplicaRole {
    Correct,
    Byzantine,
}

#[derive(Clone, Debug, Default)]
struct ByzantineStats {
    votes_omitted: usize,
    double_votes: usize,
    wrong_votes: usize,
}

struct SimNode {
    id: ReplicaId,
    engine: RaiEngine,
    seen: BTreeSet<u64>,
    role: ReplicaRole,
    byzantine_behavior: ByzantineBehavior,
    byzantine_step: u64,
    byzantine_stats: ByzantineStats,
    byzantine_direct_actions: BTreeSet<ElectionId>,
}

impl SimNode {
    fn new(
        id: ReplicaId,
        engine: RaiEngine,
        role: ReplicaRole,
        byzantine_behavior: ByzantineBehavior,
    ) -> Self {
        Self {
            id,
            engine,
            seen: BTreeSet::new(),
            role,
            byzantine_behavior,
            byzantine_step: 0,
            byzantine_stats: ByzantineStats::default(),
            byzantine_direct_actions: BTreeSet::new(),
        }
    }

    fn is_byzantine(&self) -> bool {
        self.role == ReplicaRole::Byzantine
    }

    fn is_silent_byzantine(&self) -> bool {
        self.is_byzantine() && self.byzantine_behavior == ByzantineBehavior::Silent
    }

    /// Produces a deterministic mix of validly signed but arbitrary votes.
    /// Receiver-side validation remains unchanged: invalid/wrong messages are
    /// rejected or quarantined, and equivocations never count one signer twice.
    fn arbitrary_votes(
        &mut self,
        election: &ElectionId,
        suggested: VoteValue,
    ) -> Result<Vec<SimMessage>> {
        debug_assert!(self.is_byzantine());
        if self.byzantine_behavior == ByzantineBehavior::Silent {
            self.byzantine_stats.votes_omitted += 1;
            return Ok(Vec::new());
        }
        let step = self.byzantine_step;
        self.byzantine_step = self.byzantine_step.wrapping_add(1);
        let sign = |value| {
            SignedVote::new(
                &self.engine.crypto,
                self.id,
                election.clone(),
                COMMITTEE_ID,
                value,
                VoteKind::First,
            )
            .map(SimMessage::Vote)
        };

        match step % 4 {
            0 => {
                self.byzantine_stats.votes_omitted += 1;
                Ok(Vec::new())
            }
            1 => {
                self.byzantine_stats.double_votes += 1;
                let alternative = self.alternative_vote_value(election, suggested, step);
                Ok(vec![sign(suggested)?, sign(alternative)?])
            }
            2 => {
                self.byzantine_stats.wrong_votes += 1;
                let wrong = self.wrong_vote_value(election, step);
                Ok(vec![sign(wrong)?])
            }
            _ => Ok(vec![sign(suggested)?]),
        }
    }

    fn alternative_vote_value(
        &self,
        election: &ElectionId,
        suggested: VoteValue,
        step: u64,
    ) -> VoteValue {
        if let Some(slot) = election.slot() {
            if let Some(hash) = self
                .engine
                .blocks()
                .candidates_at_slot(slot)
                .into_iter()
                .find(|hash| VoteValue::Candidate(*hash) != suggested)
            {
                return VoteValue::Candidate(hash);
            }
        }
        if suggested != VoteValue::Timeout {
            VoteValue::Timeout
        } else {
            self.wrong_vote_value(election, step)
        }
    }

    fn wrong_vote_value(&self, election: &ElectionId, step: u64) -> VoteValue {
        VoteValue::Candidate(Hash32::digest(
            format!("byzantine-wrong:{}:{step}:{election}", self.id).as_bytes(),
        ))
    }
}

#[derive(Default)]
struct ApplyOutcome {
    outbound: Vec<SimMessage>,
}

pub fn run_timed_six_node_simulation(
    mut config: TimedSimulationConfig,
) -> Result<SimulationReport> {
    config.validate()?;
    if let Some(rate) = config.blocks_per_second {
        let open_ms = (config.epoch_close_ms - config.epoch_start_ms) as u64;
        let required_accounts = rate
            .checked_mul(open_ms)
            .and_then(|product| product.checked_add(999))
            .map(|rounded| rounded / 1_000)
            .ok_or_else(|| {
                RaiError::InvalidConfiguration(
                    "--blocks-per-second workload size overflows u64".into(),
                )
            })?;
        config.account_count = config.account_count.max(required_accounts);
    }
    if config.epochs == 1 && config.blocks_per_second.is_none() {
        run_single_epoch_simulation(config)
    } else {
        run_linked_multi_epoch_simulation(config)
    }
}

fn run_single_epoch_simulation(config: TimedSimulationConfig) -> Result<SimulationReport> {
    config.validate()?;
    let fault_weight = config.fault_weight()?;
    let participation_weight = config.participation_weight()?;
    let replicas = (1..=NODE_COUNT as u64).collect::<Vec<_>>();
    let replica_keys = DemoKeyStore::deterministic(replicas.clone());
    let (client, genesis_accounts) = generated_genesis(config.account_count)?;
    let start_ms = config.epoch_start_ms as u64;
    let close_ms = config.epoch_close_ms as u64;
    let mut logger = EventLogger::new(&config)?;
    let mut network = AdversarialNetwork::new(config.seed, start_ms);
    let mut nodes = BTreeMap::new();

    for replica in &replicas {
        let node_keys = replica_keys.signer_view(*replica).ok_or_else(|| {
            RaiError::InvalidConfiguration(format!("missing private key for replica {replica}"))
        })?;
        let mut engine = RaiEngine::with_genesis(
            node_keys,
            COMMITTEE_ID,
            genesis_accounts.clone(),
            fault_weight,
            participation_weight,
        )?;
        engine.set_election_timeout_ms(config.close_round_timeout_ms as u64);
        let role = if config.is_byzantine(*replica) {
            ReplicaRole::Byzantine
        } else {
            ReplicaRole::Correct
        };
        nodes.insert(
            *replica,
            SimNode::new(*replica, engine, role, config.byzantine_behavior),
        );
    }
    set_global_time(&mut nodes, start_ms, &config)?;

    let account = config.seed % config.account_count + 1;
    let slot = Slot::new(account, 1);
    let election = ElectionId::Slot { slot, epoch: 0 };
    let selected_genesis = genesis_accounts
        .iter()
        .find(|genesis| genesis.account == slot.account)
        .expect("selected configured account");
    let conflicting_accounts = select_conflicting_accounts(&config, account);
    let selected_conflicted = conflicting_accounts.contains(&account);
    let mut report = SimulationReport {
        epochs_requested: 1,
        client_accounts: config.account_count as usize,
        target_blocks_per_second: config.blocks_per_second,
        client_conflicting_slots: conflicting_accounts.len(),
        byzantine_replicas: config.byzantine_replicas,
        slow_replicas: config.slow_replicas,
        ..SimulationReport::default()
    };
    let mut ingress_replicas = BTreeSet::new();
    let mut selected_primary_hash = None;

    // Produce one block for every configured account/slot and a second valid,
    // same-parent block for the configured percentage. Conflict slots are
    // delivered 3/3 before any network delivery; ordinary slots use one
    // round-robin ingress replica.
    for (index, genesis) in genesis_accounts
        .iter()
        .take(config.account_count as usize)
        .enumerate()
    {
        let account_slot = Slot::new(genesis.account, 1);
        let account_election = ElectionId::Slot {
            slot: account_slot,
            epoch: 0,
        };
        register_client_request(&mut report, &account_election, start_ms);
        for node in nodes.values_mut() {
            node.engine
                .register_derived_election(account_election.clone())?;
        }
        let primary_signed = client.sign_block(Block {
            slot: account_slot,
            parent: genesis.hash(),
            balance: genesis.balance,
            representative: genesis.representative,
            sends: vec![crate::block::Send {
                destination: genesis.account,
                amount: 1,
            }],
            receives: Vec::new(),
        })?;
        let primary_hash = primary_signed.hash();
        if genesis.account == account {
            selected_primary_hash = Some(primary_hash);
        }
        report.proposals += 1;
        if conflicting_accounts.contains(&genesis.account) {
            let conflicting_signed = client.sign_block(Block {
                slot: account_slot,
                parent: genesis.hash(),
                balance: genesis.balance,
                representative: genesis.representative,
                sends: vec![crate::block::Send {
                    destination: genesis.account,
                    amount: 2,
                }],
                receives: Vec::new(),
            })?;
            let conflicting_hash = conflicting_signed.hash();
            report.proposals += 1;

            for replica in 1..=3 {
                apply_local_client_block(
                    replica,
                    &account_election,
                    primary_signed.clone(),
                    &mut nodes,
                    &mut network,
                    &config,
                    &mut logger,
                )?;
                ingress_replicas.insert(replica);
                report.client_submissions += 1;
                log_client_submit(
                    &mut logger,
                    start_ms,
                    genesis.account,
                    replica,
                    primary_hash,
                    true,
                )?;
            }
            for replica in 4..=NODE_COUNT as u64 {
                apply_local_client_block(
                    replica,
                    &account_election,
                    conflicting_signed.clone(),
                    &mut nodes,
                    &mut network,
                    &config,
                    &mut logger,
                )?;
                ingress_replicas.insert(replica);
                report.client_submissions += 1;
                log_client_submit(
                    &mut logger,
                    start_ms,
                    genesis.account,
                    replica,
                    conflicting_hash,
                    true,
                )?;
            }
            let primary_relay = (1..=3)
                .find(|replica| !config.is_byzantine(*replica))
                .expect("a 3-replica ingress group contains a correct relay");
            let conflicting_relay = (4..=NODE_COUNT as u64)
                .find(|replica| !config.is_byzantine(*replica))
                .expect("a 3-replica ingress group contains a correct relay");
            publish_already_applied(
                primary_relay,
                SimMessage::Block {
                    election: account_election.clone(),
                    signed: primary_signed,
                },
                &mut nodes,
                &mut network,
                &config,
                &mut logger,
            )?;
            publish_already_applied(
                conflicting_relay,
                SimMessage::Block {
                    election: account_election,
                    signed: conflicting_signed,
                },
                &mut nodes,
                &mut network,
                &config,
                &mut logger,
            )?;
            logger.emit(
                LogLevel::Protocol,
                "SPLIT_FIRST_VOTES",
                &format!(
                    "time={start_ms} account={} left={} right={} left_ingress=3 right_ingress=3",
                    genesis.account,
                    primary_hash.short(),
                    conflicting_hash.short()
                ),
            )?;
        } else {
            let target = replicas[index % replicas.len()];
            apply_local_client_block(
                target,
                &account_election,
                primary_signed.clone(),
                &mut nodes,
                &mut network,
                &config,
                &mut logger,
            )?;
            publish_already_applied(
                target,
                SimMessage::Block {
                    election: account_election,
                    signed: primary_signed,
                },
                &mut nodes,
                &mut network,
                &config,
                &mut logger,
            )?;
            ingress_replicas.insert(target);
            report.client_submissions += 1;
            log_client_submit(
                &mut logger,
                start_ms,
                genesis.account,
                target,
                primary_hash,
                false,
            )?;
        }
    }
    let selected_primary_hash = selected_primary_hash.expect("selected account was generated");
    report.client_ingress_replicas = ingress_replicas.len();

    drain_network_until(
        close_ms,
        &mut nodes,
        &mut network,
        &config,
        &mut logger,
        &mut report,
    )?;
    set_global_time(&mut nodes, close_ms, &config)?;
    network.now_ms = close_ms;

    if !selected_conflicted {
        for node in nodes.values_mut() {
            complete_if_strong(&mut node.engine, &election, selected_primary_hash)?;
        }
    }

    // Replicas independently attempt the timeout action. Correct nodes use the
    // guarded engine API; Byzantine nodes may omit, equivocate, or vote wrongly.
    for replica in &replicas {
        let votes = emit_followup_votes(nodes.get_mut(replica).expect("known replica"), &election)?;
        for vote in votes {
            publish_already_applied(
                *replica,
                vote,
                &mut nodes,
                &mut network,
                &config,
                &mut logger,
            )?;
        }
    }

    // Start closing independently on every replica. Correct reports can differ
    // if a partition or loss pattern gave replicas different visibility, and
    // those signed reports are reconciled only through network delivery. A
    // Byzantine replica withholds its report so liveness has to rely on the
    // specified n-f correct-report quorum.
    for replica in &replicas {
        let node = nodes.get_mut(replica).expect("known replica");
        let start_result = if node.is_byzantine() {
            node.engine.start_closing(0).map(|()| None)
        } else {
            node.engine.start_closing_with_report(0, *replica).map(Some)
        };
        match start_result {
            Ok(Some(signed_report)) => {
                publish_already_applied(
                    *replica,
                    SimMessage::Report(signed_report),
                    &mut nodes,
                    &mut network,
                    &config,
                    &mut logger,
                )?;
            }
            Ok(None) => {}
            Err(error) => log_nonfatal_node_error(
                *replica,
                "start-closing",
                &error,
                &mut logger,
                &mut report,
            )?,
        }
    }

    let close_timings = drive_close_protocol_networked(
        0,
        &mut nodes,
        &mut network,
        &config,
        &mut logger,
        &mut report,
    )?;

    let retry_start = network
        .now_ms
        .saturating_add(config.next_epoch_delay_ms as u64)
        .max(close_ms.saturating_add(config.next_epoch_delay_ms as u64));
    let mut retry_outcome = None;
    if selected_conflicted && retry_start <= config.stop_ms && all_correct_nodes_closed(&nodes, 0) {
        network.now_ms = retry_start;
        set_global_time(&mut nodes, retry_start, &config)?;
        let retry = ElectionId::Slot { slot, epoch: 1 };
        for node in nodes.values_mut() {
            if node.engine.epoch_state(1) == Some(EpochState::Open) {
                if let Err(error) = node.engine.register_derived_election(retry.clone()) {
                    log_nonfatal_node_error(
                        node.id,
                        "register-retry",
                        &error,
                        &mut logger,
                        &mut report,
                    )?;
                }
            }
        }

        let retry_signed = client.sign_block(Block {
            slot,
            parent: selected_genesis.hash(),
            balance: selected_genesis.balance,
            representative: selected_genesis.representative,
            sends: vec![crate::block::Send {
                destination: slot.account,
                amount: 3,
            }],
            receives: Vec::new(),
        })?;
        let retry_hash = retry_signed.hash();
        retry_outcome = Some((retry.clone(), retry_hash));
        report.proposals += 1;
        register_client_request(&mut report, &retry, retry_start);
        report.client_submissions += 1;
        let retry_proposer = first_correct_replica(&nodes);
        logger.emit(
            LogLevel::Protocol,
            "CLIENT_SUBMIT",
            &format!(
                "time={retry_start} account={} target={} block={} retry=true",
                slot.account,
                retry_proposer,
                retry_hash.short()
            ),
        )?;
        let local_outcome = {
            let proposer = nodes
                .get_mut(&retry_proposer)
                .expect("known correct retry proposer");
            apply_message(
                proposer,
                &SimMessage::Block {
                    election: retry.clone(),
                    signed: retry_signed.clone(),
                },
            )
        };
        match local_outcome {
            Ok(outcome) => {
                for outbound in outcome.outbound {
                    publish_already_applied(
                        retry_proposer,
                        outbound,
                        &mut nodes,
                        &mut network,
                        &config,
                        &mut logger,
                    )?;
                }
            }
            Err(error) => log_nonfatal_node_error(
                retry_proposer,
                "local-retry",
                &error,
                &mut logger,
                &mut report,
            )?,
        }
        publish_already_applied(
            retry_proposer,
            SimMessage::Block {
                election: retry.clone(),
                signed: retry_signed,
            },
            &mut nodes,
            &mut network,
            &config,
            &mut logger,
        )?;

        drive_retry_network(
            &retry,
            retry_hash,
            &mut nodes,
            &mut network,
            &config,
            &mut logger,
            &mut report,
        )?;
    }

    drain_network_until(
        config.stop_ms,
        &mut nodes,
        &mut network,
        &config,
        &mut logger,
        &mut report,
    )?;
    summarize(
        &election,
        retry_outcome.as_ref(),
        config.epoch_start_ms as u64,
        retry_outcome.as_ref().map(|_| retry_start),
        close_timings,
        &nodes,
        &mut network,
        &config,
        &mut logger,
        &mut report,
    )?;
    logger.flush()?;
    Ok(report)
}

fn run_linked_multi_epoch_simulation(config: TimedSimulationConfig) -> Result<SimulationReport> {
    let fault_weight = config.fault_weight()?;
    let participation_weight = config.participation_weight()?;
    let replicas = (1..=NODE_COUNT as u64).collect::<Vec<_>>();
    let replica_keys = DemoKeyStore::deterministic(replicas.clone());
    let (client, genesis_accounts) = generated_genesis(config.account_count)?;
    let mut logger = EventLogger::new(&config)?;
    let mut network = AdversarialNetwork::new(config.seed, config.epoch_start_ms as u64);
    let mut nodes = BTreeMap::new();

    for replica in &replicas {
        let node_keys = replica_keys.signer_view(*replica).ok_or_else(|| {
            RaiError::InvalidConfiguration(format!("missing private key for replica {replica}"))
        })?;
        let mut engine = RaiEngine::with_genesis(
            node_keys,
            COMMITTEE_ID,
            genesis_accounts.clone(),
            fault_weight,
            participation_weight,
        )?;
        engine.set_election_timeout_ms(config.close_round_timeout_ms as u64);
        let role = if config.is_byzantine(*replica) {
            ReplicaRole::Byzantine
        } else {
            ReplicaRole::Correct
        };
        nodes.insert(
            *replica,
            SimNode::new(*replica, engine, role, config.byzantine_behavior),
        );
    }

    let mut report = SimulationReport {
        epochs_requested: config.epochs as usize,
        client_accounts: config.account_count as usize,
        target_blocks_per_second: config.blocks_per_second,
        byzantine_replicas: config.byzantine_replicas,
        slow_replicas: config.slow_replicas,
        ..SimulationReport::default()
    };
    let mut ingress_replicas = BTreeSet::new();
    let close_offset = (config.epoch_close_ms - config.epoch_start_ms) as u64;
    let epoch_budget = config
        .stop_ms
        .checked_sub(config.epoch_start_ms as u64)
        .ok_or_else(|| {
            RaiError::InvalidConfiguration("--stop-ms leaves no per-epoch time budget".into())
        })?;
    let mut epoch_start = config.epoch_start_ms as u64;

    for epoch in 0..config.epochs {
        let epoch_deadline = epoch_start.checked_add(epoch_budget).ok_or_else(|| {
            RaiError::InvalidConfiguration("multi-epoch deadline overflow".into())
        })?;
        let close_at = epoch_start.checked_add(close_offset).ok_or_else(|| {
            RaiError::InvalidConfiguration("multi-epoch close time overflow".into())
        })?;
        let mut epoch_config = config.clone();
        epoch_config.stop_ms = epoch_deadline;

        drain_network_until(
            epoch_start,
            &mut nodes,
            &mut network,
            &epoch_config,
            &mut logger,
            &mut report,
        )?;
        network.now_ms = network.now_ms.max(epoch_start);
        set_global_time(&mut nodes, network.now_ms, &epoch_config)?;

        let max_waves = if config.blocks_per_second.is_none()
            && config.block_interval_ms == 0
            && config.conflicting_block_percentage == 0.0
        {
            close_at
                .saturating_sub(epoch_start)
                .checked_div(config.tick_ms)
                .unwrap_or(0)
                .max(1)
        } else {
            1
        };
        let mut elections = Vec::new();
        for _ in 0..max_waves {
            let wave_start = network.now_ms.max(epoch_start);
            if wave_start >= close_at {
                break;
            }
            let wave = submit_linked_epoch_workload(
                epoch,
                wave_start,
                close_at,
                &client,
                &mut nodes,
                &mut network,
                &epoch_config,
                &mut logger,
                &mut report,
                &mut ingress_replicas,
            )?;
            if wave.is_empty() {
                break;
            }
            elections.extend(wave.iter().cloned());

            // A positive interval already paces the complete configured
            // workload. Conflicting parents may be released at close and
            // therefore cannot safely feed a prebuilt successor chain.
            if config.block_interval_ms > 0 || config.conflicting_block_percentage > 0.0 {
                break;
            }
            if !drain_until_wave_finalized(
                close_at,
                &wave,
                &mut nodes,
                &mut network,
                &epoch_config,
                &mut logger,
                &mut report,
            )? {
                break;
            }
        }
        if elections.is_empty() {
            break;
        }

        drain_network_until(
            close_at,
            &mut nodes,
            &mut network,
            &epoch_config,
            &mut logger,
            &mut report,
        )?;
        network.now_ms = close_at;
        set_global_time(&mut nodes, close_at, &epoch_config)?;

        for (election, _) in &elections {
            for replica in &replicas {
                let votes =
                    emit_followup_votes(nodes.get_mut(replica).expect("known replica"), election)?;
                for vote in votes {
                    publish_already_applied(
                        *replica,
                        vote,
                        &mut nodes,
                        &mut network,
                        &epoch_config,
                        &mut logger,
                    )?;
                }
            }
        }

        for replica in &replicas {
            let node = nodes.get_mut(replica).expect("known replica");
            if node.engine.epoch_state(epoch) != Some(EpochState::Open) {
                continue;
            }
            let start_result = if node.is_byzantine() {
                node.engine.start_closing(epoch).map(|()| None)
            } else {
                node.engine
                    .start_closing_with_report(epoch, *replica)
                    .map(Some)
            };
            match start_result {
                Ok(Some(signed_report)) => {
                    publish_already_applied(
                        *replica,
                        SimMessage::Report(signed_report),
                        &mut nodes,
                        &mut network,
                        &epoch_config,
                        &mut logger,
                    )?;
                }
                Ok(None) => {}
                Err(error) => log_nonfatal_node_error(
                    *replica,
                    "start-closing",
                    &error,
                    &mut logger,
                    &mut report,
                )?,
            }
        }

        let close_timings = drive_close_protocol_networked(
            epoch,
            &mut nodes,
            &mut network,
            &epoch_config,
            &mut logger,
            &mut report,
        )?;

        let closed = record_linked_epoch_outcome(
            epoch,
            epoch_start,
            close_timings,
            &elections,
            &nodes,
            &epoch_config,
            &mut logger,
            &mut report,
        )?;
        if !closed {
            break;
        }
        epoch_start = network
            .now_ms
            .checked_add(config.next_epoch_delay_ms as u64)
            .ok_or_else(|| {
                RaiError::InvalidConfiguration("multi-epoch start time overflow".into())
            })?;
    }

    report.client_ingress_replicas = ingress_replicas.len();
    report.byzantine_votes_omitted = nodes
        .values()
        .map(|node| node.byzantine_stats.votes_omitted)
        .sum();
    report.byzantine_double_votes = nodes
        .values()
        .map(|node| node.byzantine_stats.double_votes)
        .sum();
    report.byzantine_wrong_votes = nodes
        .values()
        .map(|node| node.byzantine_stats.wrong_votes)
        .sum();
    report.network_scheduled = network.stats.scheduled;
    report.network_byzantine_scheduled = network.stats.byzantine_scheduled;
    report.network_delivered = network.stats.delivered;
    report.network_dropped = network.stats.dropped;
    report.network_duplicated = network.stats.duplicated;
    report.network_slow_delayed = network.stats.slow_delayed;
    report.network_partition_dropped = network.stats.partition_dropped;
    report.network_accepted = network.stats.accepted;
    report.network_deduplicated = network.stats.deduplicated;
    report.network_rejected = network.stats.rejected;
    report.network_queue_remaining = network.queue.len();
    report.correct_converged =
        report.correct_epochs_closed == report.epochs_requested && report.safety_faults == 0;
    report.converged = report.epochs_closed == report.epochs_requested
        && report.nodes_closed == NODE_COUNT
        && report.safety_faults == 0;

    emit_simulation_summary(&mut logger, &report)?;
    report.events_logged = logger.count;
    logger.flush()?;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn submit_linked_epoch_workload(
    epoch: u64,
    epoch_start_ms: u64,
    close_at_ms: u64,
    client: &SimulationClient,
    nodes: &mut BTreeMap<ReplicaId, SimNode>,
    network: &mut AdversarialNetwork,
    config: &TimedSimulationConfig,
    logger: &mut EventLogger,
    report: &mut SimulationReport,
    ingress_replicas: &mut BTreeSet<ReplicaId>,
) -> Result<Vec<(ElectionId, Hash32)>> {
    let correct_open = nodes
        .values()
        .filter(|node| {
            !node.is_byzantine() && node.engine.epoch_state(epoch) == Some(EpochState::Open)
        })
        .map(|node| node.id)
        .collect::<Vec<_>>();
    if correct_open.is_empty() {
        return Ok(Vec::new());
    }
    let (account_states, parent_sequences) = {
        let reference = nodes
            .get(&correct_open[0])
            .expect("known correct reference");
        let account_states = reference.engine.blocks().account_states()?;
        let parent_sequences = account_states
            .iter()
            .map(|(account, state)| {
                reference
                    .engine
                    .blocks()
                    .candidate(state.frontier)
                    .map(|candidate| (*account, candidate.block.slot.sequence))
                    .ok_or_else(|| RaiError::UnknownCandidate(state.frontier.to_string()))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        (account_states, parent_sequences)
    };
    let primary_account = config.seed.wrapping_add(epoch) % config.account_count + 1;
    let mut selection_config = config.clone();
    selection_config.seed = config.seed ^ epoch.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let conflicting_accounts = select_conflicting_accounts(&selection_config, primary_account);
    report.client_conflicting_slots += conflicting_accounts.len();
    let mut elections = Vec::new();

    for (index, account) in (1..=config.account_count).enumerate() {
        let offset_ms = match config.blocks_per_second {
            Some(rate) => (index as u64).saturating_mul(1_000) / rate,
            None => (index as u64).saturating_mul(config.block_interval_ms as u64),
        };
        let submission_at_ms = epoch_start_ms.saturating_add(offset_ms);
        if submission_at_ms >= close_at_ms {
            break;
        }
        drain_network_until(submission_at_ms, nodes, network, config, logger, report)?;
        let state = account_states.get(&account).ok_or_else(|| {
            RaiError::InvalidConfiguration(format!("missing account state for {account}"))
        })?;
        let parent_sequence = parent_sequences
            .get(&account)
            .copied()
            .ok_or_else(|| RaiError::UnknownCandidate(state.frontier.to_string()))?;
        let slot = Slot::new(account, parent_sequence.saturating_add(1));
        let election = ElectionId::Slot { slot, epoch };
        register_client_request(report, &election, submission_at_ms);
        for node in nodes.values_mut() {
            if node.engine.epoch_state(epoch) == Some(EpochState::Open) {
                if let Err(error) = node.engine.register_derived_election(election.clone()) {
                    log_nonfatal_node_error(
                        node.id,
                        "register-epoch-election",
                        &error,
                        logger,
                        report,
                    )?;
                }
            }
        }
        let primary_signed = client.sign_block(Block {
            slot,
            parent: state.frontier,
            balance: state.balance,
            representative: state.representative,
            sends: vec![crate::block::Send {
                destination: account,
                amount: 1,
            }],
            receives: Vec::new(),
        })?;
        let primary_hash = primary_signed.hash();
        report.proposals += 1;
        elections.push((election.clone(), primary_hash));

        if conflicting_accounts.contains(&account) && correct_open.len() >= 2 {
            let conflicting_signed = client.sign_block(Block {
                slot,
                parent: state.frontier,
                balance: state.balance,
                representative: state.representative,
                sends: vec![crate::block::Send {
                    destination: account,
                    amount: 2,
                }],
                receives: Vec::new(),
            })?;
            let conflicting_hash = conflicting_signed.hash();
            report.proposals += 1;
            let split = (correct_open.len() / 2).max(1);
            let (left, right) = correct_open.split_at(split);
            for replica in left {
                apply_local_client_block(
                    *replica,
                    &election,
                    primary_signed.clone(),
                    nodes,
                    network,
                    config,
                    logger,
                )?;
                ingress_replicas.insert(*replica);
                report.client_submissions += 1;
                log_client_submit(
                    logger,
                    submission_at_ms,
                    account,
                    *replica,
                    primary_hash,
                    true,
                )?;
            }
            for replica in right {
                apply_local_client_block(
                    *replica,
                    &election,
                    conflicting_signed.clone(),
                    nodes,
                    network,
                    config,
                    logger,
                )?;
                ingress_replicas.insert(*replica);
                report.client_submissions += 1;
                log_client_submit(
                    logger,
                    submission_at_ms,
                    account,
                    *replica,
                    conflicting_hash,
                    true,
                )?;
            }
            publish_already_applied(
                left[0],
                SimMessage::Block {
                    election: election.clone(),
                    signed: primary_signed,
                },
                nodes,
                network,
                config,
                logger,
            )?;
            publish_already_applied(
                right[0],
                SimMessage::Block {
                    election: election.clone(),
                    signed: conflicting_signed,
                },
                nodes,
                network,
                config,
                logger,
            )?;
            logger.emit(
                LogLevel::Protocol,
                "SPLIT_FIRST_VOTES",
                &format!(
                    "time={submission_at_ms} epoch={epoch} account={account} left={} right={} left_ingress={} right_ingress={}",
                    primary_hash.short(),
                    conflicting_hash.short(),
                    left.len(),
                    right.len()
                ),
            )?;
        } else {
            let target = correct_open[index % correct_open.len()];
            apply_local_client_block(
                target,
                &election,
                primary_signed.clone(),
                nodes,
                network,
                config,
                logger,
            )?;
            publish_already_applied(
                target,
                SimMessage::Block {
                    election,
                    signed: primary_signed,
                },
                nodes,
                network,
                config,
                logger,
            )?;
            ingress_replicas.insert(target);
            report.client_submissions += 1;
            log_client_submit(
                logger,
                submission_at_ms,
                account,
                target,
                primary_hash,
                false,
            )?;
        }
    }
    Ok(elections)
}

fn record_linked_epoch_outcome(
    epoch: u64,
    epoch_start_ms: u64,
    close_timings: ClosePhaseTimings,
    elections: &[(ElectionId, Hash32)],
    nodes: &BTreeMap<ReplicaId, SimNode>,
    _config: &TimedSimulationConfig,
    logger: &mut EventLogger,
    report: &mut SimulationReport,
) -> Result<bool> {
    assert_epoch_safety(epoch, elections.iter().map(|(election, _)| election), nodes)?;

    let correct_node_count = nodes.values().filter(|node| !node.is_byzantine()).count();
    let mut close_hashes = BTreeSet::new();
    let mut correct_close_hashes = BTreeSet::new();
    let mut ledger_roots = BTreeSet::new();
    let mut correct_ledger_roots = BTreeSet::new();
    let mut nodes_closed = 0;
    let mut correct_nodes_closed = 0;
    let mut nodes_finalized = 0;
    let mut correct_nodes_finalized = 0;

    for node in nodes.values() {
        if node.engine.epoch_state(epoch) != Some(EpochState::Closed) {
            continue;
        }
        nodes_closed += 1;
        let is_correct = !node.is_byzantine();
        if is_correct {
            correct_nodes_closed += 1;
        }
        if let Some(hash) = node.engine.close_hash(epoch) {
            close_hashes.insert(hash);
            if is_correct {
                correct_close_hashes.insert(hash);
            }
        }
        if let Some(root) = node.engine.ledger_root(epoch) {
            nodes_finalized += 1;
            ledger_roots.insert(root);
            if is_correct {
                correct_nodes_finalized += 1;
                correct_ledger_roots.insert(root);
            }
        }
    }

    if correct_close_hashes.len() > 1 || correct_ledger_roots.len() > 1 {
        report.safety_faults += 1;
    }
    let correct_closed =
        correct_nodes_closed == correct_node_count && correct_close_hashes.len() == 1;
    if correct_closed {
        report.correct_epochs_closed += 1;
        if let Some(hash) = correct_close_hashes.iter().next().copied() {
            report.epoch_close_hashes.push(hash);
            report.last_close_hash = Some(hash);
        }
    }
    if nodes_closed > 0 && close_hashes.len() == 1 {
        report.epochs_closed += 1;
    }

    report.nodes_closed = nodes_closed;
    report.correct_nodes_closed = correct_nodes_closed;
    report.distinct_close_hashes = close_hashes.len();
    report.correct_distinct_close_hashes = correct_close_hashes.len();
    report.nodes_finalized = nodes_finalized;
    report.correct_nodes_finalized = if correct_ledger_roots.len() == 1 {
        correct_nodes_finalized
    } else {
        0
    };
    report.correct_distinct_finalized_hashes = correct_ledger_roots.len();

    if let Some(reference) = nodes.values().find(|node| !node.is_byzantine()) {
        if let Some(state) = reference.engine.close_state(epoch) {
            let finalized_in_epoch = state
                .statuses
                .iter()
                .filter(|(election, status)| {
                    election.epoch() == epoch && matches!(status, SlotStatus::Finalized { .. })
                })
                .count();
            if let Some(finalized_at) = close_timings.first_correct_close_ms {
                add_latency_samples(
                    &mut report.epoch_commit_latency_samples,
                    &mut report.average_epoch_commit_latency_ms,
                    1,
                    finalized_at.saturating_sub(epoch_start_ms),
                );
                report.committed_requests += finalized_in_epoch;
                update_throughput(report, finalized_at);
            }
            report.released += state
                .statuses
                .iter()
                .filter(|(election, status)| {
                    election.epoch() == epoch && matches!(status, SlotStatus::Released { .. })
                })
                .count();
            report.selected += state
                .statuses
                .iter()
                .filter(|(election, status)| {
                    election.epoch() == epoch
                        && matches!(
                            status,
                            SlotStatus::Finalized {
                                via: FinalityEvidence::CloseRecord,
                                ..
                            }
                        )
                })
                .count();

            let committee_ids = reference.engine.committee_ids_for_epoch(epoch)?;
            let committees = committee_ids
                .into_iter()
                .map(|id| {
                    reference
                        .engine
                        .committees
                        .get(&id)
                        .cloned()
                        .ok_or(RaiError::UnknownCommittee(id))
                })
                .collect::<Result<Vec<_>>>()?;
            report.epoch_snapshots.push(EpochSnapshot {
                epoch,
                accounts: state.accounts.clone(),
                committees: committees.clone(),
            });
            for (account, account_state) in &state.accounts {
                logger.emit(
                    LogLevel::Protocol,
                    "EPOCH_ACCOUNT_STATE",
                    &format!(
                        "epoch={epoch} account={account} balance={} representative={} frontier={}",
                        account_state.balance,
                        account_state.representative,
                        account_state.frontier.short()
                    ),
                )?;
            }
            for committee in committees {
                let weights = committee
                    .weights
                    .iter()
                    .map(|(replica, weight)| format!("{replica}:{weight}"))
                    .collect::<Vec<_>>()
                    .join(",");
                logger.emit(
                    LogLevel::Protocol,
                    "EPOCH_COMMITTEE",
                    &format!(
                        "epoch={epoch} committee={} weights=[{weights}] total_weight={} f={} p={}",
                        committee.id,
                        committee.total_weight(),
                        committee.f,
                        committee.p
                    ),
                )?;
            }
        }
        report.finalized_slots = reference
            .engine
            .blocks()
            .account_states()?
            .values()
            .filter_map(|state| reference.engine.blocks().candidate(state.frontier))
            .map(|block| block.block.slot.sequence as usize)
            .sum();
    }
    report.timeout_blocks += elections
        .iter()
        .filter(|(election, _)| {
            nodes.values().any(|node| {
                matches!(
                    node.engine.derive_result(election),
                    Ok(Some(GlobalResult::Timeout))
                )
            })
        })
        .count();

    if let (Some(start), Some(end)) = (
        close_timings.first_close_cut_ms,
        close_timings.first_close_record_ms,
    ) {
        add_latency_samples(
            &mut report.finalized_close_cut_latency_samples,
            &mut report.average_finalized_close_cut_latency_ms,
            1,
            end.saturating_sub(start),
        );
    }
    if let (Some(start), Some(end)) = (
        close_timings.first_close_record_ms,
        close_timings.first_correct_close_ms,
    ) {
        add_latency_samples(
            &mut report.finalized_close_record_latency_samples,
            &mut report.average_finalized_close_record_latency_ms,
            1,
            end.saturating_sub(start),
        );
    }

    logger.emit(
        LogLevel::Summary,
        "EPOCH_COMPLETE",
        &format!(
            "epoch={epoch} closed={nodes_closed}/{NODE_COUNT} correct_closed={correct_nodes_closed}/{correct_node_count} close_hashes={} correct_close_hashes={} client_requests={} committed_requests={} offered_requests_per_second_milli={} throughput_slots_per_second_milli={} avg_slot_finalization_latency_ms={} avg_epoch_commit_latency_ms={} avg_close_cut_latency_ms={} avg_close_record_latency_ms={} safety_faults={}",
            close_hashes.len(),
            correct_close_hashes.len(),
            report.client_requests,
            report.committed_requests,
            report.offered_requests_per_second_milli,
            report.throughput_slots_per_second_milli,
            report.average_slot_finalization_latency_ms,
            report.average_epoch_commit_latency_ms,
            report.average_finalized_close_cut_latency_ms,
            report.average_finalized_close_record_latency_ms,
            report.safety_faults,
        ),
    )?;
    Ok(correct_closed)
}

fn add_latency_samples(count: &mut usize, average_ms: &mut u64, added: usize, latency_ms: u64) {
    if added == 0 {
        return;
    }
    let old_total = (*average_ms as u128) * (*count as u128);
    let added_total = (latency_ms as u128) * (added as u128);
    *count += added;
    *average_ms = ((old_total + added_total) / (*count as u128)) as u64;
}

fn register_client_request(
    report: &mut SimulationReport,
    election: &ElectionId,
    submitted_at_ms: u64,
) {
    report.client_requests += 1;
    report
        .slot_submission_times
        .insert(election.clone(), submitted_at_ms);
    report.first_submission_ms = Some(
        report
            .first_submission_ms
            .map_or(submitted_at_ms, |current| current.min(submitted_at_ms)),
    );
}

fn observe_slot_consensus(
    nodes: &BTreeMap<ReplicaId, SimNode>,
    now_ms: u64,
    report: &mut SimulationReport,
) {
    let pending = report
        .slot_submission_times
        .iter()
        .filter(|(election, _)| !report.consensus_observed.contains(*election))
        .map(|(election, submitted)| (election.clone(), *submitted))
        .collect::<Vec<_>>();

    for (election, submitted_at) in pending {
        let results = nodes
            .values()
            .filter(|node| !node.is_byzantine())
            .map(|node| node.engine.derive_result(&election))
            .collect::<Result<Vec<_>>>();
        let Ok(results) = results else {
            continue;
        };
        let agreed = results.first().cloned().flatten();
        let finalized = matches!(agreed, Some(GlobalResult::Fast(_) | GlobalResult::Final(_)))
            && results.iter().all(|result| *result == agreed);
        if finalized {
            report.consensus_observed.insert(election);
            add_latency_samples(
                &mut report.slot_finalization_latency_samples,
                &mut report.average_slot_finalization_latency_ms,
                1,
                now_ms.saturating_sub(submitted_at),
            );
        }
    }
}

fn observe_one_slot_consensus(
    nodes: &BTreeMap<ReplicaId, SimNode>,
    election: &ElectionId,
    now_ms: u64,
    report: &mut SimulationReport,
) {
    let Some(submitted_at) = report.slot_submission_times.get(election).copied() else {
        return;
    };
    if report.consensus_observed.contains(election) {
        return;
    }
    let results = nodes
        .values()
        .filter(|node| !node.is_byzantine())
        .map(|node| node.engine.derive_result(election))
        .collect::<Result<Vec<_>>>();
    let Ok(results) = results else {
        return;
    };
    let agreed = results.first().cloned().flatten();
    let finalized = matches!(agreed, Some(GlobalResult::Fast(_) | GlobalResult::Final(_)))
        && results.iter().all(|result| *result == agreed);
    if finalized {
        report.consensus_observed.insert(election.clone());
        add_latency_samples(
            &mut report.slot_finalization_latency_samples,
            &mut report.average_slot_finalization_latency_ms,
            1,
            now_ms.saturating_sub(submitted_at),
        );
    }
}

fn update_throughput(report: &mut SimulationReport, completed_at_ms: u64) {
    report.last_completion_ms = Some(completed_at_ms);
    if let Some(started_at_ms) = report.first_submission_ms {
        report.benchmark_elapsed_ms = completed_at_ms.saturating_sub(started_at_ms);
        report.throughput_slots_per_second = if report.benchmark_elapsed_ms == 0 {
            0
        } else {
            (report.committed_requests as u64).saturating_mul(1_000) / report.benchmark_elapsed_ms
        };
        report.offered_requests_per_second_milli = if report.benchmark_elapsed_ms == 0 {
            0
        } else {
            (report.client_requests as u64).saturating_mul(1_000_000) / report.benchmark_elapsed_ms
        };
        report.throughput_slots_per_second_milli = if report.benchmark_elapsed_ms == 0 {
            0
        } else {
            (report.committed_requests as u64).saturating_mul(1_000_000)
                / report.benchmark_elapsed_ms
        };
    }
}

fn finalized_value(result: Option<GlobalResult>) -> Option<Hash32> {
    match result {
        Some(GlobalResult::Fast(value) | GlobalResult::Final(value)) => Some(value),
        _ => None,
    }
}

fn assert_epoch_safety<'a>(
    epoch: u64,
    slot_elections: impl IntoIterator<Item = &'a ElectionId>,
    nodes: &BTreeMap<ReplicaId, SimNode>,
) -> Result<()> {
    for election in slot_elections {
        let values = nodes
            .values()
            .filter(|node| !node.is_byzantine())
            .filter_map(|node| node.engine.derive_result(election).transpose())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|result| finalized_value(Some(result)))
            .collect::<BTreeSet<_>>();
        assert!(
            values.len() <= 1,
            "safety violation: correct replicas finalized conflicting blocks for {election}: {values:?}"
        );
    }

    for election_kind in ["close-cut", "close-record"] {
        let mut values = BTreeSet::new();
        for round in 0..MAX_PROTOCOL_STEPS as u32 {
            let election = match election_kind {
                "close-cut" => ElectionId::CloseCut { epoch, round },
                "close-record" => ElectionId::CloseRecord { epoch, round },
                _ => unreachable!(),
            };
            for node in nodes.values().filter(|node| !node.is_byzantine()) {
                if let Ok(result) = node.engine.derive_result(&election) {
                    values.extend(finalized_value(result));
                }
            }
        }
        assert!(
            values.len() <= 1,
            "safety violation: correct replicas finalized conflicting values for the epoch {epoch} {election_kind} election: {values:?}"
        );
    }
    Ok(())
}

fn apply_local_client_block(
    replica: ReplicaId,
    election: &ElectionId,
    signed: SignedBlock,
    nodes: &mut BTreeMap<ReplicaId, SimNode>,
    network: &mut AdversarialNetwork,
    config: &TimedSimulationConfig,
    logger: &mut EventLogger,
) -> Result<()> {
    let outcome = {
        let node = nodes.get_mut(&replica).expect("known replica");
        apply_message(
            node,
            &SimMessage::Block {
                election: election.clone(),
                signed,
            },
        )?
    };
    for outbound in outcome.outbound {
        publish_already_applied(replica, outbound, nodes, network, config, logger)?;
    }
    Ok(())
}

fn select_conflicting_accounts(
    config: &TimedSimulationConfig,
    primary_account: AccountId,
) -> BTreeSet<AccountId> {
    let quota =
        ((config.account_count as f64 * config.conflicting_block_percentage / 100.0).round()
            as usize)
            .min(config.account_count as usize);
    if quota == 0 {
        return BTreeSet::new();
    }

    // Keep workload selection independent from the network RNG stream. The
    // primary account is included whenever the quota is nonzero so the legacy
    // close/release/retry scenario remains represented at the default rate.
    let mut remaining = (1..=config.account_count)
        .filter(|account| *account != primary_account)
        .collect::<Vec<_>>();
    let mut rng = DeterministicRng::new(config.seed ^ 0xc11e_17c0_5e1e_c7ed);
    for index in (1..remaining.len()).rev() {
        let swap = rng.range_inclusive(0, index as u64) as usize;
        remaining.swap(index, swap);
    }
    let mut selected = BTreeSet::from([primary_account]);
    selected.extend(remaining.into_iter().take(quota.saturating_sub(1)));
    selected
}

fn log_client_submit(
    logger: &mut EventLogger,
    at_ms: u64,
    account: AccountId,
    target: ReplicaId,
    block: Hash32,
    conflict: bool,
) -> Result<()> {
    logger.emit(
        LogLevel::Protocol,
        "CLIENT_SUBMIT",
        &format!(
            "time={at_ms} account={account} target={target} block={} conflict={conflict}",
            block.short()
        ),
    )
}

fn complete_if_strong(
    engine: &mut RaiEngine,
    election: &ElectionId,
    expected_hash: Hash32,
) -> Result<()> {
    match engine.derive_result(election)? {
        Some(GlobalResult::Fast(hash) | GlobalResult::Final(hash)) if hash == expected_hash => {
            engine.complete_block(election, hash)?;
        }
        _ => {}
    }
    Ok(())
}

fn apply_message(node: &mut SimNode, payload: &SimMessage) -> Result<ApplyOutcome> {
    let mut outcome = ApplyOutcome::default();
    match payload {
        SimMessage::Block { election, signed } => {
            if node.is_byzantine() {
                if election.slot() != Some(signed.block.slot) {
                    return Err(RaiError::Inadmissible(format!(
                        "received block for slot {}, but election is {}",
                        signed.block.slot, election
                    )));
                }
                let hash = node.engine.submit_block(signed.clone())?;
                outcome
                    .outbound
                    .extend(node.arbitrary_votes(election, VoteValue::Candidate(hash))?);
            } else {
                let update = node.engine.receive_block_and_vote_first_valid_all(
                    node.id,
                    election,
                    signed.clone(),
                )?;
                if update.is_some() {
                    outcome.outbound.extend(sign_votes_for_all_committees(
                        node,
                        election,
                        VoteValue::Candidate(signed.hash()),
                        VoteKind::First,
                    )?);
                }
            }
        }
        SimMessage::Vote(vote) => {
            submit_network_vote(&mut node.engine, vote.clone())?;
        }
        SimMessage::Report(report) => {
            node.engine.submit_report(report.clone())?;
        }
        SimMessage::CloseCut {
            election,
            candidate,
        } => {
            if node.engine.epoch_state(election.epoch()) == Some(EpochState::Closed) {
                return Ok(outcome);
            }
            node.engine.register_derived_election(election.clone())?;
            let hash = node.engine.accept_close_cut_candidate(candidate.clone())?;
            let _ = node.engine.start_election_timer(election);
            if node.is_byzantine() {
                outcome
                    .outbound
                    .extend(node.arbitrary_votes(election, VoteValue::Candidate(hash))?);
            } else {
                match node
                    .engine
                    .cast_first_vote_all(node.id, election, VoteValue::Candidate(hash))
                {
                    Ok(_) => outcome.outbound.extend(sign_votes_for_all_committees(
                        node,
                        election,
                        VoteValue::Candidate(hash),
                        VoteKind::First,
                    )?),
                    Err(RaiError::SafetyFault(message)) => {
                        return Err(RaiError::SafetyFault(message));
                    }
                    Err(_) => {}
                }
            }
        }
        SimMessage::CloseRecord { election, package } => {
            if node.engine.epoch_state(election.epoch()) == Some(EpochState::Closed) {
                return Ok(outcome);
            }
            node.engine.register_derived_election(election.clone())?;
            let hash = node.engine.accept_close_record_candidate(package.clone())?;
            let _ = node.engine.start_election_timer(election);
            if node.is_byzantine() {
                outcome
                    .outbound
                    .extend(node.arbitrary_votes(election, VoteValue::Candidate(hash))?);
            } else {
                match node
                    .engine
                    .cast_first_vote_all(node.id, election, VoteValue::Candidate(hash))
                {
                    Ok(_) => outcome.outbound.extend(sign_votes_for_all_committees(
                        node,
                        election,
                        VoteValue::Candidate(hash),
                        VoteKind::First,
                    )?),
                    Err(RaiError::SafetyFault(message)) => {
                        return Err(RaiError::SafetyFault(message));
                    }
                    Err(_) => {}
                }
            }
        }
    }
    Ok(outcome)
}

fn sign_votes_for_all_committees(
    node: &SimNode,
    election: &ElectionId,
    value: VoteValue,
    kind: VoteKind,
) -> Result<Vec<SimMessage>> {
    node.engine
        .applicable_election_committees(node.id, election)?
        .into_iter()
        .map(|committee| {
            SignedVote::new(
                &node.engine.crypto,
                node.id,
                election.clone(),
                committee,
                value,
                kind,
            )
            .map(SimMessage::Vote)
        })
        .collect()
}

fn submit_network_vote(engine: &mut RaiEngine, vote: SignedVote) -> Result<()> {
    match engine.submit_vote_with_candidate_data(vote.clone()) {
        Ok(_) => Ok(()),
        Err(RaiError::UnknownElection(_)) => {
            if engine.epoch_state(vote.election.epoch()) == Some(EpochState::Closed) {
                return Ok(());
            }
            engine.register_derived_election(vote.election.clone())?;
            engine.submit_vote_with_candidate_data(vote)?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn publish_already_applied(
    sender: ReplicaId,
    payload: SimMessage,
    nodes: &mut BTreeMap<ReplicaId, SimNode>,
    network: &mut AdversarialNetwork,
    config: &TimedSimulationConfig,
    logger: &mut EventLogger,
) -> Result<()> {
    if nodes
        .get(&sender)
        .expect("known sender")
        .is_silent_byzantine()
    {
        return Ok(());
    }
    let message_id = network.register_message(sender, payload);
    nodes
        .get_mut(&sender)
        .expect("known sender")
        .seen
        .insert(message_id);
    network.broadcast(message_id, sender, None, config, logger)?;
    Ok(())
}

fn drain_until_wave_finalized(
    close_at_ms: u64,
    elections: &[(ElectionId, Hash32)],
    nodes: &mut BTreeMap<ReplicaId, SimNode>,
    network: &mut AdversarialNetwork,
    config: &TimedSimulationConfig,
    logger: &mut EventLogger,
    report: &mut SimulationReport,
) -> Result<bool> {
    loop {
        let finalized = elections.iter().all(|(election, _)| {
            let mut results = nodes
                .values()
                .filter(|node| !node.is_byzantine())
                .map(|node| node.engine.derive_result(election));
            let Some(Ok(Some(first @ (GlobalResult::Fast(_) | GlobalResult::Final(_))))) =
                results.next()
            else {
                return false;
            };
            results.all(|result| result == Ok(Some(first.clone())))
        });
        if finalized {
            for node in nodes.values_mut().filter(|node| !node.is_byzantine()) {
                for (election, expected) in elections {
                    node.engine.complete_block(election, *expected)?;
                }
            }
            return Ok(true);
        }

        let Some(next_delivery_ms) = network.next_delivery_ms() else {
            return Ok(false);
        };
        if next_delivery_ms >= close_at_ms {
            return Ok(false);
        }
        drain_network_until(next_delivery_ms, nodes, network, config, logger, report)?;
    }
}

fn drain_network_until(
    until_ms: u64,
    nodes: &mut BTreeMap<ReplicaId, SimNode>,
    network: &mut AdversarialNetwork,
    config: &TimedSimulationConfig,
    logger: &mut EventLogger,
    report: &mut SimulationReport,
) -> Result<()> {
    while let Some(envelope) = network.pop_due(until_ms.min(config.stop_ms)) {
        let previous_time = network.now_ms;
        network.now_ms = envelope.deliver_at_ms;
        if config.realtime && network.now_ms > previous_time {
            thread::sleep(Duration::from_millis(network.now_ms - previous_time));
        }
        set_global_time(nodes, network.now_ms, config)?;

        if network.partition_blocks(config, &envelope) {
            network.stats.dropped += 1;
            network.stats.partition_dropped += 1;
            logger.emit(
                LogLevel::Network,
                "NETWORK_DROP",
                &format!(
                    "time={} id={} from={} to={} kind={} reason=partition",
                    network.now_ms,
                    envelope.message_id,
                    envelope.from,
                    envelope.to,
                    envelope.payload.label()
                ),
            )?;
            continue;
        }

        network.stats.delivered += 1;
        logger.emit(
            LogLevel::Network,
            "NETWORK_DELIVER",
            &format!(
                "time={} id={} from={} to={} kind={}",
                network.now_ms,
                envelope.message_id,
                envelope.from,
                envelope.to,
                envelope.payload.label()
            ),
        )?;

        let newly_seen = nodes
            .get_mut(&envelope.to)
            .expect("known receiver")
            .seen
            .insert(envelope.message_id);
        if !newly_seen {
            network.stats.deduplicated += 1;
            continue;
        }

        let apply_result = {
            let receiver = nodes.get_mut(&envelope.to).expect("known receiver");
            apply_message(receiver, &envelope.payload)
        };
        match apply_result {
            Ok(outcome) => {
                network.stats.accepted += 1;
                let receiver_is_silent = nodes
                    .get(&envelope.to)
                    .expect("known receiver")
                    .is_silent_byzantine();
                if !receiver_is_silent {
                    network.broadcast(
                        envelope.message_id,
                        envelope.to,
                        Some(envelope.from),
                        config,
                        logger,
                    )?;
                }
                for outbound in outcome.outbound {
                    publish_already_applied(envelope.to, outbound, nodes, network, config, logger)?;
                }
                if let Some(election) = envelope.payload.slot_election() {
                    observe_one_slot_consensus(nodes, election, network.now_ms, report);
                }
            }
            Err(error) => {
                nodes
                    .get_mut(&envelope.to)
                    .expect("known receiver")
                    .seen
                    .remove(&envelope.message_id);
                network.stats.rejected += 1;
                log_nonfatal_node_error(
                    envelope.to,
                    envelope.payload.label(),
                    &error,
                    logger,
                    report,
                )?;
            }
        }
    }
    if until_ms <= config.stop_ms {
        network.now_ms = network.now_ms.max(until_ms);
        set_global_time(nodes, network.now_ms, config)?;
        observe_slot_consensus(nodes, network.now_ms, report);
    }
    Ok(())
}

fn drive_close_protocol_networked(
    epoch: u64,
    nodes: &mut BTreeMap<ReplicaId, SimNode>,
    network: &mut AdversarialNetwork,
    config: &TimedSimulationConfig,
    logger: &mut EventLogger,
    report: &mut SimulationReport,
) -> Result<ClosePhaseTimings> {
    let mut timings = ClosePhaseTimings::default();
    for _ in 0..MAX_PROTOCOL_STEPS {
        // Evidence delivered exactly at the simulation deadline must still be
        // processed locally. In particular, installing an already certified
        // instance-wide close decision requires no additional network time.
        let deadline_reached = network.now_ms >= config.stop_ms;
        drain_network_until(network.now_ms, nodes, network, config, logger, report)?;

        let mut protocol_messages = Vec::<(ReplicaId, SimMessage)>::new();
        let mut waiting = BTreeMap::<ReplicaId, BTreeSet<ElectionId>>::new();
        let mut any_open = false;

        for replica in 1..=NODE_COUNT as u64 {
            if nodes
                .get(&replica)
                .expect("known replica")
                .is_silent_byzantine()
            {
                continue;
            }
            let state = nodes
                .get(&replica)
                .expect("known replica")
                .engine
                .epoch_state(epoch);
            if state == Some(EpochState::Closed) {
                continue;
            }
            if state != Some(EpochState::Closing) {
                continue;
            }
            any_open = true;
            let action = nodes
                .get_mut(&replica)
                .expect("known replica")
                .engine
                .drive_close_protocol(epoch);
            match action {
                Ok(CloseProtocolAction::BroadcastCloseCut {
                    election,
                    candidate,
                    hash,
                }) => {
                    timings.first_close_cut_ms.get_or_insert(network.now_ms);
                    logger.emit(
                        LogLevel::Protocol,
                        "CLOSE_CUT_BROADCAST",
                        &format!(
                            "time={} replica={replica} election={election} hash={} elections={}",
                            network.now_ms,
                            hash.short(),
                            candidate.elections.len()
                        ),
                    )?;
                    report.close_cut_rounds =
                        report.close_cut_rounds.max(election_round_count(&election));
                    protocol_messages.push((
                        replica,
                        SimMessage::CloseCut {
                            election,
                            candidate,
                        },
                    ));
                }
                Ok(CloseProtocolAction::BroadcastCloseRecord {
                    election,
                    package,
                    hash,
                }) => {
                    timings.first_close_record_ms.get_or_insert(network.now_ms);
                    logger.emit(
                        LogLevel::Protocol,
                        "CLOSE_RECORD_BROADCAST",
                        &format!(
                            "time={} replica={replica} election={election} hash={}",
                            network.now_ms,
                            hash.short()
                        ),
                    )?;
                    if report.close_record_rounds == 0 {
                        logger.emit(
                            LogLevel::Summary,
                            "CLOSE_CUT_CERTIFIED",
                            &format!("replica={replica}"),
                        )?;
                    }
                    report.close_record_rounds = report
                        .close_record_rounds
                        .max(election_round_count(&election));
                    protocol_messages
                        .push((replica, SimMessage::CloseRecord { election, package }));
                }
                Ok(CloseProtocolAction::AwaitCloseCut { election })
                | Ok(CloseProtocolAction::AwaitCloseRecord { election }) => {
                    waiting.entry(replica).or_default().insert(election);
                }
                Ok(CloseProtocolAction::DrainCut { pending }) => {
                    waiting.entry(replica).or_default().extend(pending);
                }
                Ok(CloseProtocolAction::AwaitReports) => {}
                Ok(CloseProtocolAction::Closed { .. }) => {}
                Err(error) => {
                    log_nonfatal_node_error(replica, "drive-close", &error, logger, report)?
                }
            }
        }

        if !any_open || all_correct_nodes_closed(nodes, epoch) {
            if all_correct_nodes_closed(nodes, epoch) {
                timings.first_correct_close_ms.get_or_insert(network.now_ms);
            }
            break;
        }
        if deadline_reached {
            break;
        }

        let broadcast_candidates = !protocol_messages.is_empty();
        if broadcast_candidates {
            for (sender, payload) in protocol_messages {
                // drive_close_protocol has already accepted and registered the
                // candidate locally. Re-applying it casts the sender's own vote.
                let local = {
                    let node = nodes.get_mut(&sender).expect("known sender");
                    apply_message(node, &payload)
                };
                match local {
                    Ok(outcome) => {
                        for outbound in outcome.outbound {
                            publish_already_applied(
                                sender, outbound, nodes, network, config, logger,
                            )?;
                        }
                        publish_already_applied(sender, payload, nodes, network, config, logger)?;
                    }
                    Err(error) => log_nonfatal_node_error(
                        sender,
                        "local-close-candidate",
                        &error,
                        logger,
                        report,
                    )?,
                }
            }
            let target = network
                .now_ms
                .saturating_add(config.close_round_timeout_ms as u64)
                .min(config.stop_ms);
            drain_network_until(target, nodes, network, config, logger, report)?;
            if waiting.is_empty() {
                continue;
            }
        }

        // Candidate broadcasts and locally pending actions must make progress
        // independently. In particular, a Byzantine replica can continuously
        // open successor close rounds; that traffic must not starve correct
        // replicas that are draining the certified cut. A candidate broadcast
        // already advanced time by one round timeout above, so one more tick is
        // enough to cross the strict timer deadline. With no broadcast, advance
        // by a full timeout before attempting second-look and timeout actions.
        let followup_delay = if broadcast_candidates {
            1
        } else {
            (config.close_round_timeout_ms as u64).saturating_add(1)
        };
        let next_time = network
            .now_ms
            .saturating_add(followup_delay)
            .min(config.stop_ms);
        if next_time == network.now_ms {
            break;
        }
        network.now_ms = next_time;
        set_global_time(nodes, next_time, config)?;
        let mut followup_votes = Vec::new();
        for (replica, elections) in waiting {
            for election in elections {
                match emit_followup_votes(
                    nodes.get_mut(&replica).expect("known replica"),
                    &election,
                ) {
                    Ok(votes) => {
                        followup_votes.extend(votes.into_iter().map(|vote| (replica, vote)))
                    }
                    Err(error) => {
                        log_nonfatal_node_error(replica, "followup-action", &error, logger, report)?
                    }
                }
            }
        }
        for (replica, vote) in followup_votes {
            publish_already_applied(replica, vote, nodes, network, config, logger)?;
        }
        if !broadcast_candidates {
            network.anti_entropy(nodes, config, logger)?;
        }
        let target = network
            .now_ms
            .saturating_add(config.close_round_timeout_ms as u64)
            .min(config.stop_ms);
        drain_network_until(target, nodes, network, config, logger, report)?;
    }
    if all_correct_nodes_closed(nodes, epoch) {
        timings.first_correct_close_ms.get_or_insert(network.now_ms);
    }
    Ok(timings)
}

fn drive_retry_network(
    retry: &ElectionId,
    retry_hash: Hash32,
    nodes: &mut BTreeMap<ReplicaId, SimNode>,
    network: &mut AdversarialNetwork,
    config: &TimedSimulationConfig,
    logger: &mut EventLogger,
    report: &mut SimulationReport,
) -> Result<()> {
    let correct_node_count = nodes.values().filter(|node| !node.is_byzantine()).count();
    for _ in 0..MAX_PROTOCOL_STEPS {
        let mut new_votes = Vec::new();
        for replica in 1..=NODE_COUNT as u64 {
            let node = nodes.get_mut(&replica).expect("known replica");
            if node.engine.epoch_state(0) != Some(EpochState::Closed) {
                continue;
            }
            if node.is_byzantine() {
                if node.byzantine_direct_actions.insert(retry.clone()) {
                    for vote in node.arbitrary_votes(retry, VoteValue::Candidate(retry_hash))? {
                        new_votes.push((replica, vote));
                    }
                }
            } else if node
                .engine
                .first_vote_choice(replica, COMMITTEE_ID, retry)
                .is_none()
            {
                match node.engine.cast_first_vote(
                    replica,
                    retry,
                    COMMITTEE_ID,
                    VoteValue::Candidate(retry_hash),
                ) {
                    Ok(_) => new_votes.push((
                        replica,
                        SimMessage::Vote(SignedVote::new(
                            &node.engine.crypto,
                            replica,
                            retry.clone(),
                            COMMITTEE_ID,
                            VoteValue::Candidate(retry_hash),
                            VoteKind::First,
                        )?),
                    )),
                    Err(RaiError::SafetyFault(message)) => {
                        return Err(RaiError::SafetyFault(message));
                    }
                    Err(_) => {}
                }
            }
        }
        for (replica, vote) in new_votes {
            publish_already_applied(replica, vote, nodes, network, config, logger)?;
        }

        let target = network
            .now_ms
            .saturating_add(config.close_round_timeout_ms as u64)
            .min(config.stop_ms);
        drain_network_until(target, nodes, network, config, logger, report)?;

        let finalized = nodes
            .values()
            .filter(|node| {
                !node.is_byzantine()
                    && node
                        .engine
                        .blocks()
                        .finalized(retry.slot().expect("slot retry"))
                        == Some(retry_hash)
            })
            .count();
        if finalized == correct_node_count || network.now_ms >= config.stop_ms {
            break;
        }

        for node in nodes.values_mut() {
            match node.engine.derive_result(retry) {
                Ok(Some(result))
                    if matches!(&result, GlobalResult::Fast(_) | GlobalResult::Final(_))
                        && result.value() == Some(retry_hash) =>
                {
                    if let Err(error) = node.engine.complete_block(retry, retry_hash) {
                        log_nonfatal_node_error(node.id, "complete-retry", &error, logger, report)?;
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    log_nonfatal_node_error(node.id, "derive-retry", &error, logger, report)?
                }
            }
        }

        let finalized = nodes
            .values()
            .filter(|node| {
                !node.is_byzantine()
                    && node
                        .engine
                        .blocks()
                        .finalized(retry.slot().expect("slot retry"))
                        == Some(retry_hash)
            })
            .count();
        if finalized == correct_node_count {
            break;
        }
        if network.now_ms >= config.stop_ms {
            break;
        }
        network.anti_entropy(nodes, config, logger)?;
        if network.queue.is_empty() {
            let next = network
                .now_ms
                .saturating_add(config.close_round_timeout_ms as u64)
                .min(config.stop_ms);
            if next == network.now_ms {
                break;
            }
            network.now_ms = next;
            set_global_time(nodes, next, config)?;
        }
    }
    Ok(())
}

fn all_correct_nodes_closed(nodes: &BTreeMap<ReplicaId, SimNode>, epoch: u64) -> bool {
    nodes
        .values()
        .filter(|node| !node.is_byzantine())
        .all(|node| node.engine.epoch_state(epoch) == Some(EpochState::Closed))
}

fn first_correct_replica(nodes: &BTreeMap<ReplicaId, SimNode>) -> ReplicaId {
    nodes
        .values()
        .find(|node| !node.is_byzantine())
        .map(|node| node.id)
        .expect("validated configuration has a correct replica")
}

fn emit_followup_votes(node: &mut SimNode, election: &ElectionId) -> Result<Vec<SimMessage>> {
    if node.is_byzantine() {
        if !node.byzantine_direct_actions.insert(election.clone()) {
            return Ok(Vec::new());
        }
        return node.arbitrary_votes(election, VoteValue::Timeout);
    }
    let committees = node
        .engine
        .applicable_election_committees(node.id, election)?;
    let has_first_vote = committees.iter().any(|committee| {
        node.engine
            .first_vote_choice(node.id, *committee, election)
            .is_some()
    });
    if !has_first_vote {
        if election.is_close() {
            match node.engine.preferred_close_value(election) {
                Ok(Some(preferred)) => match node.engine.cast_first_vote_all(
                    node.id,
                    election,
                    VoteValue::Candidate(preferred),
                ) {
                    Ok(_) => {
                        return sign_votes_for_all_committees(
                            node,
                            election,
                            VoteValue::Candidate(preferred),
                            VoteKind::First,
                        );
                    }
                    Err(RaiError::SafetyFault(message)) => {
                        return Err(RaiError::SafetyFault(message));
                    }
                    Err(_) => {}
                },
                Err(RaiError::SafetyFault(message)) => {
                    return Err(RaiError::SafetyFault(message));
                }
                Ok(None) | Err(_) => {}
            }
        }
        match node
            .engine
            .cast_first_vote_all(node.id, election, VoteValue::Timeout)
        {
            Ok(_) => {
                return sign_votes_for_all_committees(
                    node,
                    election,
                    VoteValue::Timeout,
                    VoteKind::First,
                );
            }
            Err(RaiError::SafetyFault(message)) => {
                return Err(RaiError::SafetyFault(message));
            }
            Err(_) => return Ok(Vec::new()),
        }
    }

    let many_values = committees
        .iter()
        .filter_map(|committee| node.engine.committees.get(committee))
        .flat_map(|committee| node.engine.pool.many_values(committee, election))
        .collect::<BTreeSet<_>>();
    let mut votes = Vec::new();
    for value in many_values {
        match node
            .engine
            .cast_notarization_vote_all(node.id, election, value)
        {
            Ok(_) => votes.extend(sign_votes_for_all_committees(
                node,
                election,
                value,
                VoteKind::Notarization,
            )?),
            Err(RaiError::SafetyFault(message)) => {
                return Err(RaiError::SafetyFault(message));
            }
            Err(_) => {}
        }
    }

    match node
        .engine
        .cast_notarization_vote_all(node.id, election, VoteValue::Timeout)
    {
        Ok(_) => votes.extend(sign_votes_for_all_committees(
            node,
            election,
            VoteValue::Timeout,
            VoteKind::Notarization,
        )?),
        Err(RaiError::SafetyFault(message)) => return Err(RaiError::SafetyFault(message)),
        Err(_) => {}
    }
    Ok(votes)
}

fn summarize(
    old_election: &ElectionId,
    retry_outcome: Option<&(ElectionId, Hash32)>,
    epoch_start_ms: u64,
    retry_start_ms: Option<u64>,
    close_timings: ClosePhaseTimings,
    nodes: &BTreeMap<ReplicaId, SimNode>,
    network: &mut AdversarialNetwork,
    _config: &TimedSimulationConfig,
    logger: &mut EventLogger,
    report: &mut SimulationReport,
) -> Result<()> {
    assert_epoch_safety(
        0,
        std::iter::once(old_election)
            .chain(retry_outcome.map(|(election, _)| election).into_iter()),
        nodes,
    )?;

    let slot = old_election.slot().expect("slot election");
    let mut close_hashes = BTreeSet::new();
    let mut finalized_hashes = BTreeSet::new();
    let mut correct_close_hashes = BTreeSet::new();
    let mut correct_finalized_hashes = BTreeSet::new();
    let correct_node_count = nodes.values().filter(|node| !node.is_byzantine()).count();

    if let Some(reference) = nodes.values().find(|node| !node.is_byzantine()) {
        if let Some(state) = reference.engine.close_state(0) {
            let committees = reference
                .engine
                .committee_ids_for_epoch(0)?
                .into_iter()
                .map(|id| {
                    reference
                        .engine
                        .committees
                        .get(&id)
                        .cloned()
                        .ok_or(RaiError::UnknownCommittee(id))
                })
                .collect::<Result<Vec<_>>>()?;
            report.epoch_snapshots.push(EpochSnapshot {
                epoch: 0,
                accounts: state.accounts.clone(),
                committees: committees.clone(),
            });
            for (account, account_state) in &state.accounts {
                logger.emit(
                    LogLevel::Protocol,
                    "EPOCH_ACCOUNT_STATE",
                    &format!(
                        "epoch=0 account={account} balance={} representative={} frontier={}",
                        account_state.balance,
                        account_state.representative,
                        account_state.frontier.short()
                    ),
                )?;
            }
            for committee in committees {
                let weights = committee
                    .weights
                    .iter()
                    .map(|(replica, weight)| format!("{replica}:{weight}"))
                    .collect::<Vec<_>>()
                    .join(",");
                logger.emit(
                    LogLevel::Protocol,
                    "EPOCH_COMMITTEE",
                    &format!(
                        "epoch=0 committee={} weights=[{weights}] total_weight={} f={} p={}",
                        committee.id,
                        committee.total_weight(),
                        committee.f,
                        committee.p
                    ),
                )?;
            }
            let finalized = state
                .statuses
                .values()
                .filter(|status| matches!(status, SlotStatus::Finalized { .. }))
                .count();
            if let Some(end) = close_timings.first_correct_close_ms {
                add_latency_samples(
                    &mut report.epoch_commit_latency_samples,
                    &mut report.average_epoch_commit_latency_ms,
                    1,
                    end.saturating_sub(epoch_start_ms),
                );
                report.committed_requests += finalized;
                update_throughput(report, end);
            }
        }
    }
    if let (Some(start), Some(end)) = (
        close_timings.first_close_cut_ms,
        close_timings.first_close_record_ms,
    ) {
        add_latency_samples(
            &mut report.finalized_close_cut_latency_samples,
            &mut report.average_finalized_close_cut_latency_ms,
            1,
            end.saturating_sub(start),
        );
    }
    if let (Some(start), Some(end)) = (
        close_timings.first_close_record_ms,
        close_timings.first_correct_close_ms,
    ) {
        add_latency_samples(
            &mut report.finalized_close_record_latency_samples,
            &mut report.average_finalized_close_record_latency_ms,
            1,
            end.saturating_sub(start),
        );
    }
    if retry_outcome.is_some() {
        if retry_start_ms.is_some() {
            report.committed_requests += 1;
            update_throughput(report, network.now_ms);
        }
    }

    for node in nodes.values() {
        let is_correct = !node.is_byzantine();
        report.byzantine_votes_omitted += node.byzantine_stats.votes_omitted;
        report.byzantine_double_votes += node.byzantine_stats.double_votes;
        report.byzantine_wrong_votes += node.byzantine_stats.wrong_votes;
        if node.engine.epoch_state(0) == Some(EpochState::Closed) {
            report.nodes_closed += 1;
            if is_correct {
                report.correct_nodes_closed += 1;
            }
            if let Some(hash) = node.engine.close_hash(0) {
                close_hashes.insert(hash);
                if is_correct {
                    correct_close_hashes.insert(hash);
                }
            }
            if let Some(state) = node.engine.close_state(0) {
                if matches!(
                    state.statuses.get(old_election),
                    Some(SlotStatus::Released { .. })
                ) {
                    report.released = 1;
                }
                if state.statuses.values().any(|status| {
                    matches!(
                        status,
                        SlotStatus::Finalized {
                            via: FinalityEvidence::CloseRecord,
                            ..
                        }
                    )
                }) {
                    report.selected = 1;
                }
            }
        }
        let decision_election = retry_outcome
            .map(|(election, _)| election)
            .unwrap_or(old_election);
        let decision_result = node.engine.derive_result(decision_election).ok().flatten();
        if matches!(
            decision_result,
            Some(GlobalResult::Fast(_) | GlobalResult::Final(_))
        ) {
            report.fast_blocks = 1;
        }
        if let Some(hash) = node.engine.blocks().finalized(slot) {
            report.nodes_finalized += 1;
            finalized_hashes.insert(hash);
            if is_correct {
                report.correct_nodes_finalized += 1;
                correct_finalized_hashes.insert(hash);
            }
        }
        match node.engine.derive_result(old_election) {
            Ok(Some(GlobalResult::Timeout)) => report.timeout_blocks = 1,
            Ok(Some(GlobalResult::Notarized(_))) => report.notarized_blocks = 1,
            Ok(None) => report.unresolved_blocks = 1,
            Ok(_) => {}
            Err(_) => {}
        }
    }

    report.epochs_closed = if report.nodes_closed > 0 { 1 } else { 0 };
    report.correct_epochs_closed = usize::from(report.correct_nodes_closed == correct_node_count);
    report.finalized_slots = if report.nodes_finalized > 0 { 1 } else { 0 };
    report.distinct_close_hashes = close_hashes.len();
    report.correct_distinct_close_hashes = correct_close_hashes.len();
    report.correct_distinct_finalized_hashes = correct_finalized_hashes.len();
    report.last_close_hash = if close_hashes.len() == 1 {
        close_hashes.iter().next().copied()
    } else {
        None
    };
    if let Some(hash) = report.last_close_hash {
        report.epoch_close_hashes.push(hash);
    }
    if close_hashes.len() > 1 || finalized_hashes.len() > 1 {
        report.safety_faults += 1;
    }
    report.converged = report.nodes_closed == NODE_COUNT
        && report.nodes_finalized == NODE_COUNT
        && report.distinct_close_hashes == 1
        && finalized_hashes.len() == 1
        && report.safety_faults == 0;
    let correct_decision_split =
        correct_close_hashes.len() > 1 || correct_finalized_hashes.len() > 1;
    report.correct_converged = report.correct_nodes_closed == correct_node_count
        && report.correct_nodes_finalized == correct_node_count
        && report.correct_distinct_close_hashes == 1
        && report.correct_distinct_finalized_hashes == 1
        && !correct_decision_split;

    report.network_scheduled = network.stats.scheduled;
    report.network_byzantine_scheduled = network.stats.byzantine_scheduled;
    report.network_delivered = network.stats.delivered;
    report.network_dropped = network.stats.dropped;
    report.network_duplicated = network.stats.duplicated;
    report.network_slow_delayed = network.stats.slow_delayed;
    report.network_partition_dropped = network.stats.partition_dropped;
    report.network_accepted = network.stats.accepted;
    report.network_deduplicated = network.stats.deduplicated;
    report.network_rejected = network.stats.rejected;
    report.network_queue_remaining = network.queue.len();
    if report.nodes_closed > 0 {
        logger.emit(
            LogLevel::Summary,
            "CLOSE_RECORD_CERTIFIED",
            &format!(
                "nodes_closed={} distinct_hashes={} hash={}",
                report.nodes_closed,
                report.distinct_close_hashes,
                report
                    .last_close_hash
                    .map_or_else(|| "none".into(), |hash| hash.short())
            ),
        )?;
    }
    if report.released > 0 {
        logger.emit(
            LogLevel::Summary,
            "CERTIFIED_RELEASE",
            &format!("nodes_closed={}", report.nodes_closed),
        )?;
    }
    if report.nodes_finalized > 0 {
        logger.emit(
            LogLevel::Summary,
            if retry_outcome.is_some() {
                "RETRY_FINALIZED"
            } else {
                "BLOCK_FINALIZED"
            },
            &format!("nodes_finalized={}", report.nodes_finalized),
        )?;
    }
    emit_simulation_summary(logger, report)?;
    report.events_logged = logger.count;
    Ok(())
}

fn emit_simulation_summary(logger: &mut EventLogger, report: &SimulationReport) -> Result<()> {
    let passed = report.correct_converged && report.safety_faults == 0;
    let status = if passed { "PASS" } else { "FAIL" };
    let target = report
        .target_blocks_per_second
        .map_or_else(|| "unlimited".to_string(), |rate| rate.to_string());
    let mut details = format!(
        "status={status} epochs={}/{} finalized_requests={}/{} target_blocks_per_second={} end_to_end_offered_blocks_per_second={} throughput_blocks_per_second={} throughput_slots_per_second={} avg_slot_finalization_latency_ms={}",
        report.correct_epochs_closed,
        report.epochs_requested,
        report.committed_requests,
        report.client_requests,
        target,
        fixed_milli(report.offered_requests_per_second_milli),
        fixed_milli(report.throughput_slots_per_second_milli),
        fixed_milli(report.throughput_slots_per_second_milli),
        report.average_slot_finalization_latency_ms,
    );
    if !passed {
        let reason = if report.safety_faults > 0 {
            "safety_fault"
        } else if report.correct_epochs_closed < report.epochs_requested {
            "correct_replicas_did_not_close_all_epochs"
        } else {
            "correct_replicas_did_not_converge"
        };
        details.push_str(&format!(
            " reason={reason} safety_faults={}",
            report.safety_faults
        ));
    }
    logger.emit_final_summary(&details)
}

fn fixed_milli(value: u64) -> String {
    format!("{}.{:03}", value / 1_000, value % 1_000)
}

fn log_nonfatal_node_error(
    replica: ReplicaId,
    action: &str,
    error: &RaiError,
    logger: &mut EventLogger,
    report: &mut SimulationReport,
) -> Result<()> {
    if matches!(error, RaiError::SafetyFault(_)) {
        report.safety_faults += 1;
    }
    logger.emit(
        LogLevel::Protocol,
        "NODE_REJECT",
        &format!("replica={replica} action={action} error={error}"),
    )
}

fn set_global_time(
    nodes: &mut BTreeMap<ReplicaId, SimNode>,
    global_ms: u64,
    config: &TimedSimulationConfig,
) -> Result<()> {
    for replica in 1..=NODE_COUNT as u64 {
        let offset = config.clock_offsets_ms[(replica - 1) as usize] as i128;
        let local = (global_ms as i128 + offset).max(0) as u64;
        let node = nodes.get_mut(&replica).expect("known replica");
        if local >= node.engine.now_ms() {
            node.engine.set_now_ms(local)?;
        }
    }
    Ok(())
}

fn election_round_count(election: &ElectionId) -> usize {
    match election {
        ElectionId::CloseCut { round, .. } | ElectionId::CloseRecord { round, .. } => {
            *round as usize + 1
        }
        ElectionId::Slot { .. } => 0,
    }
}

fn round_up_to_tick(value: u64, tick: u64) -> u64 {
    if tick <= 1 {
        return value;
    }
    (value.saturating_add(tick - 1) / tick).saturating_mul(tick)
}

fn next_value<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| RaiError::InvalidConfiguration(format!("missing value for {flag}")))
}

fn parse_u64(value: &str, flag: &str) -> Result<u64> {
    value.parse().map_err(|_| {
        RaiError::InvalidConfiguration(format!("invalid unsigned integer for {flag}: {value}"))
    })
}

fn parse_usize(value: &str, flag: &str) -> Result<usize> {
    value.parse().map_err(|_| {
        RaiError::InvalidConfiguration(format!("invalid unsigned integer for {flag}: {value}"))
    })
}

fn parse_i64(value: &str, flag: &str) -> Result<i64> {
    value
        .parse()
        .map_err(|_| RaiError::InvalidConfiguration(format!("invalid integer for {flag}: {value}")))
}

fn parse_probability(value: &str, flag: &str) -> Result<f64> {
    let parsed = value.parse::<f64>().map_err(|_| {
        RaiError::InvalidConfiguration(format!("invalid probability for {flag}: {value}"))
    })?;
    if !(0.0..=1.0).contains(&parsed) {
        return Err(RaiError::InvalidConfiguration(format!(
            "probability for {flag} must be in [0,1]"
        )));
    }
    Ok(parsed)
}

fn parse_percentage(value: &str, flag: &str) -> Result<f64> {
    let parsed = value.parse::<f64>().map_err(|_| {
        RaiError::InvalidConfiguration(format!("invalid percentage for {flag}: {value}"))
    })?;
    if !(0.0..=100.0).contains(&parsed) {
        return Err(RaiError::InvalidConfiguration(format!(
            "percentage for {flag} must be in [0,100]"
        )));
    }
    Ok(parsed)
}

fn parse_offsets(value: &str) -> Result<[i64; NODE_COUNT]> {
    let parsed = value
        .split(',')
        .map(|part| part.trim().parse::<i64>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| RaiError::InvalidConfiguration("invalid clock offset list".into()))?;
    parsed.try_into().map_err(|_| {
        RaiError::InvalidConfiguration("clock offset list must contain six values".into())
    })
}

fn parse_partition(value: &str) -> Result<PartitionWindow> {
    let mut fields = value.splitn(3, ':');
    let start = fields
        .next()
        .ok_or_else(|| RaiError::InvalidConfiguration("partition start is missing".into()))?;
    let end = fields
        .next()
        .ok_or_else(|| RaiError::InvalidConfiguration("partition end is missing".into()))?;
    let left = fields.next().ok_or_else(|| {
        RaiError::InvalidConfiguration("partition replica list is missing".into())
    })?;
    let start_ms = parse_u64(start, "--partition start")?;
    let end_ms = parse_u64(end, "--partition end")?;
    let replicas = left
        .split(',')
        .map(|part| part.trim().parse::<ReplicaId>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| {
            RaiError::InvalidConfiguration(
                "partition replica list must be comma-separated integers".into(),
            )
        })?;
    PartitionWindow::new(start_ms, end_ms, replicas)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet_config(seed: u64) -> TimedSimulationConfig {
        TimedSimulationConfig {
            epoch_start_ms: 10,
            epoch_close_ms: 180,
            next_epoch_delay_ms: 30,
            block_interval_ms: 40,
            tick_ms: 2,
            stop_ms: 2_000,
            clock_offsets_ms: [0; NODE_COUNT],
            log_level: LogLevel::Summary,
            log_file: None,
            print_logs: false,
            realtime: false,
            seed,
            latency_min_ms: 1,
            latency_max_ms: 8,
            reorder_window_ms: 6,
            drop_rate: 0.0,
            duplicate_rate: 0.0,
            close_round_timeout_ms: 60,
            partitions: Vec::new(),
            ..TimedSimulationConfig::default()
        }
    }

    #[test]
    fn same_seed_produces_same_report() {
        let first = run_timed_six_node_simulation(quiet_config(77)).unwrap();
        let second = run_timed_six_node_simulation(quiet_config(77)).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.client_accounts, 6);
        assert_eq!(first.client_ingress_replicas, 6);
        assert!(first.client_submissions >= first.client_accounts);
    }

    #[test]
    fn client_owns_multiple_accounts_but_cannot_sign_an_unowned_one() {
        let client = SimulationClient::deterministic([1, 2]);
        assert!(client.owns(1));
        assert!(client.owns(2));
        assert!(!client.owns(3));
        let genesis_one = client.genesis_account(1, 1_000, 1).unwrap();
        let genesis_two = client.genesis_account(2, 1_000, 2).unwrap();
        for (account, genesis) in [(1, genesis_one), (2, genesis_two)] {
            assert!(client
                .sign_block(Block {
                    slot: Slot::new(account, 1),
                    parent: genesis.hash(),
                    balance: 1_000,
                    representative: account,
                    sends: Vec::new(),
                    receives: Vec::new(),
                })
                .is_ok());
        }
        assert!(client
            .sign_block(Block {
                slot: Slot::new(3, 1),
                parent: Hash32::ZERO,
                balance: 1_000,
                representative: 3,
                sends: Vec::new(),
                receives: Vec::new(),
            })
            .is_err());
    }

    #[test]
    fn full_drop_prevents_progress_without_causing_a_safety_fault() {
        let mut config = quiet_config(88);
        config.drop_rate = 1.0;
        let report = run_timed_six_node_simulation(config).unwrap();
        assert!(report.network_dropped > 0);
        assert!(!report.converged);
        assert_eq!(report.safety_faults, 0);
    }

    #[test]
    fn partition_is_enforced_and_healing_does_not_split_safety() {
        let mut config = quiet_config(99);
        config.partitions = vec![PartitionWindow::new(0, 700, [1, 2, 3]).unwrap()];
        let report = run_timed_six_node_simulation(config).unwrap();
        assert!(report.network_partition_dropped > 0);
        assert!(report.distinct_close_hashes <= 1);
        assert_eq!(report.safety_faults, 0);
    }
}
