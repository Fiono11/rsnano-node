use super::{stopped_counter::StoppedCounter, vote_counter::VoteCounter};
use crate::consensus::{
    active_elections::AEC_STAT_KEY,
    election::{ConfirmationType, Election, ElectionBehavior},
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
}

impl AecStats {
    pub fn started(&mut self, behavior: ElectionBehavior) {
        self.started += 1;
        self.started_by_behavor[behavior as usize] += 1;
    }

    pub fn stopped(&mut self, election: &Election) {
        self.stopped_counter.stopped(election);
    }
}

impl StatsSource for AecStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(AEC_STAT_KEY, "loop", self.ticked);
        result.insert(AEC_STAT_KEY, "block_conflict", self.conflicts);
        result.insert(AEC_STAT_KEY, "started", self.started);

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
