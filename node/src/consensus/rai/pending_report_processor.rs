use std::sync::{Arc, RwLock};

use rsnano_ledger::RepWeightCache;
use rsnano_types::{RaiElectionId, RaiEpoch, RaiPendingReport, RaiSlot};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{
    NoopRaiStatePersistence, RaiCloseState, RaiCommitteeProvider, RaiCommitteeSet,
    RaiPendingReportInsertError, RaiStatePersistence, RepWeightRaiCommitteeProvider,
};

pub struct RaiPendingReportProcessor {
    close_state: Arc<RwLock<RaiCloseState>>,
    committee_provider: Arc<dyn RaiCommitteeProvider>,
    persistence: Arc<dyn RaiStatePersistence>,
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
        Self::with_committee_provider_and_persistence(
            close_state,
            committee_provider,
            Arc::new(NoopRaiStatePersistence),
            stats,
        )
    }

    pub fn with_committee_provider_and_persistence(
        close_state: Arc<RwLock<RaiCloseState>>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        persistence: Arc<dyn RaiStatePersistence>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            close_state,
            committee_provider,
            persistence,
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

        let Some(committees) = self
            .committee_provider
            .try_committees_for(&pending_report_election_id(report))
        else {
            self.stats
                .inc(StatType::RaiPendingReportProcessor, DetailType::Ignored);
            return Err(RaiPendingReportProcessError::MissingCommitteeHistory);
        };

        if !committees.contains(&report.reporter) {
            self.stats
                .inc(StatType::RaiPendingReportProcessor, DetailType::Ignored);
            return Err(RaiPendingReportProcessError::InvalidReporter);
        }

        let (result, snapshot) = {
            let mut close_state = self.close_state.write().unwrap();
            let slots = report.slots.clone();
            let result = close_state.insert_pending_report(report.clone());

            let snapshot = if result.is_ok() {
                let visible_slots = slots
                    .into_iter()
                    .filter(|slot| {
                        report_visibility_reached(&close_state, report.epoch, slot, &committees)
                    })
                    .collect::<Vec<_>>();
                close_state.mark_visible_slots(report.epoch, visible_slots);
                Some(close_state.snapshot())
            } else {
                None
            };

            (result, snapshot)
        };

        match result {
            Ok(()) => {
                if let Some(snapshot) = snapshot {
                    self.persistence.save_close_state(&snapshot);
                }
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

fn report_visibility_reached(
    close_state: &RaiCloseState,
    epoch: RaiEpoch,
    slot: &RaiSlot,
    committees: &RaiCommitteeSet,
) -> bool {
    if committees.is_empty() {
        return false;
    }

    let reports = close_state.pending_reports(epoch);
    committees.iter().all(|committee| {
        let report_count = reports
            .iter()
            .filter(|report| report.slots.contains(slot) && committee.contains(&report.reporter))
            .count();
        committee.has_visibility_quorum(report_count)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiPendingReportProcessError {
    Invalid,
    InvalidReporter,
    MissingCommitteeHistory,
    Duplicate,
}

fn pending_report_election_id(report: &RaiPendingReport) -> RaiElectionId {
    RaiElectionId::CloseCut {
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
    use std::collections::HashMap;

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

    #[test]
    fn duplicate_report_is_ignored_for_visibility_quorum() {
        let first_reporter = PrivateKey::from(1);
        let second_reporter = PrivateKey::from(2);
        let third_reporter = PrivateKey::from(3);
        let fourth_reporter = PrivateKey::from(4);
        let fixture = Fixture::with_committee(
            first_reporter.clone(),
            committee_from_keys([
                &first_reporter,
                &second_reporter,
                &third_reporter,
                &fourth_reporter,
            ]),
        );
        let target = slot(1);
        let first = RaiPendingReport::new(&first_reporter, 7, vec![target]);
        let duplicate = RaiPendingReport::new(&first_reporter, 7, vec![target]);

        assert_eq!(fixture.processor.process(&first), Ok(()));
        assert_eq!(
            fixture.processor.process(&duplicate),
            Err(RaiPendingReportProcessError::Duplicate)
        );
        assert!(!fixture.close_state.read().unwrap().is_visible(7, &target));

        let second = RaiPendingReport::new(&second_reporter, 7, vec![target]);
        assert_eq!(fixture.processor.process(&second), Ok(()));

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.pending_report_count(7), 2);
        assert!(state.is_visible(7, &target));
    }

    #[test]
    fn invalid_signature_is_rejected_and_does_not_count_toward_visibility() {
        let first_reporter = PrivateKey::from(1);
        let invalid_reporter = PrivateKey::from(2);
        let second_valid_reporter = PrivateKey::from(3);
        let fourth_reporter = PrivateKey::from(4);
        let fixture = Fixture::with_committee(
            first_reporter.clone(),
            committee_from_keys([
                &first_reporter,
                &invalid_reporter,
                &second_valid_reporter,
                &fourth_reporter,
            ]),
        );
        let target = slot(1);

        assert_eq!(
            fixture
                .processor
                .process(&RaiPendingReport::new(&first_reporter, 7, vec![target])),
            Ok(())
        );

        let mut invalid = RaiPendingReport::new(&invalid_reporter, 7, vec![target]);
        invalid.slots.push(slot(2));
        assert_eq!(
            fixture.processor.process(&invalid),
            Err(RaiPendingReportProcessError::Invalid)
        );
        assert!(!fixture.close_state.read().unwrap().is_visible(7, &target));

        assert_eq!(
            fixture.processor.process(&RaiPendingReport::new(
                &second_valid_reporter,
                7,
                vec![target]
            )),
            Ok(())
        );
        assert!(fixture.close_state.read().unwrap().is_visible(7, &target));
    }

    #[test]
    fn f_plus_one_reports_per_relevant_committee_make_slot_visible() {
        let first_a = PrivateKey::from(1);
        let second_a = PrivateKey::from(2);
        let third_a = PrivateKey::from(3);
        let fourth_a = PrivateKey::from(4);
        let first_b = PrivateKey::from(5);
        let second_b = PrivateKey::from(6);
        let third_b = PrivateKey::from(7);
        let fourth_b = PrivateKey::from(8);
        let committee_a = committee_from_keys([&first_a, &second_a, &third_a, &fourth_a]);
        let committee_b = committee_from_keys([&first_b, &second_b, &third_b, &fourth_b]);
        assert_eq!(committee_a.thresholds().max_faulty + 1, 2);
        assert_eq!(committee_b.thresholds().max_faulty + 1, 2);
        let fixture = Fixture::with_provider(
            first_a.clone(),
            StaticCommitteeProvider::with_closed_committees(
                committee_a.clone(),
                [(4, committee_a), (5, committee_b)],
            ),
        );
        let target = slot(1);

        for reporter in [&first_a, &second_a, &first_b] {
            assert_eq!(
                fixture
                    .processor
                    .process(&RaiPendingReport::new(reporter, 7, vec![target])),
                Ok(())
            );
        }
        assert!(!fixture.close_state.read().unwrap().is_visible(7, &target));

        assert_eq!(
            fixture
                .processor
                .process(&RaiPendingReport::new(&second_b, 7, vec![target])),
            Ok(())
        );
        assert!(fixture.close_state.read().unwrap().is_visible(7, &target));
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
            Self::with_committee(
                reporter.clone(),
                committee([(reporter.public_key(), Amount::raw(100))]),
            )
        }

        fn with_committee(reporter: PrivateKey, committee: RaiCommittee) -> Self {
            Self::with_provider(reporter, StaticCommitteeProvider::single(committee))
        }

        fn with_provider(reporter: PrivateKey, provider: StaticCommitteeProvider) -> Self {
            let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
            let stats = Arc::new(Stats::default());
            let processor = RaiPendingReportProcessor::with_committee_provider(
                close_state.clone(),
                Arc::new(provider),
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
        genesis_committee: RaiCommittee,
        closed_committees: HashMap<RaiEpoch, RaiCommittee>,
    }

    impl StaticCommitteeProvider {
        fn single(committee: RaiCommittee) -> Self {
            Self {
                genesis_committee: committee,
                closed_committees: HashMap::new(),
            }
        }

        fn with_closed_committees<const N: usize>(
            genesis_committee: RaiCommittee,
            closed_committees: [(RaiEpoch, RaiCommittee); N],
        ) -> Self {
            Self {
                genesis_committee,
                closed_committees: closed_committees.into_iter().collect(),
            }
        }
    }

    impl RaiCommitteeProvider for StaticCommitteeProvider {
        fn genesis_committee(&self) -> RaiCommittee {
            self.genesis_committee.clone()
        }

        fn committee_for_closed_epoch(&self, epoch: RaiEpoch) -> Option<RaiCommittee> {
            self.closed_committees
                .get(&epoch)
                .cloned()
                .or_else(|| Some(self.genesis_committee.clone()))
        }
    }

    fn committee<const N: usize>(values: [(PublicKey, Amount); N]) -> RaiCommittee {
        RaiCommitteeDeriver::new().derive_committee(values)
    }

    fn committee_from_keys<const N: usize>(keys: [&PrivateKey; N]) -> RaiCommittee {
        committee(keys.map(|key| (key.public_key(), Amount::raw(100))))
    }

    fn slot(account_height: u64) -> RaiSlot {
        RaiSlot::new(Account::from(1), account_height)
    }
}
