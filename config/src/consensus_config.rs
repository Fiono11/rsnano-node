use std::{cmp::{max, min}, time::Duration};

pub const DEFAULT_STALE_THRESHOLD: Duration = Duration::from_secs(60);
pub const DEFAULT_MAX_FORKS_PER_ROOT: usize = 10;
pub const DEFAULT_MAX_LEN: usize = 1024 * 16;
pub const DEFAULT_MAX_QUEUE: usize = 1024 * 16;

#[derive(Clone, Debug, PartialEq)]
pub struct HintedSchedulerConfig {
    pub check_interval: Duration,
    pub block_cooldown: Duration,
    pub hinting_threshold_percent: u32,
    pub vacancy_threshold_percent: u32,
    /// Limit of hinted elections as percentage of `active_elections_size`
    pub hinted_limit_percentage: usize,
}

impl HintedSchedulerConfig {
    pub fn default_for_dev_network() -> Self {
        Self {
            check_interval: Duration::from_millis(100),
            block_cooldown: Duration::from_millis(100),
            ..Default::default()
        }
    }
}

impl Default for HintedSchedulerConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_millis(1000),
            block_cooldown: Duration::from_millis(5000),
            hinting_threshold_percent: 10,
            vacancy_threshold_percent: 20,
            hinted_limit_percentage: 20,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptimisticSchedulerConfig {
    /// Minimum difference between confirmation frontier and account frontier to become a candidate for optimistic confirmation
    pub gap_threshold: u64,

    /// Maximum number of candidates stored in memory
    pub max_size: usize,

    /// Limit of optimistic elections as percentage of `active_elections_size`
    pub optimistic_limit_percentage: usize,
}

impl OptimisticSchedulerConfig {
    pub fn new() -> Self {
        Self {
            gap_threshold: 32,
            max_size: 1024 * 64,
            optimistic_limit_percentage: 10,
        }
    }
}

impl Default for OptimisticSchedulerConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PriorityBucketConfig {
    /// Maximum number of blocks to sort by priority per bucket.
    pub max_blocks: usize,

    /// Number of guaranteed slots per bucket available for election activation.
    pub reserved_elections: usize,

    /// Maximum number of slots per bucket available for election activation if the active election count is below the configured limit. (node.active_elections.size)
    pub max_elections: usize,
}

impl Default for PriorityBucketConfig {
    fn default() -> Self {
        Self {
            max_blocks: 1024 * 8,
            reserved_elections: 100,
            max_elections: 150,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestAggregatorConfig {
    pub threads: usize,
    pub max_queue: usize,
    pub batch_size: usize,
}

impl RequestAggregatorConfig {
    pub fn new(parallelism: usize) -> Self {
        Self {
            threads: max(1, min(parallelism / 2, 4)),
            max_queue: 128,
            batch_size: 16,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VoteCacheConfig {
    pub max_size: usize,
    pub max_voters: usize,
    pub age_cutoff: Duration,
}

impl Default for VoteCacheConfig {
    fn default() -> Self {
        Self {
            max_size: 1024 * 64,
            max_voters: 64,
            age_cutoff: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VoteProcessorConfig {
    pub max_pr_queue: usize,
    pub max_non_pr_queue: usize,
    pub pr_priority: usize,
    pub threads: usize,
    pub batch_size: usize,
    pub max_triggered: usize,
}

impl VoteProcessorConfig {
    pub fn new(parallelism: usize) -> Self {
        Self {
            max_pr_queue: 256,
            max_non_pr_queue: 32,
            pr_priority: 3,
            threads: max(1, min(4, parallelism / 2)),
            batch_size: 1024,
            max_triggered: 16384,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveElectionsConfig {
    /// Maximum number of simultaneous active elections (AEC size)
    pub max_elections: usize,
    /// Maximum cache size for recently_confirmed
    pub confirmation_cache: usize,
}

impl Default for ActiveElectionsConfig {
    fn default() -> Self {
        Self {
            max_elections: 5000,
            confirmation_cache: 65536,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RebroadcastHistoryConfig {
    /// Minimum amount of time between rebroadcasts for the same hash from the same representative
    pub rebroadcast_min_gap: Duration,

    /// Maximum number of representatives to track rebroadcasts for
    pub max_representatives: usize,

    /// Maximum number of recently broadcast hashes to keep per representative
    pub max_blocks_per_rep: usize,
}

impl RebroadcastHistoryConfig {
    pub const DEFAULT_MAX_REPS: usize = 100;
    pub const DEFAULT_MAX_BLOCKS_PER_REP: usize = 1024 * 32;
    pub const DEFAULT_REBROADCAST_MIN_GAP: Duration = Duration::from_secs(90);
}

impl Default for RebroadcastHistoryConfig {
    fn default() -> Self {
        Self {
            rebroadcast_min_gap: Self::DEFAULT_REBROADCAST_MIN_GAP,
            max_representatives: Self::DEFAULT_MAX_REPS,
            max_blocks_per_rep: Self::DEFAULT_MAX_BLOCKS_PER_REP,
        }
    }
}