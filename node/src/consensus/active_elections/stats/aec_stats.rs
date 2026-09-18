use std::time::Duration;

use rsnano_nullable_clock::Timestamp;
use rsnano_utils::stats::{StatsCollection, StatsSource};
use strum::{EnumCount, IntoEnumIterator};

use super::{stopped_counter::StoppedCounter, vote_counter::VoteCounter};
use crate::consensus::{
    active_elections::AEC_STAT_KEY,
    election::{ConfirmationType, Election, ElectionBehavior, ElectionState},
};

#[derive(Default)]
pub(crate) struct AecStats {
    pub vote_counter: VoteCounter,
    stopped_counter: StoppedCounter,
    pub ticked: u64,
    pub conflicts: u64,
    pub started: u64,
    pub started_by_behavor: [u64; ElectionBehavior::COUNT],
    pub block_confirmations: [usize; ConfirmationType::COUNT],
    pub activate_failed_duplicate: u64,
    /// A low-prio election got replaced by one with a higher priority
    pub replaced: u64,
    pub activate_success: u64,
    /// Activation of a block failed, because it was recently confirmed
    pub activate_failed_confirmed: u64,
    /// Kudzu: a bucket exceeded its cap because every election in it holds votes
    pub over_capacity: u64,
    /// Kudzu: elections which collected a notarization certificate
    pub terminated: u64,
    /// RAI: epoch advances
    pub epochs_advanced: u64,
    /// RAI: epoch advances made to follow the representatives already ahead
    pub epochs_followed: u64,
    /// RAI: elections started in an epoch this node had already left
    pub stale_started: u64,
    /// RAI: elections of the current epoch started for a vote rather than a block
    pub started_for_vote: u64,
    /// Kudzu: elections which collected a timeout certificate before any notarization certificate
    pub timed_out: u64,
    /// Kudzu: elections which became settled before they were finalized
    pub settled: u64,
    /// Kudzu: elections finalized by a fast finalization certificate
    pub finalized_fast: u64,
    /// Kudzu: elections finalized by a finalization certificate
    pub finalized_final: u64,
    /// Evicted elections by state
    pub evicted_by_state: [u64; ElectionState::COUNT],
    /// Evicted elections which had votes
    pub evicted_with_votes: u64,
    /// Evicted elections which had more candidates than one
    pub evicted_forks: u64,
    /// Kudzu: election start -> first notarization certificate, elections with one candidate
    pub termination_nonfork: LatencyHistogram,
    /// Kudzu: election start -> first notarization certificate, elections with several candidates
    pub termination_fork: LatencyHistogram,
    /// Kudzu: election start -> finalization, elections with one candidate
    pub finalization_nonfork: LatencyHistogram,
    /// Kudzu: election start -> finalization, elections with several candidates
    pub finalization_fork: LatencyHistogram,
}

/// Count, sum, maximum, percentiles and a coarse histogram of durations in
/// milliseconds. The first `MAX_SAMPLES` durations are kept for the percentiles.
#[derive(Default)]
pub(crate) struct LatencyHistogram {
    count: u64,
    sum_ms: u64,
    max_ms: u64,
    buckets: [u64; LatencyHistogram::BOUNDS.len() + 1],
    samples: Vec<u32>,
}

impl LatencyHistogram {
    const BOUNDS: [u64; 8] = [50, 100, 200, 500, 1000, 2000, 5000, 15000];
    const MAX_SAMPLES: usize = 1 << 20;

    fn record(&mut self, duration: Duration) {
        let ms = duration.as_millis() as u64;
        self.count += 1;
        self.sum_ms += ms;
        self.max_ms = self.max_ms.max(ms);
        let bucket = Self::BOUNDS
            .iter()
            .position(|bound| ms <= *bound)
            .unwrap_or(Self::BOUNDS.len());
        self.buckets[bucket] += 1;
        if self.samples.len() < Self::MAX_SAMPLES {
            self.samples.push(ms.min(u32::MAX as u64) as u32);
        }
    }

    /// Nearest-rank percentiles of the recorded samples
    fn percentiles(&self) -> [(&'static str, u64); 3] {
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let at = |p: usize| -> u64 {
            if sorted.is_empty() {
                return 0;
            }
            let rank = (sorted.len() * p).div_ceil(100).max(1);
            sorted[rank - 1] as u64
        };
        [("p50_ms", at(50)), ("p95_ms", at(95)), ("p99_ms", at(99))]
    }

    fn collect_stats(&self, result: &mut StatsCollection, key: &'static str) {
        result.insert(key, "count", self.count);
        result.insert(key, "sum_ms", self.sum_ms);
        result.insert(key, "max_ms", self.max_ms);
        for (name, value) in self.percentiles() {
            result.insert(key, name, value);
        }
        for (i, bound) in Self::BOUNDS.iter().enumerate() {
            result.insert(key, LATENCY_BUCKET_NAMES[i], self.buckets[i]);
            let _ = bound;
        }
        result.insert(
            key,
            LATENCY_BUCKET_NAMES[Self::BOUNDS.len()],
            self.buckets[Self::BOUNDS.len()],
        );
    }
}

const LATENCY_BUCKET_NAMES: [&str; 9] = [
    "le_50ms", "le_100ms", "le_200ms", "le_500ms", "le_1s", "le_2s", "le_5s", "le_15s", "gt_15s",
];

impl AecStats {
    pub fn started(&mut self, behavior: ElectionBehavior) {
        self.started += 1;
        self.started_by_behavor[behavior as usize] += 1;
    }

    pub fn stopped(&mut self, election: &Election) {
        self.stopped_counter.stopped(election);
    }

    pub fn evicted(&mut self, election: &Election) {
        self.evicted_by_state[election.state() as usize] += 1;
        if election.vote_count() > 0 {
            self.evicted_with_votes += 1;
        }
        if election.block_count() > 1 {
            self.evicted_forks += 1;
        }
    }

    /// Kudzu: count how an election moved on after a vote was applied
    pub fn kudzu_transition(
        &mut self,
        old_state: ElectionState,
        election: &Election,
        was_in_block_tree: bool,
        now: Timestamp,
    ) {
        let new_state = election.state();
        let certificates = election.certificates();
        let fork = election.block_count() > 1;
        let age = election.start().elapsed(now);

        if !was_in_block_tree && certificates.has_block() {
            self.terminated += 1;
            if fork {
                self.termination_fork.record(age);
            } else {
                self.termination_nonfork.record(age);
            }
        }
        if old_state == new_state {
            return;
        }
        if !old_state.is_terminated() && new_state.is_terminated() && !certificates.has_block() {
            self.timed_out += 1;
        }
        if new_state == ElectionState::Settled {
            self.settled += 1;
        }
        if new_state == ElectionState::Confirmed {
            if certificates.fast.is_some() {
                self.finalized_fast += 1;
            } else {
                self.finalized_final += 1;
            }
            if fork {
                self.finalization_fork.record(age);
            } else {
                self.finalization_nonfork.record(age);
            }
        }
    }
}

impl StatsSource for AecStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(AEC_STAT_KEY, "loop", self.ticked);
        result.insert(AEC_STAT_KEY, "block_conflict", self.conflicts);
        result.insert(AEC_STAT_KEY, "started", self.started);
        result.insert(AEC_STAT_KEY, "terminated", self.terminated);
        result.insert(AEC_STAT_KEY, "epochs_advanced", self.epochs_advanced);
        result.insert(AEC_STAT_KEY, "epochs_followed", self.epochs_followed);
        result.insert(AEC_STAT_KEY, "stale_started", self.stale_started);
        result.insert(AEC_STAT_KEY, "started_for_vote", self.started_for_vote);
        result.insert(AEC_STAT_KEY, "timed_out", self.timed_out);
        result.insert(AEC_STAT_KEY, "settled", self.settled);
        result.insert(AEC_STAT_KEY, "finalized_fast", self.finalized_fast);
        result.insert(AEC_STAT_KEY, "finalized_final", self.finalized_final);
        result.insert(AEC_STAT_KEY, "evicted_with_votes", self.evicted_with_votes);
        result.insert(AEC_STAT_KEY, "evicted_forks", self.evicted_forks);
        self.termination_nonfork
            .collect_stats(result, "latency_termination_nonfork");
        self.termination_fork
            .collect_stats(result, "latency_termination_fork");
        self.finalization_nonfork
            .collect_stats(result, "latency_finalization_nonfork");
        self.finalization_fork
            .collect_stats(result, "latency_finalization_fork");
        for state in ElectionState::iter() {
            result.insert(
                "active_elections_evicted",
                state.as_str(),
                self.evicted_by_state[state as usize],
            );
        }

        for behavior in ElectionBehavior::iter() {
            result.insert(
                "active_elections_started",
                behavior.as_str(),
                self.started_by_behavor[behavior as usize],
            );
        }

        for conf_type in ConfirmationType::iter() {
            result.insert(
                "confirmation_observer",
                conf_type.as_str(),
                self.block_confirmations[conf_type as usize],
            );
        }

        result.insert(
            BUCKET_KEY,
            "activate_failed_duplicate",
            self.activate_failed_duplicate,
        );
        result.insert(BUCKET_KEY, "replaced", self.replaced);
        result.insert(BUCKET_KEY, "activate_success", self.activate_success);
        result.insert(
            BUCKET_KEY,
            "activate_failed_confirmed",
            self.activate_failed_confirmed,
        );
        result.insert(BUCKET_KEY, "over_capacity", self.over_capacity);

        self.vote_counter.collect_stats(result);
        self.stopped_counter.collect_stats(result);
    }
}

const BUCKET_KEY: &str = "election_bucket";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_percentiles_are_nearest_rank() {
        let mut histogram = LatencyHistogram::default();
        assert_eq!(histogram.percentiles()[0], ("p50_ms", 0));

        for ms in 1..=100 {
            histogram.record(Duration::from_millis(ms));
        }

        assert_eq!(
            histogram.percentiles(),
            [("p50_ms", 50), ("p95_ms", 95), ("p99_ms", 99)]
        );
        assert_eq!(histogram.count, 100);
        assert_eq!(histogram.max_ms, 100);
    }
}
