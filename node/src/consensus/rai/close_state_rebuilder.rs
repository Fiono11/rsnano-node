use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rsnano_types::{
    BlockHash, PublicKey, RaiCloseAttempt, RaiElectionId, RaiElectionValue, RaiEpoch,
    RaiPendingReport, RaiSlot,
};

use super::{
    CloseRecordEntries, RaiActiveElectionsSnapshot, RaiCloseEpochSnapshot, RaiCloseRecordValue,
    RaiCloseRecordValueSnapshot, RaiCloseState, RaiCloseStateSnapshot, RaiCloseValueSnapshot,
    RaiClosedSlotSnapshot, RaiClosedSlotState, RaiCommitteeProvider, RaiCommitteeSet, RaiElection,
    RaiElectionOutcome, RaiElectionSnapshot, RaiElectionStatus, RaiEpochPhase,
    RaiVisibilityTracker, VisibleSlots,
};

pub struct RaiCloseStateRebuilder<'a> {
    committee_provider: &'a dyn RaiCommitteeProvider,
}

impl<'a> RaiCloseStateRebuilder<'a> {
    pub fn new(committee_provider: &'a dyn RaiCommitteeProvider) -> Self {
        Self { committee_provider }
    }

    pub fn rebuild(
        &self,
        close_state: Option<RaiCloseStateSnapshot>,
        active_elections: Option<&RaiActiveElectionsSnapshot>,
    ) -> RaiCloseState {
        RaiCloseState::from_snapshot(self.rebuild_snapshot(close_state, active_elections))
    }

    pub fn rebuild_snapshot(
        &self,
        close_state: Option<RaiCloseStateSnapshot>,
        active_elections: Option<&RaiActiveElectionsSnapshot>,
    ) -> RaiCloseStateSnapshot {
        let source = close_state.unwrap_or_default();
        let mut epochs = BTreeMap::new();
        for epoch in source.epochs {
            epochs.insert(epoch.epoch, EpochRebuildState::from_snapshot(epoch));
        }
        epochs
            .entry(source.current_epoch)
            .or_insert_with(|| EpochRebuildState::open(source.current_epoch));

        for state in epochs.values_mut() {
            self.rebuild_report_visibility(state);
        }

        if let Some(active_elections) = active_elections {
            self.rebuild_from_active_elections(&mut epochs, active_elections);
        }

        for state in epochs.values_mut() {
            state.rebuild_close_values();
        }

        if let Some(active_elections) = active_elections {
            self.rebuild_cut_sets(&mut epochs, active_elections);
            self.rebuild_closed_slots(&mut epochs, active_elections);
        } else {
            for state in epochs.values_mut() {
                state.retain_closed_slots_in_cut();
            }
        }

        let mut previous_close_hash = BlockHash::ZERO;
        let mut previous_frontiers = BTreeMap::new();
        for state in epochs.values_mut() {
            state.rebuild_close_record_values(previous_close_hash, &previous_frontiers);
            if let Some(close_hash) = state.close_hash {
                previous_close_hash = close_hash;
                if let Some(value) = state.close_record_values.get(&close_hash) {
                    previous_frontiers = value.record.frontiers.clone();
                }
            }
        }

        for state in epochs.values_mut() {
            state.rebuild_attempt_progress();
        }

        let current_epoch = normalize_current_epoch(source.current_epoch, &mut epochs);
        RaiCloseStateSnapshot {
            current_epoch,
            epochs: epochs
                .into_values()
                .map(EpochRebuildState::snapshot)
                .collect(),
        }
    }

    fn rebuild_report_visibility(&self, state: &mut EpochRebuildState) {
        let Some(committees) =
            self.committee_provider
                .try_committees_for(&RaiElectionId::CloseCut {
                    epoch: state.epoch,
                    attempt: 0,
                })
        else {
            return;
        };
        if committees.is_empty() {
            return;
        }

        let candidate_slots: BTreeSet<_> = state
            .pending_reports
            .values()
            .flat_map(|report| report.slots.iter().copied())
            .collect();
        for slot in candidate_slots {
            if report_visibility_reached(state, &slot, &committees) {
                state.visible_slots.insert(slot);
            }
        }
    }

    fn rebuild_from_active_elections(
        &self,
        epochs: &mut BTreeMap<RaiEpoch, EpochRebuildState>,
        active_elections: &RaiActiveElectionsSnapshot,
    ) {
        for election in &active_elections.elections {
            match &election.id {
                RaiElectionId::Slot { slot, epoch } => {
                    if self
                        .committee_provider
                        .try_committees_for(&election.id)
                        .is_some_and(|committees| vote_visibility_reached(election, &committees))
                    {
                        epochs
                            .entry(*epoch)
                            .or_insert_with(|| EpochRebuildState::open(*epoch))
                            .visible_slots
                            .insert(*slot);
                    }
                }
                RaiElectionId::CloseCut { epoch, attempt } => {
                    let state = epochs
                        .entry(*epoch)
                        .or_insert_with(|| EpochRebuildState::open(*epoch));
                    state.started_close_attempts.insert(*attempt);
                    if state.phase == RaiEpochPhase::Open {
                        state.phase = RaiEpochPhase::Closing;
                    }
                }
                RaiElectionId::CloseRecord { epoch, attempt } => {
                    let state = epochs
                        .entry(*epoch)
                        .or_insert_with(|| EpochRebuildState::open(*epoch));
                    state.started_close_record_attempts.insert(*attempt);
                    if state.phase == RaiEpochPhase::Open {
                        state.phase = RaiEpochPhase::Closing;
                    }
                }
            }
        }
    }

    fn rebuild_cut_sets(
        &self,
        epochs: &mut BTreeMap<RaiEpoch, EpochRebuildState>,
        active_elections: &RaiActiveElectionsSnapshot,
    ) {
        let mut candidates = Vec::new();
        for election in &active_elections.elections {
            let (epoch, attempt) = match &election.id {
                RaiElectionId::CloseCut { epoch, attempt } => (*epoch, *attempt),
                _ => continue,
            };
            let Some(committees) = self.committee_provider.try_committees_for(&election.id) else {
                continue;
            };
            let election = RaiElection::from_snapshot(election.clone());
            if let Some(RaiElectionOutcome::Fast(RaiElectionValue::CloseCutHash(hash))) =
                election.merged_outcome(&committees)
            {
                candidates.push((epoch, attempt, hash));
            }
        }
        candidates.sort_by_key(|(epoch, attempt, hash)| (*epoch, *attempt, *hash));

        for (epoch, attempt, hash) in candidates {
            let Some(state) = epochs.get_mut(&epoch) else {
                continue;
            };
            let Some(cut) = state.close_values.get(&hash).cloned() else {
                continue;
            };
            if state.cut_set.is_none() {
                state.cut_set = Some(cut);
            }
            if state.phase == RaiEpochPhase::Open {
                state.phase = RaiEpochPhase::Closing;
            }
            state.processed_close_attempts.insert(attempt);
        }
    }

    fn rebuild_closed_slots(
        &self,
        epochs: &mut BTreeMap<RaiEpoch, EpochRebuildState>,
        active_elections: &RaiActiveElectionsSnapshot,
    ) {
        let mut closed_slot_states = HashMap::new();
        for election in &active_elections.elections {
            let (slot, epoch) = match &election.id {
                RaiElectionId::Slot { slot, epoch } => (*slot, *epoch),
                _ => continue,
            };
            if election.status != RaiElectionStatus::DrainComplete {
                continue;
            }
            let Some(committees) = self.committee_provider.try_committees_for(&election.id) else {
                continue;
            };
            let election = RaiElection::from_snapshot(election.clone());
            let Some(outcome) = election.merged_outcome(&committees) else {
                continue;
            };
            if let Some(state) = closed_slot_state_from_outcome(outcome) {
                closed_slot_states.insert((epoch, slot), state);
            }
        }

        for (epoch, state) in epochs {
            let Some(cut) = state.cut_set.as_ref() else {
                state.closed_slots.clear();
                continue;
            };
            state.closed_slots.retain(|slot, _| cut.contains(slot));
            for slot in cut {
                if let Some(closed_state) = closed_slot_states.get(&(*epoch, *slot)) {
                    state.closed_slots.insert(*slot, *closed_state);
                }
            }
        }
    }
}

fn closed_slot_state_from_outcome(outcome: RaiElectionOutcome) -> Option<RaiClosedSlotState> {
    match outcome {
        RaiElectionOutcome::Fast(RaiElectionValue::Block(block))
        | RaiElectionOutcome::Final(RaiElectionValue::Block(block)) => {
            Some(RaiClosedSlotState::Finalized(block))
        }
        RaiElectionOutcome::Notarized(RaiElectionValue::Block(block)) => {
            Some(RaiClosedSlotState::Carry(block))
        }
        RaiElectionOutcome::Timeout => Some(RaiClosedSlotState::Released),
        RaiElectionOutcome::Notarized(_)
        | RaiElectionOutcome::Fast(_)
        | RaiElectionOutcome::Final(_)
        | RaiElectionOutcome::SafetyFault => None,
    }
}

#[derive(Clone, Debug)]
struct EpochRebuildState {
    epoch: RaiEpoch,
    phase: RaiEpochPhase,
    close_hash: Option<BlockHash>,
    pending_reports: BTreeMap<PublicKey, RaiPendingReport>,
    visible_slots: VisibleSlots,
    close_values: BTreeMap<BlockHash, VisibleSlots>,
    started_close_attempts: BTreeSet<RaiCloseAttempt>,
    processed_close_attempts: BTreeSet<RaiCloseAttempt>,
    cut_set: Option<VisibleSlots>,
    closed_slots: BTreeMap<RaiSlot, RaiClosedSlotState>,
    close_record_values: BTreeMap<BlockHash, RaiCloseRecordValue>,
    started_close_record_attempts: BTreeSet<RaiCloseAttempt>,
    processed_close_record_attempts: BTreeSet<RaiCloseAttempt>,
}

impl EpochRebuildState {
    fn open(epoch: RaiEpoch) -> Self {
        Self {
            epoch,
            phase: RaiEpochPhase::Open,
            close_hash: None,
            pending_reports: BTreeMap::new(),
            visible_slots: VisibleSlots::new(),
            close_values: BTreeMap::new(),
            started_close_attempts: BTreeSet::new(),
            processed_close_attempts: BTreeSet::new(),
            cut_set: None,
            closed_slots: BTreeMap::new(),
            close_record_values: BTreeMap::new(),
            started_close_record_attempts: BTreeSet::new(),
            processed_close_record_attempts: BTreeSet::new(),
        }
    }

    fn from_snapshot(snapshot: RaiCloseEpochSnapshot) -> Self {
        let mut state = Self::open(snapshot.epoch);
        state.phase = snapshot.phase;
        for report in snapshot.pending_reports {
            if report.epoch == snapshot.epoch && report.validate().is_ok() {
                state
                    .pending_reports
                    .entry(report.reporter)
                    .or_insert(report);
            }
        }
        state.visible_slots = snapshot.visible_slots.into_iter().collect();
        for close_value in snapshot.close_values {
            let slots: VisibleSlots = close_value.slots.into_iter().collect();
            if close_value.hash == RaiVisibilityTracker::hash_visible_slots(&slots) {
                state.close_values.insert(close_value.hash, slots);
            }
        }
        state.started_close_attempts = snapshot.started_close_attempts.into_iter().collect();
        state.processed_close_attempts = snapshot.processed_close_attempts.into_iter().collect();
        state.cut_set = snapshot.cut_set.map(|slots| slots.into_iter().collect());
        state.closed_slots = snapshot
            .closed_slots
            .into_iter()
            .map(|closed| (closed.slot, closed.state))
            .collect();
        for value in snapshot.close_record_values {
            let entries: CloseRecordEntries = value
                .states
                .into_iter()
                .map(|closed| (closed.slot, closed.state))
                .collect();
            if value.hash == value.record.hash() {
                state.close_record_values.insert(
                    value.hash,
                    RaiCloseRecordValue {
                        record: value.record,
                        entries,
                    },
                );
            }
        }
        state.close_hash = snapshot
            .close_hash
            .filter(|hash| state.close_record_values.contains_key(hash));
        state.started_close_record_attempts =
            snapshot.started_close_record_attempts.into_iter().collect();
        state.processed_close_record_attempts = snapshot
            .processed_close_record_attempts
            .into_iter()
            .collect();
        state
    }

    fn rebuild_close_values(&mut self) {
        if let Some(cut) = &self.cut_set {
            self.visible_slots.extend(cut.iter().copied());
        }

        self.close_values
            .retain(|_, slots| slots.iter().all(|slot| self.visible_slots.contains(slot)));

        if let Some(cut) = &self.cut_set {
            self.close_values
                .entry(RaiVisibilityTracker::hash_visible_slots(cut))
                .or_insert_with(|| cut.clone());
        }

        if !self.started_close_attempts.is_empty()
            || !self.close_values.is_empty()
            || self.cut_set.is_some()
        {
            let hash = RaiVisibilityTracker::hash_visible_slots(&self.visible_slots);
            self.close_values
                .entry(hash)
                .or_insert_with(|| self.visible_slots.clone());
        }
    }

    fn rebuild_attempt_progress(&mut self) {
        if self.cut_set.is_some() && self.phase == RaiEpochPhase::Open {
            self.phase = RaiEpochPhase::Closing;
        }

        let started = self.started_close_attempts.clone();
        self.processed_close_attempts
            .retain(|attempt| started.contains(attempt));

        if let Some(max_attempt) = started.iter().next_back().copied() {
            for attempt in started.range(..max_attempt) {
                self.processed_close_attempts.insert(*attempt);
            }
        }

        let started_records = self.started_close_record_attempts.clone();
        self.processed_close_record_attempts
            .retain(|attempt| started_records.contains(attempt));

        if let Some(max_attempt) = started_records.iter().next_back().copied() {
            for attempt in started_records.range(..max_attempt) {
                self.processed_close_record_attempts.insert(*attempt);
            }
        }
    }

    fn retain_closed_slots_in_cut(&mut self) {
        if let Some(cut) = &self.cut_set {
            self.closed_slots.retain(|slot, _| cut.contains(slot));
        } else {
            self.closed_slots.clear();
        }
    }

    fn rebuild_close_record_values(
        &mut self,
        previous_close_hash: BlockHash,
        previous_frontiers: &BTreeMap<rsnano_types::Account, BlockHash>,
    ) {
        let Some(entries) = self.current_close_record_entries() else {
            self.close_record_values.clear();
            self.close_hash = None;
            return;
        };

        let record = RaiCloseState::close_record_from_entries(
            self.epoch,
            previous_close_hash,
            previous_frontiers,
            &entries,
        );
        self.close_record_values.retain(|stored_hash, value| {
            if *stored_hash != value.record.hash()
                || value.record.epoch != self.epoch
                || value.record.previous_close_hash != previous_close_hash
                || value.entries != entries
            {
                return false;
            }
            if self.epoch > 0 {
                return value.record == record;
            }
            record
                .frontiers
                .iter()
                .all(|(account, frontier)| value.record.frontiers.get(account) == Some(frontier))
        });
        if !self.started_close_record_attempts.is_empty() || !self.close_record_values.is_empty() {
            if self.close_record_values.is_empty() {
                self.close_record_values
                    .insert(record.hash(), RaiCloseRecordValue { record, entries });
            }
        }
        if !self
            .close_hash
            .is_some_and(|hash| self.close_record_values.contains_key(&hash))
        {
            self.close_hash = None;
        }
    }

    fn current_close_record_entries(&self) -> Option<CloseRecordEntries> {
        let cut = self.cut_set.as_ref()?;
        let mut entries = CloseRecordEntries::new();
        for slot in cut {
            entries.insert(*slot, *self.closed_slots.get(slot)?);
        }
        Some(entries)
    }

    fn snapshot(self) -> RaiCloseEpochSnapshot {
        RaiCloseEpochSnapshot {
            epoch: self.epoch,
            phase: self.phase,
            close_hash: self.close_hash,
            pending_reports: self.pending_reports.into_values().collect(),
            visible_slots: self.visible_slots.into_iter().collect(),
            close_values: self
                .close_values
                .into_iter()
                .map(|(hash, slots)| RaiCloseValueSnapshot {
                    hash,
                    slots: slots.into_iter().collect(),
                })
                .collect(),
            started_close_attempts: self.started_close_attempts.into_iter().collect(),
            processed_close_attempts: self.processed_close_attempts.into_iter().collect(),
            cut_set: self.cut_set.map(|cut| cut.into_iter().collect()),
            closed_slots: self
                .closed_slots
                .into_iter()
                .map(|(slot, state)| RaiClosedSlotSnapshot { slot, state })
                .collect(),
            close_record_values: self
                .close_record_values
                .into_iter()
                .map(|(hash, value)| RaiCloseRecordValueSnapshot {
                    hash,
                    record: value.record,
                    states: value
                        .entries
                        .into_iter()
                        .map(|(slot, state)| RaiClosedSlotSnapshot { slot, state })
                        .collect(),
                })
                .collect(),
            started_close_record_attempts: self.started_close_record_attempts.into_iter().collect(),
            processed_close_record_attempts: self
                .processed_close_record_attempts
                .into_iter()
                .collect(),
        }
    }
}

fn report_visibility_reached(
    state: &EpochRebuildState,
    slot: &RaiSlot,
    committees: &RaiCommitteeSet,
) -> bool {
    committees.iter().all(|committee| {
        let report_count = state
            .pending_reports
            .values()
            .filter(|report| report.slots.contains(slot) && committee.contains(&report.reporter))
            .count();
        committee.has_visibility_quorum(report_count)
    })
}

fn vote_visibility_reached(election: &RaiElectionSnapshot, committees: &RaiCommitteeSet) -> bool {
    if committees.is_empty() {
        return false;
    }

    let voters: HashSet<_> = election
        .vote_states
        .iter()
        .map(|vote_state| vote_state.voter)
        .collect();
    committees.iter().any(|committee| {
        let vote_count = voters
            .iter()
            .filter(|voter| committee.contains(voter))
            .count();
        committee.has_visibility_quorum(vote_count)
    })
}

fn normalize_current_epoch(
    mut current_epoch: RaiEpoch,
    epochs: &mut BTreeMap<RaiEpoch, EpochRebuildState>,
) -> RaiEpoch {
    epochs
        .entry(current_epoch)
        .or_insert_with(|| EpochRebuildState::open(current_epoch));
    while epochs
        .get(&current_epoch)
        .is_some_and(|state| state.phase == RaiEpochPhase::Closed)
    {
        current_epoch += 1;
        epochs
            .entry(current_epoch)
            .or_insert_with(|| EpochRebuildState::open(current_epoch));
    }
    current_epoch
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{
        RaiCommittee, RaiCommitteeDeriver, RaiTallySnapshot, RaiVoteStateSnapshot,
    };
    use rsnano_types::{Account, Amount, PrivateKey};

    #[test]
    fn rebuilds_visibility_from_persisted_slot_votes() {
        let first = PrivateKey::from(1);
        let second = PrivateKey::from(2);
        let third = PrivateKey::from(3);
        let fourth = PrivateKey::from(4);
        let committee = committee_from_keys([&first, &second, &third, &fourth]);
        let slot = slot(1);
        let active_elections = RaiActiveElectionsSnapshot {
            elections: vec![slot_election_snapshot(
                slot,
                7,
                RaiElectionStatus::Active,
                vec![
                    vote_state(&first, 0, RaiElectionValue::Block(BlockHash::from(1))),
                    vote_state(&second, 0, RaiElectionValue::Block(BlockHash::from(1))),
                ],
                None,
            )],
        };

        let snapshot = rebuild_snapshot(
            committee,
            Some(close_snapshot(7, vec![close_epoch(7)])),
            Some(&active_elections),
        );

        assert_eq!(snapshot.epochs[0].visible_slots, vec![slot]);
    }

    #[test]
    fn rebuilds_visibility_from_persisted_pending_reports() {
        let first = PrivateKey::from(1);
        let second = PrivateKey::from(2);
        let third = PrivateKey::from(3);
        let fourth = PrivateKey::from(4);
        let committee = committee_from_keys([&first, &second, &third, &fourth]);
        let slot = slot(1);
        let mut epoch = close_epoch(7);
        epoch.pending_reports = vec![
            RaiPendingReport::new(&first, 7, vec![slot]),
            RaiPendingReport::new(&second, 7, vec![slot]),
        ];

        let snapshot = rebuild_snapshot(committee, Some(close_snapshot(7, vec![epoch])), None);

        assert_eq!(snapshot.epochs[0].visible_slots, vec![slot]);
    }

    #[test]
    fn records_current_close_value_after_rebuilding_visibility() {
        let first = PrivateKey::from(1);
        let second = PrivateKey::from(2);
        let third = PrivateKey::from(3);
        let fourth = PrivateKey::from(4);
        let committee = committee_from_keys([&first, &second, &third, &fourth]);
        let slot = slot(1);
        let mut epoch = close_epoch(7);
        epoch.started_close_attempts = vec![0];
        epoch.close_values = vec![RaiCloseValueSnapshot {
            hash: BlockHash::from(123),
            slots: vec![slot],
        }];
        let active_elections = RaiActiveElectionsSnapshot {
            elections: vec![slot_election_snapshot(
                slot,
                7,
                RaiElectionStatus::Active,
                vec![
                    vote_state(&first, 0, RaiElectionValue::Block(BlockHash::from(1))),
                    vote_state(&second, 0, RaiElectionValue::Block(BlockHash::from(1))),
                ],
                None,
            )],
        };

        let snapshot = rebuild_snapshot(
            committee,
            Some(close_snapshot(7, vec![epoch])),
            Some(&active_elections),
        );
        let visible: VisibleSlots = [slot].into_iter().collect();
        let close_hash = RaiVisibilityTracker::hash_visible_slots(&visible);

        assert_eq!(snapshot.epochs[0].close_values.len(), 1);
        assert_eq!(snapshot.epochs[0].close_values[0].hash, close_hash);
        assert_eq!(snapshot.epochs[0].close_values[0].slots, vec![slot]);
    }

    #[test]
    fn rebuilds_cut_from_fast_close_cut_certificate() {
        let key = PrivateKey::from(1);
        let committee = committee_from_keys([&key]);
        let slot = slot(1);
        let cut: VisibleSlots = [slot].into_iter().collect();
        let close_hash = RaiVisibilityTracker::hash_visible_slots(&cut);
        let mut epoch = close_epoch(7);
        epoch.phase = RaiEpochPhase::Closing;
        epoch.visible_slots = vec![slot];
        epoch.close_values = vec![RaiCloseValueSnapshot {
            hash: close_hash,
            slots: vec![slot],
        }];
        let active_elections = RaiActiveElectionsSnapshot {
            elections: vec![RaiElectionSnapshot {
                id: RaiElectionId::CloseCut {
                    epoch: 7,
                    attempt: 0,
                },
                status: RaiElectionStatus::Active,
                vote_states: vec![vote_state(
                    &key,
                    0,
                    RaiElectionValue::CloseCutHash(close_hash),
                )],
                tallies: vec![RaiTallySnapshot {
                    value: RaiElectionValue::CloseCutHash(close_hash),
                    per_committee: vec![1],
                }],
                notarization_tallies: Vec::new(),
                final_tallies: Vec::new(),
                winner: None,
                confirmed_value: None,
            }],
        };

        let snapshot = rebuild_snapshot(
            committee,
            Some(close_snapshot(7, vec![epoch])),
            Some(&active_elections),
        );

        assert_eq!(snapshot.epochs[0].cut_set, Some(vec![slot]));
        assert_eq!(snapshot.epochs[0].processed_close_attempts, vec![0]);
    }

    #[test]
    fn rebuilds_closed_slots_from_confirmed_cut_slot() {
        let key = PrivateKey::from(1);
        let committee = committee_from_keys([&key]);
        let slot = slot(1);
        let outcome = RaiElectionValue::Block(BlockHash::from(9));
        let mut epoch = close_epoch(7);
        epoch.phase = RaiEpochPhase::Closing;
        epoch.cut_set = Some(vec![slot]);
        let active_elections = RaiActiveElectionsSnapshot {
            elections: vec![slot_election_snapshot(
                slot,
                7,
                RaiElectionStatus::DrainComplete,
                vec![vote_state(&key, 0, outcome.clone())],
                Some(outcome.clone()),
            )],
        };

        let snapshot = rebuild_snapshot(
            committee,
            Some(close_snapshot(7, vec![epoch])),
            Some(&active_elections),
        );

        assert_eq!(
            snapshot.epochs[0].closed_slots,
            vec![RaiClosedSlotSnapshot {
                slot,
                state: RaiClosedSlotState::Finalized(BlockHash::from(9))
            }]
        );
    }

    fn rebuild_snapshot(
        committee: RaiCommittee,
        close_state: Option<RaiCloseStateSnapshot>,
        active_elections: Option<&RaiActiveElectionsSnapshot>,
    ) -> RaiCloseStateSnapshot {
        let provider = StaticCommitteeProvider { committee };
        RaiCloseStateRebuilder::new(&provider).rebuild_snapshot(close_state, active_elections)
    }

    fn close_snapshot(
        current_epoch: RaiEpoch,
        epochs: Vec<RaiCloseEpochSnapshot>,
    ) -> RaiCloseStateSnapshot {
        RaiCloseStateSnapshot {
            current_epoch,
            epochs,
        }
    }

    fn close_epoch(epoch: RaiEpoch) -> RaiCloseEpochSnapshot {
        RaiCloseEpochSnapshot {
            epoch,
            phase: RaiEpochPhase::Open,
            close_hash: None,
            pending_reports: Vec::new(),
            visible_slots: Vec::new(),
            close_values: Vec::new(),
            started_close_attempts: Vec::new(),
            processed_close_attempts: Vec::new(),
            cut_set: None,
            closed_slots: Vec::new(),
            close_record_values: Vec::new(),
            started_close_record_attempts: Vec::new(),
            processed_close_record_attempts: Vec::new(),
        }
    }

    fn slot_election_snapshot(
        slot: RaiSlot,
        epoch: RaiEpoch,
        status: RaiElectionStatus,
        vote_states: Vec<RaiVoteStateSnapshot>,
        confirmed_value: Option<RaiElectionValue>,
    ) -> RaiElectionSnapshot {
        let tallies = confirmed_value
            .iter()
            .map(|value| RaiTallySnapshot {
                value: value.clone(),
                per_committee: vec![vote_states.len()],
            })
            .collect();
        RaiElectionSnapshot {
            id: RaiElectionId::Slot { slot, epoch },
            status,
            vote_states,
            tallies,
            notarization_tallies: Vec::new(),
            final_tallies: Vec::new(),
            winner: confirmed_value.clone(),
            confirmed_value,
        }
    }

    fn vote_state(
        key: &PrivateKey,
        committee_index: usize,
        value: RaiElectionValue,
    ) -> RaiVoteStateSnapshot {
        RaiVoteStateSnapshot {
            voter: key.public_key(),
            committee_index,
            first: Some(value),
            notarized: Vec::new(),
            final_vote: None,
        }
    }

    fn committee_from_keys<const N: usize>(keys: [&PrivateKey; N]) -> RaiCommittee {
        RaiCommitteeDeriver::new().derive_committee(
            keys.map(|key| (key.public_key(), Amount::raw(100)))
                .into_iter(),
        )
    }

    fn slot(account_height: u64) -> RaiSlot {
        RaiSlot::new(Account::from(1), account_height)
    }

    struct StaticCommitteeProvider {
        committee: RaiCommittee,
    }

    impl RaiCommitteeProvider for StaticCommitteeProvider {
        fn genesis_committee(&self) -> RaiCommittee {
            self.committee.clone()
        }

        fn committee_for_closed_epoch(&self, _epoch: RaiEpoch) -> Option<RaiCommittee> {
            Some(self.committee.clone())
        }
    }
}
