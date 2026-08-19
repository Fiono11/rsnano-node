use std::{collections::BTreeSet, time::Duration};

use rsnano_ledger::RepWeights;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, BlockHash, ConfirmationHeightInfo, PrivateKey, QualifiedRoot, RaiEpoch,
};

use super::{RaiCloseKind, RaiClosingPhase, RaiEpochManager, RaiReport};

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
        close_starts: Vec<(RaiCloseKind, RaiEpoch, u32, BlockHash)>,
    }

    impl RaiEpochLoopDriver for TestDriver {
        fn start_close_election(
            &mut self,
            kind: RaiCloseKind,
            epoch: RaiEpoch,
            round: u32,
            _root: QualifiedRoot,
            hash: BlockHash,
        ) {
            self.close_starts.push((kind, epoch, round, hash));
        }

        fn close_election_winner(
            &self,
            _kind: RaiCloseKind,
            _epoch: RaiEpoch,
            _round: u32,
        ) -> Option<BlockHash> {
            None
        }

        fn commit_close_record(
            &mut self,
            _epoch: RaiEpoch,
            _frontiers: &crate::consensus::rai::RaiFrontierMap,
        ) -> bool {
            true
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

    #[test]
    fn report_quorum_at_epoch_boundary_waits_for_collection_grace() {
        let start = Timestamp::new_test_instance();
        let duration = Duration::from_secs(30);
        let local = PrivateKey::from(1);
        let remote = PrivateKey::from(2);
        let committee = Arc::new(RepWeights::from([
            (local.public_key(), rsnano_types::Amount::raw(1)),
            (remote.public_key(), rsnano_types::Amount::raw(1)),
        ]));
        let mut service = RaiEpochLoop::new(
            RaiEpochManager::new(committee, BlockHash::ZERO),
            TestDriver::default(),
            local,
            duration,
            start,
        );

        // The remote report plus the locally generated boundary report form a
        // complete quorum.  Closing must still pass through the collection
        // barrier instead of starting from an arrival-order-dependent subset.
        service.process(RaiEpochEvent::ReportReceived(RaiReport::new(
            &remote,
            RaiEpoch::ZERO,
            [],
        )));
        let boundary = start + duration;
        service.process(RaiEpochEvent::Tick(boundary));
        assert!(service.driver().close_starts.is_empty());
        assert_eq!(
            service.epoch_state().closing.unwrap().phase,
            RaiClosingPhase::CollectingReports
        );

        service.process(RaiEpochEvent::Tick(boundary + Duration::from_millis(1)));
        assert_eq!(service.driver().close_starts.len(), 1);
    }
}

/// The deliberately small boundary between the lifecycle state machine and
/// the ledger, active-election container, and network.
pub trait RaiEpochLoopDriver {
    fn visible_obligations(&self, _epoch: RaiEpoch) -> BTreeSet<rsnano_types::RaiSlotId> {
        BTreeSet::new()
    }

    fn vote_visible_obligations(&self, _epoch: RaiEpoch) -> BTreeSet<rsnano_types::RaiSlotId> {
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

    /// Returns the persistent, authenticated vote evidence accumulated for a
    /// close round.  The epoch loop derives death/carry from this evidence;
    /// a local election timeout is intentionally insufficient to retry.
    fn close_election_evidence(
        &self,
        _kind: RaiCloseKind,
        _epoch: RaiEpoch,
        _round: u32,
    ) -> Option<super::RaiElectionVoteState> {
        None
    }

    fn obligations_settled(
        &self,
        _epoch: RaiEpoch,
        _obligations: &BTreeSet<rsnano_types::RaiSlotId>,
    ) -> bool {
        false
    }

    /// Persistent vote evidence for a normal slot election.
    fn slot_vote_evidence(
        &self,
        _epoch: RaiEpoch,
        _root: &QualifiedRoot,
    ) -> Option<super::RaiElectionVoteState> {
        None
    }

    /// The epoch-local segment ending at the certified winner.
    fn epoch_frontier_segment(
        &self,
        _epoch: RaiEpoch,
        _root: &QualifiedRoot,
        _winner: BlockHash,
    ) -> Vec<(Account, ConfirmationHeightInfo)> {
        Vec::new()
    }

    /// Applies the cut to active elections. Included old elections and all
    /// successor-epoch elections remain enabled.
    fn apply_decided_cut(
        &mut self,
        _epoch: RaiEpoch,
        _included: &BTreeSet<rsnano_types::RaiSlotId>,
    ) {
    }

    fn confirmation_heights(&self) -> Vec<(rsnano_types::Account, ConfirmationHeightInfo)> {
        Vec::new()
    }

    fn certified_weights(&self, _epoch: RaiEpoch) -> Option<RepWeights> {
        None
    }

    /// Durably finalizes every suffix selected by the certified frontier map.
    /// Returning true acknowledges that the ledger commit completed and is
    /// the gate for publishing the close decision.
    fn commit_close_record(&mut self, epoch: RaiEpoch, frontiers: &super::RaiFrontierMap) -> bool;

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

    pub fn into_epoch_manager(self) -> RaiEpochManager {
        self.epoch_manager
    }

    pub fn into_parts(self) -> (RaiEpochManager, D) {
        (self.epoch_manager, self.driver)
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
                let _ = self.epoch_manager.reports_mut().insert(report);
            }
            RaiEpochEvent::SlotEvidenceChanged { epoch, root } => {
                if let Some(evidence) = self.driver.slot_vote_evidence(epoch, &root) {
                    let slot = rsnano_types::RaiSlotId {
                        epoch,
                        root: root.clone(),
                    };
                    // Derive the winner before borrowing the driver mutably for
                    // its corresponding epoch-local ledger segment.
                    let outcome = self
                        .epoch_manager
                        .happy_path_drain(epoch)
                        .and_then(|drain| drain.persistent_evidence_outcome(&slot, &evidence));
                    if let Some(outcome) = outcome {
                        let segment = match outcome {
                            super::RaiDrainOutcome::Finalized(winner)
                            | super::RaiDrainOutcome::Selected(winner) => {
                                self.driver.epoch_frontier_segment(epoch, &root, winner)
                            }
                            super::RaiDrainOutcome::ReleasedTimeout
                            | super::RaiDrainOutcome::ReleasedConflict => Vec::new(),
                        };
                        let _ = self
                            .epoch_manager
                            .record_drain_evidence(epoch, &slot, &evidence, segment);
                    }
                }
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
        if let Some(closing) = state.closing {
            if closing.phase == RaiClosingPhase::CollectingReports {
                // Batch report arrivals until the next protocol tick. This
                // preserves the W-F barrier while giving epidemic delivery a
                // deterministic collection window, avoiding needless round-0
                // preference splits caused solely by per-message ordering.
                // Give epidemic report repair enough time to converge before
                // falling back to the W-F minimum. The fallback preserves
                // liveness with missing/faulty reporters; healthy committees
                // normally reach full coverage first and avoid an artificial
                // close-version split caused only by network scheduling.
                // Three seconds is also the accelerated test epoch length and
                // proved too short for six peers to relay all chunked reports
                // while block/vote traffic is active. Starting at that edge
                // split otherwise-healthy replicas across different cuts.
                const REPORT_COLLECTION_GRACE: Duration = Duration::from_secs(5);
                if self
                    .epoch_manager
                    .full_report_coverage_available(closing.epoch)
                    || now >= state.open_started_at + REPORT_COLLECTION_GRACE
                {
                    self.maybe_start_cut(closing.epoch);
                }
            }
            return;
        }
        if now < state.open_started_at + self.epoch_duration {
            return;
        }

        let closing = state.open_epoch;
        let obligations = self.driver.visible_obligations(closing);
        let reports = RaiReport::new_chunks(&self.local_key, closing, obligations);
        if self.epoch_manager.start_closing(now) {
            // Store our own report through the same validation/deduplication
            // path before publishing it. Repeated ticks are consequently inert.
            for report in reports {
                let _ = self.epoch_manager.reports_mut().insert(report.clone());
                self.driver.broadcast_report(report);
            }
            // Do not bypass the report-collection barrier merely because the
            // reports already on hand happen to form W-F.  The next tick will
            // start the cut after full coverage or REPORT_COLLECTION_GRACE.
        }
    }

    fn maybe_start_cut(&mut self, epoch: RaiEpoch) {
        if self.epoch_manager.closing_epoch().is_none_or(|closing| {
            closing.epoch != epoch || closing.phase != RaiClosingPhase::CollectingReports
        }) || !self.epoch_manager.report_quorum_available(epoch)
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
        let Some(drain) = self.epoch_manager.happy_path_drain(epoch) else {
            return;
        };
        if !drain.is_complete() {
            return;
        }
        let Some(committee) = self.driver.certified_weights(epoch) else {
            return;
        };
        if let Some((root, hash)) = self.epoch_manager.begin_close_record(committee) {
            self.driver
                .start_close_election(RaiCloseKind::Record, epoch, 0, root, hash);
        }
    }

    fn close_election_changed(&mut self, kind: RaiCloseKind, epoch: RaiEpoch, round: u32) {
        if let Some(evidence) = self.driver.close_election_evidence(kind, epoch, round) {
            match kind {
                RaiCloseKind::Cut => {
                    self.epoch_manager
                        .store_close_cut_evidence(epoch, round, evidence);
                }
                RaiCloseKind::Record => {
                    self.epoch_manager
                        .store_close_record_evidence(epoch, round, evidence);
                }
            }
        }

        let Some(hash) = self.driver.close_election_winner(kind, epoch, round) else {
            let next = match kind {
                RaiCloseKind::Cut => self.epoch_manager.advance_close_cut_round(),
                RaiCloseKind::Record => self
                    .epoch_manager
                    .advance_close_record_round(self.driver.confirmation_heights()),
            };
            if let Some((root, hash)) = next {
                let next_round = match kind {
                    RaiCloseKind::Cut => self.epoch_manager.close_cut_round(epoch),
                    RaiCloseKind::Record => self.epoch_manager.close_record_round(epoch),
                };
                if let Some(next_round) = next_round {
                    self.driver
                        .start_close_election(kind, epoch, next_round, root, hash);
                }
            }
            return;
        };
        match kind {
            RaiCloseKind::Cut => {
                if self.epoch_manager.install_cut(epoch, round, hash).is_ok() {
                    let obligations = self
                        .epoch_manager
                        .obligations_to_drain(epoch)
                        .cloned()
                        .unwrap_or_default();
                    self.driver.apply_decided_cut(epoch, &obligations);
                    let _ = self
                        .epoch_manager
                        .initialize_drain_frontiers(epoch, self.driver.confirmation_heights());
                    self.maybe_start_record(epoch);
                }
            }
            RaiCloseKind::Record => {
                let Some(weights) = self.driver.certified_weights(epoch) else {
                    return;
                };
                let _ = self.epoch_manager.install_certified_close_record_after(
                    epoch,
                    round,
                    hash,
                    weights,
                    |epoch, frontiers| self.driver.commit_close_record(epoch, frontiers),
                );
            }
        }
    }
}
