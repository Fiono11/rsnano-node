/// High-level user-facing config stored in `NodeConfig`.
#[derive(Clone, Debug, PartialEq)]
pub struct OptimisticSchedulerConfig {
    /// Minimum difference between confirmation frontier and account frontier to become a candidate for optimistic confirmation
    pub gap_threshold: u64,

    /// Maximum number of candidates stored in memory
    pub max_size: usize,

    /// Limit of optimistic elections as percentage of `active_elections_size`
    pub optimistic_limit_percentage: usize,

    /// Minimum time a candidate must wait before being scheduled
    pub activation_delay: std::time::Duration,
}

impl OptimisticSchedulerConfig {
    pub fn new() -> Self {
        Self {
            gap_threshold: 32,
            max_size: 1024 * 64,
            optimistic_limit_percentage: 10,
            activation_delay: std::time::Duration::from_secs(30),
        }
    }

    pub fn new_for_dev_network() -> Self {
        Self {
            activation_delay: std::time::Duration::from_secs(2),
            ..Self::new()
        }
    }
}

impl Default for OptimisticSchedulerConfig {
    fn default() -> Self {
        Self::new()
    }
}
