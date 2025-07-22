use std::cmp::{max, min};

#[derive(Clone, Debug, PartialEq)]
pub struct MessageProcessorConfig {
    pub threads: usize,
    pub max_queue: usize,
}

impl MessageProcessorConfig {
    pub fn new(parallelism: usize) -> Self {
        Self {
            threads: min(2, max(parallelism / 4, 1)),
            max_queue: 64,
        }
    }
}