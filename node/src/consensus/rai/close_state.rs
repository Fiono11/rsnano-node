use std::collections::{BTreeMap, BTreeSet, HashMap};

use rsnano_types::{
    Blake2HashBuilder, BlockHash, PublicKey, RaiCloseAttempt, RaiCloseRecord, RaiEpoch,
    RaiPendingReport, RaiSlot,
};

pub type VisibleSlots = BTreeSet<RaiSlot>;
pub type CloseRecordEntries = BTreeMap<RaiSlot, RaiClosedSlotState>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiClosedSlotState {
    Finalized(BlockHash),
    Carry(BlockHash),
    Released,
}

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

    pub fn is_slot_vote_acceptable(&self, epoch: RaiEpoch, slot: &RaiSlot) -> bool {
        let Some(state) = self.epoch(epoch) else {
            return true;
        };

        match state.phase {
            RaiEpochPhase::Open => true,
            RaiEpochPhase::Closing => state
                .cut_set
                .as_ref()
                .map_or(true, |cut| cut.contains(slot)),
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

    pub fn current_close_record_hash(
        &self,
        epoch: RaiEpoch,
    ) -> Result<BlockHash, RaiEpochTransitionError> {
        Ok(self.current_close_record(epoch)?.hash())
    }

    pub fn current_close_record(
        &self,
        epoch: RaiEpoch,
    ) -> Result<RaiCloseRecord, RaiEpochTransitionError> {
        let entries = self
            .epoch(epoch)
            .ok_or(RaiEpochTransitionError::CutMissing)?
            .current_close_record_entries()?;
        Ok(Self::close_record_from_entries(
            self.previous_close_hash(epoch)?,
            &entries,
        ))
    }

    pub fn record_current_close_record_value(
        &mut self,
        epoch: RaiEpoch,
    ) -> Result<BlockHash, RaiEpochTransitionError> {
        let previous_close_hash = self.previous_close_hash(epoch)?;
        self.epoch_mut(epoch)
            .record_current_close_record_value(previous_close_hash)
    }

    pub fn close_record_hash_from_entries(
        &self,
        epoch: RaiEpoch,
        entries: &CloseRecordEntries,
    ) -> Result<BlockHash, RaiEpochTransitionError> {
        Ok(Self::close_record_from_entries(self.previous_close_hash(epoch)?, entries).hash())
    }

    pub fn close_record_value(
        &self,
        epoch: RaiEpoch,
        hash: &BlockHash,
    ) -> Option<&RaiCloseRecordValue> {
        self.epoch(epoch)
            .and_then(|state| state.close_record_values.get(hash))
    }

    pub fn has_close_record_value(&self, epoch: RaiEpoch, hash: &BlockHash) -> bool {
        self.close_record_value(epoch, hash).is_some()
    }

    pub fn has_close_record_values(&self, epoch: RaiEpoch) -> bool {
        self.epoch(epoch)
            .is_some_and(|state| !state.close_record_values.is_empty())
    }

    pub fn certified_close_hash(&self, epoch: RaiEpoch) -> Option<BlockHash> {
        self.epoch(epoch).and_then(|state| state.close_hash)
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

    pub fn record_close_record_attempt_started(
        &mut self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    ) -> bool {
        self.epoch_mut(epoch)
            .started_close_record_attempts
            .insert(attempt)
    }

    pub fn close_record_attempt_started(&self, epoch: RaiEpoch, attempt: RaiCloseAttempt) -> bool {
        self.epoch(epoch)
            .is_some_and(|state| state.started_close_record_attempts.contains(&attempt))
    }

    pub fn started_close_record_attempts(&self, epoch: RaiEpoch) -> Vec<RaiCloseAttempt> {
        self.epoch(epoch)
            .map(|state| {
                state
                    .started_close_record_attempts
                    .iter()
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn record_close_record_attempt_processed(
        &mut self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    ) -> bool {
        self.epoch_mut(epoch)
            .processed_close_record_attempts
            .insert(attempt)
    }

    pub fn close_record_attempt_processed(
        &self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    ) -> bool {
        self.epoch(epoch)
            .is_some_and(|state| state.processed_close_record_attempts.contains(&attempt))
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
        states: impl IntoIterator<Item = (RaiSlot, RaiClosedSlotState)>,
    ) -> Result<(), RaiEpochTransitionError> {
        let state = self.epoch_mut(epoch);
        if state.cut_set.is_none() {
            return Err(RaiEpochTransitionError::CutMissing);
        }

        state.closed_slots.extend(states);
        Ok(())
    }

    pub fn closed_slot_state(
        &self,
        epoch: RaiEpoch,
        slot: &RaiSlot,
    ) -> Option<&RaiClosedSlotState> {
        self.epoch(epoch)
            .and_then(|state| state.closed_slots.get(slot))
    }

    pub fn cut_drained(&self, epoch: RaiEpoch) -> bool {
        self.epoch(epoch)
            .is_some_and(RaiCloseEpochState::cut_drained)
    }

    pub fn is_slot_vote_released(&self, epoch: RaiEpoch, slot: &RaiSlot) -> bool {
        let Some(state) = self.epoch(epoch) else {
            return false;
        };
        if state.phase != RaiEpochPhase::Closed {
            return false;
        }
        let Some(cut) = &state.cut_set else {
            return false;
        };
        if !cut.contains(slot) {
            return true;
        }

        matches!(
            state.closed_slots.get(slot),
            Some(RaiClosedSlotState::Released)
        )
    }

    pub fn hash_close_record_entries(entries: &CloseRecordEntries) -> BlockHash {
        let mut bytes = Vec::with_capacity(
            std::mem::size_of::<u64>()
                + entries
                    .len()
                    .saturating_mul(RaiSlot::SERIALIZED_SIZE + 1 + BlockHash::SERIALIZED_SIZE),
        );
        bytes.extend((entries.len() as u64).to_be_bytes());
        for (slot, state) in entries {
            slot.serialize(&mut bytes)
                .expect("writing to Vec should succeed");
            Self::write_closed_slot_state_bytes(&mut bytes, state);
        }

        Blake2HashBuilder::new()
            .update("rai close record value ")
            .update(bytes)
            .build()
    }

    pub fn hash_close_record_finalized_entries(entries: &CloseRecordEntries) -> BlockHash {
        let finalized: Vec<_> = entries
            .iter()
            .filter_map(|(slot, state)| match state {
                RaiClosedSlotState::Finalized(block) => Some((*slot, *block)),
                RaiClosedSlotState::Carry(_) | RaiClosedSlotState::Released => None,
            })
            .collect();

        Self::hash_close_record_block_entries(b"rai close record finalized ", &finalized)
    }

    pub fn hash_close_record_carry_entries(entries: &CloseRecordEntries) -> BlockHash {
        let carried: Vec<_> = entries
            .iter()
            .filter_map(|(slot, state)| match state {
                RaiClosedSlotState::Carry(block) => Some((*slot, *block)),
                RaiClosedSlotState::Finalized(_) | RaiClosedSlotState::Released => None,
            })
            .collect();

        Self::hash_close_record_block_entries(b"rai close record carry ", &carried)
    }

    pub fn close_record_from_entries(
        previous_close_hash: BlockHash,
        entries: &CloseRecordEntries,
    ) -> RaiCloseRecord {
        RaiCloseRecord::new(
            previous_close_hash,
            Self::hash_close_record_finalized_entries(entries),
            Self::hash_close_record_carry_entries(entries),
        )
    }

    fn hash_close_record_block_entries(
        domain: &[u8],
        entries: &[(RaiSlot, BlockHash)],
    ) -> BlockHash {
        let mut bytes = Vec::with_capacity(
            std::mem::size_of::<u64>()
                + entries
                    .len()
                    .saturating_mul(RaiSlot::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE),
        );
        bytes.extend((entries.len() as u64).to_be_bytes());
        for (slot, block) in entries {
            slot.serialize(&mut bytes)
                .expect("writing to Vec should succeed");
            block
                .serialize(&mut bytes)
                .expect("writing to Vec should succeed");
        }

        Blake2HashBuilder::new()
            .update(domain)
            .update(bytes)
            .build()
    }

    fn write_closed_slot_state_bytes(bytes: &mut Vec<u8>, state: &RaiClosedSlotState) {
        match state {
            RaiClosedSlotState::Finalized(block) => {
                bytes.push(0);
                block
                    .serialize(bytes)
                    .expect("writing to Vec should succeed");
            }
            RaiClosedSlotState::Carry(block) => {
                bytes.push(1);
                block
                    .serialize(bytes)
                    .expect("writing to Vec should succeed");
            }
            RaiClosedSlotState::Released => {
                bytes.push(2);
                BlockHash::ZERO
                    .serialize(bytes)
                    .expect("writing to Vec should succeed");
            }
        }
    }

    pub fn certify_close_record(
        &mut self,
        epoch: RaiEpoch,
        hash: &BlockHash,
    ) -> Result<(), RaiEpochTransitionError> {
        let state = self.epoch_mut(epoch);
        if !state.close_record_values.contains_key(hash) {
            return Err(RaiEpochTransitionError::CloseRecordMissing);
        }
        state.close_hash = Some(*hash);
        Ok(())
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

        if state.close_hash.is_none() {
            if state.close_record_values.len() == 1 {
                state.close_hash = state.close_record_values.keys().next().copied();
            } else {
                return Err(RaiEpochTransitionError::CloseRecordMissing);
            }
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

    fn previous_close_hash(&self, epoch: RaiEpoch) -> Result<BlockHash, RaiEpochTransitionError> {
        if epoch == 0 {
            return Ok(BlockHash::ZERO);
        }

        self.certified_close_hash(epoch - 1)
            .ok_or(RaiEpochTransitionError::CloseRecordMissing)
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
    pub close_hash: Option<BlockHash>,
    pub pending_reports: Vec<RaiPendingReport>,
    pub visible_slots: Vec<RaiSlot>,
    pub close_values: Vec<RaiCloseValueSnapshot>,
    pub started_close_attempts: Vec<RaiCloseAttempt>,
    pub processed_close_attempts: Vec<RaiCloseAttempt>,
    pub cut_set: Option<Vec<RaiSlot>>,
    pub closed_slots: Vec<RaiClosedSlotSnapshot>,
    pub close_record_values: Vec<RaiCloseRecordValueSnapshot>,
    pub started_close_record_attempts: Vec<RaiCloseAttempt>,
    pub processed_close_record_attempts: Vec<RaiCloseAttempt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseValueSnapshot {
    pub hash: BlockHash,
    pub slots: Vec<RaiSlot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiClosedSlotSnapshot {
    pub slot: RaiSlot,
    pub state: RaiClosedSlotState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseRecordValueSnapshot {
    pub hash: BlockHash,
    pub record: RaiCloseRecord,
    pub states: Vec<RaiClosedSlotSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseRecordValue {
    pub record: RaiCloseRecord,
    pub entries: CloseRecordEntries,
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
    CloseRecordMissing,
}

#[derive(Clone, Debug)]
struct RaiCloseEpochState {
    phase: RaiEpochPhase,
    close_hash: Option<BlockHash>,
    pending_reports: HashMap<PublicKey, RaiPendingReport>,
    visibility: RaiVisibilityTracker,
    started_close_attempts: BTreeSet<RaiCloseAttempt>,
    processed_close_attempts: BTreeSet<RaiCloseAttempt>,
    cut_set: Option<VisibleSlots>,
    closed_slots: HashMap<RaiSlot, RaiClosedSlotState>,
    close_record_values: HashMap<BlockHash, RaiCloseRecordValue>,
    started_close_record_attempts: BTreeSet<RaiCloseAttempt>,
    processed_close_record_attempts: BTreeSet<RaiCloseAttempt>,
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
            close_hash: None,
            pending_reports: HashMap::new(),
            visibility: RaiVisibilityTracker::new(),
            started_close_attempts: BTreeSet::new(),
            processed_close_attempts: BTreeSet::new(),
            cut_set: None,
            closed_slots: HashMap::new(),
            close_record_values: HashMap::new(),
            started_close_record_attempts: BTreeSet::new(),
            processed_close_record_attempts: BTreeSet::new(),
        }
    }

    fn from_snapshot(snapshot: RaiCloseEpochSnapshot) -> Self {
        Self {
            phase: snapshot.phase,
            close_hash: snapshot.close_hash,
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
                .map(|closed| (closed.slot, closed.state))
                .collect(),
            close_record_values: snapshot
                .close_record_values
                .into_iter()
                .map(|value| {
                    (
                        value.hash,
                        RaiCloseRecordValue {
                            record: value.record,
                            entries: value
                                .states
                                .into_iter()
                                .map(|closed| (closed.slot, closed.state))
                                .collect(),
                        },
                    )
                })
                .collect(),
            started_close_record_attempts: snapshot
                .started_close_record_attempts
                .into_iter()
                .collect(),
            processed_close_record_attempts: snapshot
                .processed_close_record_attempts
                .into_iter()
                .collect(),
        }
    }

    fn snapshot(&self, epoch: RaiEpoch) -> RaiCloseEpochSnapshot {
        let mut pending_reports: Vec<_> = self.pending_reports.values().cloned().collect();
        pending_reports.sort_by_key(|report| report.reporter);

        let mut closed_slots: Vec<_> = self
            .closed_slots
            .iter()
            .map(|(slot, state)| RaiClosedSlotSnapshot {
                slot: *slot,
                state: *state,
            })
            .collect();
        closed_slots.sort_by_key(|closed| closed.slot);

        let mut close_record_values: Vec<_> = self
            .close_record_values
            .iter()
            .map(|(hash, value)| {
                let mut states: Vec<_> = value
                    .entries
                    .iter()
                    .map(|(slot, state)| RaiClosedSlotSnapshot {
                        slot: *slot,
                        state: *state,
                    })
                    .collect();
                states.sort_by_key(|closed| closed.slot);
                RaiCloseRecordValueSnapshot {
                    hash: *hash,
                    record: value.record,
                    states,
                }
            })
            .collect();
        close_record_values.sort_by_key(|value| value.hash);

        RaiCloseEpochSnapshot {
            epoch,
            phase: self.phase,
            close_hash: self.close_hash,
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
            close_record_values,
            started_close_record_attempts: self
                .started_close_record_attempts
                .iter()
                .copied()
                .collect(),
            processed_close_record_attempts: self
                .processed_close_record_attempts
                .iter()
                .copied()
                .collect(),
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

    fn current_close_record_entries(&self) -> Result<CloseRecordEntries, RaiEpochTransitionError> {
        let Some(cut) = &self.cut_set else {
            return Err(RaiEpochTransitionError::CutMissing);
        };

        let mut entries = CloseRecordEntries::new();
        for slot in cut {
            let Some(state) = self.closed_slots.get(slot) else {
                return Err(RaiEpochTransitionError::CutNotDrained);
            };
            entries.insert(*slot, *state);
        }
        Ok(entries)
    }

    fn record_current_close_record_value(
        &mut self,
        previous_close_hash: BlockHash,
    ) -> Result<BlockHash, RaiEpochTransitionError> {
        let entries = self.current_close_record_entries()?;
        let record = RaiCloseState::close_record_from_entries(previous_close_hash, &entries);
        let hash = record.hash();
        self.close_record_values
            .entry(hash)
            .or_insert(RaiCloseRecordValue { record, entries });
        Ok(hash)
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
        let closed_state = RaiClosedSlotState::Finalized(BlockHash::from(7));

        assert_eq!(state.start_closing(0), Ok(()));
        assert_eq!(state.current_epoch_phase(), RaiEpochPhase::Closing);
        assert_eq!(state.install_cut(0, cut), Ok(true));
        assert_eq!(state.record_cut_drain(0, [(slot(1), closed_state)]), Ok(()));
        assert_eq!(
            state.advance_epoch(0),
            Err(RaiEpochTransitionError::CloseRecordMissing)
        );
        state.record_current_close_record_value(0).unwrap();
        assert_eq!(state.advance_epoch(0), Ok(1));

        assert_eq!(state.current_epoch(), 1);
        assert_eq!(state.epoch_phase(0), Some(RaiEpochPhase::Closed));
        assert_eq!(state.current_epoch_phase(), RaiEpochPhase::Open);
        assert_eq!(state.closed_slot_state(0, &slot(1)), Some(&closed_state));
    }

    #[test]
    fn records_close_record_value_after_cut_drain() {
        let mut state = RaiCloseState::new();
        let cut = [slot(1)].into_iter().collect();
        let closed_state = RaiClosedSlotState::Finalized(BlockHash::from(7));
        state.start_closing(0).unwrap();
        state.install_cut(0, cut).unwrap();

        assert_eq!(
            state.record_current_close_record_value(0),
            Err(RaiEpochTransitionError::CutNotDrained)
        );

        state
            .record_cut_drain(0, [(slot(1), closed_state)])
            .unwrap();
        let hash = state.record_current_close_record_value(0).unwrap();

        assert!(state.has_close_record_value(0, &hash));
        assert_eq!(
            state
                .close_record_value(0, &hash)
                .unwrap()
                .entries
                .get(&slot(1)),
            Some(&closed_state)
        );
    }

    #[test]
    fn close_record_roots_split_finalized_carry_and_released_slots() {
        let mut entries = CloseRecordEntries::new();
        entries.insert(slot(1), RaiClosedSlotState::Finalized(BlockHash::from(7)));
        entries.insert(slot(2), RaiClosedSlotState::Carry(BlockHash::from(8)));
        entries.insert(slot(3), RaiClosedSlotState::Released);

        let record = RaiCloseState::close_record_from_entries(BlockHash::from(6), &entries);

        assert_eq!(
            record.finalized_root,
            RaiCloseState::hash_close_record_finalized_entries(&entries)
        );
        assert_eq!(
            record.carry_root,
            RaiCloseState::hash_close_record_carry_entries(&entries)
        );

        let mut no_carry = entries.clone();
        no_carry.insert(slot(2), RaiClosedSlotState::Released);
        assert_ne!(
            record.carry_root,
            RaiCloseState::close_record_from_entries(BlockHash::from(6), &no_carry).carry_root
        );
    }

    #[test]
    fn release_requires_closed_epoch_with_certified_release_case() {
        let mut excluded = RaiCloseState::new();
        excluded.start_closing(0).unwrap();
        excluded.install_cut(0, VisibleSlots::new()).unwrap();
        excluded
            .record_cut_drain(0, std::iter::empty::<(RaiSlot, RaiClosedSlotState)>())
            .unwrap();
        excluded.record_current_close_record_value(0).unwrap();
        excluded.advance_epoch(0).unwrap();
        assert!(excluded.is_slot_vote_released(0, &slot(1)));

        let mut timed_out = RaiCloseState::new();
        timed_out.start_closing(0).unwrap();
        timed_out
            .install_cut(0, [slot(1)].into_iter().collect())
            .unwrap();
        timed_out
            .record_cut_drain(0, [(slot(1), RaiClosedSlotState::Released)])
            .unwrap();
        timed_out.record_current_close_record_value(0).unwrap();
        timed_out.advance_epoch(0).unwrap();
        assert!(timed_out.is_slot_vote_released(0, &slot(1)));

        let mut carried_or_finalized = RaiCloseState::new();
        carried_or_finalized.start_closing(0).unwrap();
        carried_or_finalized
            .install_cut(0, [slot(1)].into_iter().collect())
            .unwrap();
        carried_or_finalized
            .record_cut_drain(
                0,
                [(slot(1), RaiClosedSlotState::Carry(BlockHash::from(7)))],
            )
            .unwrap();
        carried_or_finalized
            .record_current_close_record_value(0)
            .unwrap();
        carried_or_finalized.advance_epoch(0).unwrap();
        assert!(!carried_or_finalized.is_slot_vote_released(0, &slot(1)));
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
    fn closing_epoch_accepts_passive_votes_until_cut_is_installed() {
        let mut state = RaiCloseState::new();

        assert!(state.is_slot_vote_acceptable(0, &slot(1)));
        state.start_closing(0).unwrap();
        assert!(state.is_slot_vote_acceptable(0, &slot(1)));
        state
            .install_cut(0, [slot(1)].into_iter().collect())
            .unwrap();

        assert!(state.is_slot_vote_acceptable(0, &slot(1)));
        assert!(!state.is_slot_vote_acceptable(0, &slot(2)));
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
