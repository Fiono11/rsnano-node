use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use rsnano_ledger::{RepWeightCache, RepWeights};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, BlockHash, ConfirmationHeightInfo, QualifiedRoot, RaiEpoch, RaiSlotId,
};

use super::{
    CloseCutDecisionError, RaiCloseCut, RaiCloseCutStore, RaiCloseRecord, RaiCloseRecordStore,
    RaiFrontierMap, RaiReportStore,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RaiClosingPhase {
    #[default]
    CollectingReports,
    ElectingCut,
    Draining,
    ElectingRecord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaiClosingEpoch {
    pub epoch: RaiEpoch,
    pub phase: RaiClosingPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiDrainOutcome {
    Finalized(BlockHash),
    Selected(BlockHash),
    ReleasedTimeout,
    ReleasedConflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaiCertifiedRelease {
    pub close_epoch: RaiEpoch,
    pub close_record_hash: BlockHash,
}

/// Certificate-derived resolution of every election frozen into a close cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiHappyPathDrain {
    pub epoch: RaiEpoch,
    pub obligations: BTreeSet<RaiSlotId>,
    pub finalized: BTreeMap<RaiSlotId, BlockHash>,
    pub selected: BTreeMap<RaiSlotId, BlockHash>,
    pub released: BTreeMap<RaiSlotId, RaiDrainOutcome>,
}

impl RaiHappyPathDrain {
    pub fn is_complete(&self) -> bool {
        self.obligations.iter().all(|slot| {
            self.finalized.contains_key(slot)
                || self.selected.contains_key(slot)
                || self.released.contains_key(slot)
        })
    }

    /// Resolves an obligation from persistent certificate evidence. Releases
    /// never advance the close-local frontier.
    pub fn record_persistent_evidence(
        &mut self,
        slot: &RaiSlotId,
        evidence: &super::RaiElectionVoteState,
    ) -> Option<RaiDrainOutcome> {
        if !self.obligations.contains(slot) || evidence.committees.is_empty() {
            return None;
        }
        if let Some(hash) = self.finalized.get(slot) {
            return Some(RaiDrainOutcome::Finalized(*hash));
        }
        if let Some(hash) = self.selected.get(slot) {
            return Some(RaiDrainOutcome::Selected(*hash));
        }
        if let Some(outcome) = self.released.get(slot) {
            return Some(*outcome);
        }
        if (0..evidence.committees.len())
            .any(|committee| evidence.has_timeout_certificate(committee))
        {
            self.released
                .insert(slot.clone(), RaiDrainOutcome::ReleasedTimeout);
            return Some(RaiDrainOutcome::ReleasedTimeout);
        }
        let mut certified = None;
        for committee in 0..evidence.committees.len() {
            let hash = match evidence.local_result(committee) {
                Some(super::RaiLocalResult::Fast(hash) | super::RaiLocalResult::Final(hash)) => {
                    hash
                }
                Some(super::RaiLocalResult::Timeout) => {
                    self.released
                        .insert(slot.clone(), RaiDrainOutcome::ReleasedConflict);
                    return Some(RaiDrainOutcome::ReleasedConflict);
                }
                Some(super::RaiLocalResult::Notarized(hash)) => hash,
                None => return None,
            };
            if certified
                .replace(hash)
                .is_some_and(|previous| previous != hash)
            {
                self.released
                    .insert(slot.clone(), RaiDrainOutcome::ReleasedConflict);
                return Some(RaiDrainOutcome::ReleasedConflict);
            }
        }
        let hash = certified?;
        let globally_strong = (0..evidence.committees.len()).all(|committee| {
            matches!(
                evidence.local_result(committee),
                Some(super::RaiLocalResult::Fast(candidate) | super::RaiLocalResult::Final(candidate))
                    if candidate == hash
            )
        });
        let target = if globally_strong {
            &mut self.finalized
        } else {
            &mut self.selected
        };
        match target.entry(slot.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(hash);
                Some(if globally_strong {
                    RaiDrainOutcome::Finalized(hash)
                } else {
                    RaiDrainOutcome::Selected(hash)
                })
            }
            std::collections::btree_map::Entry::Occupied(entry) => (*entry.get() == hash)
                .then_some(if globally_strong {
                    RaiDrainOutcome::Finalized(hash)
                } else {
                    RaiDrainOutcome::Selected(hash)
                }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaiEpochState {
    pub open_epoch: RaiEpoch,
    pub open_started_at: Timestamp,
    pub closing: Option<RaiClosingEpoch>,
    pub closed_through: Option<RaiEpoch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseRecordDecisionError {
    WrongPhase,
    MissingPreimage,
    InvalidRecord,
    ImmutableDecision,
}

impl std::fmt::Display for CloseRecordDecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongPhase => "epoch is not closing its record",
            Self::MissingPreimage => "canonical close-record preimage is unavailable",
            Self::InvalidRecord => "close record does not match confirmation heights",
            Self::ImmutableDecision => "the epoch already has a different close record",
        })
    }
}

impl std::error::Error for CloseRecordDecisionError {}

/// Owns the immutable representative-weight views used by RAI elections.
///
/// The live representative cache is deliberately not retained. A caller must
/// explicitly record a snapshot, which makes it impossible for later cache
/// updates to alter an already recorded committee.
pub struct RaiEpochManager {
    state: RaiEpochState,
    genesis_governing_hash: BlockHash,
    genesis_committee: Arc<RepWeights>,
    committees: BTreeMap<RaiEpoch, Arc<RepWeights>>,
    cut_hashes: BTreeMap<RaiEpoch, BlockHash>,
    close_hashes: BTreeMap<RaiEpoch, BlockHash>,
    reports: RaiReportStore,
    close_cuts: RaiCloseCutStore,
    close_records: RaiCloseRecordStore,
    visible_obligations: BTreeMap<RaiEpoch, BTreeSet<RaiSlotId>>,
    frozen_obligations: BTreeMap<RaiEpoch, BTreeSet<RaiSlotId>>,
    drains: BTreeMap<RaiEpoch, RaiHappyPathDrain>,
    known_slots: BTreeSet<RaiSlotId>,
    released_slots: BTreeMap<RaiSlotId, RaiCertifiedRelease>,
    drain_frontiers: BTreeMap<RaiEpoch, BTreeMap<Account, ConfirmationHeightInfo>>,
    cut_rounds: BTreeMap<RaiEpoch, super::RaiCloseRoundTracker>,
    record_rounds: BTreeMap<RaiEpoch, super::RaiCloseRoundTracker>,
}

/// Minimum state which must be written atomically with close installation.
/// The frontier map is retained because reconstructing an old confirmation-
/// height view from the mutable ledger is not possible after it advances.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiDurableCloseState {
    pub epoch: RaiEpoch,
    pub close_hash: BlockHash,
    pub previous_close_hash: BlockHash,
    pub frontiers: RaiFrontierMap,
    pub committee: RepWeights,
    pub current_epoch: RaiEpoch,
    pub committees: BTreeMap<RaiEpoch, RepWeights>,
    pub decided_cut_hash: BlockHash,
    pub frozen_obligations: BTreeSet<RaiSlotId>,
}

/// Restart image for an in-progress close. Evidence is retained, while
/// death/carry is deliberately recomputed after restore.
#[derive(Clone, Debug)]
pub struct RaiDurableCloseRoundState {
    pub epoch: RaiEpoch,
    pub phase: RaiClosingPhase,
    pub cut_rounds: Option<super::RaiCloseRoundTracker>,
    pub record_rounds: Option<super::RaiCloseRoundTracker>,
    pub close_cuts: RaiCloseCutStore,
    pub close_records: RaiCloseRecordStore,
    pub visible_obligations: Option<BTreeSet<RaiSlotId>>,
}

impl RaiEpochManager {
    pub fn new(genesis_committee: Arc<RepWeights>, genesis_governing_hash: BlockHash) -> Self {
        Self {
            state: RaiEpochState {
                open_epoch: RaiEpoch::ZERO,
                open_started_at: Timestamp::default(),
                closing: None,
                closed_through: None,
            },
            genesis_governing_hash,
            genesis_committee,
            committees: BTreeMap::new(),
            cut_hashes: BTreeMap::new(),
            close_hashes: BTreeMap::new(),
            reports: RaiReportStore::default(),
            close_cuts: RaiCloseCutStore::default(),
            close_records: RaiCloseRecordStore::default(),
            visible_obligations: BTreeMap::new(),
            frozen_obligations: BTreeMap::new(),
            drains: BTreeMap::new(),
            known_slots: BTreeSet::new(),
            released_slots: BTreeMap::new(),
            drain_frontiers: BTreeMap::new(),
            cut_rounds: BTreeMap::new(),
            record_rounds: BTreeMap::new(),
        }
    }

    /// Freezes the currently visible weights for `epoch`.
    pub fn snapshot_committee(
        &mut self,
        epoch: RaiEpoch,
        live_weights: &RepWeightCache,
    ) -> Arc<RepWeights> {
        self.committees
            .entry(epoch)
            .or_insert_with(|| Arc::new(live_weights.read().clone()))
            .clone()
    }

    /// Installs an already frozen snapshot, for example while restoring state.
    pub fn insert_committee(
        &mut self,
        epoch: RaiEpoch,
        committee: Arc<RepWeights>,
    ) -> Option<Arc<RepWeights>> {
        self.committees.insert(epoch, committee)
    }

    pub fn record_close_hash(
        &mut self,
        epoch: RaiEpoch,
        close_hash: BlockHash,
    ) -> Option<BlockHash> {
        match self.close_hashes.entry(epoch) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(close_hash);
                None
            }
            std::collections::btree_map::Entry::Occupied(entry) => Some(*entry.get()),
        }
    }

    pub fn current_epoch(&self) -> RaiEpoch {
        self.state.open_epoch
    }

    pub fn state(&self) -> &RaiEpochState {
        &self.state
    }

    pub fn set_open_started_at(&mut self, started_at: Timestamp) {
        self.state.open_started_at = started_at;
    }

    pub fn closing_epoch(&self) -> Option<RaiClosingEpoch> {
        self.state.closing
    }

    /// Test/bootstrap helper. Production epoch advancement is performed only
    /// by `start_closing`.
    pub fn open_epoch(&mut self, epoch: RaiEpoch) {
        self.state.open_epoch = epoch;
        self.state.closing = None;
    }

    /// Starts closing the open epoch and immediately opens its successor.
    /// Returning `false` leaves an existing close untouched.
    pub fn start_closing(&mut self, now: Timestamp) -> bool {
        if self.state.closing.is_some() {
            return false;
        }
        let closing = self.state.open_epoch;
        let Some(next) = closing.number().checked_add(1).map(RaiEpoch::new) else {
            return false;
        };
        self.state.closing = Some(RaiClosingEpoch {
            epoch: closing,
            phase: RaiClosingPhase::CollectingReports,
        });
        self.state.open_epoch = next;
        self.state.open_started_at = now;
        true
    }

    pub fn reports(&self) -> &RaiReportStore {
        &self.reports
    }
    pub fn reports_mut(&mut self) -> &mut RaiReportStore {
        &mut self.reports
    }
    pub fn close_cuts(&self) -> &RaiCloseCutStore {
        &self.close_cuts
    }

    pub fn report_quorum_available(&self, epoch: RaiEpoch) -> bool {
        self.close_committee(epoch)
            .is_some_and(|committee| self.reports.has_quorum(epoch, &committee))
    }

    /// Freezes visibility and creates the canonical round-zero candidate.
    pub fn begin_cut_election(
        &mut self,
        vote_visible: impl IntoIterator<Item = RaiSlotId>,
    ) -> Option<(QualifiedRoot, BlockHash)> {
        let closing = self.state.closing?;
        if closing.phase != RaiClosingPhase::CollectingReports {
            return None;
        }
        let epoch = closing.epoch;
        let committee = self.close_committee(epoch)?;
        if !self.reports.has_quorum(epoch, &committee) {
            return None;
        }
        let mut visible = self.reports.visible_from_reports(epoch, &committee);
        visible.extend(vote_visible);
        let cut = RaiCloseCut::new(epoch, visible.clone());
        let hash = self.close_cuts.insert(cut);
        self.visible_obligations.insert(epoch, visible);
        self.state.closing.as_mut().unwrap().phase = RaiClosingPhase::ElectingCut;
        self.cut_rounds
            .entry(epoch)
            .or_insert_with(|| super::RaiCloseRoundTracker::new(super::RaiCloseKind::Cut, epoch))
            .start_round_zero(hash);
        Some((super::rai_close_cut_root(epoch, 0), hash))
    }

    /// Rebuild an undecided fresh cut as authenticated visibility grows.
    /// Fresh values are replica-relative until certificate support, so the
    /// active round may adopt a newly converged validated preimage.
    pub fn refresh_close_cut_candidate(
        &mut self,
        epoch: RaiEpoch,
        round: u32,
        vote_visible: impl IntoIterator<Item = RaiSlotId>,
    ) -> Option<BlockHash> {
        if self.state.closing
            != Some(RaiClosingEpoch {
                epoch,
                phase: RaiClosingPhase::ElectingCut,
            })
            || self.cut_hashes.contains_key(&epoch)
        {
            return None;
        }
        let committee = self.close_committee(epoch)?;
        if !self.reports.has_quorum(epoch, &committee) {
            return None;
        }
        let mut visible = self.reports.visible_from_reports(epoch, &committee);
        visible.extend(vote_visible);
        let hash = self
            .close_cuts
            .insert(RaiCloseCut::new(epoch, visible.clone()));
        self.visible_obligations.insert(epoch, visible);
        self.cut_rounds
            .get_mut(&epoch)?
            .add_validated_preimage(round, hash);
        Some(hash)
    }

    pub fn decide_close_cut(
        &mut self,
        epoch: RaiEpoch,
        round: u32,
        hash: BlockHash,
    ) -> Result<&BTreeSet<RaiSlotId>, CloseCutDecisionError> {
        if let Some(existing) = self.cut_hashes.get(&epoch) {
            return if *existing == hash {
                self.frozen_obligations
                    .get(&epoch)
                    .ok_or(CloseCutDecisionError::MissingPreimage)
            } else {
                Err(CloseCutDecisionError::ImmutableDecision)
            };
        }
        if self.state.closing
            != Some(RaiClosingEpoch {
                epoch,
                phase: RaiClosingPhase::ElectingCut,
            })
        {
            return Err(CloseCutDecisionError::WrongPhase);
        }
        let cut = self
            .close_cuts
            .get(&hash)
            .cloned()
            .ok_or(CloseCutDecisionError::MissingPreimage)?;
        let expected = self
            .visible_obligations
            .get(&epoch)
            .ok_or(CloseCutDecisionError::InvalidCut)?;
        if cut.epoch != epoch || &cut.obligations != expected || cut.hash() != hash {
            return Err(CloseCutDecisionError::InvalidCut);
        }
        let tracker = self
            .cut_rounds
            .get_mut(&epoch)
            .ok_or(CloseCutDecisionError::WrongPhase)?;
        if !tracker.decide(round, hash) {
            return Err(CloseCutDecisionError::MissingPreimage);
        }
        self.cut_hashes.insert(epoch, hash);
        self.known_slots.extend(cut.obligations.iter().cloned());
        self.frozen_obligations.insert(epoch, cut.obligations);
        self.drains.insert(
            epoch,
            RaiHappyPathDrain {
                epoch,
                obligations: self.frozen_obligations[&epoch].clone(),
                finalized: BTreeMap::new(),
                selected: BTreeMap::new(),
                released: BTreeMap::new(),
            },
        );
        self.state.closing.as_mut().unwrap().phase = RaiClosingPhase::Draining;
        Ok(self.frozen_obligations.get(&epoch).expect("just inserted"))
    }

    pub fn install_cut(
        &mut self,
        epoch: RaiEpoch,
        round: u32,
        hash: BlockHash,
    ) -> Result<&BTreeSet<RaiSlotId>, CloseCutDecisionError> {
        self.decide_close_cut(epoch, round, hash)
    }

    pub fn decided_close_hash(&self, epoch: RaiEpoch) -> Option<BlockHash> {
        self.cut_hashes.get(&epoch).copied()
    }

    pub fn decided_cut_hashes(&self) -> &BTreeMap<RaiEpoch, BlockHash> {
        &self.cut_hashes
    }

    pub fn close_cut_round(&self, epoch: RaiEpoch) -> Option<u32> {
        self.cut_rounds
            .get(&epoch)
            .map(|rounds| rounds.current_round())
    }

    pub fn close_cut_tracker(&self, epoch: RaiEpoch) -> Option<&super::RaiCloseRoundTracker> {
        self.cut_rounds.get(&epoch)
    }

    pub fn store_close_cut_evidence(
        &mut self,
        epoch: RaiEpoch,
        round: u32,
        evidence: super::RaiElectionVoteState,
    ) -> bool {
        self.cut_rounds
            .get_mut(&epoch)
            .is_some_and(|rounds| rounds.store_evidence(round, evidence))
    }

    /// Called by the existing election scheduler after new vote evidence is
    /// stored. `None` means the source round is still live/unknown and must not
    /// be advanced merely because its local timer expired.
    pub fn advance_close_cut_round(&mut self) -> Option<(QualifiedRoot, BlockHash)> {
        let closing = self.state.closing?;
        let epoch = closing.epoch;
        if closing.phase != RaiClosingPhase::ElectingCut || self.cut_hashes.contains_key(&epoch) {
            return None;
        }
        let fresh = self.visible_obligations.get(&epoch).map(|obligations| {
            self.close_cuts
                .insert(RaiCloseCut::new(epoch, obligations.clone()))
        })?;
        let action = self.cut_rounds.get_mut(&epoch)?.next(fresh);
        let (round, hash) = match action {
            super::RaiCloseRoundAction::StartFresh { round, hash }
            | super::RaiCloseRoundAction::StartCarry { round, hash } => (round, hash),
            _ => return None,
        };
        self.cut_rounds
            .get_mut(&epoch)?
            .add_validated_preimage(round, hash);
        Some((super::rai_close_cut_root(epoch, round), hash))
    }

    /// Derives the sole round-zero close candidate after every cut obligation
    /// has reached the existing confirmation-height state.
    pub fn begin_close_record(&mut self) -> Option<(QualifiedRoot, BlockHash)> {
        let closing = self.state.closing?;
        if closing.phase != RaiClosingPhase::Draining
            || !self.drains.get(&closing.epoch)?.is_complete()
        {
            return None;
        }
        let epoch = closing.epoch;
        let previous = self
            .state
            .closing
            .unwrap()
            .epoch
            .number()
            .checked_sub(1)
            .and_then(|e| self.close_hashes.get(&RaiEpoch::new(e)).copied())
            .unwrap_or(BlockHash::ZERO);
        let record =
            RaiCloseRecord::new(epoch, previous, self.drain_frontiers.get(&epoch)?.clone());
        let hash = self.close_records.insert(record);
        self.state.closing.as_mut().unwrap().phase = RaiClosingPhase::ElectingRecord;
        self.record_rounds
            .entry(epoch)
            .or_insert_with(|| super::RaiCloseRoundTracker::new(super::RaiCloseKind::Record, epoch))
            .start_round_zero(hash);
        Some((super::rai_close_record_root(epoch, 0), hash))
    }

    pub fn install_close_record(
        &mut self,
        epoch: RaiEpoch,
        round: u32,
        hash: BlockHash,
    ) -> Result<&RaiFrontierMap, CloseRecordDecisionError> {
        let certified_weights = self
            .close_committee(epoch)
            .ok_or(CloseRecordDecisionError::MissingPreimage)?
            .as_ref()
            .clone();
        self.install_certified_close_record(epoch, round, hash, certified_weights)
    }

    pub fn install_certified_close_record(
        &mut self,
        epoch: RaiEpoch,
        round: u32,
        hash: BlockHash,
        certified_weights: RepWeights,
    ) -> Result<&RaiFrontierMap, CloseRecordDecisionError> {
        if let Some(existing) = self.close_hashes.get(&epoch) {
            return if *existing == hash {
                Ok(&self
                    .close_records
                    .get(&hash)
                    .ok_or(CloseRecordDecisionError::MissingPreimage)?
                    .frontiers)
            } else {
                Err(CloseRecordDecisionError::ImmutableDecision)
            };
        }
        if self.state.closing
            != Some(RaiClosingEpoch {
                epoch,
                phase: RaiClosingPhase::ElectingRecord,
            })
        {
            return Err(CloseRecordDecisionError::WrongPhase);
        }
        let record = self
            .close_records
            .get(&hash)
            .ok_or(CloseRecordDecisionError::MissingPreimage)?;
        let actual = self
            .drain_frontiers
            .get(&epoch)
            .ok_or(CloseRecordDecisionError::MissingPreimage)?;
        let previous = epoch
            .number()
            .checked_sub(1)
            .and_then(|e| self.close_hashes.get(&RaiEpoch::new(e)).copied())
            .unwrap_or(BlockHash::ZERO);
        if record.epoch != epoch
            || record.previous != previous
            || &record.frontiers != actual
            || record.hash() != hash
        {
            return Err(CloseRecordDecisionError::InvalidRecord);
        }
        let tracker = self
            .record_rounds
            .get_mut(&epoch)
            .ok_or(CloseRecordDecisionError::WrongPhase)?;
        if !tracker.decide(round, hash) {
            return Err(CloseRecordDecisionError::MissingPreimage);
        }
        self.close_hashes.insert(epoch, hash);
        self.committees
            .entry(epoch)
            .or_insert_with(|| Arc::new(certified_weights));
        self.state.closed_through = Some(epoch);
        if let Some(drain) = self.drains.get_mut(&epoch) {
            drain.finalized.append(&mut drain.selected);
        }
        for slot in self
            .known_slots
            .iter()
            .filter(|slot| slot.epoch <= epoch)
            .cloned()
        {
            self.released_slots
                .entry(slot)
                .or_insert(RaiCertifiedRelease {
                    close_epoch: epoch,
                    close_record_hash: hash,
                });
        }
        self.state.closing = None;
        Ok(&self
            .close_records
            .get(&hash)
            .expect("validated above")
            .frontiers)
    }

    pub fn installed_close_hash(&self, epoch: RaiEpoch) -> Option<BlockHash> {
        self.close_hashes.get(&epoch).copied()
    }

    pub fn close_record_round(&self, epoch: RaiEpoch) -> Option<u32> {
        self.record_rounds
            .get(&epoch)
            .map(|rounds| rounds.current_round())
    }

    pub fn close_record_tracker(&self, epoch: RaiEpoch) -> Option<&super::RaiCloseRoundTracker> {
        self.record_rounds.get(&epoch)
    }

    pub fn store_close_record_evidence(
        &mut self,
        epoch: RaiEpoch,
        round: u32,
        evidence: super::RaiElectionVoteState,
    ) -> bool {
        self.record_rounds
            .get_mut(&epoch)
            .is_some_and(|rounds| rounds.store_evidence(round, evidence))
    }

    pub fn advance_close_record_round(
        &mut self,
        _confirmation_heights: impl IntoIterator<
            Item = (rsnano_types::Account, rsnano_types::ConfirmationHeightInfo),
        >,
    ) -> Option<(QualifiedRoot, BlockHash)> {
        let closing = self.state.closing?;
        let epoch = closing.epoch;
        if closing.phase != RaiClosingPhase::ElectingRecord
            || self.close_hashes.contains_key(&epoch)
        {
            return None;
        }
        let previous = epoch
            .number()
            .checked_sub(1)
            .and_then(|e| self.close_hashes.get(&RaiEpoch::new(e)).copied())
            .unwrap_or(BlockHash::ZERO);
        // Close-record retries are derived from the immutable close-local
        // replay captured while draining. Ordinary confirmation-height writes
        // after that point must not perturb a fresh retry candidate.
        let frontiers = self.drain_frontiers.get(&epoch)?.clone();
        let fresh = self
            .close_records
            .insert(RaiCloseRecord::new(epoch, previous, frontiers));
        let action = self.record_rounds.get_mut(&epoch)?.next(fresh);
        let (round, hash) = match action {
            super::RaiCloseRoundAction::StartFresh { round, hash }
            | super::RaiCloseRoundAction::StartCarry { round, hash } => (round, hash),
            _ => return None,
        };
        // Carries can only be returned when their opening was already stored;
        // fresh values are inserted immediately above.
        self.record_rounds
            .get_mut(&epoch)?
            .add_validated_preimage(round, hash);
        Some((super::rai_close_record_root(epoch, round), hash))
    }

    pub fn durable_close_state(&self, epoch: RaiEpoch) -> Option<RaiDurableCloseState> {
        let close_hash = *self.close_hashes.get(&epoch)?;
        let record = self.close_records.get(&close_hash)?;
        Some(RaiDurableCloseState {
            epoch,
            close_hash,
            previous_close_hash: record.previous,
            frontiers: record.frontiers.clone(),
            committee: self.close_committee(epoch)?.as_ref().clone(),
            current_epoch: self.state.open_epoch,
            committees: self
                .committees
                .iter()
                .map(|(epoch, committee)| (*epoch, committee.as_ref().clone()))
                .collect(),
            decided_cut_hash: *self.cut_hashes.get(&epoch)?,
            frozen_obligations: self.frozen_obligations.get(&epoch)?.clone(),
        })
    }

    /// Restores the safety locks before the node accepts votes after restart.
    pub fn restore_close_state(
        &mut self,
        state: RaiDurableCloseState,
    ) -> Result<(), CloseRecordDecisionError> {
        let record = RaiCloseRecord::new(
            state.epoch,
            state.previous_close_hash,
            state.frontiers.clone(),
        );
        if record.hash() != state.close_hash {
            return Err(CloseRecordDecisionError::InvalidRecord);
        }
        if let Some(existing) = self.close_hashes.get(&state.epoch)
            && *existing != state.close_hash
        {
            return Err(CloseRecordDecisionError::ImmutableDecision);
        }
        self.close_records.insert(record);
        self.close_hashes.insert(state.epoch, state.close_hash);
        self.cut_hashes.insert(state.epoch, state.decided_cut_hash);
        self.frozen_obligations
            .insert(state.epoch, state.frozen_obligations);
        for (epoch, committee) in state.committees {
            match self.committees.entry(epoch) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Arc::new(committee));
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if entry.get().as_ref() != &committee =>
                {
                    return Err(CloseRecordDecisionError::ImmutableDecision);
                }
                _ => {}
            }
        }
        if state.current_epoch >= self.state.open_epoch {
            self.state.open_epoch = state.current_epoch;
        }
        self.state.closed_through = Some(
            self.state
                .closed_through
                .map_or(state.epoch, |epoch| epoch.max(state.epoch)),
        );
        Ok(())
    }

    pub fn obligations_to_drain(&self, epoch: RaiEpoch) -> Option<&BTreeSet<RaiSlotId>> {
        self.frozen_obligations.get(&epoch)
    }

    pub fn happy_path_drain(&self, epoch: RaiEpoch) -> Option<&RaiHappyPathDrain> {
        self.drains.get(&epoch)
    }

    pub fn happy_path_drains(&self) -> &BTreeMap<RaiEpoch, RaiHappyPathDrain> {
        &self.drains
    }

    pub fn record_known_slot(&mut self, slot: RaiSlotId) {
        self.known_slots.insert(slot);
    }

    pub fn released_slots(&self) -> &BTreeMap<RaiSlotId, RaiCertifiedRelease> {
        &self.released_slots
    }

    pub fn certified_release(&self, slot: &RaiSlotId) -> Option<&RaiCertifiedRelease> {
        self.released_slots.get(slot)
    }

    /// After a cut, only included elections from the closing epoch remain
    /// enabled. Elections in the already-open successor are unaffected.
    pub fn slot_election_enabled(&self, epoch: RaiEpoch, root: &QualifiedRoot) -> bool {
        let slot = RaiSlotId {
            epoch,
            root: root.clone(),
        };
        if self.released_slots.contains_key(&slot) {
            return false;
        }
        self.state.closing.is_none_or(|closing| {
            closing.epoch != epoch
                || self
                    .frozen_obligations
                    .get(&epoch)
                    .is_none_or(|included| included.contains(&slot))
        })
    }

    /// Captures the certified state immediately preceding this epoch. It may be
    /// called repeatedly, but the first snapshot is immutable.
    pub fn initialize_drain_frontiers(
        &mut self,
        epoch: RaiEpoch,
        base: impl IntoIterator<Item = (Account, ConfirmationHeightInfo)>,
    ) -> bool {
        if !self.drains.contains_key(&epoch) || self.drain_frontiers.contains_key(&epoch) {
            return false;
        }
        self.drain_frontiers
            .insert(epoch, base.into_iter().collect());
        true
    }

    /// Applies an epoch-local cemented segment after its persistent vote
    /// certificate has been validated.
    pub fn record_drain_evidence(
        &mut self,
        epoch: RaiEpoch,
        slot: &RaiSlotId,
        evidence: &super::RaiElectionVoteState,
        segment: impl IntoIterator<Item = (Account, ConfirmationHeightInfo)>,
    ) -> Option<RaiDrainOutcome> {
        let outcome = self
            .drains
            .get_mut(&epoch)?
            .record_persistent_evidence(slot, evidence)?;
        if matches!(
            outcome,
            RaiDrainOutcome::Finalized(_) | RaiDrainOutcome::Selected(_)
        ) {
            let frontiers = self.drain_frontiers.get_mut(&epoch)?;
            for (account, info) in segment {
                let current = frontiers.entry(account).or_default();
                if info.height > current.height {
                    *current = info;
                }
            }
        }
        Some(outcome)
    }

    pub fn drain_frontiers(
        &self,
        epoch: RaiEpoch,
    ) -> Option<&BTreeMap<Account, ConfirmationHeightInfo>> {
        self.drain_frontiers.get(&epoch)
    }

    pub fn durable_close_round_state(&self) -> Option<RaiDurableCloseRoundState> {
        self.state.closing.and_then(|closing| {
            matches!(
                closing.phase,
                RaiClosingPhase::ElectingCut | RaiClosingPhase::ElectingRecord
            )
            .then(|| RaiDurableCloseRoundState {
                epoch: closing.epoch,
                phase: closing.phase,
                cut_rounds: self.cut_rounds.get(&closing.epoch).map(|r| r.snapshot()),
                record_rounds: self.record_rounds.get(&closing.epoch).map(|r| r.snapshot()),
                close_cuts: self.close_cuts.clone(),
                close_records: self.close_records.clone(),
                visible_obligations: self.visible_obligations.get(&closing.epoch).cloned(),
            })
        })
    }

    pub fn restore_close_round_state(
        &mut self,
        state: RaiDurableCloseRoundState,
    ) -> Result<(), CloseRecordDecisionError> {
        if !matches!(
            state.phase,
            RaiClosingPhase::ElectingCut | RaiClosingPhase::ElectingRecord
        ) {
            return Err(CloseRecordDecisionError::WrongPhase);
        }
        let cut = state
            .cut_rounds
            .and_then(super::RaiCloseRoundTracker::from_snapshot);
        let record = state
            .record_rounds
            .and_then(super::RaiCloseRoundTracker::from_snapshot);
        if state.phase == RaiClosingPhase::ElectingCut && cut.is_none()
            || state.phase == RaiClosingPhase::ElectingRecord && record.is_none()
        {
            return Err(CloseRecordDecisionError::MissingPreimage);
        }
        self.state.open_epoch = RaiEpoch::new(
            state
                .epoch
                .number()
                .checked_add(1)
                .ok_or(CloseRecordDecisionError::InvalidRecord)?,
        );
        self.state.closing = Some(RaiClosingEpoch {
            epoch: state.epoch,
            phase: state.phase,
        });
        self.close_cuts = state.close_cuts;
        self.close_records = state.close_records;
        if let Some(rounds) = cut {
            self.cut_rounds.insert(state.epoch, rounds);
        }
        if let Some(rounds) = record {
            self.record_rounds.insert(state.epoch, rounds);
        }
        if let Some(visible) = state.visible_obligations {
            self.visible_obligations.insert(state.epoch, visible);
        }
        Ok(())
    }

    /// Returns genesis for a negative epoch and requires recorded state for a
    /// non-negative epoch.
    pub fn committee_at(&self, epoch: i64) -> Option<Arc<RepWeights>> {
        if epoch < 0 {
            Some(self.genesis_committee.clone())
        } else {
            self.committees.get(&RaiEpoch::new(epoch as u64)).cloned()
        }
    }

    /// Committees eligible to vote on slots in `epoch` (`e-3` and `e-2`).
    /// Equal snapshots are returned once.
    pub fn slot_committees(&self, epoch: RaiEpoch) -> Option<Vec<Arc<RepWeights>>> {
        let epoch = i64::try_from(epoch.number()).ok()?;
        let first = self.committee_at(epoch.checked_sub(3)?)?;
        let second = self.committee_at(epoch.checked_sub(2)?)?;

        if Arc::ptr_eq(&first, &second) || first == second {
            Some(vec![first])
        } else {
            Some(vec![first, second])
        }
    }

    /// Committee eligible to vote on reports and the close for `epoch`.
    pub fn close_committee(&self, epoch: RaiEpoch) -> Option<Arc<RepWeights>> {
        let epoch = i64::try_from(epoch.number()).ok()?;
        self.committee_at(epoch.checked_sub(2)?)
    }

    /// The certified close which governs `epoch`.
    pub fn governing_hash(&self, epoch: RaiEpoch) -> Option<BlockHash> {
        let Some(governing_epoch) = epoch.number().checked_sub(2) else {
            return Some(self.genesis_governing_hash);
        };
        self.close_hashes
            .get(&RaiEpoch::new(governing_epoch))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::rai::RaiReport;
    use rsnano_types::{
        Account, Amount, ConfirmationHeightInfo, PrivateKey, PublicKey, RaiCommitteeScope,
    };

    fn weights(rep: u64, amount: u128) -> Arc<RepWeights> {
        Arc::new(RepWeights::from([(
            PublicKey::from(rep),
            Amount::raw(amount),
        )]))
    }

    fn private_weights(key: &PrivateKey, amount: u128) -> Arc<RepWeights> {
        Arc::new(RepWeights::from([(key.public_key(), Amount::raw(amount))]))
    }

    fn final_evidence(key: &PrivateKey, hash: BlockHash) -> super::super::RaiElectionVoteState {
        let mut evidence = super::super::RaiElectionVoteState::new(vec![private_weights(key, 100)]);
        evidence
            .record_final_vote(key.public_key(), hash, RaiCommitteeScope::All)
            .unwrap();
        evidence
    }

    fn timeout_evidence(key: &PrivateKey) -> super::super::RaiElectionVoteState {
        let mut evidence = super::super::RaiElectionVoteState::new(vec![private_weights(key, 100)]);
        evidence
            .record_notarization_vote(
                key.public_key(),
                super::super::BlockHashOrTimeout::Timeout,
                RaiCommitteeScope::All,
            )
            .unwrap();
        evidence
    }

    fn fork_conflict_evidence(key: &PrivateKey) -> super::super::RaiElectionVoteState {
        let mut evidence = super::super::RaiElectionVoteState::new(vec![private_weights(key, 100)]);
        for hash in [BlockHash::from(10), BlockHash::from(11)] {
            evidence
                .record_notarization_vote(
                    key.public_key(),
                    super::super::BlockHashOrTimeout::Block(hash),
                    RaiCommitteeScope::All,
                )
                .unwrap();
        }
        evidence
    }

    fn slot(root: QualifiedRoot) -> RaiSlotId {
        RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root,
        }
    }

    #[test]
    fn empty_cut_drain_is_immediately_complete() {
        let drain = RaiHappyPathDrain {
            epoch: RaiEpoch::ZERO,
            obligations: BTreeSet::new(),
            finalized: BTreeMap::new(),
            selected: BTreeMap::new(),
            released: BTreeMap::new(),
        };

        assert!(drain.is_complete());
    }

    #[test]
    fn final_certificate_settles_only_its_obligation() {
        let key = PrivateKey::from(1);
        let first = slot(QualifiedRoot::new(1.into(), 2.into()));
        let second = slot(QualifiedRoot::new(3.into(), 4.into()));
        let hash = BlockHash::from(10);
        let mut drain = RaiHappyPathDrain {
            epoch: RaiEpoch::ZERO,
            obligations: BTreeSet::from([first.clone(), second]),
            finalized: BTreeMap::new(),
            selected: BTreeMap::new(),
            released: BTreeMap::new(),
        };

        assert_eq!(
            drain.record_persistent_evidence(&first, &final_evidence(&key, hash)),
            Some(RaiDrainOutcome::Finalized(hash))
        );
        assert!(!drain.is_complete());
    }

    #[test]
    fn local_timeout_does_not_settle_an_obligation() {
        let root = slot(QualifiedRoot::new(1.into(), 2.into()));
        let mut drain = RaiHappyPathDrain {
            epoch: RaiEpoch::ZERO,
            obligations: BTreeSet::from([root.clone()]),
            finalized: BTreeMap::new(),
            selected: BTreeMap::new(),
            released: BTreeMap::new(),
        };
        let mut evidence = super::super::RaiElectionVoteState::default();
        evidence.outcome = super::super::RaiOutcome::TimedOut;

        assert_eq!(drain.record_persistent_evidence(&root, &evidence), None);
        assert!(!drain.is_complete());
    }

    #[test]
    fn certified_timeout_releases_a_drain_obligation() {
        let key = PrivateKey::from(1);
        let root = slot(QualifiedRoot::new(1.into(), 2.into()));
        let mut drain = RaiHappyPathDrain {
            epoch: RaiEpoch::ZERO,
            obligations: BTreeSet::from([root.clone()]),
            finalized: BTreeMap::new(),
            selected: BTreeMap::new(),
            released: BTreeMap::new(),
        };

        assert_eq!(
            drain.record_persistent_evidence(&root, &timeout_evidence(&key)),
            Some(RaiDrainOutcome::ReleasedTimeout)
        );
        assert!(drain.is_complete());
        assert!(drain.finalized.is_empty());
    }

    #[test]
    fn fork_only_conflict_releases_a_drain_obligation() {
        let key = PrivateKey::from(1);
        let root = slot(QualifiedRoot::new(1.into(), 2.into()));
        let mut drain = RaiHappyPathDrain {
            epoch: RaiEpoch::ZERO,
            obligations: BTreeSet::from([root.clone()]),
            finalized: BTreeMap::new(),
            selected: BTreeMap::new(),
            released: BTreeMap::new(),
        };

        assert_eq!(
            drain.record_persistent_evidence(&root, &fork_conflict_evidence(&key)),
            Some(RaiDrainOutcome::ReleasedConflict)
        );
        assert!(drain.is_complete());
        assert!(drain.finalized.is_empty());
    }

    #[test]
    fn drain_finishes_only_after_every_cut_member_finalizes() {
        let key = PrivateKey::from(1);
        let first = slot(QualifiedRoot::new(1.into(), 2.into()));
        let second = slot(QualifiedRoot::new(3.into(), 4.into()));
        let mut drain = RaiHappyPathDrain {
            epoch: RaiEpoch::ZERO,
            obligations: BTreeSet::from([first.clone(), second.clone()]),
            finalized: BTreeMap::new(),
            selected: BTreeMap::new(),
            released: BTreeMap::new(),
        };

        drain.record_persistent_evidence(&first, &final_evidence(&key, BlockHash::from(10)));
        assert!(!drain.is_complete());
        drain.record_persistent_evidence(&second, &final_evidence(&key, BlockHash::from(11)));
        assert!(drain.is_complete());
    }

    #[test]
    fn successor_epoch_finalization_cannot_advance_captured_frontier() {
        let key = PrivateKey::from(1);
        let root = slot(QualifiedRoot::new(1.into(), 2.into()));
        let account = Account::from(7);
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::ZERO);
        manager.drains.insert(
            RaiEpoch::ZERO,
            RaiHappyPathDrain {
                epoch: RaiEpoch::ZERO,
                obligations: BTreeSet::from([root.clone()]),
                finalized: BTreeMap::new(),
                selected: BTreeMap::new(),
                released: BTreeMap::new(),
            },
        );
        manager.initialize_drain_frontiers(
            RaiEpoch::ZERO,
            [(account, ConfirmationHeightInfo::new(2, BlockHash::from(20)))],
        );
        manager.record_drain_evidence(
            RaiEpoch::ZERO,
            &root,
            &final_evidence(&key, BlockHash::from(30)),
            [(account, ConfirmationHeightInfo::new(3, BlockHash::from(30)))],
        );

        // An unrestricted ledger view could now be at epoch 1 / height 4; it
        // is never consulted by the drain.
        assert_eq!(
            manager.drain_frontiers(RaiEpoch::ZERO).unwrap()[&account],
            ConfirmationHeightInfo::new(3, BlockHash::from(30))
        );
    }

    #[test]
    fn negative_epochs_use_genesis() {
        let genesis = weights(1, 100);
        let manager = RaiEpochManager::new(genesis.clone(), BlockHash::from(7));

        assert!(Arc::ptr_eq(&manager.committee_at(-1).unwrap(), &genesis));
        assert!(Arc::ptr_eq(&manager.committee_at(-100).unwrap(), &genesis));
    }

    #[test]
    fn early_epochs_collapse_duplicate_genesis_committees() {
        let manager = RaiEpochManager::new(weights(1, 100), BlockHash::from(7));

        assert_eq!(manager.slot_committees(0.into()).unwrap().len(), 1);
        assert_eq!(manager.slot_committees(1.into()).unwrap().len(), 1);
    }

    #[test]
    fn later_slots_select_both_historical_snapshots() {
        let first = weights(1, 100);
        let second = weights(2, 200);
        let mut manager = RaiEpochManager::new(weights(9, 900), BlockHash::from(7));
        manager.insert_committee(1.into(), first.clone());
        manager.insert_committee(2.into(), second.clone());

        let selected = manager.slot_committees(4.into()).unwrap();
        assert_eq!(selected, vec![first, second]);
    }

    #[test]
    fn close_selects_epoch_minus_two() {
        let expected = weights(1, 100);
        let mut manager = RaiEpochManager::new(weights(9, 900), BlockHash::from(7));
        manager.insert_committee(2.into(), expected.clone());

        assert!(Arc::ptr_eq(
            &manager.close_committee(4.into()).unwrap(),
            &expected
        ));
    }

    #[test]
    fn recorded_snapshot_is_not_changed_with_live_weights() {
        let live = RepWeightCache::default();
        let first = PublicKey::from(1);
        let second = PublicKey::from(2);
        live.put(first, Amount::raw(100));
        let mut manager = RaiEpochManager::new(weights(9, 900), BlockHash::from(7));

        let frozen = manager.snapshot_committee(0.into(), &live);
        live.put(first, Amount::ZERO);
        live.put(second, Amount::raw(200));

        assert_eq!(frozen.weight(&first), Amount::raw(100));
        assert_eq!(frozen.weight(&second), Amount::ZERO);
        assert_eq!(manager.committee_at(0).unwrap(), frozen);
    }

    #[test]
    fn missing_history_prevents_committee_selection_for_vote_validation() {
        let manager = RaiEpochManager::new(weights(1, 100), BlockHash::from(7));

        assert!(manager.slot_committees(3.into()).is_none());
        assert!(manager.close_committee(2.into()).is_none());
    }

    #[test]
    fn governing_hash_is_genesis_then_epoch_minus_two_close() {
        let genesis = BlockHash::from(7);
        let mut manager = RaiEpochManager::new(weights(1, 100), genesis);
        let close_0 = BlockHash::from(40);
        let close_1 = BlockHash::from(41);
        manager.record_close_hash(0.into(), close_0);
        manager.record_close_hash(1.into(), close_1);

        assert_eq!(manager.governing_hash(0.into()), Some(genesis));
        assert_eq!(manager.governing_hash(1.into()), Some(genesis));
        assert_eq!(manager.governing_hash(2.into()), Some(close_0));
        assert_eq!(manager.governing_hash(3.into()), Some(close_1));
    }

    #[test]
    fn missing_epoch_minus_two_close_has_no_governing_context() {
        let manager = RaiEpochManager::new(weights(1, 100), BlockHash::from(7));

        assert_eq!(manager.governing_hash(2.into()), None);
    }

    #[test]
    fn round_zero_cut_decision_freezes_obligations_and_is_immutable() {
        let key = PrivateKey::from(1);
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        let obligation = slot(QualifiedRoot::new(11.into(), 12.into()));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, 0.into(), []))
            .unwrap();
        let (root, hash) = manager.begin_cut_election([obligation.clone()]).unwrap();
        assert_eq!(root, crate::consensus::rai::rai_close_cut_root(0.into(), 0));
        assert_eq!(
            manager.closing_epoch().unwrap().phase,
            RaiClosingPhase::ElectingCut
        );

        assert_eq!(
            manager.install_cut(0.into(), 1, hash),
            Err(CloseCutDecisionError::MissingPreimage)
        );
        assert_eq!(
            manager.install_cut(0.into(), 0, hash).unwrap(),
            &BTreeSet::from([obligation])
        );
        assert_eq!(
            manager.closing_epoch().unwrap().phase,
            RaiClosingPhase::Draining
        );
        assert_eq!(manager.decided_close_hash(0.into()), Some(hash));
        assert_eq!(
            manager.install_cut(0.into(), 0, BlockHash::from(999)),
            Err(CloseCutDecisionError::ImmutableDecision)
        );
    }

    #[test]
    fn cut_starts_at_w_minus_f_report_weight() {
        let first = PrivateKey::from(1);
        let second = PrivateKey::from(2);
        let third = PrivateKey::from(3);
        let committee = Arc::new(RepWeights::from([
            (first.public_key(), Amount::raw(8)),
            (second.public_key(), Amount::raw(9)),
            (third.public_key(), Amount::raw(3)),
        ]));
        let mut manager = RaiEpochManager::new(committee, BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&first, 0.into(), []))
            .unwrap();

        assert!(manager.begin_cut_election([]).is_none());
        assert_eq!(
            manager.closing_epoch().unwrap().phase,
            RaiClosingPhase::CollectingReports
        );

        manager
            .reports_mut()
            .insert(RaiReport::new(&second, 0.into(), []))
            .unwrap();
        assert!(manager.begin_cut_election([]).is_some());
        assert_eq!(
            manager.closing_epoch().unwrap().phase,
            RaiClosingPhase::ElectingCut
        );
    }

    #[test]
    fn cut_exclusion_releases_only_after_certified_close_installation() {
        let key = PrivateKey::from(1);
        let old = slot(QualifiedRoot::new(11.into(), 12.into()));
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.record_known_slot(old.clone());
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
            .unwrap();
        let (_, cut) = manager.begin_cut_election([]).unwrap();
        manager.install_cut(RaiEpoch::ZERO, 0, cut).unwrap();

        assert!(manager.certified_release(&old).is_none());

        manager.initialize_drain_frontiers(RaiEpoch::ZERO, []);
        let (_, close) = manager.begin_close_record().unwrap();
        manager
            .install_close_record(RaiEpoch::ZERO, 0, close)
            .unwrap();

        assert_eq!(
            manager.certified_release(&old),
            Some(&RaiCertifiedRelease {
                close_epoch: RaiEpoch::ZERO,
                close_record_hash: close,
            })
        );
        assert!(!manager.slot_election_enabled(old.epoch, &old.root));
    }

    #[test]
    fn certified_release_is_epoch_qualified() {
        let key = PrivateKey::from(1);
        let root = QualifiedRoot::new(11.into(), 12.into());
        let old = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: root.clone(),
        };
        let retry = RaiSlotId {
            epoch: RaiEpoch::new(1),
            root,
        };
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.record_known_slot(old.clone());
        manager.record_known_slot(retry.clone());
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
            .unwrap();
        let (_, cut) = manager.begin_cut_election([]).unwrap();
        manager.install_cut(RaiEpoch::ZERO, 0, cut).unwrap();
        manager.initialize_drain_frontiers(RaiEpoch::ZERO, []);
        let (_, close) = manager.begin_close_record().unwrap();
        manager
            .install_close_record(RaiEpoch::ZERO, 0, close)
            .unwrap();

        assert!(manager.certified_release(&old).is_some());
        assert!(manager.certified_release(&retry).is_none());
        assert!(manager.slot_election_enabled(retry.epoch, &retry.root));
    }

    #[test]
    fn close_record_waits_for_drain_validates_and_is_immutable() {
        let key = PrivateKey::from(1);
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, 0.into(), []))
            .unwrap();
        let (_, cut) = manager.begin_cut_election([]).unwrap();
        manager.install_cut(0.into(), 0, cut).unwrap();
        let frontiers = [(
            Account::from(1),
            ConfirmationHeightInfo::new(4, BlockHash::from(40)),
        )];

        assert!(manager.begin_close_record().is_none());
        assert!(manager.initialize_drain_frontiers(0.into(), frontiers.clone()));
        let (root, close) = manager.begin_close_record().unwrap();
        assert_eq!(
            root,
            crate::consensus::rai::rai_close_record_root(0.into(), 0)
        );

        let expected: RaiFrontierMap = frontiers.clone().into_iter().collect();
        assert_eq!(
            manager
                .install_certified_close_record(
                    0.into(),
                    0,
                    close,
                    weights(2, 200).as_ref().clone(),
                )
                .unwrap(),
            &expected
        );
        assert!(manager.closing_epoch().is_none());
        assert_eq!(manager.current_epoch(), RaiEpoch::new(1));
        assert_eq!(
            manager.committee_at(0).unwrap().weight(&PublicKey::from(2)),
            Amount::raw(200)
        );
        assert_eq!(manager.installed_close_hash(0.into()), Some(close));
        assert_eq!(
            manager.install_close_record(0.into(), 0, BlockHash::from(99)),
            Err(CloseRecordDecisionError::ImmutableDecision)
        );
        manager
            .install_certified_close_record(0.into(), 0, close, weights(3, 300).as_ref().clone())
            .unwrap();
        assert_eq!(manager.current_epoch(), RaiEpoch::new(1));
        assert_eq!(
            manager.committee_at(0).unwrap().weight(&PublicKey::from(3)),
            Amount::ZERO
        );

        let durable = manager.durable_close_state(0.into()).unwrap();
        let mut restarted = RaiEpochManager::new(weights(1, 100), BlockHash::from(7));
        restarted.restore_close_state(durable).unwrap();
        assert!(restarted.closing_epoch().is_none());
        assert_eq!(restarted.current_epoch(), RaiEpoch::new(1));
        assert_eq!(restarted.committee_at(0), manager.committee_at(0));
        assert_eq!(restarted.installed_close_hash(0.into()), Some(close));
        assert_eq!(restarted.governing_hash(1.into()), Some(BlockHash::from(7)));
    }

    #[test]
    fn pending_cut_obligation_blocks_record_creation() {
        let key = PrivateKey::from(1);
        let obligation = slot(QualifiedRoot::new(11.into(), 12.into()));
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
            .unwrap();
        let (_, cut) = manager.begin_cut_election([obligation.clone()]).unwrap();
        manager.install_cut(RaiEpoch::ZERO, 0, cut).unwrap();

        assert!(!manager.slot_election_enabled(RaiEpoch::ZERO, &QualifiedRoot::ZERO));
        assert!(manager.slot_election_enabled(RaiEpoch::ZERO, &obligation.root));
        assert!(manager.slot_election_enabled(RaiEpoch::new(1), &QualifiedRoot::ZERO));
        assert!(manager.begin_close_record().is_none());
    }

    #[test]
    fn closing_epoch_zero_keeps_epoch_one_open_and_addressable() {
        let mut manager = RaiEpochManager::new(weights(1, 100), BlockHash::from(7));
        let now = Timestamp::new_test_instance();

        assert!(manager.start_closing(now));
        assert_eq!(manager.current_epoch(), RaiEpoch::new(1));
        assert_eq!(manager.state().open_started_at, now);
        assert_eq!(
            manager.closing_epoch(),
            Some(RaiClosingEpoch {
                epoch: RaiEpoch::ZERO,
                phase: RaiClosingPhase::CollectingReports,
            })
        );
        assert!(!manager.start_closing(now));
        assert_eq!(manager.current_epoch(), RaiEpoch::new(1));
        assert_eq!(manager.closing_epoch().unwrap().epoch, RaiEpoch::ZERO);
    }

    #[test]
    fn close_transitions_are_idempotent() {
        let key = PrivateKey::from(1);
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, 0.into(), []))
            .unwrap();
        let (_, cut) = manager.begin_cut_election([]).unwrap();
        assert!(manager.begin_cut_election([]).is_none());
        manager.install_cut(0.into(), 0, cut).unwrap();
        manager.install_cut(0.into(), 0, cut).unwrap();

        let frontiers = [(
            Account::from(1),
            ConfirmationHeightInfo::new(4, BlockHash::from(40)),
        )];
        manager.initialize_drain_frontiers(0.into(), frontiers);
        let (_, record) = manager.begin_close_record().unwrap();
        assert!(manager.begin_close_record().is_none());
        manager.install_close_record(0.into(), 0, record).unwrap();
        manager.install_close_record(0.into(), 0, record).unwrap();
        assert_eq!(manager.state().closed_through, Some(RaiEpoch::ZERO));
    }

    #[test]
    fn replicas_bind_close_record_to_the_drained_cut() {
        let key = PrivateKey::from(1);
        let frontiers = [
            (
                Account::from(2),
                ConfirmationHeightInfo::new(5, BlockHash::from(50)),
            ),
            (
                Account::from(1),
                ConfirmationHeightInfo::new(4, BlockHash::from(40)),
            ),
        ];
        let mut hashes = Vec::new();

        for _ in 0..4 {
            let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
            manager.start_closing(Timestamp::new_test_instance());
            manager
                .reports_mut()
                .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
                .unwrap();
            let (_, cut) = manager.begin_cut_election([]).unwrap();
            manager.install_cut(RaiEpoch::ZERO, 0, cut).unwrap();
            manager.initialize_drain_frontiers(RaiEpoch::ZERO, frontiers.clone());
            hashes.push(manager.begin_close_record().unwrap().1);
        }

        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn close_record_rejects_unknown_and_noncanonical_preimages() {
        let key = PrivateKey::from(1);
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
            .unwrap();
        let (_, cut) = manager.begin_cut_election([]).unwrap();
        manager.install_cut(RaiEpoch::ZERO, 0, cut).unwrap();
        let frontiers = [(
            Account::from(1),
            ConfirmationHeightInfo::new(4, BlockHash::from(40)),
        )];
        manager.initialize_drain_frontiers(RaiEpoch::ZERO, frontiers.clone());
        let (_, canonical) = manager.begin_close_record().unwrap();

        assert_eq!(
            manager.install_close_record(RaiEpoch::ZERO, 0, BlockHash::from(999)),
            Err(CloseRecordDecisionError::MissingPreimage)
        );

        for invalid in [
            RaiCloseRecord::new(RaiEpoch::ZERO, BlockHash::from(123), frontiers),
            RaiCloseRecord::new(
                RaiEpoch::ZERO,
                BlockHash::ZERO,
                [(
                    Account::from(1),
                    ConfirmationHeightInfo::new(5, BlockHash::from(50)),
                )],
            ),
        ] {
            let invalid_hash = manager.close_records.insert(invalid);
            manager
                .record_rounds
                .get_mut(&RaiEpoch::ZERO)
                .unwrap()
                .add_validated_preimage(0, invalid_hash);
            assert_eq!(
                manager.install_close_record(RaiEpoch::ZERO, 0, invalid_hash),
                Err(CloseRecordDecisionError::InvalidRecord)
            );
        }

        manager
            .install_close_record(RaiEpoch::ZERO, 0, canonical)
            .unwrap();
        assert!(
            manager
                .install_close_record(RaiEpoch::ZERO, 0, canonical)
                .is_ok()
        );
        assert_eq!(
            manager.install_close_record(RaiEpoch::ZERO, 0, BlockHash::from(998)),
            Err(CloseRecordDecisionError::ImmutableDecision)
        );
    }
}
