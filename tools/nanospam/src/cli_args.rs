use crate::domain::{RateSpec, SpamStrategy, spam_logic::SpamSpec};
use clap::Parser;
use std::path::PathBuf;

const DEFAULT_RATE: &str = "1+50@3s";

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub(crate) struct CliArgs {
    /// Directory for generated node data (defaults to ~/NanoSpam)
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Number of principal representatives
    #[arg(long, default_value_t = 1)]
    pub prs: usize,

    /// Only create the node config files and set up the wallets, then exit
    #[arg(long, default_value_t = false)]
    pub setup_only: bool,

    /// Attach to an already running node that was set up by a previous nanospam run
    #[arg(long, default_value_t = false)]
    pub attach: bool,

    #[arg(long)]
    /// Block rate in the form "1000+50@3s" or "1000"
    pub rate: Option<String>,

    #[arg(long)]
    /// Number of blocks to publish
    pub blocks: Option<usize>,

    /// Don't wait for a block to get confirmed before publishing the next block
    #[arg(long, default_value_t = false)]
    pub unconfirmed: bool,

    /// Query frontiers of the spam accounts before starting spam
    #[arg(long, default_value_t = false)]
    pub sync: bool,

    /// Only publish change blocks. This requires --sync
    #[arg(long, default_value_t = false)]
    pub change: bool,

    /// Run the C++ nano_node (must be in $PATH)
    #[arg(long, default_value_t = false)]
    pub cpp: bool,

    /// Use RocksDB (works only for nano_node)
    #[arg(long, default_value_t = false)]
    pub rocksdb: bool,

    /// Disable sending a high priority block every 10s
    #[arg(long, default_value_t = false)]
    pub no_prio: bool,

    /// Limit confirmations per second
    #[arg(long, default_value_t = 0)]
    pub cps_limit: u32,

    /// Don't kill the node processes on exit
    #[arg(long, default_value_t = false)]
    pub no_kill: bool,

    /// Don't republish delayed blocks after 10 seconds
    #[arg(long, default_value_t = false)]
    pub no_republish: bool,

    /// Maximum number of individual accounts to use to produce blocks
    #[arg(long, default_value_t = 500000)]
    pub accounts: usize,

    /// Randomly drop publish messages
    #[arg(long, default_value_t = 0)]
    pub drop_percentage: usize,

    /// Percentage of blocks that should have forks
    #[arg(long, default_value_t = 0)]
    pub fork_percentage: usize,

    /// Number of nodes that receive the fork (0 sends it to every second node)
    #[arg(long, default_value_t = 0)]
    pub fork_recipients: usize,

    /// Seconds to collect termination and finalization certificates after publishing
    #[arg(long, default_value_t = 0)]
    pub timeout: u64,

    /// Print only the final performance summary when tracing is disabled
    #[arg(long, default_value_t = false)]
    pub final_results_only: bool,

    /// Number of RAI epochs to schedule (zero disables epoch transitions)
    #[cfg(feature = "rai_protocol")]
    #[arg(long, default_value_t = 0)]
    pub epochs: usize,

    /// Duration of each RAI epoch in seconds
    #[cfg(feature = "rai_protocol")]
    #[arg(long, default_value_t = 35)]
    pub epoch_duration: u64,
}

impl CliArgs {
    pub(crate) fn spam_spec(&self) -> anyhow::Result<SpamSpec> {
        Ok(SpamSpec {
            spam_strategy: self.strategy(),
            max_blocks: self.blocks.unwrap_or(0),
            rate: self.rate_spec()?,
            fork_probability: self.fork_probability(),
            track_confirmations: !self.unconfirmed,
            #[cfg(feature = "rai_protocol")]
            expected_epochs: self.epochs,
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
        !self.attach && !self.sync
    }

    fn strategy(&self) -> SpamStrategy {
        if self.change {
            SpamStrategy::Change
        } else {
            SpamStrategy::SendReceive
        }
    }

    fn rate_spec(&self) -> Result<RateSpec, anyhow::Error> {
        let rate: RateSpec = self.rate.as_deref().unwrap_or(DEFAULT_RATE).parse()?;
        Ok(rate)
    }
}
