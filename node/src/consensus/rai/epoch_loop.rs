use std::{collections::BTreeSet, time::Duration};

use rsnano_ledger::RepWeights;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, ConfirmationHeightInfo, PrivateKey, QualifiedRoot, RaiEpoch};

use super::{RaiCloseKind, RaiClosingPhase, RaiEpochManager, RaiReport, ReportInsert};

/// Notifications consumed by the epoch lifecycle service.
#[derive(Clone, Debug)]
pub enum RaiEpochEvent {
    Tick(Timestamp),
    ReportReceived(RaiReport),
    SlotEvidenceChanged {
        epoch: RaiEpoch,
        root: QualifiedRoot,
    },
    CloseElectionChanged {
        kind: RaiCloseKind,
        epoch: RaiEpoch,
        round: u32,
    },
    Stop,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rsnano_ledger::RepWeights;
    use rsnano_types::{BlockHash, PrivateKey};

    use super::*;

    #[derive(Default)]
    struct TestDriver {
        reports: Vec<RaiReport>,
    }

    impl RaiEpochLoopDriver for TestDriver {
        fn start_close_election(
            &mut self,
            _kind: RaiCloseKind,
            _epoch: RaiEpoch,
            _round: u32,
            _root: QualifiedRoot,
            _hash: BlockHash,
        ) {
        }

        fn close_election_winner(
            &self,
            _kind: RaiCloseKind,
            _epoch: RaiEpoch,
            _round: u32,
        ) -> Option<BlockHash> {
            None
        }

        fn broadcast_report(&mut self, report: RaiReport) {
            self.reports.push(report);
        }
    }

    fn epoch_loop(start: Timestamp, duration: Duration) -> RaiEpochLoop<TestDriver> {
        RaiEpochLoop::new(
            RaiEpochManager::new(Arc::new(RepWeights::default()), BlockHash::ZERO),
            TestDriver::default(),
            PrivateKey::from(1),
            duration,
            start,
        )
    }

    #[test]
    fn deadline_opens_successor_and_emits_one_report() {
        let start = Timestamp::new_test_instance();
        let duration = Duration::from_secs(30);
        let mut service = epoch_loop(start, duration);

        service.process(RaiEpochEvent::Tick(
            start + duration - Duration::from_nanos(1),
        ));
        assert_eq!(service.epoch_state().open_epoch, RaiEpoch::ZERO);
        assert!(service.epoch_state().closing.is_none());
        assert!(service.driver().reports.is_empty());

        let deadline = start + duration;
        service.process(RaiEpochEvent::Tick(deadline));
        assert_eq!(service.epoch_state().open_epoch, RaiEpoch::new(1));
        assert_eq!(service.epoch_state().open_started_at, deadline);
        assert_eq!(
            service.epoch_state().closing,
            Some(super::super::RaiClosingEpoch {
                epoch: RaiEpoch::ZERO,
                phase: RaiClosingPhase::CollectingReports,
            })
        );
        assert_eq!(service.driver().reports.len(), 1);
        assert_eq!(service.driver().reports[0].epoch, RaiEpoch::ZERO);

        service.process(RaiEpochEvent::Tick(deadline + duration));
        service.process(RaiEpochEvent::Tick(deadline + duration + duration));
        assert_eq!(service.driver().reports.len(), 1);
    }

    #[test]
    fn stop_makes_the_loop_inert() {
        let start = Timestamp::new_test_instance();
        let mut service = epoch_loop(start, Duration::from_secs(1));
        service.process(RaiEpochEvent::Stop);
        service.process(RaiEpochEvent::Tick(start + Duration::from_secs(1)));

        assert!(service.is_stopped());
        assert_eq!(service.epoch_state().open_epoch, RaiEpoch::ZERO);
        assert!(service.driver().reports.is_empty());
    }
}

/// The deliberately small boundary between the lifecycle state machine and
/// the ledger, active-election container, and network.
pub trait RaiEpochLoopDriver {
    fn visible_obligations(&self, _epoch: RaiEpoch) -> BTreeSet<QualifiedRoot> {
        BTreeSet::new()
    }

    fn reports_ready(&self, _manager: &RaiEpochManager, _epoch: RaiEpoch) -> bool {
        true
    }

    fn vote_visible_obligations(&self, _epoch: RaiEpoch) -> BTreeSet<QualifiedRoot> {
        BTreeSet::new()
    }

    fn start_close_election(
        &mut self,
        kind: RaiCloseKind,
        epoch: RaiEpoch,
        round: u32,
        root: QualifiedRoot,
        hash: BlockHash,
    );

    fn close_election_winner(
        &self,
        kind: RaiCloseKind,
        epoch: RaiEpoch,
        round: u32,
    ) -> Option<BlockHash>;

    fn obligations_settled(
        &self,
        _epoch: RaiEpoch,
        _obligations: &BTreeSet<QualifiedRoot>,
    ) -> bool {
        false
    }

    fn confirmation_heights(&self) -> Vec<(rsnano_types::Account, ConfirmationHeightInfo)> {
        Vec::new()
    }

    fn certified_weights(&self, _epoch: RaiEpoch) -> Option<RepWeights> {
        None
    }

    fn broadcast_report(&mut self, report: RaiReport);
}

/// Drives the single open epoch and (at most) one concurrently closing epoch.
///
/// It contains no timer thread of its own. The node's ticker sends `Tick`
/// events, which also makes the state machine deterministic with a null clock.
pub struct RaiEpochLoop<D> {
    epoch_manager: RaiEpochManager,
    driver: D,
    local_key: PrivateKey,
    epoch_duration: Duration,
    stopped: bool,
}

impl<D: RaiEpochLoopDriver> RaiEpochLoop<D> {
    pub fn new(
        mut epoch_manager: RaiEpochManager,
        driver: D,
        local_key: PrivateKey,
        epoch_duration: Duration,
        started_at: Timestamp,
    ) -> Self {
        epoch_manager.set_open_started_at(started_at);
        Self {
            epoch_manager,
            driver,
            local_key,
            epoch_duration,
            stopped: false,
        }
    }

    pub fn epoch_state(&self) -> &super::RaiEpochState {
        self.epoch_manager.state()
    }

    pub fn epoch_manager(&self) -> &RaiEpochManager {
        &self.epoch_manager
    }

    pub fn epoch_manager_mut(&mut self) -> &mut RaiEpochManager {
        &mut self.epoch_manager
    }

    pub fn driver(&self) -> &D {
        &self.driver
    }

    pub fn driver_mut(&mut self) -> &mut D {
        &mut self.driver
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    pub fn process(&mut self, event: RaiEpochEvent) {
        if self.stopped {
            return;
        }
        match event {
            RaiEpochEvent::Tick(now) => self.tick(now),
            RaiEpochEvent::ReportReceived(report) => {
                let epoch = report.epoch;
                if matches!(
                    self.epoch_manager.reports_mut().insert(report),
                    Ok(ReportInsert::Added | ReportInsert::Duplicate)
                ) {
                    self.maybe_start_cut(epoch);
                }
            }
            RaiEpochEvent::SlotEvidenceChanged { epoch, root: _ } => {
                self.maybe_start_record(epoch);
            }
            RaiEpochEvent::CloseElectionChanged { kind, epoch, round } => {
                self.close_election_changed(kind, epoch, round);
            }
            RaiEpochEvent::Stop => self.stopped = true,
        }
    }

    fn tick(&mut self, now: Timestamp) {
        let state = *self.epoch_manager.state();
        if state.closing.is_some() || now < state.open_started_at + self.epoch_duration {
            return;
        }

        let closing = state.open_epoch;
        let obligations = self.driver.visible_obligations(closing);
        let report = RaiReport::new(&self.local_key, closing, obligations);
        if self.epoch_manager.start_closing(now) {
            // Store our own report through the same validation/deduplication
            // path before publishing it. Repeated ticks are consequently inert.
            let _ = self.epoch_manager.reports_mut().insert(report.clone());
            self.driver.broadcast_report(report);
        }
    }

    fn maybe_start_cut(&mut self, epoch: RaiEpoch) {
        if self.epoch_manager.closing_epoch().is_none_or(|closing| {
            closing.epoch != epoch || closing.phase != RaiClosingPhase::CollectingReports
        }) || !self.driver.reports_ready(&self.epoch_manager, epoch)
        {
            return;
        }
        let visible = self.driver.vote_visible_obligations(epoch);
        if let Some((root, hash)) = self.epoch_manager.begin_cut_election(visible) {
            self.driver
                .start_close_election(RaiCloseKind::Cut, epoch, 0, root, hash);
        }
    }

    fn maybe_start_record(&mut self, epoch: RaiEpoch) {
        let Some(obligations) = self.epoch_manager.obligations_to_drain(epoch).cloned() else {
            return;
        };
        if !self.driver.obligations_settled(epoch, &obligations) {
            return;
        }
        if let Some((root, hash)) = self
            .epoch_manager
            .begin_record_election(true, self.driver.confirmation_heights())
        {
            self.driver
                .start_close_election(RaiCloseKind::Record, epoch, 0, root, hash);
        }
    }

    fn close_election_changed(&mut self, kind: RaiCloseKind, epoch: RaiEpoch, round: u32) {
        let Some(hash) = self.driver.close_election_winner(kind, epoch, round) else {
            return;
        };
        match kind {
            RaiCloseKind::Cut => {
                if self.epoch_manager.install_cut(epoch, round, hash).is_ok() {
                    self.maybe_start_record(epoch);
                }
            }
            RaiCloseKind::Record => {
                let Some(weights) = self.driver.certified_weights(epoch) else {
                    return;
                };
                let _ = self.epoch_manager.install_record(
                    epoch,
                    round,
                    hash,
                    self.driver.confirmation_heights(),
                    weights,
                );
            }
        }
    }
}
