use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct ConfirmingSetConfig {
    pub batch_size: usize,
    /// Maximum number of dependent blocks to be stored in memory during processing
    pub max_blocks: usize,
    pub max_queued_notifications: usize,

    /// Maximum number of failed blocks to wait for requeuing
    pub max_deferred: usize,
    /// Max age of deferred blocks before they are dropped
    pub deferred_age_cutoff: Duration,
}

impl Default for ConfirmingSetConfig {
    fn default() -> Self {
        Self {
            batch_size: 256,
            max_blocks: 16 * 1024,
            max_queued_notifications: 8,
            max_deferred: 16 * 1024,
            deferred_age_cutoff: Duration::from_secs(15 * 60),
        }
    }
}