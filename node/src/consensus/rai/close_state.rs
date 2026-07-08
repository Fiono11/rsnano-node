use std::collections::{BTreeSet, HashMap};

use rsnano_types::{Blake2HashBuilder, BlockHash, PublicKey, RaiEpoch, RaiPendingReport, RaiSlot};

pub type VisibleSlots = BTreeSet<RaiSlot>;

#[derive(Clone, Debug, Default)]
pub struct RaiCloseState {
    epochs: HashMap<RaiEpoch, RaiCloseEpochState>,
}

impl RaiCloseState {
    pub fn new() -> Self {
        Default::default()
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

    fn epoch(&self, epoch: RaiEpoch) -> Option<&RaiCloseEpochState> {
        self.epochs.get(&epoch)
    }

    fn epoch_mut(&mut self, epoch: RaiEpoch) -> &mut RaiCloseEpochState {
        self.epochs.entry(epoch).or_default()
    }
}

#[derive(Clone, Debug, Default)]
struct RaiCloseEpochState {
    pending_reports: HashMap<PublicKey, RaiPendingReport>,
    visibility: RaiVisibilityTracker,
}

impl RaiCloseEpochState {
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
    fn refreshing_visible_slots_maintains_close_values_after_first_snapshot() {
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

    fn slot(account_height: u64) -> RaiSlot {
        RaiSlot::new(Account::from(1), account_height)
    }
}
