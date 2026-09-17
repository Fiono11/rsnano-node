use super::{stopped_counter::StoppedCounter, vote_counter::VoteCounter};
use crate::consensus::{
    active_elections::AEC_STAT_KEY,
    election::{Certificates, ConfirmationType, Election, ElectionBehavior, ElectionState},
};
use rsnano_utils::stats::{StatsCollection, StatsSource};
use strum::{EnumCount, IntoEnumIterator};

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
    /// Kudzu: elections which collected a notarization certificate
    pub terminated: u64,
    /// Kudzu: elections which collected a timeout certificate before any notarization certificate
    pub timed_out: u64,
    /// Kudzu: elections which became settled before they were finalized
    pub settled: u64,
    /// Kudzu: elections finalized by a fast finalization certificate
    pub finalized_fast: u64,
    /// Kudzu: elections finalized by a finalization certificate
    pub finalized_final: u64,
}

impl AecStats {
    pub fn started(&mut self, behavior: ElectionBehavior) {
        self.started += 1;
        self.started_by_behavor[behavior as usize] += 1;
    }

    pub fn stopped(&mut self, election: &Election) {
        self.stopped_counter.stopped(election);
    }

    /// Kudzu: count how an election moved on after a vote was applied
    pub fn kudzu_transition(
        &mut self,
        old_state: ElectionState,
        new_state: ElectionState,
        certificates: &Certificates,
    ) {
        if old_state == new_state {
            return;
        }
        if !old_state.is_terminated() && new_state.is_terminated() {
            if certificates.has_block() {
                self.terminated += 1;
            } else {
                self.timed_out += 1;
            }
        } else if old_state == ElectionState::TimedOut && certificates.has_block() {
            self.terminated += 1;
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
        }
    }
}

impl StatsSource for AecStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(AEC_STAT_KEY, "loop", self.ticked);
        result.insert(AEC_STAT_KEY, "block_conflict", self.conflicts);
        result.insert(AEC_STAT_KEY, "started", self.started);
        result.insert(AEC_STAT_KEY, "terminated", self.terminated);
        result.insert(AEC_STAT_KEY, "timed_out", self.timed_out);
        result.insert(AEC_STAT_KEY, "settled", self.settled);
        result.insert(AEC_STAT_KEY, "finalized_fast", self.finalized_fast);
        result.insert(AEC_STAT_KEY, "finalized_final", self.finalized_final);

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

        self.vote_counter.collect_stats(result);
        self.stopped_counter.collect_stats(result);
    }
}

const BUCKET_KEY: &str = "election_bucket";
