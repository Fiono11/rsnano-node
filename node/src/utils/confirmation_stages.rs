use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rsnano_types::{BlockHash, UnixMillisTimestamp};

use crate::consensus::election::ConfirmedElection;

/// Diagnostic: where the blocks cemented within one second spent their
/// time, from ledger insertion to cementation. One summary line per second
/// tells whether a slow period is spent before the election starts, in the
/// first-vote round, in the final-vote round (including an epoch gate), or
/// in cementation.
#[derive(Default)]
pub(crate) struct ConfirmationStages {
    second: Option<u64>,
    samples: Vec<StageSample>,
    forks: usize,
    dependents: usize,
    no_election: usize,
}

/// The stages of one block cemented with its own single-candidate election
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StageSample {
    /// Ledger insertion to election start
    pub queued_ms: u64,
    /// Election start to notarization certificate
    pub to_nc_ms: Option<u64>,
    /// Notarization certificate to finality
    pub nc_to_final_ms: Option<u64>,
    /// Election start to finality being allowed; zero unless gated
    pub gate_ms: u64,
    /// Finality to cementation
    pub cement_ms: u64,
    /// Finality to the hand-over to the confirming set
    pub handoff_ms: Option<u64>,
    /// Hand-over to the cemented block coming back for its dependents
    pub cementing_ms: Option<u64>,
    /// Cemented block back to its confirmation being published
    pub publish_ms: Option<u64>,
    /// Ledger insertion to cementation
    pub total_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StageRecord {
    Sample(StageSample),
    /// The election had more than one candidate
    Fork,
    /// Cemented as the dependency of another election's winner
    Dependent,
    /// Cemented without an election, by a checkpoint for instance
    NoElection,
}

impl StageRecord {
    /// The stages of `block`, inserted into the ledger at `inserted`,
    /// cemented at `cemented_ms` on behalf of `election`
    pub fn new(
        block: &BlockHash,
        inserted: UnixMillisTimestamp,
        election: &ConfirmedElection,
        cemented_ms: u64,
    ) -> Self {
        if election.winner.hash() != *block {
            return Self::Dependent;
        }
        if election.block_count > 1 {
            return Self::Fork;
        }
        if election.election_duration.is_zero() && election.notarized_after.is_none() {
            return Self::NoElection;
        }
        let inserted_ms = inserted.as_u64();
        let finalized_ms = unix_ms(election.election_end);
        let duration = millis(election.election_duration);
        let started_ms = finalized_ms.saturating_sub(duration);
        let to_nc_ms = election.notarized_after.map(millis);
        let handed_ms = election.handed_to_cementing.map(unix_ms);
        let seen_ms = election.cemented_seen.map(unix_ms);
        Self::Sample(StageSample {
            queued_ms: started_ms.saturating_sub(inserted_ms),
            to_nc_ms,
            nc_to_final_ms: to_nc_ms.map(|to_nc| duration.saturating_sub(to_nc)),
            gate_ms: election.eligible_after.map(millis).unwrap_or(duration),
            cement_ms: cemented_ms.saturating_sub(finalized_ms),
            handoff_ms: handed_ms.map(|handed| handed.saturating_sub(finalized_ms)),
            cementing_ms: handed_ms
                .zip(seen_ms)
                .map(|(handed, seen)| seen.saturating_sub(handed)),
            publish_ms: seen_ms.map(|seen| cemented_ms.saturating_sub(seen)),
            total_ms: cemented_ms.saturating_sub(inserted_ms),
        })
    }
}

impl ConfirmationStages {
    /// Adds a block cemented at `now_ms`. Returns the summary of the
    /// previous second once a block of a later second arrives.
    pub fn record(&mut self, now_ms: u64, record: StageRecord) -> Option<String> {
        let second = now_ms / 1000;
        let summary = match self.second {
            Some(current) if current != second => {
                let line = self.summary(current);
                *self = Self::default();
                Some(line)
            }
            _ => None,
        };
        self.second = Some(second);
        match record {
            StageRecord::Sample(sample) => self.samples.push(sample),
            StageRecord::Fork => self.forks += 1,
            StageRecord::Dependent => self.dependents += 1,
            StageRecord::NoElection => self.no_election += 1,
        }
        summary
    }

    fn summary(&self, second: u64) -> String {
        let stage = |pick: fn(&StageSample) -> Option<u64>| {
            let mut values: Vec<u64> = self.samples.iter().filter_map(pick).collect();
            values.sort_unstable();
            format!(
                "{}/{}/{}",
                percentile(&values, 50),
                percentile(&values, 90),
                values.last().copied().unwrap_or(0)
            )
        };
        let gated = self.samples.iter().filter(|s| s.gate_ms > 0).count();
        format!(
            "CONFIRM_STAGES second={} n={} gated={} forks={} dependents={} no_election={} \
             queued={} to_nc={} nc_to_final={} gate={} cement={} handoff={} cementing={} \
             publish={} total={}",
            second,
            self.samples.len(),
            gated,
            self.forks,
            self.dependents,
            self.no_election,
            stage(|s| Some(s.queued_ms)),
            stage(|s| s.to_nc_ms),
            stage(|s| s.nc_to_final_ms),
            stage(|s| Some(s.gate_ms)),
            stage(|s| Some(s.cement_ms)),
            stage(|s| s.handoff_ms),
            stage(|s| s.cementing_ms),
            stage(|s| s.publish_ms),
            stage(|s| Some(s.total_ms)),
        )
    }
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(sorted.len() - 1) * percent / 100]
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(millis)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::ConfirmationType;
    use rsnano_types::SavedBlock;

    #[test]
    fn stages_of_an_election_add_up_from_insertion_to_cementation() {
        let block = SavedBlock::new_test_instance();
        let election = election_for(&block, 1_000_300, 300, Some(100), Some(0));

        let record = StageRecord::new(
            &block.hash(),
            UnixMillisTimestamp::new(999_950),
            &election,
            1_000_320,
        );

        assert_eq!(
            record,
            StageRecord::Sample(StageSample {
                queued_ms: 50,
                to_nc_ms: Some(100),
                nc_to_final_ms: Some(200),
                gate_ms: 0,
                cement_ms: 20,
                handoff_ms: None,
                cementing_ms: None,
                publish_ms: None,
                total_ms: 370,
            })
        );
    }

    #[test]
    fn a_gated_election_reports_when_finality_was_allowed() {
        let block = SavedBlock::new_test_instance();
        let election = election_for(&block, 1_006_000, 6_000, Some(200), Some(5_800));

        let StageRecord::Sample(sample) = StageRecord::new(
            &block.hash(),
            UnixMillisTimestamp::new(1_000_000),
            &election,
            1_006_000,
        ) else {
            panic!("expected a sample");
        };

        assert_eq!(sample.gate_ms, 5_800);
    }

    #[test]
    fn cementation_is_split_at_the_hand_over_and_the_return() {
        let block = SavedBlock::new_test_instance();
        let mut election = election_for(&block, 1_000_300, 300, Some(100), Some(0));
        election.handed_to_cementing = Some(UNIX_EPOCH + Duration::from_millis(1_000_310));
        election.cemented_seen = Some(UNIX_EPOCH + Duration::from_millis(1_000_450));

        let StageRecord::Sample(sample) = StageRecord::new(
            &block.hash(),
            UnixMillisTimestamp::new(999_950),
            &election,
            1_000_470,
        ) else {
            panic!("expected a sample");
        };

        assert_eq!(sample.cement_ms, 170);
        assert_eq!(sample.handoff_ms, Some(10));
        assert_eq!(sample.cementing_ms, Some(140));
        assert_eq!(sample.publish_ms, Some(20));
    }

    #[test]
    fn a_block_cemented_for_another_winner_is_a_dependent() {
        let block = SavedBlock::new_test_instance();
        let election = election_for(&block, 1_000_300, 300, Some(100), Some(0));

        let record = StageRecord::new(
            &BlockHash::from(42),
            UnixMillisTimestamp::new(999_950),
            &election,
            1_000_320,
        );

        assert_eq!(record, StageRecord::Dependent);
    }

    #[test]
    fn a_block_cemented_without_an_election_is_counted_as_such() {
        let block = SavedBlock::new_test_instance();
        let election =
            ConfirmedElection::new(block.clone(), ConfirmationType::InactiveConfirmationHeight);

        let record = StageRecord::new(
            &block.hash(),
            UnixMillisTimestamp::new(999_950),
            &election,
            1_000_320,
        );

        assert_eq!(record, StageRecord::NoElection);
    }

    #[test]
    fn summarizes_a_second_once_the_next_one_starts() {
        let mut stages = ConfirmationStages::default();

        assert_eq!(stages.record(5_100, StageRecord::Sample(sample(100))), None);
        assert_eq!(stages.record(5_900, StageRecord::Sample(sample(300))), None);
        assert_eq!(stages.record(5_950, StageRecord::Fork), None);
        let line = stages
            .record(6_000, StageRecord::Sample(sample(900)))
            .unwrap();

        assert_eq!(
            line,
            "CONFIRM_STAGES second=5 n=2 gated=0 forks=1 dependents=0 no_election=0 \
             queued=0/0/0 to_nc=10/10/10 nc_to_final=100/100/300 gate=0/0/0 cement=0/0/0 \
             handoff=0/0/0 cementing=0/0/0 publish=0/0/0 total=100/100/300"
        );
        let next = stages.record(7_000, StageRecord::NoElection).unwrap();
        assert!(next.starts_with("CONFIRM_STAGES second=6 n=1 "));
    }

    /* Test helpers */

    fn election_for(
        block: &SavedBlock,
        finalized_ms: u64,
        duration_ms: u64,
        notarized_after_ms: Option<u64>,
        eligible_after_ms: Option<u64>,
    ) -> ConfirmedElection {
        let mut election =
            ConfirmedElection::new(block.clone(), ConfirmationType::ActiveConfirmedQuorum);
        election.election_end = UNIX_EPOCH + Duration::from_millis(finalized_ms);
        election.election_duration = Duration::from_millis(duration_ms);
        election.notarized_after = notarized_after_ms.map(Duration::from_millis);
        election.eligible_after = eligible_after_ms.map(Duration::from_millis);
        election
    }

    fn sample(total_ms: u64) -> StageSample {
        StageSample {
            queued_ms: 0,
            to_nc_ms: Some(10),
            nc_to_final_ms: Some(total_ms),
            gate_ms: 0,
            cement_ms: 0,
            handoff_ms: None,
            cementing_ms: None,
            publish_ms: None,
            total_ms,
        }
    }
}
