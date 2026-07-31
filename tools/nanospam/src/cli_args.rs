use crate::domain::{RateSpec, SpamStrategy, spam_logic::SpamSpec};
use clap::Parser;

const DEFAULT_RATE: &str = "1+50@3s";
const DEFAULT_RAI_EPOCH_DURATION_MS: u64 = 30_000;
const DEFAULT_RAI_TICK_INTERVAL_MS: u64 = 100;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub(crate) struct CliArgs {
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

    /// RAI epoch duration for generated rsnano configs, in milliseconds
    #[arg(long, value_parser = parse_nonzero_duration)]
    pub rai_epoch_duration_ms: Option<u64>,

    /// RAI close-loop tick interval for generated rsnano configs, in milliseconds
    #[arg(long, value_parser = parse_nonzero_duration)]
    pub rai_tick_interval_ms: Option<u64>,
}

impl CliArgs {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
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
    fn parses_rai_timing_options() {
        let args = CliArgs::try_parse_from([
            "nanospam",
            "--rai-epoch-duration-ms",
            "5000",
            "--rai-tick-interval-ms",
            "100",
        ])
        .unwrap();

        assert_eq!(args.rai_epoch_duration_ms, Some(5000));
        assert_eq!(args.rai_tick_interval_ms, Some(100));
        args.validate().unwrap();
    }

    #[test]
    fn rejects_zero_rai_durations() {
        assert!(CliArgs::try_parse_from(["nanospam", "--rai-epoch-duration-ms", "0"]).is_err());
        assert!(CliArgs::try_parse_from(["nanospam", "--rai-tick-interval-ms", "0"]).is_err());
    }

    #[test]
    fn rejects_tick_interval_larger_than_epoch_duration() {
        let args = CliArgs::try_parse_from([
            "nanospam",
            "--rai-epoch-duration-ms",
            "99",
            "--rai-tick-interval-ms",
            "100",
        ])
        .unwrap();

        assert!(args.validate().is_err());
    }

    #[test]
    fn validates_rai_timing_against_defaults() {
        let short_epoch =
            CliArgs::try_parse_from(["nanospam", "--rai-epoch-duration-ms", "99"]).unwrap();
        assert!(short_epoch.validate().is_err());

        let long_tick =
            CliArgs::try_parse_from(["nanospam", "--rai-tick-interval-ms", "30001"]).unwrap();
        assert!(long_tick.validate().is_err());
    }
}
