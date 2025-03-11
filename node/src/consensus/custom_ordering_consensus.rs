use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rsnano_core::{Amount, BlockHash, VoteWithWeightInfo};
use rsnano_ledger::{Election, ElectionBehavior, RepWeightCache};
use rsnano_stats::{Stats, StatType, DetailType};

/// Custom consensus algorithm configuration for ordering elections
#[derive(Clone, Debug, PartialEq)]
pub struct CustomOrderingConsensusConfig {
    /// Minimum weight threshold (percentage of online weight)
    pub min_weight_threshold_pct: f64,
    /// Minimum time a block must be in the election
    pub min_time_threshold_ms: u64,
    /// Maximum time to keep blocks in confirmation
    pub max_time_threshold_ms: u64,
    /// Minimum required number of distinct voters
    pub min_voters: usize,
}

impl Default for CustomOrderingConsensusConfig {
    fn default() -> Self {
        Self {
            min_weight_threshold_pct: 50.0, // Default to 50% threshold
            min_time_threshold_ms: 1000,    // 1 second
            max_time_threshold_ms: 10000,   // 10 seconds
            min_voters: 5,                  // Require at least 5 voters
        }
    }
}

pub struct CustomOrderingConsensus {
    config: CustomOrderingConsensusConfig,
    rep_weights: Arc<RepWeightCache>,
    stats: Arc<Stats>,
    // Track when blocks were first seen in an election
    block_timestamps: Mutex<HashMap<BlockHash, Instant>>,
}

impl CustomOrderingConsensus {
    pub fn new(
        config: CustomOrderingConsensusConfig,
        rep_weights: Arc<RepWeightCache>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            config,
            rep_weights,
            stats,
            block_timestamps: Mutex::new(HashMap::new()),
        }
    }

    /// Record a block being added to an ordering election
    pub fn record_block(&self, hash: BlockHash) {
        let mut timestamps = self.block_timestamps.lock().unwrap();
        // Only insert if not already present
        timestamps.entry(hash).or_insert_with(Instant::now);
    }

    /// Clean up timestamps for a block that's been removed from elections
    pub fn remove_block(&self, hash: &BlockHash) {
        let mut timestamps = self.block_timestamps.lock().unwrap();
        timestamps.remove(hash);
    }

    /// Determine if the given election's winner meets our custom consensus criteria
    pub fn has_consensus(&self, election: &Election) -> bool {
        // Only apply custom consensus to ordering elections
        if election.behavior != ElectionBehavior::Ordering {
            return false;
        }

        if let Some(winner_hash) = election.winner_hash() {
            // Check if the block has been in the election long enough
            let timestamps = self.block_timestamps.lock().unwrap();
            if let Some(timestamp) = timestamps.get(&winner_hash) {
                let elapsed = timestamp.elapsed();
                
                // If it's been too long, just confirm it (timeout)
                if elapsed > Duration::from_millis(self.config.max_time_threshold_ms) {
                    self.stats.inc(StatType::CustomConsensus, DetailType::Timeout);
                    return true;
                }
                
                // Check minimum time threshold
                if elapsed < Duration::from_millis(self.config.min_time_threshold_ms) {
                    return false;
                }
                
                // Gather votes with weights to check weight and voter count
                let votes_with_weights = election.votes_with_weight(&self.rep_weights);
                
                // Check voter count
                if votes_with_weights.len() < self.config.min_voters {
                    return false;
                }
                
                // Calculate total weight and winner's weight
                let total_weight = self.rep_weights.online_stake();
                let mut winner_weight = Amount::zero();
                
                // Sum up all votes for the winner
                for vote in &votes_with_weights {
                    if vote.hash == winner_hash {
                        winner_weight += vote.weight;
                    }
                }
                
                // Check if the winner has enough weight
                let winner_percent = (winner_weight.as_u128() as f64 / total_weight.as_u128() as f64) * 100.0;
                if winner_percent >= self.config.min_weight_threshold_pct {
                    self.stats.inc(StatType::CustomConsensus, DetailType::Confirmed);
                    return true;
                }
            }
        }
        
        false
    }
} 