use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use rsnano_ledger::{RepWeightCache, RepWeights};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, BlockHash, ConfirmationHeightInfo, QualifiedRoot, RaiEpoch, RaiSlotId, Root,
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
        // Every mutation keeps these maps mutually exclusive. Counting their
        // entries therefore avoids walking the entire frozen cut on every
        // close tick.
        let resolved = self.finalized.len() + self.selected.len() + self.released.len();
        debug_assert!(resolved <= self.obligations.len());
        resolved == self.obligations.len()
    }

    /// Derives an obligation outcome from persistent certificate evidence
    /// without changing the drain. Releases never advance the close-local
    /// frontier.
    pub fn persistent_evidence_outcome(
        &self,
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
            return Some(RaiDrainOutcome::ReleasedTimeout);
        }
        let mut certified = None;
        for committee in 0..evidence.committees.len() {
            let hash = match evidence.local_result(committee) {
                Some(super::RaiLocalResult::Fast(hash) | super::RaiLocalResult::Final(hash)) => {
                    hash
                }
                Some(super::RaiLocalResult::Timeout) => {
                    return Some(RaiDrainOutcome::ReleasedConflict);
                }
                Some(super::RaiLocalResult::Notarized(hash)) => hash,
                None => return None,
            };
            if certified
                .replace(hash)
                .is_some_and(|previous| previous != hash)
            {
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
        Some(if globally_strong {
            RaiDrainOutcome::Finalized(hash)
        } else {
            RaiDrainOutcome::Selected(hash)
        })
    }

    /// Resolves an obligation from persistent certificate evidence. Releases
    /// never advance the close-local frontier.
    pub fn record_persistent_evidence(
        &mut self,
        slot: &RaiSlotId,
        evidence: &super::RaiElectionVoteState,
    ) -> Option<RaiDrainOutcome> {
        let outcome = self.persistent_evidence_outcome(slot, evidence)?;
        let target = match outcome {
            RaiDrainOutcome::Finalized(_) => &mut self.finalized,
            RaiDrainOutcome::Selected(_) => &mut self.selected,
            RaiDrainOutcome::ReleasedTimeout | RaiDrainOutcome::ReleasedConflict => {
                return match self.released.entry(slot.clone()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(outcome);
                        Some(outcome)
                    }
                    std::collections::btree_map::Entry::Occupied(entry) => {
                        (*entry.get() == outcome).then_some(outcome)
                    }
                };
            }
        };
        let hash = match outcome {
            RaiDrainOutcome::Finalized(hash) | RaiDrainOutcome::Selected(hash) => hash,
            RaiDrainOutcome::ReleasedTimeout | RaiDrainOutcome::ReleasedConflict => unreachable!(),
        };
        match target.entry(slot.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(hash);
                Some(outcome)
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                (*entry.get() == hash).then_some(outcome)
            }
        }
    }

    /// Restores a locally validated notarized outcome without retaining the
    /// quorum's individual votes. The outcome is provisional until the close
    /// record includes it.
    pub fn record_notarized(
        &mut self,
        slot: &RaiSlotId,
        hash: BlockHash,
    ) -> Option<RaiDrainOutcome> {
        if !self.obligations.contains(slot)
            || self.finalized.contains_key(slot)
            || self.released.contains_key(slot)
        {
            return None;
        }
        match self.selected.entry(slot.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(hash);
                Some(RaiDrainOutcome::Selected(hash))
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                (*entry.get() == hash).then_some(RaiDrainOutcome::Selected(hash))
            }
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
    LedgerCommitFailed,
}

impl std::fmt::Display for CloseRecordDecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongPhase => "epoch is not closing its record",
            Self::MissingPreimage => "canonical close-record preimage is unavailable",
            Self::InvalidRecord => "close record does not match confirmation heights",
            Self::ImmutableDecision => "the epoch already has a different close record",
            Self::LedgerCommitFailed => "close-record ledger finalization failed",
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
    close_record_committees: BTreeMap<BlockHash, RepWeights>,
    visible_obligations: BTreeMap<RaiEpoch, BTreeSet<RaiSlotId>>,
    frozen_obligations: BTreeMap<RaiEpoch, BTreeSet<RaiSlotId>>,
    drains: BTreeMap<RaiEpoch, RaiHappyPathDrain>,
    known_slots: BTreeSet<RaiSlotId>,
    /// Epochs which still lock a qualified root against a successor retry.
    ///
    /// `slot_election_enabled` is called once per visible election while an
    /// epoch boundary is prepared. Looking for an older unresolved slot in
    /// `known_slots` made that preparation quadratic in the number of slots.
    /// Keep the same information indexed by root so each eligibility check is
    /// independent of the total slot history.
    unresolved_epochs_by_root: HashMap<QualifiedRoot, BTreeSet<RaiEpoch>>,
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
    pub close_record_committees: BTreeMap<BlockHash, RepWeights>,
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
            close_record_committees: BTreeMap::new(),
            visible_obligations: BTreeMap::new(),
            frozen_obligations: BTreeMap::new(),
            drains: BTreeMap::new(),
            known_slots: BTreeSet::new(),
            unresolved_epochs_by_root: HashMap::new(),
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

    pub fn close_record_versions(&self) -> Vec<RaiCloseRecord> {
        self.close_records.all()
    }

    pub fn close_record(&self, hash: &BlockHash) -> Option<&RaiCloseRecord> {
        self.close_records.get(hash)
    }

    pub fn close_cut_versions(&self) -> Vec<RaiCloseCut> {
        self.close_cuts.all()
    }

    pub fn close_cut(&self, hash: &BlockHash) -> Option<&RaiCloseCut> {
        self.close_cuts.get(hash)
    }

    /// Retains a transferred canonical cut preimage for the active close-cut
    /// election. Signed vote evidence remains the sole source of authority;
    /// receiving this hash-checked preimage only makes cached votes applicable.
    pub fn reconcile_close_cut(
        &mut self,
        cut: RaiCloseCut,
        round: u32,
    ) -> Option<(RaiEpoch, u32, BlockHash)> {
        let closing = self.state.closing?;
        if closing.epoch != cut.epoch || closing.phase != RaiClosingPhase::ElectingCut {
            return None;
        }
        let rounds = self.cut_rounds.get_mut(&closing.epoch)?;
        rounds.round(round)?;
        let hash = self.close_cuts.insert(cut);
        rounds.add_validated_preimage(round, hash);
        Some((closing.epoch, round, hash))
    }

    /// Retains a transferred canonical preimage for the active close-record
    /// election. Certificate support is still established exclusively by the
    /// signed vote leaves; receiving a preimage grants no authority by itself.
    pub fn reconcile_close_record(
        &mut self,
        record: RaiCloseRecord,
        round: u32,
    ) -> Option<(RaiEpoch, u32, BlockHash)> {
        let closing = self.state.closing?;
        if closing.epoch != record.epoch
            || !matches!(
                closing.phase,
                RaiClosingPhase::Draining | RaiClosingPhase::ElectingRecord
            )
        {
            return None;
        }
        let expected_previous = record
            .epoch
            .number()
            .checked_sub(1)
            .and_then(|e| self.close_hashes.get(&RaiEpoch::new(e)).copied())
            .unwrap_or(BlockHash::ZERO);
        if record.previous != expected_previous {
            return None;
        }
        let hash = self.close_records.insert(record);
        // A peer may enter the record election before this replica completes
        // its local drain.  Retain the hash-checked preimage without replacing
        // the local fresh frontier.  Once our record round exists, admit it as
        // an alternative validated candidate and replay the already retained
        // signed votes against it.
        if let Some(rounds) = self.record_rounds.get_mut(&closing.epoch) {
            rounds.round(round)?;
            rounds.add_validated_preimage(round, hash);
            if let Some(committee) = self.close_record_committees.values().next().cloned() {
                self.close_record_committees
                    .entry(hash)
                    .or_insert(committee);
            }
            return Some((closing.epoch, round, hash));
        }
        (round == 0).then_some((closing.epoch, round, hash))
    }
    pub fn close_cuts(&self) -> &RaiCloseCutStore {
        &self.close_cuts
    }

    pub fn report_quorum_available(&self, epoch: RaiEpoch) -> bool {
        self.close_committee(epoch)
            .is_some_and(|committee| self.reports.has_quorum(epoch, &committee))
    }

    pub fn full_report_coverage_available(&self, epoch: RaiEpoch) -> bool {
        self.close_committee(epoch)
            .is_some_and(|committee| self.reports.has_full_coverage(epoch, &committee))
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
        tracing::debug!(
            hash = ?cut.hash(),
            obligations = cut.obligations.len(),
            round = 0,
            "RAI close-cut candidate updated"
        );
        let hash = self.close_cuts.insert(cut);
        self.visible_obligations.insert(epoch, visible);
        self.state.closing.as_mut().unwrap().phase = RaiClosingPhase::ElectingCut;
        self.cut_rounds
            .entry(epoch)
            .or_insert_with(|| super::RaiCloseRoundTracker::new(super::RaiCloseKind::Cut, epoch))
            .start_round_zero(hash);
        Some((super::rai_close_cut_root(epoch, 0), hash))
    }

    /// Rebuild the replica's fresh cut preference as authenticated visibility
    /// grows. This does not add the value to the active round: a changed
    /// preference may be voted only in a successor round after the current
    /// round is positively dead.
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
        let cut = RaiCloseCut::new(epoch, visible.clone());
        let hash = cut.hash();
        let unchanged = self.visible_obligations.get(&epoch) == Some(&visible)
            && self
                .cut_rounds
                .get(&epoch)
                .and_then(|rounds| rounds.round(round))
                .is_some_and(|state| state.validated_preimages.contains(&hash));
        if unchanged {
            return None;
        }
        tracing::debug!(
            ?hash,
            obligations = cut.obligations.len(),
            round,
            "RAI close-cut candidate updated"
        );
        self.close_cuts.insert(cut);
        self.visible_obligations.insert(epoch, visible);
        // Retain the validated preimage so remote votes and a later
        // certificate for this hash can be checked. This is admissibility
        // state only; the vote generator separately enforces the immutable
        // first-vote slot for the active round.
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
        // The round tracker records every locally validated preimage.  Once a
        // quorum certificate selects one of them, later report delivery must
        // not invalidate that decision merely because the replica's fresh
        // candidate has grown in the meantime.
        if cut.epoch != epoch || cut.hash() != hash {
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
        for slot in cut.obligations.iter().cloned() {
            self.record_known_slot(slot);
        }
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

    /// Resolve an installed close-cut election by its synthetic request root.
    pub fn installed_close_cut_for_root(
        &self,
        requested_root: &Root,
    ) -> Option<(RaiEpoch, u32, BlockHash)> {
        self.cut_rounds.iter().find_map(|(epoch, tracker)| {
            let (round, hash) = tracker.decision()?;
            if self.cut_hashes.get(epoch) != Some(&hash)
                || super::rai_close_cut_root(*epoch, round).root != *requested_root
            {
                return None;
            }
            Some((*epoch, round, hash))
        })
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
        let source_result = {
            let rounds = self.cut_rounds.get(&epoch)?;
            rounds.round(rounds.current_round())?.derive()
        };
        let fresh = match source_result {
            super::RaiCloseRoundResult::Pending | super::RaiCloseRoundResult::Decided(_) => {
                return None;
            }
            super::RaiCloseRoundResult::LiveCarry(hash) => hash,
            super::RaiCloseRoundResult::Dead => {
                let obligations = self.visible_obligations.get(&epoch)?.clone();
                self.close_cuts.insert(RaiCloseCut::new(epoch, obligations))
            }
        };
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
    pub fn begin_close_record(
        &mut self,
        committee: RepWeights,
    ) -> Option<(QualifiedRoot, BlockHash)> {
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
        tracing::debug!(
            hash = ?record.hash(),
            frontiers = record.frontiers.len(),
            round = 0,
            "RAI close-record candidate updated"
        );
        let hash = self.close_records.insert(record);
        self.close_record_committees.insert(hash, committee);
        self.state.closing.as_mut().unwrap().phase = RaiClosingPhase::ElectingRecord;
        self.record_rounds
            .entry(epoch)
            .or_insert_with(|| super::RaiCloseRoundTracker::new(super::RaiCloseKind::Record, epoch))
            .start_round_zero(hash);
        let record_committee = self.close_record_committees[&hash].clone();
        if let Some(rounds) = self.record_rounds.get_mut(&epoch) {
            for version in self.close_records.all() {
                if version.epoch == epoch && version.previous == previous {
                    let version_hash = version.hash();
                    rounds.add_validated_preimage(0, version_hash);
                    self.close_record_committees
                        .entry(version_hash)
                        .or_insert_with(|| record_committee.clone());
                }
            }
        }
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
        self.install_certified_close_record_after(epoch, round, hash, certified_weights, |_, _| {
            true
        })
    }

    /// Installs a certified close record only after its selected ledger
    /// frontiers have been durably finalized by the caller. The callback runs
    /// before any close hash, committee, release, or closed-through state is
    /// published; returning false leaves the epoch in ElectingRecord so the
    /// same certificate can be retried safely.
    pub fn install_certified_close_record_after(
        &mut self,
        epoch: RaiEpoch,
        round: u32,
        hash: BlockHash,
        _certified_weights: RepWeights,
        commit: impl FnOnce(RaiEpoch, &RaiFrontierMap) -> bool,
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
        let previous = epoch
            .number()
            .checked_sub(1)
            .and_then(|e| self.close_hashes.get(&RaiEpoch::new(e)).copied())
            .unwrap_or(BlockHash::ZERO);
        let local_frontiers = self
            .drain_frontiers
            .get(&epoch)
            .ok_or(CloseRecordDecisionError::MissingPreimage)?;
        if record.epoch != epoch
            || record.previous != previous
            || record.hash() != hash
            || (&record.frontiers != local_frontiers
                && !self.close_record_committees.contains_key(&hash))
        {
            return Err(CloseRecordDecisionError::InvalidRecord);
        }
        let certified_weights = self
            .close_record_committees
            .get(&hash)
            .cloned()
            .ok_or(CloseRecordDecisionError::MissingPreimage)?;
        let mut tracker_probe = self
            .record_rounds
            .get(&epoch)
            .ok_or(CloseRecordDecisionError::WrongPhase)?
            .clone();
        if !tracker_probe.decide(round, hash) {
            return Err(CloseRecordDecisionError::MissingPreimage);
        }
        let frontiers = record.frontiers.clone();
        if !commit(epoch, &frontiers) {
            return Err(CloseRecordDecisionError::LedgerCommitFailed);
        }
        let tracker = self
            .record_rounds
            .get_mut(&epoch)
            .ok_or(CloseRecordDecisionError::WrongPhase)?;
        assert!(
            tracker.decide(round, hash),
            "validated close decision changed"
        );
        // The certificate, not this replica's provisional drain observation,
        // selects the durable frontier map.
        self.drain_frontiers.insert(epoch, frontiers);
        self.close_hashes.insert(epoch, hash);
        self.committees
            .entry(epoch)
            .or_insert_with(|| Arc::new(certified_weights));
        self.state.closed_through = Some(epoch);
        let drained_releases = self
            .drains
            .get(&epoch)
            .map(|drain| drain.released.keys().cloned().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        if let Some(drain) = self.drains.get_mut(&epoch) {
            drain.finalized.append(&mut drain.selected);
        }
        let released = self
            .known_slots
            .iter()
            // A close certificate releases old obligations omitted by the cut
            // and cut members whose certified drain outcome was timeout or
            // conflict. Finalized/selected cut members remain locked so a
            // successor cannot retry their starting slot.
            .filter(|slot| {
                slot.epoch <= epoch
                    && (drained_releases.contains(*slot)
                        || self
                            .frozen_obligations
                            .get(&slot.epoch)
                            .is_none_or(|included| !included.contains(*slot)))
            })
            .cloned()
            .collect::<Vec<_>>();
        for slot in released {
            if self.released_slots.contains_key(&slot) {
                continue;
            }
            self.released_slots.insert(
                slot.clone(),
                RaiCertifiedRelease {
                    close_epoch: epoch,
                    close_record_hash: hash,
                },
            );
            let remove_root = self
                .unresolved_epochs_by_root
                .get_mut(&slot.root)
                .is_some_and(|epochs| {
                    epochs.remove(&slot.epoch);
                    epochs.is_empty()
                });
            if remove_root {
                self.unresolved_epochs_by_root.remove(&slot.root);
            }
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

    /// Resolve an installed close-record election by its synthetic request
    /// root. The round and winning digest come from the certified decision,
    /// rather than being guessed from the current open epoch.
    pub fn installed_close_record_for_root(
        &self,
        requested_root: &Root,
    ) -> Option<(RaiEpoch, u32, BlockHash)> {
        self.record_rounds.iter().find_map(|(epoch, tracker)| {
            let (round, hash) = tracker.decision()?;
            if self.close_hashes.get(epoch) != Some(&hash)
                || super::rai_close_record_root(*epoch, round).root != *requested_root
            {
                return None;
            }
            Some((*epoch, round, hash))
        })
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
        let source_result = {
            let rounds = self.record_rounds.get(&epoch)?;
            rounds.round(rounds.current_round())?.derive()
        };
        let fresh = match source_result {
            super::RaiCloseRoundResult::Pending | super::RaiCloseRoundResult::Decided(_) => {
                return None;
            }
            super::RaiCloseRoundResult::LiveCarry(hash) => hash,
            super::RaiCloseRoundResult::Dead => {
                let previous = epoch
                    .number()
                    .checked_sub(1)
                    .and_then(|e| self.close_hashes.get(&RaiEpoch::new(e)).copied())
                    .unwrap_or(BlockHash::ZERO);
                // Close-record retries are derived from the immutable close-local
                // replay captured while draining. Ordinary confirmation-height writes
                // after that point must not perturb a fresh retry candidate.
                let frontiers = self.drain_frontiers.get(&epoch)?.clone();
                let record = RaiCloseRecord::new(epoch, previous, frontiers);
                let frontier_count = record.frontiers.len();
                let hash = self.close_records.insert(record);
                tracing::debug!(
                    ?hash,
                    frontiers = frontier_count,
                    round = self
                        .close_record_round(epoch)
                        .unwrap_or(0)
                        .saturating_add(1),
                    "RAI close-record candidate updated"
                );
                hash
            }
        };
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

    /// Certificate-unresolved slots at the point a close-drain scheduler is
    /// initialized. The cut is immutable, so a scheduler can retain this set
    /// and remove entries as they settle instead of rebuilding it each tick.
    pub fn unresolved_drain_obligations(&self, epoch: RaiEpoch) -> Option<Vec<RaiSlotId>> {
        let drain = self.drains.get(&epoch)?;
        Some(
            drain
                .obligations
                .iter()
                .filter(|slot| {
                    !drain.finalized.contains_key(*slot)
                        && !drain.selected.contains_key(*slot)
                        && !drain.released.contains_key(*slot)
                })
                .cloned()
                .collect(),
        )
    }

    /// Slots which still require a durable-ledger check while draining. A
    /// durable finalization is terminal, while selected and released outcomes
    /// remain eligible for the existing durable-upgrade path.
    pub fn obligations_requiring_durable_check(&self, epoch: RaiEpoch) -> Option<Vec<RaiSlotId>> {
        let drain = self.drains.get(&epoch)?;
        Some(
            drain
                .obligations
                .iter()
                .filter(|slot| !drain.finalized.contains_key(*slot))
                .cloned()
                .collect(),
        )
    }

    pub fn happy_path_drain(&self, epoch: RaiEpoch) -> Option<&RaiHappyPathDrain> {
        self.drains.get(&epoch)
    }

    pub fn happy_path_drains(&self) -> &BTreeMap<RaiEpoch, RaiHappyPathDrain> {
        &self.drains
    }

    pub fn record_known_slot(&mut self, slot: RaiSlotId) {
        if self.known_slots.insert(slot.clone()) && !self.released_slots.contains_key(&slot) {
            self.unresolved_epochs_by_root
                .entry(slot.root)
                .or_default()
                .insert(slot.epoch);
        }
    }

    pub fn released_slots(&self) -> &BTreeMap<RaiSlotId, RaiCertifiedRelease> {
        &self.released_slots
    }

    pub fn certified_release(&self, slot: &RaiSlotId) -> Option<&RaiCertifiedRelease> {
        self.released_slots.get(slot)
    }

    /// After a cut, only included elections from the closing epoch remain
    /// enabled. An unrelated election in the already-open successor remains
    /// enabled, but a same-root retry must wait for every earlier known slot
    /// to be released by a certified close record.
    pub fn slot_election_enabled(&self, epoch: RaiEpoch, root: &QualifiedRoot) -> bool {
        let slot = RaiSlotId {
            epoch,
            root: root.clone(),
        };
        if self.released_slots.contains_key(&slot) {
            return false;
        }
        if self
            .unresolved_epochs_by_root
            .get(root)
            .and_then(|epochs| epochs.first())
            .is_some_and(|oldest| *oldest < epoch)
        {
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

    pub fn record_notarized_drain(
        &mut self,
        epoch: RaiEpoch,
        slot: &RaiSlotId,
        hash: BlockHash,
        segment: impl IntoIterator<Item = (Account, ConfirmationHeightInfo)>,
    ) -> Option<RaiDrainOutcome> {
        let outcome = self.drains.get_mut(&epoch)?.record_notarized(slot, hash)?;
        let frontiers = self.drain_frontiers.get_mut(&epoch)?;
        for (account, info) in segment {
            let current = frontiers.entry(account).or_default();
            if info.height > current.height {
                *current = info;
            }
        }
        Some(outcome)
    }

    /// Resolve a cut obligation from the ledger's durable RAI-finalization
    /// index. Finalized slots do not need their old vote certificate retained.
    pub fn record_finalized_drain(
        &mut self,
        epoch: RaiEpoch,
        slot: &RaiSlotId,
        hash: BlockHash,
        segment: impl IntoIterator<Item = (Account, ConfirmationHeightInfo)>,
    ) -> bool {
        let Some(drain) = self.drains.get_mut(&epoch) else {
            return false;
        };
        if slot.epoch != epoch || !drain.obligations.contains(slot) {
            return false;
        }
        if drain
            .finalized
            .get(slot)
            .is_some_and(|existing| *existing != hash)
        {
            return false;
        }
        // Resolution maps are mutually exclusive. A durable ledger
        // finalization supersedes an earlier certificate-derived selection or
        // release and must not make status accounting count the slot twice.
        drain.selected.remove(slot);
        drain.released.remove(slot);
        drain.finalized.insert(slot.clone(), hash);
        let Some(frontiers) = self.drain_frontiers.get_mut(&epoch) else {
            return false;
        };
        for (account, info) in segment {
            let current = frontiers.entry(account).or_default();
            if info.height > current.height {
                *current = info;
            }
        }
        true
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
                close_record_committees: self.close_record_committees.clone(),
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
        self.close_record_committees = state.close_record_committees;
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
    use crate::consensus::rai::{RaiElectionVoteState, RaiReport};
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

    fn notarized_evidence(key: &PrivateKey, hash: BlockHash) -> super::super::RaiElectionVoteState {
        let mut evidence = super::super::RaiElectionVoteState::new(vec![private_weights(key, 100)]);
        evidence
            .record_notarization_vote(
                key.public_key(),
                super::super::BlockHashOrTimeout::Block(hash),
                RaiCommitteeScope::All,
            )
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

    #[test]
    fn drain_resolution_is_invariant_to_compatible_first_final_delivery_order() {
        use super::super::{BlockHashOrTimeout, RaiLocalResult, RaiVoteStateError};

        let reps = [
            PublicKey::from(1),
            PublicKey::from(2),
            PublicKey::from(3),
            PublicKey::from(4),
            PublicKey::from(5),
            PublicKey::from(6),
        ];
        let committee = Arc::new(RepWeights::from(reps.map(|rep| (rep, Amount::raw(1)))));
        let hash = BlockHash::from(42);
        let block = BlockHashOrTimeout::Block(hash);

        let evidence_for = |final_before_first: bool| {
            let mut evidence = RaiElectionVoteState::new(vec![committee.clone()]);

            // Three block First leaves and both timeout First leaves arrive first.
            for rep in &reps[..3] {
                evidence
                    .record_first_vote(*rep, block, RaiCommitteeScope::All)
                    .unwrap();
            }
            for rep in &reps[4..] {
                evidence
                    .record_first_vote(*rep, BlockHashOrTimeout::Timeout, RaiCommitteeScope::All)
                    .unwrap();
            }

            evidence
                .record_final_vote(reps[0], hash, RaiCommitteeScope::All)
                .unwrap();
            if final_before_first {
                evidence
                    .record_final_vote(reps[3], hash, RaiCommitteeScope::All)
                    .unwrap();
                evidence
                    .record_first_vote(reps[3], block, RaiCommitteeScope::All)
                    .unwrap();
            } else {
                evidence
                    .record_first_vote(reps[3], block, RaiCommitteeScope::All)
                    .unwrap();
                evidence
                    .record_final_vote(reps[3], hash, RaiCommitteeScope::All)
                    .unwrap();
            }

            // These complete the four observed Final leaves, but the two
            // timeout-supported Finals are invalid in either permutation.
            for rep in &reps[4..] {
                assert_eq!(
                    evidence.record_final_vote(*rep, hash, RaiCommitteeScope::All),
                    Err(RaiVoteStateError::IncompatibleFinalSupport)
                );
            }
            evidence
        };

        let final_before_first = evidence_for(true);
        let first_before_final = evidence_for(false);
        for evidence in [&final_before_first, &first_before_final] {
            assert_eq!(evidence.first_tally(0, block), Amount::raw(4));
            assert_eq!(
                evidence.first_tally(0, BlockHashOrTimeout::Timeout),
                Amount::raw(2)
            );
            assert_eq!(evidence.final_tally(0, hash), Amount::raw(2));
            assert_eq!(
                evidence.local_result(0),
                Some(RaiLocalResult::Notarized(hash))
            );
        }

        let slot = slot(QualifiedRoot::new(1.into(), 2.into()));
        let resolve = |evidence: &RaiElectionVoteState| {
            let mut drain = RaiHappyPathDrain {
                epoch: RaiEpoch::ZERO,
                obligations: BTreeSet::from([slot.clone()]),
                finalized: BTreeMap::new(),
                selected: BTreeMap::new(),
                released: BTreeMap::new(),
            };
            let outcome = drain.record_persistent_evidence(&slot, evidence);
            assert!(drain.is_complete());
            outcome
        };

        let final_before_first_outcome = resolve(&final_before_first);
        let first_before_final_outcome = resolve(&first_before_final);
        assert_eq!(
            final_before_first_outcome,
            Some(RaiDrainOutcome::Selected(hash))
        );
        assert_eq!(first_before_final_outcome, final_before_first_outcome);
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
    fn persistent_evidence_outcome_does_not_clone_or_mutate_the_drain() {
        let key = PrivateKey::from(1);
        let slot = slot(QualifiedRoot::new(1.into(), 2.into()));
        let hash = BlockHash::from(10);
        let evidence = final_evidence(&key, hash);
        let mut drain = RaiHappyPathDrain {
            epoch: RaiEpoch::ZERO,
            obligations: BTreeSet::from([slot.clone()]),
            finalized: BTreeMap::new(),
            selected: BTreeMap::new(),
            released: BTreeMap::new(),
        };

        assert_eq!(
            drain.persistent_evidence_outcome(&slot, &evidence),
            Some(RaiDrainOutcome::Finalized(hash))
        );
        assert!(drain.finalized.is_empty());
        assert!(drain.selected.is_empty());
        assert!(drain.released.is_empty());
        assert!(!drain.is_complete());

        assert_eq!(
            drain.record_persistent_evidence(&slot, &evidence),
            Some(RaiDrainOutcome::Finalized(hash))
        );
        assert!(drain.is_complete());
    }

    #[test]
    fn durable_drain_checks_skip_only_already_finalized_slots() {
        let finalized = slot(QualifiedRoot::new(1.into(), 2.into()));
        let selected = slot(QualifiedRoot::new(3.into(), 4.into()));
        let released = slot(QualifiedRoot::new(5.into(), 6.into()));
        let unresolved = slot(QualifiedRoot::new(7.into(), 8.into()));
        let mut manager = RaiEpochManager::new(Arc::new(RepWeights::default()), BlockHash::ZERO);
        manager.drains.insert(
            RaiEpoch::ZERO,
            RaiHappyPathDrain {
                epoch: RaiEpoch::ZERO,
                obligations: BTreeSet::from([
                    finalized.clone(),
                    selected.clone(),
                    released.clone(),
                    unresolved.clone(),
                ]),
                finalized: BTreeMap::from([(finalized, BlockHash::from(10))]),
                selected: BTreeMap::from([(selected.clone(), BlockHash::from(11))]),
                released: BTreeMap::from([(released.clone(), RaiDrainOutcome::ReleasedTimeout)]),
            },
        );

        assert_eq!(
            manager.obligations_requiring_durable_check(RaiEpoch::ZERO),
            Some(vec![selected, released, unresolved.clone()])
        );
        assert_eq!(
            manager.unresolved_drain_obligations(RaiEpoch::ZERO),
            Some(vec![unresolved])
        );
    }

    #[test]
    fn drain_resolution_maps_remain_mutually_exclusive() {
        let key = PrivateKey::from(1);
        let finalized = slot(QualifiedRoot::new(1.into(), 2.into()));
        let selected = slot(QualifiedRoot::new(3.into(), 4.into()));
        let released = slot(QualifiedRoot::new(5.into(), 6.into()));
        let mut drain = RaiHappyPathDrain {
            epoch: RaiEpoch::ZERO,
            obligations: BTreeSet::from([finalized.clone(), selected.clone(), released.clone()]),
            finalized: BTreeMap::new(),
            selected: BTreeMap::new(),
            released: BTreeMap::new(),
        };

        assert_eq!(
            drain.record_persistent_evidence(
                &finalized,
                &final_evidence(&key, BlockHash::from(10)),
            ),
            Some(RaiDrainOutcome::Finalized(BlockHash::from(10)))
        );
        assert_eq!(
            drain.record_notarized(&selected, BlockHash::from(11)),
            Some(RaiDrainOutcome::Selected(BlockHash::from(11)))
        );
        assert_eq!(
            drain.record_persistent_evidence(&released, &timeout_evidence(&key)),
            Some(RaiDrainOutcome::ReleasedTimeout)
        );

        assert!(drain.is_complete());
        assert_eq!(
            drain.finalized.len() + drain.selected.len() + drain.released.len(),
            drain.obligations.len()
        );
        assert!(
            drain.finalized.keys().all(
                |slot| !drain.selected.contains_key(slot) && !drain.released.contains_key(slot)
            )
        );
        assert!(
            drain
                .selected
                .keys()
                .all(|slot| !drain.released.contains_key(slot))
        );
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
    fn durable_finalization_replaces_an_earlier_drain_outcome() {
        let root = slot(QualifiedRoot::new(1.into(), 2.into()));
        let selected = BlockHash::from(10);
        let finalized = BlockHash::from(11);
        let mut manager = RaiEpochManager::new(Arc::new(RepWeights::default()), BlockHash::ZERO);
        manager.drains.insert(
            RaiEpoch::ZERO,
            RaiHappyPathDrain {
                epoch: RaiEpoch::ZERO,
                obligations: BTreeSet::from([root.clone()]),
                finalized: BTreeMap::new(),
                selected: BTreeMap::from([(root.clone(), selected)]),
                released: BTreeMap::new(),
            },
        );
        manager.initialize_drain_frontiers(RaiEpoch::ZERO, []);

        assert!(manager.record_finalized_drain(RaiEpoch::ZERO, &root, finalized, [],));

        let drain = manager.happy_path_drain(RaiEpoch::ZERO).unwrap();
        assert_eq!(drain.finalized.get(&root), Some(&finalized));
        assert!(!drain.selected.contains_key(&root));
        assert!(!drain.released.contains_key(&root));
        assert_eq!(
            drain.finalized.len() + drain.selected.len() + drain.released.len(),
            drain.obligations.len()
        );
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
    fn certified_cut_remains_valid_after_fresh_visibility_grows() {
        let key = PrivateKey::from(1);
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        let first = slot(QualifiedRoot::new(11.into(), 12.into()));
        let late = slot(QualifiedRoot::new(21.into(), 22.into()));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
            .unwrap();
        let (_, certified) = manager.begin_cut_election([first.clone()]).unwrap();

        let fresh = manager
            .refresh_close_cut_candidate(RaiEpoch::ZERO, 0, [first.clone(), late])
            .unwrap();
        assert_ne!(fresh, certified);

        assert_eq!(
            manager.install_cut(RaiEpoch::ZERO, 0, certified).unwrap(),
            &BTreeSet::from([first])
        );
        assert_eq!(manager.decided_close_hash(RaiEpoch::ZERO), Some(certified));
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
    fn advance_gate_skips_pending_and_decided_cut_candidates() {
        let key = PrivateKey::from(1);
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
            .unwrap();
        let (_, selected) = manager.begin_cut_election([]).unwrap();
        assert_eq!(manager.close_cuts.all().len(), 1);

        manager
            .visible_obligations
            .get_mut(&RaiEpoch::ZERO)
            .unwrap()
            .insert(slot(QualifiedRoot::new(11.into(), 12.into())));
        assert!(manager.advance_close_cut_round().is_none());
        assert_eq!(manager.close_cuts.all().len(), 1);

        assert!(manager.store_close_cut_evidence(
            RaiEpoch::ZERO,
            0,
            final_evidence(&key, selected),
        ));
        manager
            .visible_obligations
            .get_mut(&RaiEpoch::ZERO)
            .unwrap()
            .insert(slot(QualifiedRoot::new(13.into(), 14.into())));
        assert!(manager.advance_close_cut_round().is_none());
        assert_eq!(manager.close_cuts.all().len(), 1);
        assert_eq!(manager.close_cut_round(RaiEpoch::ZERO), Some(0));
    }

    #[test]
    fn advance_gate_materializes_dead_cut_candidate() {
        let key = PrivateKey::from(1);
        let later = slot(QualifiedRoot::new(11.into(), 12.into()));
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
            .unwrap();
        manager.begin_cut_election([]).unwrap();
        manager
            .visible_obligations
            .get_mut(&RaiEpoch::ZERO)
            .unwrap()
            .insert(later.clone());
        assert!(manager.store_close_cut_evidence(RaiEpoch::ZERO, 0, timeout_evidence(&key),));

        let (_, fresh) = manager.advance_close_cut_round().unwrap();
        assert_eq!(manager.close_cuts.all().len(), 2);
        assert_eq!(manager.close_cut_round(RaiEpoch::ZERO), Some(1));
        assert_eq!(
            manager.close_cuts.get(&fresh).unwrap().obligations,
            BTreeSet::from([later])
        );
    }

    #[test]
    fn advance_gate_skips_pending_record_and_carries_live_hash() {
        let key = PrivateKey::from(1);
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
            .unwrap();
        let (_, cut) = manager.begin_cut_election([]).unwrap();
        manager.install_cut(RaiEpoch::ZERO, 0, cut).unwrap();
        manager.initialize_drain_frontiers(RaiEpoch::ZERO, []);
        let (_, selected) = manager.begin_close_record(RepWeights::default()).unwrap();
        assert_eq!(manager.close_records.all().len(), 1);

        manager
            .drain_frontiers
            .get_mut(&RaiEpoch::ZERO)
            .unwrap()
            .insert(
                Account::from(1),
                ConfirmationHeightInfo::new(1, BlockHash::from(10)),
            );
        let pending_reads = std::cell::Cell::new(0);
        let pending_heights = [(
            Account::from(2),
            ConfirmationHeightInfo::new(2, BlockHash::from(20)),
        )]
        .into_iter()
        .inspect(|_| pending_reads.set(pending_reads.get() + 1));
        assert!(
            manager
                .advance_close_record_round(pending_heights)
                .is_none()
        );
        assert_eq!(pending_reads.get(), 0);
        assert_eq!(manager.close_records.all().len(), 1);

        assert!(manager.store_close_record_evidence(
            RaiEpoch::ZERO,
            0,
            notarized_evidence(&key, selected),
        ));
        manager
            .drain_frontiers
            .get_mut(&RaiEpoch::ZERO)
            .unwrap()
            .insert(
                Account::from(3),
                ConfirmationHeightInfo::new(3, BlockHash::from(30)),
            );
        let carry_reads = std::cell::Cell::new(0);
        let carry_heights = [(
            Account::from(4),
            ConfirmationHeightInfo::new(4, BlockHash::from(40)),
        )]
        .into_iter()
        .inspect(|_| carry_reads.set(carry_reads.get() + 1));
        let (_, carried) = manager.advance_close_record_round(carry_heights).unwrap();
        assert_eq!(carry_reads.get(), 0);
        assert_eq!(carried, selected);
        assert_eq!(manager.close_record_round(RaiEpoch::ZERO), Some(1));
        assert_eq!(manager.close_records.all().len(), 1);
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
        let (_, close) = manager.begin_close_record(RepWeights::default()).unwrap();
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
    fn included_timeout_release_enables_successor_retry_after_close_installation() {
        let key = PrivateKey::from(1);
        let old = slot(QualifiedRoot::new(11.into(), 12.into()));
        let retry = RaiSlotId {
            epoch: RaiEpoch::new(1),
            root: old.root.clone(),
        };
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, [old.clone()]))
            .unwrap();
        let (_, cut) = manager.begin_cut_election([old.clone()]).unwrap();
        manager.install_cut(RaiEpoch::ZERO, 0, cut).unwrap();
        manager.initialize_drain_frontiers(RaiEpoch::ZERO, []);
        assert_eq!(
            manager.record_drain_evidence(RaiEpoch::ZERO, &old, &timeout_evidence(&key), [],),
            Some(RaiDrainOutcome::ReleasedTimeout)
        );
        assert!(!manager.slot_election_enabled(retry.epoch, &retry.root));

        let (_, close) = manager.begin_close_record(RepWeights::default()).unwrap();
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
        assert!(manager.slot_election_enabled(retry.epoch, &retry.root));
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
            root: root.clone(),
        };
        let later_retry = RaiSlotId {
            epoch: RaiEpoch::new(2),
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

        // The successor epoch is open, but reusing an unresolved old root is
        // a retry and therefore remains disabled until the close certifies a
        // release for the old slot.
        assert!(!manager.slot_election_enabled(retry.epoch, &retry.root));
        manager.initialize_drain_frontiers(RaiEpoch::ZERO, []);
        let (_, close) = manager.begin_close_record(RepWeights::default()).unwrap();
        manager
            .install_close_record(RaiEpoch::ZERO, 0, close)
            .unwrap();

        assert!(manager.certified_release(&old).is_some());
        assert!(manager.certified_release(&retry).is_none());
        assert!(manager.slot_election_enabled(retry.epoch, &retry.root));
        // Releasing epoch zero advances the per-root lock to epoch one; it
        // must not accidentally make every later retry eligible.
        assert!(!manager.slot_election_enabled(later_retry.epoch, &later_retry.root));
    }

    #[test]
    fn six_replicas_refresh_a_dead_split_record_round_from_exact_slot_payload() {
        const REPLICAS: usize = 6;
        let key = PrivateKey::from(1);
        let committee = private_weights(&key, 100);
        let slot = slot(QualifiedRoot::new(11.into(), 12.into()));
        let selected = BlockHash::from(22);
        let account = Account::from(33);
        let selected_frontier = ConfirmationHeightInfo::new(1, selected);
        let mut replicas = (0..REPLICAS)
            .map(|_| RaiEpochManager::new(committee.clone(), BlockHash::from(7)))
            .collect::<Vec<_>>();

        let mut round_zero = Vec::new();
        for (index, replica) in replicas.iter_mut().enumerate() {
            assert!(replica.start_closing(Timestamp::new_test_instance()));
            replica
                .reports_mut()
                .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
                .unwrap();
            let (_, cut) = replica.begin_cut_election([slot.clone()]).unwrap();
            replica.install_cut(RaiEpoch::ZERO, 0, cut).unwrap();
            assert!(replica.initialize_drain_frontiers(RaiEpoch::ZERO, []));

            // Every replica derives the same notarized slot resolution, but
            // only half initially possess its validated block preimage. This
            // recreates the observed 3/3 close-record preference split.
            let segment = (index >= REPLICAS / 2)
                .then(|| [(account, selected_frontier.clone())])
                .into_iter()
                .flatten();
            assert_eq!(
                replica.record_notarized_drain(RaiEpoch::ZERO, &slot, selected, segment),
                Some(RaiDrainOutcome::Selected(selected))
            );
            round_zero.push(
                replica
                    .begin_close_record(committee.as_ref().clone())
                    .unwrap()
                    .1,
            );
            assert!(
                replica.store_close_record_evidence(RaiEpoch::ZERO, 0, timeout_evidence(&key),)
            );
        }
        assert!(
            round_zero[..REPLICAS / 2]
                .windows(2)
                .all(|pair| pair[0] == pair[1])
        );
        assert!(
            round_zero[REPLICAS / 2..]
                .windows(2)
                .all(|pair| pair[0] == pair[1])
        );
        assert_ne!(round_zero[0], round_zero[REPLICAS / 2]);

        let mut round_one = Vec::new();
        for replica in &mut replicas {
            // Publish/reconciliation has no authority by itself: refinement
            // is accepted only for the exact hash already selected by this
            // replica's certificate-derived drain outcome.
            assert_eq!(
                replica.record_notarized_drain(
                    RaiEpoch::ZERO,
                    &slot,
                    selected,
                    [(account, selected_frontier.clone())],
                ),
                Some(RaiDrainOutcome::Selected(selected))
            );
            round_one.push(replica.advance_close_record_round([]).unwrap().1);
        }
        assert!(round_one.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(round_one[0], round_zero[REPLICAS / 2]);

        for replica in &mut replicas {
            assert!(replica.store_close_record_evidence(
                RaiEpoch::ZERO,
                1,
                final_evidence(&key, round_one[0]),
            ));
            let tracker = replica.record_rounds.get_mut(&RaiEpoch::ZERO).unwrap();
            assert_eq!(
                tracker.round(1).unwrap().derive(),
                super::super::RaiCloseRoundResult::Decided(round_one[0])
            );
            assert!(tracker.decide(1, round_one[0]));
            assert_eq!(tracker.decision(), Some((1, round_one[0])));
        }
    }

    #[test]
    fn six_replicas_close_release_and_retry_deterministically() {
        const REPLICAS: usize = 6;
        let keys = (1..=REPLICAS as u64)
            .map(PrivateKey::from)
            .collect::<Vec<_>>();
        let committee = Arc::new(RepWeights::from([
            (keys[0].public_key(), Amount::raw(1)),
            (keys[1].public_key(), Amount::raw(1)),
            (keys[2].public_key(), Amount::raw(1)),
            (keys[3].public_key(), Amount::raw(1)),
            (keys[4].public_key(), Amount::raw(1)),
            (keys[5].public_key(), Amount::raw(1)),
        ]));
        let mut replicas = (0..REPLICAS)
            .map(|_| RaiEpochManager::new(committee.clone(), BlockHash::from(7)))
            .collect::<Vec<_>>();

        let included = (1..=3u64)
            .map(|number| RaiSlotId {
                epoch: RaiEpoch::ZERO,
                root: QualifiedRoot::new(number.into(), (number + 10).into()),
            })
            .collect::<Vec<_>>();
        let omitted = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: QualifiedRoot::new(90.into(), 91.into()),
        };
        let retry = RaiSlotId {
            epoch: RaiEpoch::new(1),
            root: omitted.root.clone(),
        };

        // Publishing is deliberately epoch-qualified.  Every replica knows
        // the old minority candidate, but only one representative reports it.
        for replica in &mut replicas {
            for slot in included.iter().chain([&omitted]) {
                replica.record_known_slot(slot.clone());
            }
            assert!(replica.start_closing(Timestamp::new_test_instance()));
            assert_eq!(replica.current_epoch(), RaiEpoch::new(1));
            assert!(replica.certified_release(&omitted).is_none());
        }
        let reports = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let mut visible = included.clone();
                if index == 0 {
                    visible.push(omitted.clone());
                }
                RaiReport::new(key, RaiEpoch::ZERO, visible)
            })
            .collect::<Vec<_>>();

        for replica in &mut replicas {
            for report in &reports {
                replica.reports_mut().insert(report.clone()).unwrap();
            }
        }

        let cut_hashes = replicas
            .iter_mut()
            .map(|replica| replica.begin_cut_election([]).unwrap().1)
            .collect::<Vec<_>>();
        assert!(cut_hashes.windows(2).all(|pair| pair[0] == pair[1]));
        for (replica, cut) in replicas.iter_mut().zip(&cut_hashes) {
            assert_eq!(
                replica.install_cut(RaiEpoch::ZERO, 0, *cut).unwrap(),
                &included.iter().cloned().collect()
            );
        }

        let account = Account::from(44);
        let base = ConfirmationHeightInfo::new(1, BlockHash::from(100));
        let candidate_hashes = [
            BlockHash::from(101),
            BlockHash::from(102),
            BlockHash::from(103),
        ];
        for replica in &mut replicas {
            assert!(replica.initialize_drain_frontiers(RaiEpoch::ZERO, [(account, base.clone())]));
            for (index, slot) in included.iter().enumerate() {
                // Each slot has its own vote state. Four of six equal
                // representatives are exactly the final threshold.
                let mut evidence = RaiElectionVoteState::new(vec![committee.clone()]);
                for key in keys.iter().take(4) {
                    evidence
                        .record_final_vote(
                            key.public_key(),
                            candidate_hashes[index],
                            RaiCommitteeScope::All,
                        )
                        .unwrap();
                }
                assert_eq!(
                    replica.record_drain_evidence(
                        RaiEpoch::ZERO,
                        slot,
                        &evidence,
                        [(
                            account,
                            ConfirmationHeightInfo::new(index as u64 + 2, candidate_hashes[index],),
                        )],
                    ),
                    Some(RaiDrainOutcome::Finalized(candidate_hashes[index]))
                );
            }
        }

        let close_hashes = replicas
            .iter_mut()
            .map(|replica| replica.begin_close_record(RepWeights::default()).unwrap().1)
            .collect::<Vec<_>>();
        assert!(close_hashes.windows(2).all(|pair| pair[0] == pair[1]));

        // The same-root successor retry cannot be authorized by release until
        // the close record is installed, and its epoch never aliases the old
        // election identity.
        assert!(
            replicas
                .iter()
                .all(|replica| replica.certified_release(&omitted).is_none())
        );
        assert_ne!(omitted, retry);

        let expected_frontier = ConfirmationHeightInfo::new(4, candidate_hashes[2]);
        for (replica, close) in replicas.iter_mut().zip(&close_hashes) {
            let installed = replica
                .install_certified_close_record(
                    RaiEpoch::ZERO,
                    0,
                    *close,
                    committee.as_ref().clone(),
                )
                .unwrap();
            assert_eq!(installed[&account], expected_frontier);
        }

        let expected_releases = BTreeSet::from([omitted.clone()]);
        for replica in &replicas {
            assert_eq!(
                replica
                    .released_slots()
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                expected_releases
            );
            assert!(replica.certified_release(&omitted).is_some());
            assert!(replica.certified_release(&retry).is_none());
            assert!(replica.slot_election_enabled(retry.epoch, &retry.root));
            assert_eq!(
                replica.happy_path_drain(RaiEpoch::ZERO).unwrap().finalized,
                included
                    .iter()
                    .cloned()
                    .zip(candidate_hashes)
                    .collect::<BTreeMap<_, _>>()
            );
        }
        assert!(replicas.windows(2).all(|pair| {
            pair[0].drain_frontiers(RaiEpoch::ZERO) == pair[1].drain_frontiers(RaiEpoch::ZERO)
        }));
    }

    #[test]
    fn lagging_drain_retains_and_installs_remote_close_record() {
        let key = PrivateKey::from(1);
        let mut manager = RaiEpochManager::new(private_weights(&key, 100), BlockHash::from(7));
        manager.start_closing(Timestamp::new_test_instance());
        manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, []))
            .unwrap();
        let (_, cut) = manager.begin_cut_election([]).unwrap();
        manager.install_cut(RaiEpoch::ZERO, 0, cut).unwrap();

        let local = [(
            Account::from(1),
            ConfirmationHeightInfo::new(1, BlockHash::from(10)),
        )];
        let remote_frontiers = [(
            Account::from(1),
            ConfirmationHeightInfo::new(2, BlockHash::from(20)),
        )];
        let remote = RaiCloseRecord::new(RaiEpoch::ZERO, BlockHash::ZERO, remote_frontiers);
        let remote_hash = remote.hash();

        assert_eq!(
            manager.closing_epoch().unwrap().phase,
            RaiClosingPhase::Draining
        );
        assert_eq!(
            manager.reconcile_close_record(remote, 0),
            Some((RaiEpoch::ZERO, 0, remote_hash))
        );
        assert!(manager.initialize_drain_frontiers(RaiEpoch::ZERO, local));
        let (_, local_hash) = manager
            .begin_close_record(private_weights(&key, 100).as_ref().clone())
            .unwrap();
        assert_ne!(local_hash, remote_hash);
        assert!(
            manager
                .close_record_tracker(RaiEpoch::ZERO)
                .unwrap()
                .round(0)
                .unwrap()
                .validated_preimages
                .contains(&remote_hash)
        );

        let installed = manager
            .install_certified_close_record(
                RaiEpoch::ZERO,
                0,
                remote_hash,
                private_weights(&key, 100).as_ref().clone(),
            )
            .unwrap();
        assert_eq!(installed[&Account::from(1)].frontier, BlockHash::from(20));
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

        assert!(manager.begin_close_record(RepWeights::default()).is_none());
        assert!(manager.initialize_drain_frontiers(0.into(), frontiers.clone()));
        let derived = weights(2, 200).as_ref().clone();
        let (root, close) = manager.begin_close_record(derived).unwrap();
        assert_eq!(
            root,
            crate::consensus::rai::rai_close_record_root(0.into(), 0)
        );

        let expected: RaiFrontierMap = frontiers.clone().into_iter().collect();
        assert_eq!(
            manager.install_certified_close_record_after(
                0.into(),
                0,
                close,
                weights(2, 200).as_ref().clone(),
                |_, committed| {
                    assert_eq!(committed, &expected);
                    false
                },
            ),
            Err(CloseRecordDecisionError::LedgerCommitFailed)
        );
        assert_eq!(
            manager.closing_epoch().unwrap().phase,
            RaiClosingPhase::ElectingRecord
        );
        assert_eq!(manager.state().closed_through, None);
        assert_eq!(manager.installed_close_hash(0.into()), None);

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
        assert!(!manager.slot_election_enabled(RaiEpoch::new(1), &obligation.root));
        assert!(manager.begin_close_record(RepWeights::default()).is_none());
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
        let (_, record) = manager.begin_close_record(RepWeights::default()).unwrap();
        assert!(manager.begin_close_record(RepWeights::default()).is_none());
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
            hashes.push(manager.begin_close_record(RepWeights::default()).unwrap().1);
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
        let (_, canonical) = manager.begin_close_record(RepWeights::default()).unwrap();

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
