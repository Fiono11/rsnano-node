use std::collections::{BTreeSet, HashMap};

use rsnano_types::{
    Blake2HashBuilder, BlockHash, PublicKey, RaiCloseAttempt, RaiElectionValue, RaiEpoch,
    RaiPendingReport, RaiSlot,
};

pub type VisibleSlots = BTreeSet<RaiSlot>;

#[derive(Clone, Debug)]
pub struct RaiCloseState {
    current_epoch: RaiEpoch,
    epochs: HashMap<RaiEpoch, RaiCloseEpochState>,
}

impl RaiCloseState {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn from_snapshot(snapshot: RaiCloseStateSnapshot) -> Self {
        let mut epochs = HashMap::new();
        for epoch in snapshot.epochs {
            epochs.insert(epoch.epoch, RaiCloseEpochState::from_snapshot(epoch));
        }
        epochs
            .entry(snapshot.current_epoch)
            .or_insert_with(RaiCloseEpochState::open);

        Self {
            current_epoch: snapshot.current_epoch,
            epochs,
        }
    }

    pub fn snapshot(&self) -> RaiCloseStateSnapshot {
        let mut epochs: Vec<_> = self
            .epochs
            .iter()
            .map(|(epoch, state)| state.snapshot(*epoch))
            .collect();
        epochs.sort_by_key(|epoch| epoch.epoch);

        RaiCloseStateSnapshot {
            current_epoch: self.current_epoch,
            epochs,
        }
    }

    pub fn current_epoch(&self) -> RaiEpoch {
        self.current_epoch
    }

    pub fn current_epoch_phase(&self) -> RaiEpochPhase {
        self.epoch_phase(self.current_epoch)
            .unwrap_or(RaiEpochPhase::Open)
    }

    pub fn epoch_phase(&self, epoch: RaiEpoch) -> Option<RaiEpochPhase> {
        self.epoch(epoch).map(|state| state.phase)
    }

    pub fn start_closing(&mut self, epoch: RaiEpoch) -> Result<(), RaiEpochTransitionError> {
        if epoch != self.current_epoch {
            return Err(RaiEpochTransitionError::NotCurrent);
        }

        let state = self.epoch_mut(epoch);
        match state.phase {
            RaiEpochPhase::Open => {
                state.phase = RaiEpochPhase::Closing;
                Ok(())
            }
            RaiEpochPhase::Closing => Ok(()),
            RaiEpochPhase::Closed => Err(RaiEpochTransitionError::AlreadyClosed),
        }
    }

    pub fn is_slot_vote_enabled(&self, epoch: RaiEpoch, slot: &RaiSlot) -> bool {
        let Some(state) = self.epoch(epoch) else {
            return true;
        };

        match state.phase {
            RaiEpochPhase::Open => true,
            RaiEpochPhase::Closing => state.cut_set.as_ref().is_some_and(|cut| cut.contains(slot)),
            RaiEpochPhase::Closed => false,
        }
    }

    pub fn insert_pending_report(
        &mut self,
        report: RaiPendingReport,
    ) -> Result<(), RaiPendingReportInsertError> {
        self.epoch_mut(report.epoch).insert_pending_report(report)
    }

    pub fn pending_report(
        &self,
        epoch: RaiEpoch,
        reporter: &PublicKey,
    ) -> Option<&RaiPendingReport> {
        self.epoch(epoch)
            .and_then(|state| state.pending_report(reporter))
    }

    pub fn pending_report_count(&self, epoch: RaiEpoch) -> usize {
        self.epoch(epoch)
            .map(|state| state.pending_report_count())
            .unwrap_or_default()
    }

    pub fn pending_reports(&self, epoch: RaiEpoch) -> Vec<&RaiPendingReport> {
        self.epoch(epoch)
            .map(|state| state.pending_reports.values().collect())
            .unwrap_or_default()
    }

    pub fn mark_visible(&mut self, epoch: RaiEpoch, slot: RaiSlot) -> bool {
        self.epoch_mut(epoch).visibility.mark_visible(slot)
    }

    pub fn mark_visible_slots(
        &mut self,
        epoch: RaiEpoch,
        slots: impl IntoIterator<Item = RaiSlot>,
    ) -> bool {
        self.epoch_mut(epoch).visibility.mark_visible_slots(slots)
    }

    pub fn is_visible(&self, epoch: RaiEpoch, slot: &RaiSlot) -> bool {
        self.epoch(epoch)
            .is_some_and(|state| state.visibility.is_visible(slot))
    }

    pub fn visible_slots(&self, epoch: RaiEpoch) -> Option<&VisibleSlots> {
        self.epoch(epoch)
            .map(|state| state.visibility.visible_slots())
    }

    pub fn current_close_hash(&self, epoch: RaiEpoch) -> BlockHash {
        self.epoch(epoch)
            .map(|state| state.visibility.current_close_hash())
            .unwrap_or_else(|| RaiVisibilityTracker::hash_visible_slots(&VisibleSlots::new()))
    }

    pub fn record_current_close_value(&mut self, epoch: RaiEpoch) -> BlockHash {
        self.epoch_mut(epoch)
            .visibility
            .record_current_close_value()
    }

    pub fn close_value(&self, epoch: RaiEpoch, hash: &BlockHash) -> Option<&VisibleSlots> {
        self.epoch(epoch)
            .and_then(|state| state.visibility.close_value(hash))
    }

    pub fn close_values(&self, epoch: RaiEpoch) -> Option<&HashMap<BlockHash, VisibleSlots>> {
        self.epoch(epoch)
            .map(|state| state.visibility.close_values())
    }

    pub fn has_close_values(&self, epoch: RaiEpoch) -> bool {
        self.close_values(epoch)
            .is_some_and(|values| !values.is_empty())
    }

    pub fn record_close_attempt_started(
        &mut self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    ) -> bool {
        self.epoch_mut(epoch).started_close_attempts.insert(attempt)
    }

    pub fn close_attempt_started(&self, epoch: RaiEpoch, attempt: RaiCloseAttempt) -> bool {
        self.epoch(epoch)
            .is_some_and(|state| state.started_close_attempts.contains(&attempt))
    }

    pub fn started_close_attempts(&self, epoch: RaiEpoch) -> Vec<RaiCloseAttempt> {
        self.epoch(epoch)
            .map(|state| state.started_close_attempts.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn record_close_attempt_processed(
        &mut self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    ) -> bool {
        self.epoch_mut(epoch)
            .processed_close_attempts
            .insert(attempt)
    }

    pub fn close_attempt_processed(&self, epoch: RaiEpoch, attempt: RaiCloseAttempt) -> bool {
        self.epoch(epoch)
            .is_some_and(|state| state.processed_close_attempts.contains(&attempt))
    }

    pub fn install_cut(
        &mut self,
        epoch: RaiEpoch,
        cut: VisibleSlots,
    ) -> Result<bool, RaiEpochTransitionError> {
        let state = self.epoch_mut(epoch);
        if state.phase != RaiEpochPhase::Closing {
            return Err(RaiEpochTransitionError::NotClosing);
        }

        if state.cut_set.is_some() {
            return Ok(false);
        }

        state.cut_set = Some(cut);
        Ok(true)
    }

    pub fn cut_set(&self, epoch: RaiEpoch) -> Option<&VisibleSlots> {
        self.epoch(epoch).and_then(|state| state.cut_set.as_ref())
    }

    pub fn record_cut_drain(
        &mut self,
        epoch: RaiEpoch,
        outcomes: impl IntoIterator<Item = (RaiSlot, RaiElectionValue)>,
    ) -> Result<(), RaiEpochTransitionError> {
        let state = self.epoch_mut(epoch);
        if state.cut_set.is_none() {
            return Err(RaiEpochTransitionError::CutMissing);
        }

        state.closed_slots.extend(outcomes);
        Ok(())
    }

    pub fn closed_slot_outcome(
        &self,
        epoch: RaiEpoch,
        slot: &RaiSlot,
    ) -> Option<&RaiElectionValue> {
        self.epoch(epoch)
            .and_then(|state| state.closed_slots.get(slot))
    }

    pub fn cut_drained(&self, epoch: RaiEpoch) -> bool {
        self.epoch(epoch)
            .is_some_and(RaiCloseEpochState::cut_drained)
    }

    pub fn advance_epoch(&mut self, epoch: RaiEpoch) -> Result<RaiEpoch, RaiEpochTransitionError> {
        if epoch != self.current_epoch {
            return Err(RaiEpochTransitionError::NotCurrent);
        }

        let state = self.epoch_mut(epoch);
        if state.phase != RaiEpochPhase::Closing {
            return Err(RaiEpochTransitionError::NotClosing);
        }

        if state.cut_set.is_none() {
            return Err(RaiEpochTransitionError::CutMissing);
        }

        if !state.cut_drained() {
            return Err(RaiEpochTransitionError::CutNotDrained);
        }

        state.phase = RaiEpochPhase::Closed;
        self.current_epoch = epoch + 1;
        self.epochs
            .entry(self.current_epoch)
            .or_insert_with(RaiCloseEpochState::open);
        Ok(self.current_epoch)
    }

    fn epoch(&self, epoch: RaiEpoch) -> Option<&RaiCloseEpochState> {
        self.epochs.get(&epoch)
    }

    fn epoch_mut(&mut self, epoch: RaiEpoch) -> &mut RaiCloseEpochState {
        self.epochs.entry(epoch).or_default()
    }
}

impl Default for RaiCloseState {
    fn default() -> Self {
        let current_epoch = 0;
        let mut epochs = HashMap::new();
        epochs.insert(current_epoch, RaiCloseEpochState::open());
        Self {
            current_epoch,
            epochs,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RaiCloseStateSnapshot {
    pub current_epoch: RaiEpoch,
    pub epochs: Vec<RaiCloseEpochSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseEpochSnapshot {
    pub epoch: RaiEpoch,
    pub phase: RaiEpochPhase,
    pub pending_reports: Vec<RaiPendingReport>,
    pub visible_slots: Vec<RaiSlot>,
    pub close_values: Vec<RaiCloseValueSnapshot>,
    pub started_close_attempts: Vec<RaiCloseAttempt>,
    pub processed_close_attempts: Vec<RaiCloseAttempt>,
    pub cut_set: Option<Vec<RaiSlot>>,
    pub closed_slots: Vec<RaiClosedSlotSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseValueSnapshot {
    pub hash: BlockHash,
    pub slots: Vec<RaiSlot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiClosedSlotSnapshot {
    pub slot: RaiSlot,
    pub outcome: RaiElectionValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiEpochPhase {
    Open,
    Closing,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiEpochTransitionError {
    NotCurrent,
    NotClosing,
    AlreadyClosed,
    CutMissing,
    CutNotDrained,
}

#[derive(Clone, Debug)]
struct RaiCloseEpochState {
    phase: RaiEpochPhase,
    pending_reports: HashMap<PublicKey, RaiPendingReport>,
    visibility: RaiVisibilityTracker,
    started_close_attempts: BTreeSet<RaiCloseAttempt>,
    processed_close_attempts: BTreeSet<RaiCloseAttempt>,
    cut_set: Option<VisibleSlots>,
    closed_slots: HashMap<RaiSlot, RaiElectionValue>,
}

impl Default for RaiCloseEpochState {
    fn default() -> Self {
        Self::open()
    }
}

impl RaiCloseEpochState {
    fn open() -> Self {
        Self {
            phase: RaiEpochPhase::Open,
            pending_reports: HashMap::new(),
            visibility: RaiVisibilityTracker::new(),
            started_close_attempts: BTreeSet::new(),
            processed_close_attempts: BTreeSet::new(),
            cut_set: None,
            closed_slots: HashMap::new(),
        }
    }

    fn from_snapshot(snapshot: RaiCloseEpochSnapshot) -> Self {
        Self {
            phase: snapshot.phase,
            pending_reports: snapshot
                .pending_reports
                .into_iter()
                .map(|report| (report.reporter, report))
                .collect(),
            visibility: RaiVisibilityTracker::from_snapshot(
                snapshot.visible_slots,
                snapshot.close_values,
            ),
            started_close_attempts: snapshot.started_close_attempts.into_iter().collect(),
            processed_close_attempts: snapshot.processed_close_attempts.into_iter().collect(),
            cut_set: snapshot.cut_set.map(|slots| slots.into_iter().collect()),
            closed_slots: snapshot
                .closed_slots
                .into_iter()
                .map(|closed| (closed.slot, closed.outcome))
                .collect(),
        }
    }

    fn snapshot(&self, epoch: RaiEpoch) -> RaiCloseEpochSnapshot {
        let mut pending_reports: Vec<_> = self.pending_reports.values().cloned().collect();
        pending_reports.sort_by_key(|report| report.reporter);

        let mut closed_slots: Vec<_> = self
            .closed_slots
            .iter()
            .map(|(slot, outcome)| RaiClosedSlotSnapshot {
                slot: *slot,
                outcome: outcome.clone(),
            })
            .collect();
        closed_slots.sort_by_key(|closed| closed.slot);

        RaiCloseEpochSnapshot {
            epoch,
            phase: self.phase,
            pending_reports,
            visible_slots: self.visibility.visible_slots.iter().copied().collect(),
            close_values: self.visibility.close_value_snapshots(),
            started_close_attempts: self.started_close_attempts.iter().copied().collect(),
            processed_close_attempts: self.processed_close_attempts.iter().copied().collect(),
            cut_set: self
                .cut_set
                .as_ref()
                .map(|cut| cut.iter().copied().collect()),
            closed_slots,
        }
    }

    fn insert_pending_report(
        &mut self,
        report: RaiPendingReport,
    ) -> Result<(), RaiPendingReportInsertError> {
        if self.pending_reports.contains_key(&report.reporter) {
            return Err(RaiPendingReportInsertError::Duplicate);
        }

        self.pending_reports.insert(report.reporter, report);
        Ok(())
    }

    fn pending_report(&self, reporter: &PublicKey) -> Option<&RaiPendingReport> {
        self.pending_reports.get(reporter)
    }

    fn pending_report_count(&self) -> usize {
        self.pending_reports.len()
    }

    fn cut_drained(&self) -> bool {
        self.cut_set
            .as_ref()
            .is_some_and(|cut| cut.iter().all(|slot| self.closed_slots.contains_key(slot)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiPendingReportInsertError {
    Duplicate,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RaiVisibilityTracker {
    visible_slots: VisibleSlots,
    close_values: HashMap<BlockHash, VisibleSlots>,
}

impl RaiVisibilityTracker {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn from_snapshot(
        visible_slots: impl IntoIterator<Item = RaiSlot>,
        close_values: impl IntoIterator<Item = RaiCloseValueSnapshot>,
    ) -> Self {
        Self {
            visible_slots: visible_slots.into_iter().collect(),
            close_values: close_values
                .into_iter()
                .map(|value| (value.hash, value.slots.into_iter().collect()))
                .collect(),
        }
    }

    pub fn mark_visible(&mut self, slot: RaiSlot) -> bool {
        let inserted = self.visible_slots.insert(slot);
        if inserted {
            self.record_current_close_value_if_started();
        }
        inserted
    }

    pub fn mark_visible_slots(&mut self, slots: impl IntoIterator<Item = RaiSlot>) -> bool {
        let mut changed = false;
        for slot in slots {
            changed |= self.visible_slots.insert(slot);
        }

        if changed {
            self.record_current_close_value_if_started();
        }
        changed
    }

    pub fn is_visible(&self, slot: &RaiSlot) -> bool {
        self.visible_slots.contains(slot)
    }

    pub fn visible_slots(&self) -> &VisibleSlots {
        &self.visible_slots
    }

    pub fn current_close_hash(&self) -> BlockHash {
        Self::hash_visible_slots(&self.visible_slots)
    }

    pub fn record_current_close_value(&mut self) -> BlockHash {
        let hash = self.current_close_hash();
        self.close_values
            .entry(hash)
            .or_insert_with(|| self.visible_slots.clone());
        hash
    }

    pub fn close_value(&self, hash: &BlockHash) -> Option<&VisibleSlots> {
        self.close_values.get(hash)
    }

    pub fn close_values(&self) -> &HashMap<BlockHash, VisibleSlots> {
        &self.close_values
    }

    pub fn close_value_snapshots(&self) -> Vec<RaiCloseValueSnapshot> {
        let mut snapshots: Vec<_> = self
            .close_values
            .iter()
            .map(|(hash, slots)| RaiCloseValueSnapshot {
                hash: *hash,
                slots: slots.iter().copied().collect(),
            })
            .collect();
        snapshots.sort_by_key(|snapshot| snapshot.hash);
        snapshots
    }

    pub fn hash_visible_slots(slots: &VisibleSlots) -> BlockHash {
        let mut bytes =
            Vec::with_capacity(std::mem::size_of::<u64>() + slots.len() * RaiSlot::SERIALIZED_SIZE);
        bytes.extend((slots.len() as u64).to_be_bytes());
        for slot in slots {
            slot.serialize(&mut bytes)
                .expect("writing to Vec should succeed");
        }

        Blake2HashBuilder::new()
            .update("rai close value ")
            .update(bytes)
            .build()
    }

    fn record_current_close_value_if_started(&mut self) {
        if !self.close_values.is_empty() {
            self.record_current_close_value();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{Account, PrivateKey};

    #[test]
    fn stores_one_pending_report_per_reporter_per_epoch() {
        let key = PrivateKey::from(1);
        let reporter = key.public_key();
        let mut state = RaiCloseState::new();
        let first = RaiPendingReport::new(&key, 7, vec![slot(1)]);
        let duplicate = RaiPendingReport::new(&key, 7, vec![slot(2)]);

        assert_eq!(state.insert_pending_report(first.clone()), Ok(()));
        assert_eq!(
            state.insert_pending_report(duplicate),
            Err(RaiPendingReportInsertError::Duplicate)
        );

        assert_eq!(state.pending_report_count(7), 1);
        assert_eq!(state.pending_report(7, &reporter), Some(&first));
        assert_eq!(
            state.insert_pending_report(RaiPendingReport::new(&key, 8, vec![slot(2)])),
            Ok(())
        );
        assert_eq!(state.pending_report_count(8), 1);
    }

    #[test]
    fn stores_reports_from_different_reporters_in_the_same_epoch() {
        let key1 = PrivateKey::from(1);
        let key2 = PrivateKey::from(2);
        let mut state = RaiCloseState::new();

        assert_eq!(
            state.insert_pending_report(RaiPendingReport::new(&key1, 7, vec![slot(1)])),
            Ok(())
        );
        assert_eq!(
            state.insert_pending_report(RaiPendingReport::new(&key2, 7, vec![slot(2)])),
            Ok(())
        );

        assert_eq!(state.pending_report_count(7), 2);
    }

    #[test]
    fn tracks_visible_slots() {
        let mut state = RaiCloseState::new();

        assert_eq!(state.mark_visible(7, slot(2)), true);
        assert_eq!(state.mark_visible(7, slot(2)), false);
        assert_eq!(state.mark_visible(7, slot(1)), true);

        assert!(state.is_visible(7, &slot(1)));
        assert!(state.is_visible(7, &slot(2)));
        assert_eq!(
            state
                .visible_slots(7)
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![slot(1), slot(2)]
        );
    }

    #[test]
    fn records_current_close_value_for_visible_snapshot() {
        let mut state = RaiCloseState::new();
        state.mark_visible_slots(7, [slot(1), slot(2)]);

        let hash = state.record_current_close_value(7);

        assert_eq!(
            state
                .close_value(7, &hash)
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![slot(1), slot(2)]
        );
        assert_eq!(state.current_close_hash(7), hash);
    }

    #[test]
    fn expanding_visible_epoch_records_new_close_hash() {
        let mut state = RaiCloseState::new();
        state.mark_visible(7, slot(1));
        let first_hash = state.record_current_close_value(7);

        assert!(state.mark_visible_slots(7, [slot(2), slot(3)]));
        let second_hash = state.current_close_hash(7);

        assert_ne!(first_hash, second_hash);
        assert_eq!(
            state
                .close_value(7, &first_hash)
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![slot(1)]
        );
        assert_eq!(
            state
                .close_value(7, &second_hash)
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![slot(1), slot(2), slot(3)]
        );
        assert_eq!(state.close_values(7).unwrap().len(), 2);
    }

    #[test]
    fn close_hash_is_stable_across_visible_slot_insertion_order() {
        let mut left = RaiVisibilityTracker::new();
        let mut right = RaiVisibilityTracker::new();

        left.mark_visible_slots([slot(1), slot(2), slot(3)]);
        right.mark_visible_slots([slot(3), slot(1), slot(2)]);

        assert_eq!(left.current_close_hash(), right.current_close_hash());
    }

    #[test]
    fn initializes_epoch_zero_open() {
        let state = RaiCloseState::new();

        assert_eq!(state.current_epoch(), 0);
        assert_eq!(state.current_epoch_phase(), RaiEpochPhase::Open);
    }

    #[test]
    fn transitions_open_to_closing_to_closed_and_opens_next_epoch() {
        let mut state = RaiCloseState::new();
        let cut = [slot(1)].into_iter().collect();
        let outcome = RaiElectionValue::Block(BlockHash::from(7));

        assert_eq!(state.start_closing(0), Ok(()));
        assert_eq!(state.current_epoch_phase(), RaiEpochPhase::Closing);
        assert_eq!(state.install_cut(0, cut), Ok(true));
        assert_eq!(
            state.record_cut_drain(0, [(slot(1), outcome.clone())]),
            Ok(())
        );
        assert_eq!(state.advance_epoch(0), Ok(1));

        assert_eq!(state.current_epoch(), 1);
        assert_eq!(state.epoch_phase(0), Some(RaiEpochPhase::Closed));
        assert_eq!(state.current_epoch_phase(), RaiEpochPhase::Open);
        assert_eq!(state.closed_slot_outcome(0, &slot(1)), Some(&outcome));
    }

    #[test]
    fn closing_epoch_only_enables_votes_for_installed_cut() {
        let mut state = RaiCloseState::new();

        assert!(state.is_slot_vote_enabled(0, &slot(1)));
        state.start_closing(0).unwrap();
        assert!(!state.is_slot_vote_enabled(0, &slot(1)));
        state
            .install_cut(0, [slot(1)].into_iter().collect())
            .unwrap();

        assert!(state.is_slot_vote_enabled(0, &slot(1)));
        assert!(!state.is_slot_vote_enabled(0, &slot(2)));
    }

    #[test]
    fn records_close_attempt_progress_once() {
        let mut state = RaiCloseState::new();

        assert!(state.record_close_attempt_started(0, 0));
        assert!(!state.record_close_attempt_started(0, 0));
        assert_eq!(state.started_close_attempts(0), vec![0]);
        assert!(state.close_attempt_started(0, 0));

        assert!(state.record_close_attempt_processed(0, 0));
        assert!(!state.record_close_attempt_processed(0, 0));
        assert!(state.close_attempt_processed(0, 0));
    }

    fn slot(account_height: u64) -> RaiSlot {
        RaiSlot::new(Account::from(1), account_height)
    }
}
