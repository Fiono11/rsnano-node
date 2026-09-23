use anyhow::anyhow;
use clap::Parser;

use rsnano_types::PublicKey;

use crate::{
    domain::{RateSpec, Representatives, SpamStrategy, spam_logic::SpamSpec},
    setup::pr_key,
};

const DEFAULT_RATE: &str = "1+50@3s";

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

    /// RAI: every node advances to the next consensus epoch after this many
    /// decided elections of the current epoch (0: a single epoch)
    #[arg(long, default_value_t = 0, conflicts_with = "epoch_duration_ms")]
    pub epoch_terminated_elections: usize,

    /// RAI: every node ends its consensus epoch this long after the epoch's
    /// first election (0: no time limit)
    #[arg(long, default_value_t = 0)]
    pub epoch_duration_ms: u64,

    /// RAI: f, the Byzantine weight. This many principal representatives do
    /// not run a node; nanospam votes with their key at random instead, so
    /// they hold their share of the weight and do not follow the protocol
    #[arg(long, default_value_t = 0)]
    pub byzantine: usize,

    /// RAI: p, the weight the fast path may do without. This many principal
    /// representatives do not run a node and never vote; they still hold
    /// their share of the weight in the ledger
    #[arg(long, default_value_t = 0)]
    pub offline: usize,

    /// RAI: this many principal representatives run a node that casts no
    /// account votes: absent from the epoch's voting, present for the
    /// handoff, where they sign their report and vote in the close
    #[arg(long, default_value_t = 0)]
    pub silent: usize,
}

impl CliArgs {
    pub(crate) fn spam_spec(&self) -> anyhow::Result<SpamSpec> {
        Ok(SpamSpec {
            spam_strategy: self.strategy(),
            max_blocks: self.blocks.unwrap_or(0),
            rate: self.rate_spec()?,
            fork_probability: self.fork_probability(),
            track_confirmations: !self.unconfirmed,
            representatives: self.representatives(),
        })
    }

    /// RAI: the principal representatives of the run, which every account
    /// delegates to
    pub(crate) fn representatives(&self) -> Representatives {
        let reps: Vec<PublicKey> = (0..self.prs).map(|i| pr_key(i).public_key()).collect();
        Representatives::new(reps)
    }

    /// The representatives running a node, which nanospam talks to. The roles
    /// are taken from the end, so PR0 - the genesis representative, which
    /// funds the run and serves nanospam's RPC - is always honest: the last
    /// `byzantine` are Byzantine, the `offline` before them are offline.
    pub(crate) fn honest_prs(&self) -> usize {
        self.prs - self.byzantine - self.offline
    }

    /// RAI: whether the running representative casts account votes; the
    /// last `silent` of the running ones do not
    pub(crate) fn votes_in_accounts(&self, node_index: usize) -> bool {
        node_index + self.silent < self.honest_prs()
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.byzantine + self.offline >= self.prs {
            return Err(anyhow!(
                "{} of {} representatives would run no node; at least one must",
                self.byzantine + self.offline,
                self.prs
            ));
        }
        if self.silent >= self.honest_prs() {
            return Err(anyhow!(
                "{} of {} running representatives would cast no account vote; at least one must",
                self.silent,
                self.honest_prs()
            ));
        }
        Ok(())
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
