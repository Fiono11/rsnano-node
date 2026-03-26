/// Low-level config consumed by `OptimisticScheduler` with all values resolved to absolute numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct OptimisticSchedulerParams {
    pub gap_threshold: u64,
    pub max_candidates: usize,
    /// Maximum number of simultaneous optimistic elections (absolute value)
    pub max_elections: usize,
}
