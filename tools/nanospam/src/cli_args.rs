use crate::domain::{RateSpec, SpamStrategy, spam_logic::SpamSpec};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

const DEFAULT_RATE: &str = "1+50@3s";
const DEFAULT_RAI_EPOCH_DURATION_MS: u64 = 30_000;
const DEFAULT_RAI_TICK_INTERVAL_MS: u64 = 100;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub(crate) struct CommandLine {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Prepare funded node ledgers and stop all setup nodes.
    Setup(SetupArgs),
    /// Start a prepared network at RAI epoch 0 and run a workload.
    Run(RunArgs),
}

#[derive(Args, Debug, Clone)]
struct NetworkArgs {
    /// Directory containing the prepared PR node directories
    #[arg(long)]
    data_dir: PathBuf,

    /// Number of principal representatives
    #[arg(long, default_value_t = 1)]
    prs: usize,

    /// Maximum number of individual accounts used to produce blocks
    #[arg(long, default_value_t = 500000)]
    accounts: usize,

    /// Fund every prepared account instead of only one stake account per PR
    #[arg(long, default_value_t = false)]
    fund_all_accounts: bool,

    /// Run the C++ nano_node (must be in PATH)
    #[arg(long, default_value_t = false)]
    cpp: bool,

    /// Use RocksDB (works only for nano_node)
    #[arg(long, default_value_t = false)]
    rocksdb: bool,

    /// Limit confirmations per second
    #[arg(long, default_value_t = 0)]
    cps_limit: u32,

    /// RAI epoch duration for generated rsnano configs, in milliseconds
    #[arg(long, value_parser = parse_nonzero_duration)]
    rai_epoch_duration_ms: Option<u64>,

    /// RAI close-loop tick interval for generated rsnano configs, in milliseconds
    #[arg(long, value_parser = parse_nonzero_duration)]
    rai_tick_interval_ms: Option<u64>,

    /// Maximum total runtime for setup or run, in seconds
    #[arg(long, default_value_t = 300, value_parser = parse_nonzero_duration)]
    global_timeout_secs: u64,
}

#[derive(Args, Debug)]
struct SetupArgs {
    #[command(flatten)]
    network: NetworkArgs,
}

#[derive(Args, Debug)]
struct RunArgs {
    #[command(flatten)]
    network: NetworkArgs,

    /// Block rate in the form "1000+50@3s" or "1000"
    #[arg(long)]
    rate: Option<String>,

    /// Number of blocks to publish
    #[arg(long)]
    blocks: usize,

    /// Keep RAI epoch 0 open and disable the epoch close protocol
    #[arg(long, default_value_t = false)]
    single_rai_epoch: bool,

    /// Don't wait for a block to get confirmed before publishing the next block
    #[arg(long, default_value_t = false)]
    unconfirmed: bool,

    /// Only publish change blocks
    #[arg(long, default_value_t = false)]
    change: bool,

    /// Publish at most one change block from each prepared account
    #[arg(long, default_value_t = false)]
    one_block_per_account: bool,

    /// Publish exactly one send block from each prepared account
    #[arg(long, default_value_t = false)]
    one_send_per_account: bool,

    /// Disable sending a high priority block every 10s
    #[arg(long, default_value_t = false)]
    no_prio: bool,

    /// Don't kill the node processes on exit
    #[arg(long, default_value_t = false)]
    no_kill: bool,

    /// Don't republish delayed blocks after 10 seconds
    #[arg(long, default_value_t = false)]
    no_republish: bool,

    /// Randomly drop publish messages
    #[arg(long, default_value_t = 0)]
    drop_percentage: usize,

    /// Percentage of blocks that should have forks
    #[arg(long, default_value_t = 0)]
    fork_percentage: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Setup,
    Run,
}

#[derive(Debug)]
pub(crate) struct CliArgs {
    pub mode: Mode,
    pub data_dir: Option<PathBuf>,
    pub prs: usize,
    pub rate: Option<String>,
    pub blocks: Option<usize>,
    pub single_rai_epoch: bool,
    pub unconfirmed: bool,
    pub change: bool,
    pub one_block_per_account: bool,
    pub one_send_per_account: bool,
    pub cpp: bool,
    pub rocksdb: bool,
    pub no_prio: bool,
    pub cps_limit: u32,
    pub no_kill: bool,
    pub no_republish: bool,
    pub accounts: usize,
    pub fund_all_accounts: bool,
    pub drop_percentage: usize,
    pub fork_percentage: usize,
    pub rai_epoch_duration_ms: Option<u64>,
    pub rai_tick_interval_ms: Option<u64>,
    pub global_timeout_secs: u64,
}

impl CommandLine {
    pub(crate) fn into_args(self) -> CliArgs {
        match self.command {
            Command::Setup(args) => CliArgs::from_network(Mode::Setup, args.network),
            Command::Run(args) => {
                let mut result = CliArgs::from_network(Mode::Run, args.network);
                result.rate = args.rate;
                result.blocks = Some(args.blocks);
                result.single_rai_epoch = args.single_rai_epoch;
                result.unconfirmed = args.unconfirmed;
                result.change = args.change;
                result.one_block_per_account = args.one_block_per_account;
                result.one_send_per_account = args.one_send_per_account;
                result.no_prio = args.no_prio;
                result.no_kill = args.no_kill;
                result.no_republish = args.no_republish;
                result.drop_percentage = args.drop_percentage;
                result.fork_percentage = args.fork_percentage;
                result
            }
        }
    }
}

impl CliArgs {
    fn from_network(mode: Mode, args: NetworkArgs) -> Self {
        Self {
            mode,
            data_dir: Some(args.data_dir),
            prs: args.prs,
            rate: None,
            blocks: None,
            single_rai_epoch: false,
            unconfirmed: false,
            change: false,
            one_block_per_account: false,
            one_send_per_account: false,
            cpp: args.cpp,
            rocksdb: args.rocksdb,
            no_prio: false,
            cps_limit: args.cps_limit,
            no_kill: false,
            no_republish: false,
            accounts: args.accounts,
            fund_all_accounts: args.fund_all_accounts,
            drop_percentage: 0,
            fork_percentage: 0,
            rai_epoch_duration_ms: args.rai_epoch_duration_ms,
            rai_tick_interval_ms: args.rai_tick_interval_ms,
            global_timeout_secs: args.global_timeout_secs,
        }
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.single_rai_epoch
            && (self.rai_epoch_duration_ms.is_some() || self.rai_tick_interval_ms.is_some())
        {
            anyhow::bail!(
                "--single-rai-epoch cannot be combined with --rai-epoch-duration-ms or --rai-tick-interval-ms"
            );
        }
        let epoch_duration = self
            .rai_epoch_duration_ms
            .unwrap_or(DEFAULT_RAI_EPOCH_DURATION_MS);
        let tick_interval = self
            .rai_tick_interval_ms
            .unwrap_or(DEFAULT_RAI_TICK_INTERVAL_MS);
        if tick_interval > epoch_duration {
            anyhow::bail!(
                "--rai-tick-interval-ms must be less than or equal to --rai-epoch-duration-ms"
            );
        }
        if self.prs == 0 || self.accounts < self.prs {
            anyhow::bail!("--prs must be nonzero and --accounts must be at least --prs");
        }
        if self.one_block_per_account && self.blocks.unwrap_or(0) > self.accounts {
            anyhow::bail!("--one-block-per-account requires --blocks <= --accounts");
        }
        if self.one_send_per_account && self.blocks.unwrap_or(0) != self.accounts {
            anyhow::bail!("--one-send-per-account requires --blocks == --accounts");
        }
        if self.one_send_per_account && !self.fund_all_accounts {
            anyhow::bail!("--one-send-per-account requires --fund-all-accounts");
        }
        if self.one_send_per_account && self.one_block_per_account {
            anyhow::bail!(
                "--one-send-per-account and --one-block-per-account are mutually exclusive"
            );
        }
        if self.one_block_per_account && self.prs < 2 {
            anyhow::bail!("--one-block-per-account requires --prs >= 2");
        }
        Ok(())
    }

    pub(crate) fn spam_spec(&self) -> anyhow::Result<SpamSpec> {
        Ok(SpamSpec {
            spam_strategy: self.strategy(),
            max_blocks: self.blocks.unwrap_or(0),
            rate: self.rate_spec()?,
            fork_probability: self.fork_probability(),
            track_confirmations: !self.unconfirmed,
        })
    }

    pub(crate) fn high_prio_check(&self) -> bool {
        !self.no_prio
    }
    pub(crate) fn kill_nodes(&self) -> bool {
        !self.no_kill
    }
    pub(crate) fn fork_probability(&self) -> f64 {
        self.fork_percentage as f64 / 100.0
    }
    pub(crate) fn drop_probability(&self) -> f64 {
        self.drop_percentage as f64 / 100.0
    }
    pub(crate) fn set_up_new_nodes(&self) -> bool {
        self.mode == Mode::Setup
    }
    pub(crate) fn sync(&self) -> bool {
        self.mode == Mode::Run
    }
    pub(crate) fn setup_only(&self) -> bool {
        self.mode == Mode::Setup
    }

    fn strategy(&self) -> SpamStrategy {
        if self.one_send_per_account {
            SpamStrategy::OneSendPerAccount
        } else if self.one_block_per_account {
            SpamStrategy::OneChangePerAccount
        } else if self.change {
            SpamStrategy::Change
        } else {
            SpamStrategy::SendReceive
        }
    }

    fn rate_spec(&self) -> Result<RateSpec, anyhow::Error> {
        Ok(self.rate.as_deref().unwrap_or(DEFAULT_RATE).parse()?)
    }
}

fn parse_nonzero_duration(value: &str) -> Result<u64, String> {
    let duration = value
        .parse::<u64>()
        .map_err(|_| format!("invalid duration: {value}"))?;
    if duration == 0 {
        return Err("duration must be nonzero".to_string());
    }
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setup_command() {
        let args = CommandLine::try_parse_from([
            "nanospam",
            "setup",
            "--data-dir",
            "/tmp/rai",
            "--prs",
            "6",
            "--accounts",
            "6",
            "--global-timeout-secs",
            "1800",
        ])
        .unwrap()
        .into_args();
        assert_eq!(args.mode, Mode::Setup);
        assert_eq!(args.prs, 6);
        assert_eq!(args.global_timeout_secs, 1800);
        args.validate().unwrap();
    }

    #[test]
    fn parses_run_timing_options() {
        let args = CommandLine::try_parse_from([
            "nanospam",
            "run",
            "--data-dir",
            "/tmp/rai",
            "--blocks",
            "200",
            "--rai-epoch-duration-ms",
            "5000",
            "--rai-tick-interval-ms",
            "100",
        ])
        .unwrap()
        .into_args();
        assert_eq!(args.mode, Mode::Run);
        assert_eq!(args.rai_epoch_duration_ms, Some(5000));
        assert!(args.spam_spec().unwrap().track_confirmations);
        args.validate().unwrap();
    }

    #[test]
    fn parses_single_rai_epoch() {
        let args = CommandLine::try_parse_from([
            "nanospam",
            "run",
            "--data-dir",
            "/tmp/rai",
            "--blocks",
            "200",
            "--single-rai-epoch",
        ])
        .unwrap()
        .into_args();

        assert!(args.single_rai_epoch);
        args.validate().unwrap();
    }

    #[test]
    fn single_rai_epoch_rejects_timing_options() {
        let args = CommandLine::try_parse_from([
            "nanospam",
            "run",
            "--data-dir",
            "/tmp/rai",
            "--blocks",
            "1",
            "--single-rai-epoch",
            "--rai-epoch-duration-ms",
            "5000",
        ])
        .unwrap()
        .into_args();

        assert!(args.validate().is_err());
    }

    #[test]
    fn rejects_zero_rai_durations() {
        assert!(
            CommandLine::try_parse_from([
                "nanospam",
                "run",
                "--data-dir",
                "/tmp/rai",
                "--blocks",
                "1",
                "--rai-epoch-duration-ms",
                "0"
            ])
            .is_err()
        );
    }

    #[test]
    fn rejects_tick_interval_larger_than_epoch_duration() {
        let args = CommandLine::try_parse_from([
            "nanospam",
            "run",
            "--data-dir",
            "/tmp/rai",
            "--blocks",
            "1",
            "--rai-epoch-duration-ms",
            "99",
            "--rai-tick-interval-ms",
            "100",
        ])
        .unwrap()
        .into_args();
        assert!(args.validate().is_err());
    }

    #[test]
    fn parses_one_send_per_account_workload() {
        let args = CommandLine::try_parse_from([
            "nanospam",
            "run",
            "--data-dir",
            "/tmp/rai",
            "--prs",
            "6",
            "--accounts",
            "100",
            "--fund-all-accounts",
            "--blocks",
            "100",
            "--one-send-per-account",
        ])
        .unwrap()
        .into_args();

        args.validate().unwrap();
        assert_eq!(args.strategy(), SpamStrategy::OneSendPerAccount);
    }

    #[test]
    fn one_change_per_account_requires_two_prs() {
        let args = CommandLine::try_parse_from([
            "nanospam",
            "run",
            "--data-dir",
            "/tmp/rai",
            "--accounts",
            "1",
            "--blocks",
            "1",
            "--one-block-per-account",
        ])
        .unwrap()
        .into_args();

        assert!(args.validate().is_err());
    }
}
