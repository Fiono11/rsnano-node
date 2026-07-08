use std::sync::{Arc, RwLock};

use rsnano_ledger::RepWeightCache;
use rsnano_types::{RaiElectionId, RaiPendingReport};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{
    RaiCloseState, RaiCommitteeProvider, RaiPendingReportInsertError, RepWeightRaiCommitteeProvider,
};

pub struct RaiPendingReportProcessor {
    close_state: Arc<RwLock<RaiCloseState>>,
    committee_provider: Arc<dyn RaiCommitteeProvider>,
    stats: Arc<Stats>,
}

impl RaiPendingReportProcessor {
    pub fn new(
        close_state: Arc<RwLock<RaiCloseState>>,
        rep_weights: Arc<RepWeightCache>,
        stats: Arc<Stats>,
    ) -> Self {
        Self::with_committee_provider(
            close_state,
            Arc::new(RepWeightRaiCommitteeProvider::new(rep_weights)),
            stats,
        )
    }

    pub fn with_committee_provider(
        close_state: Arc<RwLock<RaiCloseState>>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            close_state,
            committee_provider,
            stats,
        }
    }

    pub fn process(&self, report: &RaiPendingReport) -> Result<(), RaiPendingReportProcessError> {
        self.stats
            .inc(StatType::RaiPendingReportProcessor, DetailType::Process);

        if report.validate().is_err() {
            self.stats
                .inc(StatType::RaiPendingReportProcessor, DetailType::Invalid);
            return Err(RaiPendingReportProcessError::Invalid);
        }

        let committees = self
            .committee_provider
            .committees_for(&pending_report_election_id(report));

        if !committees.contains(&report.reporter) {
            self.stats
                .inc(StatType::RaiPendingReportProcessor, DetailType::Ignored);
            return Err(RaiPendingReportProcessError::InvalidReporter);
        }

        let mut close_state = self.close_state.write().unwrap();
        let slots = report.slots.clone();
        let result = close_state.insert_pending_report(report.clone());

        match result {
            Ok(()) => {
                close_state.mark_visible_slots(report.epoch, slots);
                self.stats
                    .inc(StatType::RaiPendingReportProcessor, DetailType::Processed);
                Ok(())
            }
            Err(RaiPendingReportInsertError::Duplicate) => {
                self.stats
                    .inc(StatType::RaiPendingReportProcessor, DetailType::Duplicate);
                Err(RaiPendingReportProcessError::Duplicate)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiPendingReportProcessError {
    Invalid,
    InvalidReporter,
    Duplicate,
}

fn pending_report_election_id(report: &RaiPendingReport) -> RaiElectionId {
    RaiElectionId::Close {
        epoch: report.epoch,
        attempt: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{RaiCommittee, RaiCommitteeDeriver};
    use rsnano_types::{Account, Amount, PrivateKey, PublicKey, RaiEpoch, RaiSlot};
    use rsnano_utils::stats::Direction;

    #[test]
    fn processes_valid_report_into_visibility_state() {
        let fixture = Fixture::new();
        let report = RaiPendingReport::new(&fixture.reporter, 7, vec![slot(1), slot(2)]);

        assert_eq!(fixture.processor.process(&report), Ok(()));

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.pending_report_count(7), 1);
        assert_eq!(
            state.pending_report(7, &fixture.reporter.public_key()),
            Some(&report)
        );
        assert!(state.is_visible(7, &slot(1)));
        assert!(state.is_visible(7, &slot(2)));
        assert_eq!(
            fixture.stats.count(
                StatType::RaiPendingReportProcessor,
                DetailType::Processed,
                Direction::In
            ),
            1
        );
    }

    #[test]
    fn invalid_signature_is_rejected_before_state_update() {
        let fixture = Fixture::new();
        let mut report = RaiPendingReport::new(&fixture.reporter, 7, vec![slot(1)]);
        report.slots.push(slot(2));

        assert_eq!(
            fixture.processor.process(&report),
            Err(RaiPendingReportProcessError::Invalid)
        );

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.pending_report_count(7), 0);
        assert!(!state.is_visible(7, &slot(1)));
        assert!(!state.is_visible(7, &slot(2)));
    }

    #[test]
    fn invalid_reporter_is_rejected_before_state_update() {
        let fixture = Fixture::new();
        let outsider = PrivateKey::from(2);
        let report = RaiPendingReport::new(&outsider, 7, vec![slot(1)]);

        assert_eq!(
            fixture.processor.process(&report),
            Err(RaiPendingReportProcessError::InvalidReporter)
        );

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.pending_report_count(7), 0);
        assert!(!state.is_visible(7, &slot(1)));
    }

    #[test]
    fn duplicate_reporter_is_rejected_before_visibility_update() {
        let fixture = Fixture::new();
        let first = RaiPendingReport::new(&fixture.reporter, 7, vec![slot(1)]);
        let second = RaiPendingReport::new(&fixture.reporter, 7, vec![slot(2)]);

        assert_eq!(fixture.processor.process(&first), Ok(()));
        assert_eq!(
            fixture.processor.process(&second),
            Err(RaiPendingReportProcessError::Duplicate)
        );

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.pending_report_count(7), 1);
        assert!(state.is_visible(7, &slot(1)));
        assert!(!state.is_visible(7, &slot(2)));
    }

    struct Fixture {
        close_state: Arc<RwLock<RaiCloseState>>,
        processor: RaiPendingReportProcessor,
        reporter: PrivateKey,
        stats: Arc<Stats>,
    }

    impl Fixture {
        fn new() -> Self {
            let reporter = PrivateKey::from(1);
            let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
            let stats = Arc::new(Stats::default());
            let processor = RaiPendingReportProcessor::with_committee_provider(
                close_state.clone(),
                Arc::new(StaticCommitteeProvider::new(committee([(
                    reporter.public_key(),
                    Amount::raw(100),
                )]))),
                stats.clone(),
            );

            Self {
                close_state,
                processor,
                reporter,
                stats,
            }
        }
    }

    struct StaticCommitteeProvider {
        committee: RaiCommittee,
    }

    impl StaticCommitteeProvider {
        fn new(committee: RaiCommittee) -> Self {
            Self { committee }
        }
    }

    impl RaiCommitteeProvider for StaticCommitteeProvider {
        fn genesis_committee(&self) -> RaiCommittee {
            self.committee.clone()
        }

        fn committee_for_closed_epoch(&self, _epoch: RaiEpoch) -> Option<RaiCommittee> {
            Some(self.committee.clone())
        }
    }

    fn committee<const N: usize>(values: [(PublicKey, Amount); N]) -> RaiCommittee {
        RaiCommitteeDeriver::new().derive_committee(values)
    }

    fn slot(account_height: u64) -> RaiSlot {
        RaiSlot::new(Account::from(1), account_height)
    }
}
