/// Low-level config consumed by `OptimisticScheduler` with all values resolved to absolute numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct OptimisticSchedulerParams {
    pub gap_threshold: u64,
    pub max_size: usize,
    /// Maximum number of simultaneous optimistic elections (absolute value)
    pub max_elections: usize,
}

/// High-level user-facing config stored in `NodeConfig`.
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
