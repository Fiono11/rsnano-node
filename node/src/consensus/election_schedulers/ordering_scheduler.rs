#[derive(Clone, Debug, PartialEq)]
pub struct OrderingSchedulerConfig {
    pub committed_threshold: u32,
}

impl OrderingSchedulerConfig {
    pub fn new() -> Self {
        Self {
            committed_threshold: 1000,
        }
    }
}

impl Default for OrderingSchedulerConfig {
    fn default() -> Self {
        Self::new()
    }
}
