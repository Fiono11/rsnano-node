use std::collections::{BTreeMap, BTreeSet};

use crate::block::{AccountState, BlockStore, GenesisAccount, SendId, SignedBlock};
use crate::certificate::{derive_global_result, GlobalResult};
use crate::close::{
    hash_close_cut, CertifiedCloseState, CloseCutCandidate, ClosePackage, ElectionCut,
    JointReportProof, SignedReport, SlotStatus,
};
use crate::committee::Committee;
use crate::crypto::{CryptoProvider, DemoKeyStore};
use crate::error::{RaiError, Result};
use crate::types::{
    put_u64, CommitteeId, ElectionId, Epoch, Hash32, ReplicaId, Slot, VoteValue, Weight,
};
use crate::vote::{SignedVote, VoteKind, VotePool};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EpochState {
    Open,
    Closing,
    Closed,
}

#[derive(Clone, Debug)]
pub struct RaiEngine {
    pub crypto: DemoKeyStore,
    pub committees: BTreeMap<CommitteeId, Committee>,
    pub pool: VotePool,
    blocks: BlockStore,
    elections: BTreeMap<ElectionId, Vec<CommitteeId>>,
    outcomes: BTreeMap<ElectionId, GlobalResult>,
    epoch_states: BTreeMap<Epoch, EpochState>,
    close_cuts: BTreeMap<Epoch, BTreeSet<ElectionId>>,
    certified_close_cut_hashes: BTreeMap<Epoch, Hash32>,
    close_states: BTreeMap<Epoch, CertifiedCloseState>,
    close_hashes: BTreeMap<Epoch, Hash32>,
    derived_committees: BTreeMap<Epoch, CommitteeId>,
    genesis_committee: Option<CommitteeId>,
    committee_registry_frozen: bool,
    stake_derived_committees: bool,
    committee_fault_weight: Weight,
    committee_participation_weight: Weight,
    obligations: BTreeSet<(ReplicaId, Slot, Epoch, Hash32)>,
    slot_vote_locks: BTreeMap<(ReplicaId, Slot), ElectionId>,
    receive_vote_locks: BTreeMap<(ReplicaId, SendId), (ElectionId, Hash32)>,
    block_finalized_epochs: BTreeMap<Hash32, Epoch>,
    reports: BTreeMap<(Epoch, ReplicaId), SignedReport>,
    visible: BTreeMap<Epoch, BTreeSet<ElectionId>>,
    close_cut_candidates: BTreeMap<Hash32, CloseCutCandidate>,
    pending_close_cut_votes:
        BTreeMap<(ElectionId, CommitteeId, ReplicaId, VoteValue, VoteKind), SignedVote>,
    pending_close_record_votes:
        BTreeMap<(ElectionId, CommitteeId, ReplicaId, VoteValue, VoteKind), SignedVote>,
    close_record_candidates: BTreeMap<(Epoch, Hash32), ClosePackage>,
    package_evidence: VotePool,
    now_ms: u64,
    election_timeout_ms: u64,
    election_started_at_ms: BTreeMap<ElectionId, u64>,
}

const DEFAULT_ELECTION_TIMEOUT_MS: u64 = 1_000;

impl RaiEngine {
    /// Creates a protocol-only engine without an account authorization
    /// registry. Runtime block submission will be rejected; use
    /// `with_account_genesis` or `with_genesis` when processing blocks.
    pub fn new(crypto: DemoKeyStore, genesis_close_hash: Hash32) -> Self {
        let mut epoch_states = BTreeMap::new();
        epoch_states.insert(0, EpochState::Open);
        let mut close_hashes = BTreeMap::new();
        close_hashes.insert(u64::MAX, genesis_close_hash);
        Self {
            crypto,
            committees: BTreeMap::new(),
            pool: VotePool::default(),
            blocks: BlockStore::default(),
            elections: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            epoch_states,
            close_cuts: BTreeMap::new(),
            certified_close_cut_hashes: BTreeMap::new(),
            close_states: BTreeMap::new(),
            close_hashes,
            derived_committees: BTreeMap::new(),
            genesis_committee: None,
            committee_registry_frozen: false,
            stake_derived_committees: false,
            committee_fault_weight: 1,
            committee_participation_weight: 1,
            obligations: BTreeSet::new(),
            slot_vote_locks: BTreeMap::new(),
            receive_vote_locks: BTreeMap::new(),
            block_finalized_epochs: BTreeMap::new(),
            reports: BTreeMap::new(),
            visible: BTreeMap::new(),
            close_cut_candidates: BTreeMap::new(),
            pending_close_cut_votes: BTreeMap::new(),
            pending_close_record_votes: BTreeMap::new(),
            close_record_candidates: BTreeMap::new(),
            package_evidence: VotePool::default(),
            now_ms: 0,
            election_timeout_ms: DEFAULT_ELECTION_TIMEOUT_MS,
            election_started_at_ms: BTreeMap::new(),
        }
    }

    /// Creates an engine with an authenticated account genesis while leaving
    /// committee registration to the caller. This is useful for protocol tests
    /// and deployments whose committee source is external to account stake.
    pub fn with_account_genesis(
        crypto: DemoKeyStore,
        genesis_accounts: impl IntoIterator<Item = GenesisAccount>,
    ) -> Result<Self> {
        let genesis_accounts = genesis_accounts.into_iter().collect::<Vec<_>>();
        if let Some(account) = genesis_accounts
            .iter()
            .find(|account| !crypto.contains(account.representative))
        {
            return Err(RaiError::InvalidConfiguration(format!(
                "genesis account {} delegates to unknown replica {}",
                account.account, account.representative
            )));
        }
        let blocks = BlockStore::with_genesis(genesis_accounts)?;
        let genesis_close_hash = blocks.genesis_close_hash()?;
        let mut engine = Self::new(crypto, genesis_close_hash);
        engine.blocks = blocks;
        for genesis in engine.blocks.genesis_accounts().values() {
            engine.block_finalized_epochs.insert(genesis.hash(), 0);
        }
        Ok(engine)
    }

    /// Creates a balance-bearing engine from configured genesis blocks.
    /// The genesis representative weights form the committee used by epochs 0
    /// and 1. Every certified close later derives an immutable weighted
    /// committee snapshot from final account frontiers.
    pub fn with_genesis(
        crypto: DemoKeyStore,
        genesis_committee_id: CommitteeId,
        genesis_accounts: impl IntoIterator<Item = GenesisAccount>,
        fault_weight: Weight,
        participation_weight: Weight,
    ) -> Result<Self> {
        let mut engine = Self::with_account_genesis(crypto, genesis_accounts)?;
        let weights = engine.blocks.representative_weights()?;
        let committee = Committee::weighted(
            genesis_committee_id,
            weights,
            fault_weight,
            participation_weight,
        )?;
        engine.committees.insert(genesis_committee_id, committee);
        engine.genesis_committee = Some(genesis_committee_id);
        engine.committee_registry_frozen = true;
        engine.stake_derived_committees = true;
        engine.committee_fault_weight = fault_weight;
        engine.committee_participation_weight = participation_weight;
        Ok(engine)
    }

    pub fn account_states(&self) -> Result<BTreeMap<u64, AccountState>> {
        self.blocks.account_states()
    }

    pub fn genesis_committee_id(&self) -> Option<CommitteeId> {
        self.genesis_committee
    }

    /// Sets the monotone logical clock used by correct-sender timeout guards.
    pub fn set_now_ms(&mut self, now_ms: u64) -> Result<()> {
        if now_ms < self.now_ms {
            return Err(RaiError::InvalidConfiguration(
                "the engine clock cannot move backwards".into(),
            ));
        }
        self.now_ms = now_ms;
        Ok(())
    }

    pub fn advance_time_ms(&mut self, delta_ms: u64) -> Result<u64> {
        self.now_ms = self
            .now_ms
            .checked_add(delta_ms)
            .ok_or_else(|| RaiError::InvalidConfiguration("engine clock overflow".into()))?;
        Ok(self.now_ms)
    }

    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    pub fn set_election_timeout_ms(&mut self, timeout_ms: u64) {
        self.election_timeout_ms = timeout_ms;
    }

    pub fn election_started_at_ms(&self, election: &ElectionId) -> Option<u64> {
        self.election_started_at_ms.get(election).copied()
    }

    /// Starts an election timer when valid candidate processing begins. First
    /// votes also start the timer automatically when it is still unset.
    pub fn start_election_timer(&mut self, election: &ElectionId) -> Result<bool> {
        self.require_active(election)?;
        if self.election_started_at_ms.contains_key(election) {
            return Ok(false);
        }
        self.election_started_at_ms
            .insert(election.clone(), self.now_ms);
        Ok(true)
    }

    pub fn add_committee(&mut self, committee: Committee) -> Result<()> {
        if self.committee_registry_frozen {
            return Err(RaiError::InvalidCommittee(
                "committee registry is frozen after election activity begins".into(),
            ));
        }
        committee.validate()?;
        if self.committees.contains_key(&committee.id) {
            return Err(RaiError::InvalidCommittee(
                "committee id already registered".into(),
            ));
        }
        self.committees.insert(committee.id, committee);
        Ok(())
    }

    /// Registers an election only when the supplied committee set equals the
    /// lagged set derived from committeeAt(e-3) and committeeAt(e-2).
    pub fn register_election(
        &mut self,
        election: ElectionId,
        committee_ids: impl IntoIterator<Item = CommitteeId>,
    ) -> Result<()> {
        self.require_registration_enabled(&election)?;
        let expected = self.expected_committee_ids(election.epoch())?;
        let supplied = committee_ids.into_iter().collect::<BTreeSet<_>>();
        let expected_set = expected.iter().copied().collect::<BTreeSet<_>>();
        if supplied != expected_set {
            return Err(RaiError::InvalidCommittee(format!(
                "election {election} requires derived committees {:?}, supplied {:?}",
                expected_set, supplied
            )));
        }
        self.elections.insert(election, expected);
        Ok(())
    }

    pub fn register_derived_election(&mut self, election: ElectionId) -> Result<()> {
        // Check lifecycle enablement before deriving committees so a rejected
        // future-epoch registration cannot freeze the committee registry.
        self.require_registration_enabled(&election)?;
        let ids = self.expected_committee_ids(election.epoch())?;
        self.register_election(election, ids)
    }

    pub fn epoch_state(&self, epoch: Epoch) -> Option<EpochState> {
        self.epoch_states.get(&epoch).copied()
    }

    /// Returns a read-only view of protocol-managed block state. Completion
    /// and finalization mutations are intentionally available only through
    /// validated engine transitions.
    pub fn blocks(&self) -> &BlockStore {
        &self.blocks
    }

    /// Computes the exact Reportable set for one correct replica at the start
    /// of epoch close. Finished elections remain reportable while the signer
    /// has any unreleased non-timeout support obligation.
    pub fn reportable_elections(
        &self,
        signer: ReplicaId,
        epoch: Epoch,
    ) -> Result<BTreeSet<ElectionId>> {
        let committees = self.committees_for_epoch(epoch)?;
        if !committees
            .iter()
            .any(|committee| committee.contains(signer))
        {
            return Err(RaiError::InvalidVote(format!(
                "reporter {signer} is outside the epoch committee set"
            )));
        }

        let mut reportable = BTreeSet::new();
        for election in self.elections.keys().filter(|election| {
            matches!(election, ElectionId::Slot { epoch: election_epoch, .. } if *election_epoch == epoch)
        }) {
            if !self.election_started_at_ms.contains_key(election) {
                continue;
            }
            let finished = self.derive_result(election)?.is_some();
            let has_unreleased_support = committees
                .iter()
                .filter(|committee| committee.contains(signer))
                .flat_map(|committee| {
                    self.pool
                        .support_values(signer, committee.id, election)
                        .into_iter()
                })
                .filter_map(VoteValue::candidate)
                .any(|block| !self.certified_released_value(election, block));
            if !finished || has_unreleased_support {
                reportable.insert(election.clone());
            }
        }
        Ok(reportable)
    }

    pub fn build_signed_report(&self, signer: ReplicaId, epoch: Epoch) -> Result<SignedReport> {
        SignedReport::new(
            &self.crypto,
            signer,
            epoch,
            self.reportable_elections(signer, epoch)?,
        )
    }

    /// Performs the local StartClosing action and installs the signer's unique
    /// complete report into the local cache. The returned report is ready for
    /// broadcast by the caller's network layer.
    pub fn start_closing_with_report(
        &mut self,
        epoch: Epoch,
        signer: ReplicaId,
    ) -> Result<SignedReport> {
        let mut staged = self.clone();
        staged.freeze_committee_registry()?;
        let report = staged.build_signed_report(signer, epoch)?;
        staged.start_closing(epoch)?;
        staged.submit_report(report.clone())?;
        *self = staged;
        Ok(report)
    }

    /// Deterministically selects n-f accepted reports per close committee.
    pub fn assemble_joint_report_proof(&self, epoch: Epoch) -> Result<JointReportProof> {
        let committees = self.committees_for_epoch(epoch)?;
        let mut reports = BTreeMap::new();
        for committee in &committees {
            let mut selected = Vec::new();
            let mut selected_weight = 0;
            for report in self
                .reports
                .values()
                .filter(|report| report.epoch == epoch && committee.contains(report.signer))
            {
                selected_weight += committee.weight(report.signer);
                selected.push(report.clone());
                if selected_weight >= committee.report_threshold() {
                    break;
                }
            }
            if selected_weight < committee.report_threshold() {
                return Err(RaiError::InvalidClosePackage(format!(
                    "committee {} has accepted report weight {}, requires {}",
                    committee.id,
                    selected_weight,
                    committee.report_threshold()
                )));
            }
            reports.insert(committee.id, selected);
        }
        let proof = JointReportProof { reports };
        proof.validate(epoch, &committees, &self.crypto)?;
        Ok(proof)
    }

    /// Reconstructs the current canonical close cut from accepted reports and
    /// monotone local visibility, including signed first-vote witnesses where
    /// the joint report proof alone does not witness election start.
    pub fn build_close_cut_candidate_from_reports(
        &self,
        epoch: Epoch,
    ) -> Result<CloseCutCandidate> {
        if self.epoch_states.get(&epoch) != Some(&EpochState::Closing) {
            return Err(RaiError::InvalidConfiguration(format!(
                "close-cut construction requires epoch {epoch} to be closing"
            )));
        }
        let committees = self.committees_for_epoch(epoch)?;
        let report_proof = self.assemble_joint_report_proof(epoch)?;
        let mut elections = self.visible_elections(epoch);
        elections.extend(report_proof.report_visible(epoch, &committees));

        let mut start_witness_votes = BTreeMap::new();
        for election in &elections {
            let report_witness = committees.iter().all(|committee| {
                report_proof.report_weight(committee, election) >= committee.visibility_threshold()
            });
            if report_witness {
                continue;
            }
            let witness = self
                .pool
                .all_votes_for_election(election)
                .into_iter()
                .find(|vote| vote.kind == VoteKind::First)
                .ok_or_else(|| {
                    RaiError::InvalidClosePackage(format!(
                        "visible election {election} has no cached first-vote start witness"
                    ))
                })?;
            start_witness_votes.insert(election.clone(), vec![witness]);
        }

        let candidate = CloseCutCandidate {
            epoch,
            elections,
            report_proof,
            start_witness_votes,
        };
        candidate.validate(&committees, &self.crypto)?;
        Ok(candidate)
    }

    /// Registers and validates one close-cut round. The returned candidate is
    /// ready for broadcast and the round timer is started locally.
    pub fn prepare_close_cut_round(
        &mut self,
        epoch: Epoch,
        round: u32,
        carried: Option<Hash32>,
    ) -> Result<(ElectionId, CloseCutCandidate, Hash32)> {
        let mut staged = self.clone();
        let election = ElectionId::CloseCut { epoch, round };
        if staged.decided_close_cut(epoch)?.is_some() {
            return Err(RaiError::InvalidVote(format!(
                "close-cut instance for epoch {epoch} has already decided"
            )));
        }
        let preferred = staged.preferred_close_value(&election)?.ok_or_else(|| {
            RaiError::InvalidClosePackage(
                "successor close-cut round has neither a live carry nor positive death evidence"
                    .into(),
            )
        })?;
        if let Some(requested_carry) = carried {
            if requested_carry != preferred {
                return Err(RaiError::InvalidClosePackage(
                    "requested close-cut carry is not the protocol-selected successor value".into(),
                ));
            }
        }
        if !staged.elections.contains_key(&election) {
            staged.register_derived_election(election.clone())?;
        }
        let candidate = match staged
            .close_cut_candidates
            .get(&preferred)
            .filter(|candidate| candidate.epoch == epoch)
            .cloned()
        {
            Some(candidate) => candidate,
            None if carried.is_some() => {
                return Err(RaiError::InvalidClosePackage(
                    "carried close-cut value has no validated candidate data".into(),
                ));
            }
            None => staged.build_close_cut_candidate_from_reports(epoch)?,
        };
        let hash = staged.accept_close_cut_candidate(candidate.clone())?;
        if hash != preferred {
            return Err(RaiError::SafetyFault(
                "accepted close-cut candidate does not open the selected successor value".into(),
            ));
        }
        staged.start_election_timer(&election)?;
        *self = staged;
        Ok((election, candidate, hash))
    }

    /// Implements the normative StartClosing transition. It atomically moves
    /// the sole open epoch to closing and opens its successor.
    pub fn start_closing(&mut self, epoch: Epoch) -> Result<()> {
        if self.epoch_states.get(&epoch) != Some(&EpochState::Open) {
            return Err(RaiError::InvalidConfiguration(format!(
                "StartClosing({epoch}) requires the epoch to be open"
            )));
        }
        self.require_certified_predecessor(epoch)?;
        if self
            .epoch_states
            .iter()
            .any(|(other, state)| *other != epoch && *state == EpochState::Open)
        {
            return Err(RaiError::InvalidConfiguration(
                "more than one open epoch would violate the lifecycle invariant".into(),
            ));
        }
        if self
            .epoch_states
            .values()
            .any(|state| *state == EpochState::Closing)
        {
            return Err(RaiError::InvalidConfiguration(
                "another epoch is already closing".into(),
            ));
        }
        let next = epoch
            .checked_add(1)
            .ok_or_else(|| RaiError::InvalidConfiguration("epoch number overflow".into()))?;
        if self.epoch_states.contains_key(&next) {
            return Err(RaiError::InvalidConfiguration(format!(
                "successor epoch {next} already has lifecycle state"
            )));
        }
        self.freeze_committee_registry()?;
        self.epoch_states.insert(epoch, EpochState::Closing);
        self.epoch_states.insert(next, EpochState::Open);
        self.check_epoch_cardinality()
    }

    /// Implements the normative FinishClosing transition. A closing epoch can
    /// become closed only after its certified close state and hash are
    /// installed consistently.
    pub fn finish_closing(&mut self, epoch: Epoch) -> Result<()> {
        if self.epoch_states.get(&epoch) != Some(&EpochState::Closing) {
            return Err(RaiError::InvalidConfiguration(format!(
                "FinishClosing({epoch}) requires the epoch to be closing"
            )));
        }
        self.require_certified_predecessor(epoch)?;
        let state = self.close_states.get(&epoch).ok_or_else(|| {
            RaiError::InvalidConfiguration(format!(
                "FinishClosing({epoch}) requires a certified close state"
            ))
        })?;
        if self.close_hashes.get(&epoch) != Some(&state.close_hash) {
            return Err(RaiError::SafetyFault(format!(
                "epoch {epoch} close state and close hash disagree"
            )));
        }
        self.epoch_states.insert(epoch, EpochState::Closed);
        self.check_epoch_cardinality()
    }

    /// Compatibility transition wrapper. Invalid lifecycle jumps are rejected;
    /// callers should prefer start_closing and finish_closing.
    pub fn set_epoch_state(&mut self, epoch: Epoch, state: EpochState) -> Result<()> {
        match (self.epoch_states.get(&epoch).copied(), state) {
            (Some(current), requested) if current == requested => Ok(()),
            (Some(EpochState::Open), EpochState::Closing) => self.start_closing(epoch),
            (Some(EpochState::Closing), EpochState::Closed) => self.finish_closing(epoch),
            (current, requested) => Err(RaiError::InvalidConfiguration(format!(
                "invalid epoch transition for {epoch}: {current:?} -> {requested:?}"
            ))),
        }
    }

    pub fn submit_block(&mut self, signed: SignedBlock) -> Result<Hash32> {
        if !self.crypto.contains(signed.block.representative) {
            return Err(RaiError::Inadmissible(format!(
                "block delegates to unknown replica {}",
                signed.block.representative
            )));
        }
        self.blocks.insert_candidate(signed)
    }

    pub fn submit_report(&mut self, report: SignedReport) -> Result<bool> {
        self.require_governing_context(report.epoch)?;
        if !report.verify(&self.crypto) {
            return Err(RaiError::InvalidSignature);
        }
        let committees = self.committees_for_epoch(report.epoch)?;
        if !committees
            .iter()
            .any(|committee| committee.contains(report.signer))
        {
            return Err(RaiError::InvalidVote(format!(
                "reporter {} is outside the epoch committee set",
                report.signer
            )));
        }
        if report.elections.iter().any(|election| {
            !matches!(election, ElectionId::Slot { epoch, .. } if *epoch == report.epoch)
        }) {
            return Err(RaiError::InvalidVote(
                "report contains a non-slot or wrong-epoch election".into(),
            ));
        }
        let epoch = report.epoch;
        let key = (epoch, report.signer);
        match self.reports.get(&key) {
            Some(existing) if existing != &report => Err(RaiError::InvalidVote(
                "replica supplied conflicting reports for one epoch".into(),
            )),
            Some(_) => Ok(false),
            None => {
                self.reports.insert(key, report);
                self.update_visible(epoch)?;
                Ok(true)
            }
        }
    }

    pub fn visible_elections(&self, epoch: Epoch) -> BTreeSet<ElectionId> {
        self.visible.get(&epoch).cloned().unwrap_or_default()
    }

    /// Validates and caches close-cut candidate data. Reports and start-vote
    /// witnesses are inserted into evidence caches before local visibility and
    /// preferred-value checks are evaluated.
    pub fn accept_close_cut_candidate(&mut self, candidate: CloseCutCandidate) -> Result<Hash32> {
        self.require_governing_context(candidate.epoch)?;
        let committees = self.committees_for_epoch(candidate.epoch)?;
        let witness_votes = candidate.validate(&committees, &self.crypto)?;
        for reports in candidate.report_proof.reports.values() {
            for report in reports {
                let key = (report.epoch, report.signer);
                match self.reports.get(&key) {
                    Some(existing) if existing != report => {
                        return Err(RaiError::InvalidClosePackage(
                            "close-cut proof conflicts with a previously accepted report".into(),
                        ));
                    }
                    None => {
                        self.reports.insert(key, report.clone());
                    }
                    _ => {}
                }
            }
        }
        for vote in witness_votes {
            self.insert_evidence_vote(vote)?;
        }
        self.update_visible(candidate.epoch)?;
        let epoch = candidate.epoch;
        let hash = candidate.hash();
        self.close_cut_candidates.insert(hash, candidate);
        self.release_pending_close_cut_votes(epoch, hash)?;
        Ok(hash)
    }

    /// Installs a close cut only after its cached candidate hash has a joint
    /// fast certificate in the specified close-cut election.
    pub fn install_certified_close_cut(
        &mut self,
        close_cut_election: &ElectionId,
        hash: Hash32,
    ) -> Result<()> {
        let ElectionId::CloseCut { epoch, .. } = close_cut_election else {
            return Err(RaiError::InvalidClosePackage(
                "certified close cut requires a close-cut election".into(),
            ));
        };
        match self.derive_result(close_cut_election)? {
            Some(GlobalResult::Fast(value) | GlobalResult::Final(value)) if value == hash => {}
            _ => {
                return Err(RaiError::InvalidClosePackage(
                    "close cut is not backed by a joint fast certificate".into(),
                ));
            }
        }
        let candidate = self
            .close_cut_candidates
            .get(&hash)
            .cloned()
            .ok_or_else(|| {
                RaiError::InvalidClosePackage(
                    "decided close-cut hash has no validated candidate data".into(),
                )
            })?;
        if candidate.epoch != *epoch {
            return Err(RaiError::InvalidClosePackage(
                "close-cut candidate epoch mismatch".into(),
            ));
        }
        if let Some((_, decided_hash)) = self.decided_close_cut(*epoch)? {
            if decided_hash != hash {
                return Err(RaiError::SafetyFault(format!(
                    "close-cut rounds for epoch {epoch} decided conflicting values"
                )));
            }
        }
        if let Some(installed_hash) = self.certified_close_cut_hashes.get(epoch) {
            if *installed_hash != hash {
                return Err(RaiError::SafetyFault(format!(
                    "certified close cut for epoch {epoch} is already installed with a different hash"
                )));
            }
            let installed = self.close_cuts.get(epoch).ok_or_else(|| {
                RaiError::SafetyFault(
                    "certified close-cut hash exists without an installed cut".into(),
                )
            })?;
            if installed != &candidate.elections {
                return Err(RaiError::SafetyFault(
                    "installed close cut disagrees with its decided hash preimage".into(),
                ));
            }
            return Ok(());
        }

        // Installing the cut is one atomic transition: publish the cut, make
        // every included slot election locally known, and start any missing
        // drain timer before later code may inspect the close state.
        let mut staged = self.clone();
        staged
            .close_cuts
            .insert(*epoch, candidate.elections.clone());
        staged.certified_close_cut_hashes.insert(*epoch, hash);
        for election in &candidate.elections {
            if !staged.elections.contains_key(election) {
                staged.register_derived_election(election.clone())?;
            }
            staged
                .election_started_at_ms
                .entry(election.clone())
                .or_insert(staged.now_ms);
        }
        *self = staged;
        Ok(())
    }

    /// Compatibility wrapper used by the demos. It still requires a matching
    /// validated candidate and a joint fast or final close-cut certificate.
    pub fn install_close_cut(
        &mut self,
        epoch: Epoch,
        elections: impl IntoIterator<Item = ElectionId>,
    ) -> Result<()> {
        let expected = elections.into_iter().collect::<BTreeSet<_>>();
        let hash = hash_close_cut(epoch, expected.iter().cloned());
        let election = self
            .elections
            .keys()
            .filter(
                |election| matches!(election, ElectionId::CloseCut { epoch: e, .. } if *e == epoch),
            )
            .find(|election| {
                matches!(
                    self.derive_result(election),
                    Ok(Some(GlobalResult::Fast(value) | GlobalResult::Final(value)))
                        if value == hash
                )
            })
            .cloned()
            .ok_or_else(|| {
                RaiError::InvalidClosePackage(
                    "no decided close-cut election matches the supplied cut".into(),
                )
            })?;
        let candidate = self.close_cut_candidates.get(&hash).ok_or_else(|| {
            RaiError::InvalidClosePackage("close-cut candidate data is unavailable".into())
        })?;
        if candidate.elections != expected {
            return Err(RaiError::InvalidClosePackage(
                "supplied cut differs from validated candidate data".into(),
            ));
        }
        self.install_certified_close_cut(&election, hash)
    }

    /// Reconstructs a certificate-complete close package from the installed
    /// cut and the complete locally known evidence view. This is the normative
    /// input to a close-record round; it fails while any cut election has not
    /// yet reached a terminal merged result.
    pub fn build_close_package_from_current_evidence(&self, epoch: Epoch) -> Result<ClosePackage> {
        let cut_hash = self
            .certified_close_cut_hashes
            .get(&epoch)
            .copied()
            .ok_or_else(|| {
                RaiError::InvalidClosePackage(
                    "cannot construct a close record before the close cut is certified".into(),
                )
            })?;
        let close_cut = self
            .close_cut_candidates
            .get(&cut_hash)
            .filter(|candidate| candidate.epoch == epoch)
            .cloned()
            .ok_or_else(|| {
                RaiError::InvalidClosePackage(
                    "installed close cut has no validated candidate data".into(),
                )
            })?;
        let close_cut_election = self
            .elections
            .keys()
            .filter(|election| {
                matches!(election, ElectionId::CloseCut { epoch: election_epoch, .. } if *election_epoch == epoch)
            })
            .find(|election| {
                self.derive_result(election) == Ok(Some(GlobalResult::Fast(cut_hash)))
            })
            .cloned()
            .ok_or_else(|| {
                RaiError::InvalidClosePackage(
                    "installed close cut has no matching fast-certified round".into(),
                )
            })?;
        let evidence = self.locally_known_evidence_pool()?;
        let close_cut_votes = evidence
            .all_votes_for_election(&close_cut_election)
            .into_iter()
            .filter(|vote| {
                vote.kind == VoteKind::First && vote.value == VoteValue::Candidate(cut_hash)
            })
            .collect::<Vec<_>>();

        let committee_ids = self.expected_committee_ids_readonly(epoch)?;
        let committees = self.committees_for_epoch(epoch)?;
        let mut cuts = BTreeMap::new();
        let mut required_blocks = BTreeSet::new();
        for election in &close_cut.elections {
            let votes = evidence.all_votes_for_election(election);
            let cut = ElectionCut {
                election: election.clone(),
                committee_ids: committee_ids.clone(),
                votes,
            };
            let status =
                cut.resolve_with_local_evidence(&committees, &self.crypto, &VotePool::default())?;
            if let Some(block) = status.block() {
                required_blocks.extend(self.blocks.chain_to_genesis(block)?);
            }
            cuts.insert(election.clone(), cut);
        }

        let previous_state = epoch
            .checked_sub(1)
            .and_then(|previous| self.close_states.get(&previous));
        if let Some(previous) = previous_state {
            for status in previous.statuses.values() {
                if let Some(block) = status.block() {
                    required_blocks.extend(self.blocks.chain_to_genesis(block)?);
                }
            }
        }

        let mut excluded = BTreeSet::new();
        for (_, slot, obligation_epoch, _) in &self.obligations {
            if *obligation_epoch == epoch {
                let election = ElectionId::Slot { slot: *slot, epoch };
                if !close_cut.elections.contains(&election) {
                    excluded.insert(election);
                }
            }
        }
        for ((report_epoch, _), report) in &self.reports {
            if *report_epoch == epoch {
                excluded.extend(
                    report
                        .elections
                        .iter()
                        .filter(|election| !close_cut.elections.contains(*election))
                        .cloned(),
                );
            }
        }
        let mut exclusion_witness_votes = BTreeMap::new();
        for election in &excluded {
            let witnesses = evidence
                .all_votes_for_election(election)
                .into_iter()
                .filter(|vote| vote.kind == VoteKind::First)
                .collect::<Vec<_>>();
            if witnesses.is_empty() {
                return Err(RaiError::InvalidClosePackage(format!(
                    "excluded election {election} has no locally known first-vote witness"
                )));
            }
            exclusion_witness_votes.insert(election.clone(), witnesses);
        }

        let package_blocks = self.blocks.signed_candidates(required_blocks)?;
        ClosePackage::build_certified(
            epoch,
            self.previous_close_hash(epoch)?,
            close_cut_election,
            close_cut,
            close_cut_votes,
            cuts,
            excluded,
            exclusion_witness_votes,
            package_blocks,
            previous_state,
            &committees,
            &self.crypto,
            &self.blocks,
        )
    }

    /// Reconstructs and validates the current preferred close record, registers
    /// the requested round, and starts its timer. The returned package is ready
    /// for candidate-data broadcast.
    pub fn prepare_close_record_round(
        &mut self,
        epoch: Epoch,
        round: u32,
        carried: Option<Hash32>,
    ) -> Result<(ElectionId, ClosePackage, Hash32)> {
        let mut staged = self.clone();
        let election = ElectionId::CloseRecord { epoch, round };
        if staged.decided_close_record(epoch)?.is_some() {
            return Err(RaiError::InvalidVote(format!(
                "close-record instance for epoch {epoch} has already decided"
            )));
        }
        let preferred = staged.preferred_close_value(&election)?.ok_or_else(|| {
            RaiError::InvalidClosePackage(
                "successor close-record round has neither a live carry nor positive death evidence"
                    .into(),
            )
        })?;

        if let Some(requested_carry) = carried {
            if requested_carry != preferred {
                return Err(RaiError::InvalidClosePackage(
                    "requested close-record carry is not the protocol-selected successor value"
                        .into(),
                ));
            }
        }

        let package = match staged
            .close_record_candidates
            .get(&(epoch, preferred))
            .cloned()
        {
            Some(package) => package,
            None => {
                // An unlocked round uses the canonical package reconstructed
                // from the complete valid evidence currently known locally.
                let package = staged.build_close_package_from_current_evidence(epoch)?;
                if package.record.hash() != preferred {
                    return Err(RaiError::SafetyFault(
                        format!(
                            "fresh close-record reconstruction changed during round preparation for {election}: preferred={}, reconstructed={}",
                            preferred.short(),
                            package.record.hash().short()
                        ),
                    ));
                }
                package
            }
        };

        let hash = staged.accept_close_record_candidate(package.clone())?;
        if hash != preferred {
            return Err(RaiError::SafetyFault(
                "accepted close-record package does not open the selected successor value".into(),
            ));
        }
        if !staged.elections.contains_key(&election) {
            staged.register_derived_election(election.clone())?;
        }
        staged.start_election_timer(&election)?;
        *self = staged;
        Ok((election, package, hash))
    }

    /// Advances the deterministic local close loop by one externally visible
    /// action. Networking code broadcasts returned candidates and feeds signed
    /// reports/votes back through submit_report and submit_vote.
    pub fn drive_close_protocol(&mut self, epoch: Epoch) -> Result<CloseProtocolAction> {
        if self.epoch_states.get(&epoch) == Some(&EpochState::Closed) {
            return Ok(CloseProtocolAction::Closed {
                close_hash: self.close_hash(epoch).ok_or_else(|| {
                    RaiError::SafetyFault("closed epoch has no close hash".into())
                })?,
            });
        }
        if self.epoch_states.get(&epoch) != Some(&EpochState::Closing) {
            return Err(RaiError::InvalidConfiguration(format!(
                "drive_close_protocol requires epoch {epoch} to be closing"
            )));
        }

        if !self.certified_close_cut_hashes.contains_key(&epoch) {
            if let Some((deciding_election, hash)) = self.decided_close_cut(epoch)? {
                self.install_certified_close_cut(&deciding_election, hash)?;
            } else {
                let latest = self.latest_close_cut_round(epoch);
                match latest {
                    None => match self.prepare_close_cut_round(epoch, 0, None) {
                        Ok((election, candidate, hash)) => {
                            return Ok(CloseProtocolAction::BroadcastCloseCut {
                                election,
                                candidate,
                                hash,
                            });
                        }
                        Err(RaiError::InvalidClosePackage(_)) => {
                            return Ok(CloseProtocolAction::AwaitReports);
                        }
                        Err(error) => return Err(error),
                    },
                    Some((round, election)) => match self.derive_result(&election)? {
                        None => return Ok(CloseProtocolAction::AwaitCloseCut { election }),
                        Some(GlobalResult::Timeout) => {
                            let (next, candidate, hash) =
                                self.prepare_close_cut_round(epoch, round + 1, None)?;
                            return Ok(CloseProtocolAction::BroadcastCloseCut {
                                election: next,
                                candidate,
                                hash,
                            });
                        }
                        Some(GlobalResult::Converged(hash)) => {
                            let (next, candidate, carried_hash) =
                                self.prepare_close_cut_round(epoch, round + 1, Some(hash))?;
                            return Ok(CloseProtocolAction::BroadcastCloseCut {
                                election: next,
                                candidate,
                                hash: carried_hash,
                            });
                        }
                        Some(GlobalResult::Fast(hash) | GlobalResult::Final(hash)) => {
                            self.install_certified_close_cut(&election, hash)?;
                        }
                        Some(GlobalResult::Notarized(_)) => {
                            return Err(RaiError::SafetyFault(
                                "close-cut round reached an invalid terminal result".into(),
                            ));
                        }
                    },
                }
            }
        }

        // A close-record certificate decides the logical instance regardless
        // of this replica's current round or incomplete local drain view. The
        // package carries the evidence needed to reconstruct and install the
        // authoritative result.
        if let Some((deciding_election, hash)) = self.decided_close_record(epoch)? {
            let package = self
                .close_record_candidates
                .get(&(epoch, hash))
                .cloned()
                .ok_or_else(|| {
                    RaiError::InvalidClosePackage(
                        "decided close-record value has no validated package".into(),
                    )
                })?;
            let close_hash = self.install_close_package(&deciding_election, package)?;
            return Ok(CloseProtocolAction::Closed { close_hash });
        }

        let cut =
            self.close_cuts.get(&epoch).cloned().ok_or_else(|| {
                RaiError::SafetyFault("certified close cut was not installed".into())
            })?;
        let pending = cut
            .iter()
            .filter_map(|election| {
                matches!(self.derive_result(election), Ok(None)).then_some(election.clone())
            })
            .collect::<BTreeSet<_>>();
        if !pending.is_empty() {
            return Ok(CloseProtocolAction::DrainCut { pending });
        }

        let latest = self.latest_close_record_round(epoch);
        match latest {
            None => {
                let (election, package, hash) = self.prepare_close_record_round(epoch, 0, None)?;
                Ok(CloseProtocolAction::BroadcastCloseRecord {
                    election,
                    package,
                    hash,
                })
            }
            Some((round, election)) => match self.derive_result(&election)? {
                None => Ok(CloseProtocolAction::AwaitCloseRecord { election }),
                Some(GlobalResult::Timeout) => {
                    let (next, package, hash) =
                        self.prepare_close_record_round(epoch, round + 1, None)?;
                    Ok(CloseProtocolAction::BroadcastCloseRecord {
                        election: next,
                        package,
                        hash,
                    })
                }
                Some(GlobalResult::Converged(hash)) => {
                    let (next, package, carried_hash) =
                        self.prepare_close_record_round(epoch, round + 1, Some(hash))?;
                    Ok(CloseProtocolAction::BroadcastCloseRecord {
                        election: next,
                        package,
                        hash: carried_hash,
                    })
                }
                Some(GlobalResult::Fast(hash) | GlobalResult::Final(hash)) => {
                    let package = self
                        .close_record_candidates
                        .get(&(epoch, hash))
                        .cloned()
                        .ok_or_else(|| {
                            RaiError::InvalidClosePackage(
                                "decided close-record value has no validated package".into(),
                            )
                        })?;
                    let close_hash = self.install_close_package(&election, package)?;
                    Ok(CloseProtocolAction::Closed { close_hash })
                }
                Some(GlobalResult::Notarized(_)) => Err(RaiError::SafetyFault(
                    "close-record round reached an invalid terminal result".into(),
                )),
            },
        }
    }

    /// Validates and retains a complete close package. Before certification,
    /// the candidate is checked against current local evidence for voting. If
    /// its hash is already joint-fast-certified, only the package opening and
    /// referenced evidence are reconstructed because the decision is final.
    /// Package evidence is not inserted into the ordinary vote pool until
    /// close installation.
    pub fn accept_close_record_candidate(&mut self, package: ClosePackage) -> Result<Hash32> {
        self.require_governing_context(package.epoch)?;
        let epoch = package.epoch;
        let hash = package.record.hash();
        let previous_hash = self.previous_close_hash(package.epoch)?;
        let previous_state = package
            .epoch
            .checked_sub(1)
            .and_then(|epoch| self.close_states.get(&epoch));
        let committees = self.committees_for_epoch(package.epoch)?;

        // A close package must independently open its committed lists and
        // carry sufficient evidence for every listed status. This package-only
        // validation is also the reconstruction rule used after certification
        // and by replicas that learn the package after the decision.
        package.validate_with_blocks(
            previous_hash,
            previous_state,
            &committees,
            &self.crypto,
            &self.blocks,
        )?;
        if self.certified_close_cut_hashes.get(&package.epoch) != Some(&package.close_cut.hash()) {
            return Err(RaiError::InvalidClosePackage(
                "close package does not use the installed certified close cut".into(),
            ));
        }

        match self.decided_close_record(epoch)? {
            Some((_, certified)) if certified != hash => {
                return Err(RaiError::InvalidClosePackage(
                    "package does not open the decided close-record hash".into(),
                ));
            }
            Some(_) => {
                // The certified hash is authoritative. Late local timeout or
                // conflict evidence is retained but cannot invalidate the
                // package opening that decision.
            }
            None => {
                // Package admissibility is stable and depends only on the
                // package's represented evidence and certified predecessor.
                // Additional local evidence may change fresh preference, but
                // cannot invalidate this already valid opening.
                self.require_local_obligations_covered(&package)?;
            }
        }

        let staged_package_evidence = self.package_evidence_with(&package)?;
        self.close_record_candidates.insert((epoch, hash), package);
        self.package_evidence = staged_package_evidence;
        self.release_pending_close_record_votes(epoch, hash)?;

        // Before certification, every retained candidate has been checked
        // against the complete locally known evidence view. After
        // certification, the retained package opens the authoritative hash.
        Ok(hash)
    }

    pub fn receive_block_and_vote_first_valid(
        &mut self,
        signer: ReplicaId,
        election: &ElectionId,
        committee: CommitteeId,
        signed: SignedBlock,
    ) -> Result<Option<GlobalResultUpdate>> {
        let ElectionId::Slot { slot, .. } = election else {
            return Err(RaiError::InvalidVote(
                "first-valid block handling only applies to slot elections".into(),
            ));
        };
        if signed.block.slot != *slot {
            return Err(RaiError::Inadmissible(format!(
                "received block for slot {}, but election is {}",
                signed.block.slot, election
            )));
        }
        let hash = self.submit_block(signed)?;
        if !self.election_is_active(election) {
            return Ok(None);
        }
        if self.pool.first_value(signer, committee, election).is_some() {
            return Ok(None);
        }
        if !self.can_enter_slot_election(signer, *slot, election) {
            return Ok(None);
        }
        if !self.blocks.admissible_for_slot(*slot, hash)?
            || !self.safe_to_vote(signer, election, hash)?
        {
            return Ok(None);
        }
        self.cast_first_vote(signer, election, committee, VoteValue::Candidate(hash))
            .map(Some)
    }

    /// Handles a first-valid block as one logical vote across every committee
    /// applicable to the signer in this election.
    pub fn receive_block_and_vote_first_valid_all(
        &mut self,
        signer: ReplicaId,
        election: &ElectionId,
        signed: SignedBlock,
    ) -> Result<Option<GlobalResultUpdate>> {
        let ElectionId::Slot { slot, .. } = election else {
            return Err(RaiError::InvalidVote(
                "first-valid block handling only applies to slot elections".into(),
            ));
        };
        if signed.block.slot != *slot {
            return Err(RaiError::Inadmissible(format!(
                "received block for slot {}, but election is {}",
                signed.block.slot, election
            )));
        }
        let hash = self.submit_block(signed)?;
        if !self.election_is_active(election)
            || !self.can_enter_slot_election(signer, *slot, election)
            || !self.blocks.admissible_for_slot(*slot, hash)?
            || !self.safe_to_vote(signer, election, hash)?
        {
            return Ok(None);
        }
        let committees = self.applicable_committee_ids(signer, election)?;
        if committees.iter().any(|committee| {
            self.pool
                .first_value(signer, *committee, election)
                .is_some()
        }) {
            return Ok(None);
        }
        self.cast_first_vote_all(signer, election, VoteValue::Candidate(hash))
            .map(Some)
    }

    pub fn applicable_election_committees(
        &self,
        signer: ReplicaId,
        election: &ElectionId,
    ) -> Result<Vec<CommitteeId>> {
        self.applicable_committee_ids(signer, election)
    }

    pub fn voting_election(&self, signer: ReplicaId, slot: Slot) -> Option<&ElectionId> {
        self.slot_vote_locks
            .get(&(signer, slot))
            .filter(|election| !self.certified_released(election))
    }

    pub fn first_vote_choice(
        &self,
        signer: ReplicaId,
        committee: CommitteeId,
        election: &ElectionId,
    ) -> Option<VoteValue> {
        self.pool
            .first_choice_in_committee(signer, committee, election)
    }

    pub fn active_slot_elections(&self, slot: Slot) -> Vec<ElectionId> {
        self.elections
            .keys()
            .filter(|election| election.slot() == Some(slot) && self.election_is_active(election))
            .cloned()
            .collect()
    }

    pub fn cast_first_vote(
        &mut self,
        signer: ReplicaId,
        election: &ElectionId,
        committee: CommitteeId,
        value: VoteValue,
    ) -> Result<GlobalResultUpdate> {
        self.cast_first_vote_internal(signer, election, Some(committee), value, false)
    }

    /// Emits one logical unscoped first vote, atomically expanding it into
    /// every election committee in which the signer participates. This fails
    /// rather than leaving a correct sender partially voted.
    pub fn cast_first_vote_all(
        &mut self,
        signer: ReplicaId,
        election: &ElectionId,
        value: VoteValue,
    ) -> Result<GlobalResultUpdate> {
        self.cast_first_vote_internal(signer, election, None, value, true)
    }

    fn cast_first_vote_internal(
        &mut self,
        signer: ReplicaId,
        election: &ElectionId,
        requested_committee: Option<CommitteeId>,
        value: VoteValue,
        require_unscoped: bool,
    ) -> Result<GlobalResultUpdate> {
        self.require_active(election)?;
        self.require_governing_context(election.epoch())?;
        let applicable = self.applicable_committee_ids(signer, election)?;
        if let Some(committee) = requested_committee {
            self.require_election_committee(election, committee, signer)?;
        }

        let enabled = applicable
            .iter()
            .copied()
            .filter(|committee| {
                self.pool
                    .first_value(signer, *committee, election)
                    .is_none()
                    && self
                        .pool
                        .final_value(signer, *committee, election)
                        .is_none()
            })
            .collect::<Vec<_>>();
        if enabled.is_empty() {
            return Err(RaiError::DuplicateFirstVote);
        }

        let targets = if require_unscoped || enabled.len() == applicable.len() {
            if enabled.len() != applicable.len() {
                return Err(RaiError::InvalidVote(
                    "unscoped first vote is not enabled in every applicable committee".into(),
                ));
            }
            applicable
        } else {
            let committee = requested_committee.ok_or_else(|| {
                RaiError::InvalidVote("scoped first vote requires an explicit committee".into())
            })?;
            if !enabled.contains(&committee) {
                return Err(RaiError::DuplicateFirstVote);
            }
            vec![committee]
        };

        self.require_slot_vote_lock_available(signer, election)?;
        if value == VoteValue::Timeout {
            self.require_first_vote_timeout_elapsed(election)?;
        }
        self.require_admissible_and_safe(signer, election, value, VoteKind::First)?;
        for committee in &targets {
            self.require_first_vote_choice(signer, *committee, election, value, VoteKind::First)?;
        }

        let votes = targets
            .into_iter()
            .map(|committee| {
                SignedVote::new(
                    &self.crypto,
                    signer,
                    election.clone(),
                    committee,
                    value,
                    VoteKind::First,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        self.submit_locally_emitted_votes(votes)
    }

    pub fn cast_notarization_vote(
        &mut self,
        signer: ReplicaId,
        election: &ElectionId,
        committee: CommitteeId,
        value: VoteValue,
    ) -> Result<GlobalResultUpdate> {
        self.cast_notarization_vote_internal(signer, election, Some(committee), value, false)
    }

    /// Emits one logical unscoped notarization vote only when the same
    /// notarization action is enabled in every applicable committee.
    pub fn cast_notarization_vote_all(
        &mut self,
        signer: ReplicaId,
        election: &ElectionId,
        value: VoteValue,
    ) -> Result<GlobalResultUpdate> {
        self.cast_notarization_vote_internal(signer, election, None, value, true)
    }

    fn cast_notarization_vote_internal(
        &mut self,
        signer: ReplicaId,
        election: &ElectionId,
        requested_committee: Option<CommitteeId>,
        value: VoteValue,
        require_unscoped: bool,
    ) -> Result<GlobalResultUpdate> {
        self.require_active(election)?;
        self.require_governing_context(election.epoch())?;
        let applicable = self.applicable_committee_ids(signer, election)?;
        if let Some(committee) = requested_committee {
            self.require_election_committee(election, committee, signer)?;
        }
        let known_values = self.known_values(election);
        let mut enabled = Vec::new();
        for committee_id in &applicable {
            if self
                .pool
                .first_value(signer, *committee_id, election)
                .is_none()
                || self
                    .pool
                    .final_value(signer, *committee_id, election)
                    .is_some()
                || self
                    .pool
                    .has_notarization_vote(signer, *committee_id, election, value)
            {
                continue;
            }
            let committee = self
                .committees
                .get(committee_id)
                .ok_or(RaiError::UnknownCommittee(*committee_id))?;
            let committee_enabled = match value {
                VoteValue::Candidate(_) => {
                    self.pool.many_values(committee, election).contains(&value)
                }
                VoteValue::Timeout => self.pool.timeout_allowed_with_values(
                    committee,
                    election,
                    known_values.iter().copied(),
                ),
            };
            if committee_enabled {
                enabled.push(*committee_id);
            }
        }

        if enabled.is_empty() {
            return Err(RaiError::InvalidVote(
                "notarization action is not enabled in any applicable committee".into(),
            ));
        }
        let targets = if require_unscoped || enabled.len() == applicable.len() {
            if enabled.len() != applicable.len() {
                return Err(RaiError::InvalidVote(
                    "unscoped notarization is not enabled in every applicable committee".into(),
                ));
            }
            applicable
        } else {
            let committee = requested_committee.ok_or_else(|| {
                RaiError::InvalidVote("scoped notarization requires an explicit committee".into())
            })?;
            if !enabled.contains(&committee) {
                return Err(RaiError::InvalidVote(format!(
                    "notarization action is not enabled in committee {committee}"
                )));
            }
            vec![committee]
        };

        if matches!(value, VoteValue::Candidate(_)) {
            self.require_admissible_and_safe(signer, election, value, VoteKind::Notarization)?;
        }
        let votes = targets
            .into_iter()
            .map(|committee| {
                SignedVote::new(
                    &self.crypto,
                    signer,
                    election.clone(),
                    committee,
                    value,
                    VoteKind::Notarization,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        self.submit_locally_emitted_votes(votes)
    }

    pub fn cast_final_vote(
        &mut self,
        signer: ReplicaId,
        election: &ElectionId,
        committee: CommitteeId,
        block: Hash32,
    ) -> Result<GlobalResultUpdate> {
        self.require_active(election)?;
        self.require_governing_context(election.epoch())?;
        self.require_election_committee(election, committee, signer)?;
        if !election.is_close() && !self.blocks.is_complete(block) {
            return Err(RaiError::Incomplete(
                "final voting requires a complete block".into(),
            ));
        }

        let value = VoteValue::Candidate(block);
        let election_committees = self
            .elections
            .get(election)
            .cloned()
            .ok_or_else(|| RaiError::UnknownElection(election.to_string()))?;
        let mut applicable = Vec::new();
        for committee_id in election_committees {
            let descriptor = self
                .committees
                .get(&committee_id)
                .ok_or(RaiError::UnknownCommittee(committee_id))?;
            if descriptor.contains(signer) {
                applicable.push(committee_id);
            }
        }
        if applicable.is_empty() {
            return Err(RaiError::InvalidVote(format!(
                "replica {signer} is not a member of any committee for {election}"
            )));
        }

        // A correct sender emits final votes only when the action is enabled in
        // every applicable committee. This preserves the spec's unscoped-vote
        // semantics even though the PoC stores the resulting votes separately.
        for committee_id in &applicable {
            if self
                .pool
                .final_value(signer, *committee_id, election)
                .is_some()
            {
                return Err(RaiError::DuplicateFinalVote);
            }
            let support = self.pool.support_values(signer, *committee_id, election);
            if support.iter().any(|supported| *supported != value) {
                return Err(RaiError::InvalidVote(format!(
                    "final vote support in committee {committee_id} is not a subset of {{{value}}}"
                )));
            }
        }
        self.require_admissible_and_safe(signer, election, value, VoteKind::Final)?;

        let votes = applicable
            .iter()
            .map(|committee_id| {
                SignedVote::new(
                    &self.crypto,
                    signer,
                    election.clone(),
                    *committee_id,
                    value,
                    VoteKind::Final,
                )
            })
            .collect::<Result<Vec<_>>>()?;

        // Validate the complete scoped expansion first, then commit it as one
        // logical action so callers cannot leave a correct sender half-voted.
        let mut staged_pool = self.pool.clone();
        for vote in &votes {
            let committee = self
                .committees
                .get(&vote.committee)
                .ok_or(RaiError::UnknownCommittee(vote.committee))?;
            staged_pool.insert(vote.clone(), committee, &self.crypto)?;
        }
        self.pool = staged_pool;
        for vote in &votes {
            self.record_vote_effects(vote);
        }
        self.update_visible(election.epoch())?;
        self.refresh_outcome(election)
    }

    /// Receives a signed vote from the network. Receiver processing validates
    /// signature, scope, membership, candidate ledger validity, and signed
    /// receive-consumption history that is already verifiable locally.
    /// Sender-local stable-prefix, governing-frontier, certified-release,
    /// timing, and close-preference guards remain exclusive to the cast APIs.
    pub fn submit_vote(&mut self, vote: SignedVote) -> Result<GlobalResultUpdate> {
        self.require_election_committee(&vote.election, vote.committee, vote.signer)?;
        // A receiver must establish that a slot candidate is a valid ledger
        // transition before its stake can contribute to a certificate. It must
        // not, however, re-run sender-relative safe_to_vote predicates: those
        // depend on the sender's local protocol state and are enforced by the
        // cast_* APIs when a correct replica emits the vote.
        self.require_received_slot_vote_valid(&vote)?;

        // A non-timeout close-cut vote is not usable evidence until the
        // concrete cut, joint report proof, and start witnesses have all been
        // validated. Keep early network delivery quarantined and replay it when
        // the matching candidate data is accepted.
        if let (ElectionId::CloseCut { epoch, .. }, VoteValue::Candidate(hash)) =
            (&vote.election, vote.value)
        {
            let candidate_ready = self
                .close_cut_candidates
                .get(&hash)
                .map(|candidate| candidate.epoch == *epoch)
                .unwrap_or(false);
            if !candidate_ready {
                let committee = self
                    .committees
                    .get(&vote.committee)
                    .ok_or(RaiError::UnknownCommittee(vote.committee))?;
                if !committee.contains(vote.signer)
                    || !self
                        .crypto
                        .verify(vote.signer, &vote.signing_bytes(), &vote.signature)
                {
                    return Err(RaiError::InvalidSignature);
                }
                let key = (
                    vote.election.clone(),
                    vote.committee,
                    vote.signer,
                    vote.value,
                    vote.kind,
                );
                self.pending_close_cut_votes
                    .entry(key)
                    .or_insert(vote.clone());
                return Ok(GlobalResultUpdate {
                    derived: self.derive_result(&vote.election)?,
                    first_observation: false,
                });
            }
        }

        self.insert_vote_and_refresh(vote)
    }

    /// Receives a network vote while preserving the candidate-data dependency
    /// used by the runtime. Close-record certificates may be verified directly
    /// through [`Self::submit_vote`], but a live node defers their contribution
    /// until it has validated the package opening the voted hash.
    pub fn submit_vote_with_candidate_data(
        &mut self,
        vote: SignedVote,
    ) -> Result<GlobalResultUpdate> {
        if let (ElectionId::CloseRecord { epoch, .. }, VoteValue::Candidate(hash)) =
            (&vote.election, vote.value)
        {
            if !self.close_record_candidates.contains_key(&(*epoch, hash)) {
                self.require_election_committee(&vote.election, vote.committee, vote.signer)?;
                let committee = self
                    .committees
                    .get(&vote.committee)
                    .ok_or(RaiError::UnknownCommittee(vote.committee))?;
                if !committee.contains(vote.signer)
                    || !self
                        .crypto
                        .verify(vote.signer, &vote.signing_bytes(), &vote.signature)
                {
                    return Err(RaiError::InvalidSignature);
                }
                let key = (
                    vote.election.clone(),
                    vote.committee,
                    vote.signer,
                    vote.value,
                    vote.kind,
                );
                self.pending_close_record_votes
                    .entry(key)
                    .or_insert(vote.clone());
                return Ok(GlobalResultUpdate {
                    derived: self.derive_result(&vote.election)?,
                    first_observation: false,
                });
            }
        }
        self.submit_vote(vote)
    }

    pub fn complete_block(&mut self, election: &ElectionId, block: Hash32) -> Result<bool> {
        let ElectionId::Slot { slot, .. } = election else {
            return Err(RaiError::Incomplete(
                "only slot-election blocks enter the complete tree".into(),
            ));
        };
        if !self.blocks.admissible_for_slot(*slot, block)? {
            return Err(RaiError::Inadmissible(
                "block does not extend the complete parent tree".into(),
            ));
        }
        let result = self.derive_result(election)?.ok_or_else(|| {
            RaiError::Incomplete("no merged notarization/fast/final result exists".into())
        })?;
        if result.value() != Some(block) || matches!(&result, GlobalResult::Timeout) {
            return Err(RaiError::Incomplete(
                "block is not selected by the merged election result".into(),
            ));
        }
        let changed = self.blocks.mark_complete(block)?;
        if matches!(&result, GlobalResult::Fast(_) | GlobalResult::Final(_)) {
            self.require_receive_source_epochs(election, block)?;
            self.finalize_block_for_epoch(block, election.epoch())?;
        }
        Ok(changed)
    }

    pub fn outcome(&self, election: &ElectionId) -> Option<&GlobalResult> {
        self.outcomes.get(election)
    }

    pub fn derive_result(&self, election: &ElectionId) -> Result<Option<GlobalResult>> {
        let committees = self.committees_for(election)?;
        derive_global_result(&self.pool, &committees, election)
    }

    pub fn safe_to_vote(
        &self,
        signer: ReplicaId,
        election: &ElectionId,
        block: Hash32,
    ) -> Result<bool> {
        let ElectionId::Slot { slot, epoch } = election else {
            return Ok(true);
        };
        self.require_governing_context(*epoch)?;
        if let Some(finalized) = self.blocks.finalized(*slot) {
            if finalized != block {
                return Ok(false);
            }
        }
        let Some(signed) = self.blocks.candidate(block) else {
            return Ok(false);
        };
        if !self.stable_prefix(block)? {
            return Ok(false);
        }
        if *epoch >= 2 {
            let governing_epoch = *epoch - 2;
            let close = self.close_states.get(&governing_epoch).ok_or_else(|| {
                RaiError::UnsafeVote("governing close state is unavailable".into())
            })?;
            if let Some(frontier) = close.frontier.get(&signed.block.slot.account) {
                if !self.blocks.descends_from(block, *frontier)? {
                    return Ok(false);
                }
            }
        }
        for (voter, old_slot, old_epoch, _) in &self.obligations {
            if *voter == signer && *old_slot == *slot && *old_epoch < *epoch {
                let old_election = ElectionId::Slot {
                    slot: *slot,
                    epoch: *old_epoch,
                };
                if !self.certified_released(&old_election) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub fn close_state(&self, epoch: Epoch) -> Option<&CertifiedCloseState> {
        self.close_states.get(&epoch)
    }

    pub fn close_hash(&self, epoch: Epoch) -> Option<Hash32> {
        self.close_hashes.get(&epoch).copied()
    }

    pub fn ledger_root(&self, epoch: Epoch) -> Option<Hash32> {
        self.close_states.get(&epoch).map(|state| state.ledger_root)
    }

    pub fn derived_committee(&self, epoch: Epoch) -> Option<CommitteeId> {
        self.derived_committees.get(&epoch).copied()
    }

    pub fn committee_ids_for_epoch(&self, epoch: Epoch) -> Result<Vec<CommitteeId>> {
        self.expected_committee_ids_readonly(epoch)
    }

    pub fn install_close_package(
        &mut self,
        close_record_election: &ElectionId,
        package: ClosePackage,
    ) -> Result<Hash32> {
        let ElectionId::CloseRecord { epoch, .. } = close_record_election else {
            return Err(RaiError::InvalidClosePackage(
                "close state installation requires a close-record election".into(),
            ));
        };
        if *epoch != package.epoch {
            return Err(RaiError::InvalidClosePackage(
                "close-record election epoch does not match the package epoch".into(),
            ));
        }
        let record_hash = package.record.hash();
        match self.derive_result(close_record_election)? {
            Some(GlobalResult::Fast(value) | GlobalResult::Final(value))
                if value == record_hash => {}
            _ => {
                return Err(RaiError::InvalidClosePackage(
                    "close package is not backed by a joint fast or final close-record certificate"
                        .into(),
                ));
            }
        }
        let cached = self
            .close_record_candidates
            .get(&(*epoch, record_hash))
            .ok_or_else(|| {
                RaiError::InvalidClosePackage(
                    "close record has no validated package opening".into(),
                )
            })?;
        if cached != &package {
            return Err(RaiError::InvalidClosePackage(
                "installed package differs from the validated package opening".into(),
            ));
        }
        if self.certified_close_cut_hashes.get(epoch) != Some(&package.close_cut.hash()) {
            return Err(RaiError::InvalidClosePackage(
                "close package does not open the installed certified cut".into(),
            ));
        }

        // Joint fast certification makes the committed close record
        // authoritative. Reconstruct it from the package's own evidence and
        // roots, not from later-learned local slot evidence. The latter remains
        // in the vote pool but cannot veto or rewrite the certified decision.
        let previous_hash = self.previous_close_hash(package.epoch)?;
        let previous_state = package
            .epoch
            .checked_sub(1)
            .and_then(|previous| self.close_states.get(&previous));
        let committees = self.committees_for_epoch(package.epoch)?;
        let (validated, certified_snapshot) = package.validate_with_blocks(
            previous_hash,
            previous_state,
            &committees,
            &self.crypto,
            &self.blocks,
        )?;

        // Package validation deliberately reconstructs exactly the ledger state
        // certified for the closing epoch. The live store may already contain
        // independently finalized descendants from the adjacent open epoch.
        // Reapply the pre-install live frontier tips to the certified snapshot so
        // installing closeState[e] cannot roll the live ledger back to epoch e.
        // The atomic finalization helper still rejects any conflict between the
        // certified close ledger and later live finality.
        let live_frontier_targets = self
            .blocks
            .frontier_map()?
            .into_values()
            .filter(|hash| self.blocks.candidate(*hash).is_some())
            .collect::<Vec<_>>();
        let staged_blocks = certified_snapshot.validate_finalization_set(live_frontier_targets)?;

        let mut staged_pool = self.pool.clone();
        for vote in Self::close_package_votes(&package) {
            let committee = self
                .committees
                .get(&vote.committee)
                .ok_or(RaiError::UnknownCommittee(vote.committee))?;
            Self::insert_package_evidence(&mut staged_pool, vote, committee, &self.crypto)?;
        }
        let mut staged_finalized_epochs = self.block_finalized_epochs.clone();
        for frontier in validated.frontier.values() {
            for hash in staged_blocks.chain_to_genesis(*frontier)? {
                staged_finalized_epochs
                    .entry(hash)
                    .and_modify(|existing| *existing = (*existing).min(package.epoch))
                    .or_insert(package.epoch);
            }
        }

        let close_hash = validated.close_hash;
        let derived = if self.stake_derived_committees {
            self.derive_balance_committee(package.epoch, close_hash, &validated.accounts)?
        } else {
            let id = self.derive_registry_committee(package.epoch, close_hash)?;
            self.committees
                .get(&id)
                .cloned()
                .ok_or(RaiError::UnknownCommittee(id))?
        };
        let derived_committee = derived.id;
        if let Some(existing) = self.committees.get(&derived_committee) {
            if existing != &derived {
                return Err(RaiError::SafetyFault(
                    "derived committee id collides with different committee contents".into(),
                ));
            }
        }
        self.pool = staged_pool;
        self.blocks = staged_blocks;
        self.block_finalized_epochs = staged_finalized_epochs;
        self.committees.insert(derived_committee, derived);
        self.close_hashes.insert(package.epoch, close_hash);
        self.close_states.insert(package.epoch, validated);
        self.derived_committees
            .insert(package.epoch, derived_committee);
        self.finish_closing(package.epoch)?;
        Ok(close_hash)
    }

    fn submit_locally_emitted_votes(
        &mut self,
        votes: Vec<SignedVote>,
    ) -> Result<GlobalResultUpdate> {
        let election = votes
            .first()
            .map(|vote| vote.election.clone())
            .ok_or_else(|| RaiError::InvalidVote("empty local vote action".into()))?;
        if votes.iter().any(|vote| vote.election != election) {
            return Err(RaiError::InvalidVote(
                "one logical local action cannot span multiple elections".into(),
            ));
        }

        let mut staged_pool = self.pool.clone();
        let mut inserted_votes = Vec::new();
        for vote in votes {
            let committee = self
                .committees
                .get(&vote.committee)
                .ok_or(RaiError::UnknownCommittee(vote.committee))?;
            if staged_pool.insert(vote.clone(), committee, &self.crypto)? {
                inserted_votes.push(vote);
            }
        }
        self.pool = staged_pool;
        for vote in &inserted_votes {
            if vote.kind == VoteKind::First {
                self.election_started_at_ms
                    .entry(vote.election.clone())
                    .or_insert(self.now_ms);
            }
            self.record_vote_effects(vote);
        }
        self.update_visible(election.epoch())?;
        self.refresh_outcome(&election)
    }

    fn release_pending_close_cut_votes(&mut self, epoch: Epoch, hash: Hash32) -> Result<()> {
        let keys = self
            .pending_close_cut_votes
            .iter()
            .filter_map(|(key, vote)| {
                (matches!(&vote.election, ElectionId::CloseCut { epoch: vote_epoch, .. } if *vote_epoch == epoch)
                    && vote.value == VoteValue::Candidate(hash))
                    .then_some(key.clone())
            })
            .collect::<Vec<_>>();

        for key in keys {
            let Some(vote) = self.pending_close_cut_votes.remove(&key) else {
                continue;
            };
            match self.insert_vote_and_refresh(vote) {
                Ok(_) => {}
                Err(RaiError::SafetyFault(message)) => {
                    return Err(RaiError::SafetyFault(message));
                }
                // Invalid or conflicting messages are Byzantine input. They do
                // not invalidate otherwise valid candidate data.
                Err(_) => {}
            }
        }
        Ok(())
    }

    fn release_pending_close_record_votes(&mut self, epoch: Epoch, hash: Hash32) -> Result<()> {
        let keys = self
            .pending_close_record_votes
            .iter()
            .filter_map(|(key, vote)| {
                (matches!(&vote.election, ElectionId::CloseRecord { epoch: vote_epoch, .. } if *vote_epoch == epoch)
                    && vote.value == VoteValue::Candidate(hash))
                    .then_some(key.clone())
            })
            .collect::<Vec<_>>();

        for key in keys {
            let Some(vote) = self.pending_close_record_votes.remove(&key) else {
                continue;
            };
            match self.insert_vote_and_refresh(vote) {
                Ok(_) => {}
                Err(RaiError::SafetyFault(message)) => {
                    return Err(RaiError::SafetyFault(message));
                }
                // Invalid or conflicting messages are Byzantine input. They do
                // not invalidate otherwise valid candidate data.
                Err(_) => {}
            }
        }
        Ok(())
    }

    fn insert_vote_and_refresh(&mut self, vote: SignedVote) -> Result<GlobalResultUpdate> {
        let committee = self
            .committees
            .get(&vote.committee)
            .ok_or(RaiError::UnknownCommittee(vote.committee))?;
        let inserted = self.pool.insert(vote.clone(), committee, &self.crypto)?;
        if inserted {
            if vote.kind == VoteKind::First {
                self.election_started_at_ms
                    .entry(vote.election.clone())
                    .or_insert(self.now_ms);
            }
            self.record_vote_effects(&vote);
            self.update_visible(vote.election.epoch())?;
        }
        self.refresh_outcome(&vote.election)
    }

    fn refresh_outcome(&mut self, election: &ElectionId) -> Result<GlobalResultUpdate> {
        let derived = self.derive_result(election)?;
        if let Some(result) = derived.as_ref() {
            if matches!(result, GlobalResult::Fast(_) | GlobalResult::Final(_)) {
                if let Some(block) = result.value() {
                    if self.blocks.is_complete(block) {
                        self.require_receive_source_epochs(election, block)?;
                        self.finalize_block_for_epoch(block, election.epoch())?;
                    }
                }
            }
        }
        let first_observation = match derived.as_ref() {
            Some(result) if !self.outcomes.contains_key(election) => {
                self.outcomes.insert(election.clone(), result.clone());
                true
            }
            _ => false,
        };
        Ok(GlobalResultUpdate {
            derived,
            first_observation,
        })
    }

    fn record_vote_effects(&mut self, vote: &SignedVote) {
        if let ElectionId::Slot { slot, epoch } = &vote.election {
            self.slot_vote_locks
                .insert((vote.signer, *slot), vote.election.clone());
            if let VoteValue::Candidate(block) = vote.value {
                self.obligations.insert((vote.signer, *slot, *epoch, block));
                if let Some(candidate) = self.blocks.candidate(block) {
                    for receive in &candidate.block.receives {
                        self.receive_vote_locks
                            .entry((vote.signer, receive.send))
                            .or_insert_with(|| (vote.election.clone(), block));
                    }
                }
            }
        }
    }

    fn insert_evidence_vote(&mut self, vote: SignedVote) -> Result<bool> {
        let expected = self.expected_committee_ids(vote.election.epoch())?;
        if !expected.contains(&vote.committee) {
            return Err(RaiError::InvalidVote(
                "evidence vote is outside the derived committee set".into(),
            ));
        }
        let committee = self
            .committees
            .get(&vote.committee)
            .ok_or(RaiError::UnknownCommittee(vote.committee))?;
        let inserted = self.pool.insert(vote.clone(), committee, &self.crypto)?;
        if inserted {
            if vote.kind == VoteKind::First {
                self.election_started_at_ms
                    .entry(vote.election.clone())
                    .or_insert(self.now_ms);
            }
            self.record_vote_effects(&vote);
        }
        Ok(inserted)
    }

    fn locally_known_evidence_pool(&self) -> Result<VotePool> {
        let mut merged = self.pool.clone();
        for vote in self.package_evidence.all_votes() {
            let committee = self
                .committees
                .get(&vote.committee)
                .ok_or(RaiError::UnknownCommittee(vote.committee))?;
            Self::insert_package_evidence(&mut merged, vote, committee, &self.crypto)?;
        }
        Ok(merged)
    }

    fn package_evidence_with(&self, package: &ClosePackage) -> Result<VotePool> {
        let mut staged = self.package_evidence.clone();
        for vote in Self::close_package_votes(package) {
            let committee = self
                .committees
                .get(&vote.committee)
                .ok_or(RaiError::UnknownCommittee(vote.committee))?;
            Self::insert_package_evidence(&mut staged, vote, committee, &self.crypto)?;
        }
        Ok(staged)
    }

    fn insert_package_evidence(
        pool: &mut VotePool,
        vote: SignedVote,
        committee: &Committee,
        crypto: &DemoKeyStore,
    ) -> Result<()> {
        match pool.insert(vote, committee, crypto) {
            Ok(_) | Err(RaiError::DuplicateFirstVote | RaiError::DuplicateFinalVote) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn close_package_votes(package: &ClosePackage) -> Vec<SignedVote> {
        let mut votes = package.close_cut_votes.clone();
        votes.extend(
            package
                .close_cut
                .start_witness_votes
                .values()
                .flatten()
                .cloned(),
        );
        votes.extend(
            package
                .cuts
                .values()
                .flat_map(|cut| cut.votes.iter().cloned()),
        );
        votes.extend(package.exclusion_witness_votes.values().flatten().cloned());
        votes
    }

    fn update_visible(&mut self, epoch: Epoch) -> Result<()> {
        let committees = self.committees_for_epoch(epoch)?;
        let mut elections = self
            .elections
            .keys()
            .filter(|election| matches!(election, ElectionId::Slot { epoch: e, .. } if *e == epoch))
            .cloned()
            .collect::<BTreeSet<_>>();
        elections.extend(
            self.reports
                .values()
                .filter(|report| report.epoch == epoch)
                .flat_map(|report| report.elections.iter().cloned())
                .filter(
                    |election| matches!(election, ElectionId::Slot { epoch: e, .. } if *e == epoch),
                ),
        );
        elections.extend(
            self.pool
                .all_votes()
                .into_iter()
                .map(|vote| vote.election)
                .filter(
                    |election| matches!(election, ElectionId::Slot { epoch: e, .. } if *e == epoch),
                ),
        );
        let mut additions = BTreeSet::new();
        for election in elections {
            let vote_visible = committees.iter().any(|committee| {
                committee.weight_of_signers(
                    self.pool
                        .all_votes_for_election(&election)
                        .into_iter()
                        .filter(|vote| vote.committee == committee.id)
                        .map(|vote| vote.signer),
                ) >= committee.visibility_threshold()
            });
            let report_visible = committees.iter().all(|committee| {
                committee.weight_of_signers(
                    self.reports
                        .values()
                        .filter(|report| report.epoch == epoch && committee.contains(report.signer))
                        .filter(|report| report.elections.contains(&election))
                        .map(|report| report.signer),
                ) >= committee.visibility_threshold()
            });
            if vote_visible || report_visible {
                additions.insert(election);
            }
        }
        self.visible.entry(epoch).or_default().extend(additions);
        Ok(())
    }

    fn stable_prefix(&self, target: Hash32) -> Result<bool> {
        self.blocks.stable_prefix(target)
    }

    fn certified_released(&self, election: &ElectionId) -> bool {
        self.close_states
            .get(&election.epoch())
            .map(|state| state.released(election))
            .unwrap_or(false)
    }

    fn certified_released_value(&self, election: &ElectionId, block: Hash32) -> bool {
        if self.epoch_states.get(&election.epoch()) != Some(&EpochState::Closed) {
            return false;
        }
        if self
            .close_cuts
            .get(&election.epoch())
            .map(|cut| !cut.contains(election))
            .unwrap_or(false)
        {
            return true;
        }
        match self
            .close_states
            .get(&election.epoch())
            .and_then(|state| state.statuses.get(election))
        {
            Some(SlotStatus::Released { .. }) => true,
            Some(SlotStatus::Finalized {
                block: finalized, ..
            }) => *finalized != block,
            _ => false,
        }
    }

    fn can_enter_slot_election(
        &self,
        signer: ReplicaId,
        slot: Slot,
        election: &ElectionId,
    ) -> bool {
        match self.slot_vote_locks.get(&(signer, slot)) {
            None => true,
            Some(locked) if locked == election => true,
            Some(locked) => {
                self.certified_released(locked) && self.blocks.finalized(slot).is_none()
            }
        }
    }

    fn require_receive_source_epochs(&self, election: &ElectionId, block: Hash32) -> Result<()> {
        let candidate = self
            .blocks
            .candidate(block)
            .ok_or_else(|| RaiError::UnknownCandidate(block.to_string()))?;
        for receive in &candidate.block.receives {
            let source_epoch = self
                .block_finalized_epochs
                .get(&receive.send.source_block)
                .copied()
                .ok_or_else(|| {
                    RaiError::Inadmissible(format!(
                        "finalization epoch is unknown for send source {}",
                        receive.send.source_block.short()
                    ))
                })?;
            if source_epoch > election.epoch() {
                return Err(RaiError::Inadmissible(format!(
                    "epoch {} cannot receive a send finalized in future epoch {}",
                    election.epoch(),
                    source_epoch
                )));
            }
        }
        Ok(())
    }

    fn finalize_block_for_epoch(&mut self, block: Hash32, epoch: Epoch) -> Result<()> {
        self.blocks.finalize_chain(block)?;
        for hash in self.blocks.chain_to_genesis(block)? {
            self.block_finalized_epochs
                .entry(hash)
                .and_modify(|existing| *existing = (*existing).min(epoch))
                .or_insert(epoch);
        }
        Ok(())
    }

    fn require_receive_vote_locks_available(
        &self,
        signer: ReplicaId,
        election: &ElectionId,
        block: Hash32,
    ) -> Result<()> {
        let candidate = self
            .blocks
            .candidate(block)
            .ok_or_else(|| RaiError::UnknownCandidate(block.to_string()))?;
        for receive in &candidate.block.receives {
            if self.blocks.consumed_sends().contains_key(&receive.send) {
                return Err(RaiError::UnsafeVote(format!(
                    "send {}:{} is already consumed",
                    receive.send.source_block.short(),
                    receive.send.output_index
                )));
            }
            if let Some((locked_election, locked_block)) =
                self.receive_vote_locks.get(&(signer, receive.send))
            {
                if locked_election != election
                    && !self.certified_released_value(locked_election, *locked_block)
                {
                    return Err(RaiError::UnsafeVote(format!(
                        "replica {signer} remains locked by {locked_election} for send {}:{}",
                        receive.send.source_block.short(),
                        receive.send.output_index
                    )));
                }
            }
        }
        Ok(())
    }

    fn require_first_vote_choice(
        &self,
        signer: ReplicaId,
        committee: CommitteeId,
        election: &ElectionId,
        value: VoteValue,
        kind: VoteKind,
    ) -> Result<()> {
        if kind != VoteKind::First {
            return Ok(());
        }
        match self
            .pool
            .first_choice_in_committee(signer, committee, election)
        {
            Some(chosen) if chosen != value => Err(RaiError::InvalidVote(format!(
                "replica {signer} already selected first-vote value {chosen} for {election} in committee {committee}"
            ))),
            _ => Ok(()),
        }
    }

    fn require_slot_vote_lock_available(
        &self,
        signer: ReplicaId,
        election: &ElectionId,
    ) -> Result<()> {
        let ElectionId::Slot { slot, .. } = election else {
            return Ok(());
        };
        if self.can_enter_slot_election(signer, *slot, election) {
            Ok(())
        } else {
            let locked = self
                .slot_vote_locks
                .get(&(signer, *slot))
                .expect("unavailable slot lock has an election");
            Err(RaiError::InvalidVote(format!(
                "replica {signer} remains locked by {locked}; only certified release permits retry for slot {slot}"
            )))
        }
    }

    fn election_is_active(&self, election: &ElectionId) -> bool {
        self.require_enabled(election).is_ok() && matches!(self.derive_result(election), Ok(None))
    }

    /// A certified close record must settle every slot election already
    /// known locally through durable non-timeout vote obligations or accepted
    /// signed reports. Elections in the certified cut are resolved from their
    /// cuts; every other locally known election must be explicitly released by
    /// a close-exclusion entry.
    fn require_local_obligations_covered(&self, package: &ClosePackage) -> Result<()> {
        let epoch = package.epoch;
        let mut locally_known = BTreeSet::new();

        for (_, slot, obligation_epoch, _) in &self.obligations {
            if *obligation_epoch == epoch {
                locally_known.insert(ElectionId::Slot { slot: *slot, epoch });
            }
        }

        for ((report_epoch, _), report) in &self.reports {
            if *report_epoch != epoch {
                continue;
            }
            locally_known.extend(report.elections.iter().filter_map(|election| {
                matches!(election, ElectionId::Slot { epoch: election_epoch, .. } if *election_epoch == epoch)
                    .then_some(election.clone())
            }));
        }

        for election in locally_known {
            if !package.close_cut.elections.contains(&election)
                && !package.excluded.contains(&election)
            {
                return Err(RaiError::InvalidClosePackage(format!(
                    "locally known election {election} is omitted without a close-exclusion release proof"
                )));
            }
        }
        Ok(())
    }

    fn require_registration_enabled(&self, election: &ElectionId) -> Result<()> {
        let state = self
            .epoch_states
            .get(&election.epoch())
            .copied()
            .ok_or_else(|| {
                RaiError::InvalidConfiguration(format!(
                    "epoch {} has not been opened by the lifecycle state machine",
                    election.epoch()
                ))
            })?;
        let enabled = match election {
            ElectionId::Slot { epoch, .. } => {
                state == EpochState::Open
                    || (state == EpochState::Closing
                        && self
                            .close_cuts
                            .get(epoch)
                            .map(|cut| cut.contains(election))
                            .unwrap_or(false))
            }
            ElectionId::CloseCut { .. } | ElectionId::CloseRecord { .. } => {
                state == EpochState::Closing
            }
        };
        if enabled {
            Ok(())
        } else {
            Err(RaiError::InvalidConfiguration(format!(
                "cannot register {election} while epoch {} is {state:?}",
                election.epoch()
            )))
        }
    }

    fn require_enabled(&self, election: &ElectionId) -> Result<()> {
        let epoch_state = self
            .epoch_states
            .get(&election.epoch())
            .copied()
            .unwrap_or(EpochState::Closed);
        let enabled = match election {
            ElectionId::Slot { slot, epoch } => {
                self.blocks.finalized(*slot).is_none()
                    && (epoch_state == EpochState::Open
                        || (epoch_state == EpochState::Closing
                            && self
                                .close_cuts
                                .get(epoch)
                                .map(|cut| cut.contains(election))
                                .unwrap_or(false)))
            }
            ElectionId::CloseCut { .. } | ElectionId::CloseRecord { .. } => {
                epoch_state == EpochState::Closing
            }
        };
        if enabled {
            Ok(())
        } else {
            Err(RaiError::InvalidVote(format!(
                "election {election} is not enabled"
            )))
        }
    }

    fn require_active(&self, election: &ElectionId) -> Result<()> {
        self.require_enabled(election)?;
        let close_decided = match election {
            ElectionId::CloseCut { epoch, .. } => self.decided_close_cut(*epoch)?.is_some(),
            ElectionId::CloseRecord { epoch, .. } => self.decided_close_record(*epoch)?.is_some(),
            ElectionId::Slot { .. } => false,
        };
        if close_decided {
            return Err(RaiError::InvalidVote(format!(
                "logical close instance for {election} has already decided"
            )));
        }
        if self.derive_result(election)?.is_some() {
            return Err(RaiError::InvalidVote(format!(
                "election {election} has already finished"
            )));
        }
        Ok(())
    }

    fn require_first_vote_timeout_elapsed(&self, election: &ElectionId) -> Result<()> {
        let started_at = self
            .election_started_at_ms
            .get(election)
            .copied()
            .ok_or_else(|| {
                RaiError::InvalidVote("timeout first vote requires a started election timer".into())
            })?;
        let deadline = started_at
            .checked_add(self.election_timeout_ms)
            .ok_or_else(|| {
                RaiError::InvalidConfiguration("election timeout deadline overflow".into())
            })?;
        if self.now_ms <= deadline {
            return Err(RaiError::InvalidVote(format!(
                "timeout first vote is premature: now={}ms, deadline={}ms",
                self.now_ms, deadline
            )));
        }
        Ok(())
    }

    fn require_governing_context(&self, epoch: Epoch) -> Result<()> {
        if epoch < 2 {
            if self.close_hashes.get(&u64::MAX).is_none() || self.genesis_committee.is_none() {
                return Err(RaiError::InvalidVote(
                    "genesis governing context is unavailable".into(),
                ));
            }
            return Ok(());
        }
        let governing_epoch = epoch - 2;
        let state = self.close_states.get(&governing_epoch).ok_or_else(|| {
            RaiError::InvalidVote(format!(
                "certified governing close state {governing_epoch} is unavailable"
            ))
        })?;
        if self.close_hashes.get(&governing_epoch) != Some(&state.close_hash)
            || self.derived_committees.get(&governing_epoch).is_none()
        {
            return Err(RaiError::InvalidVote(
                "governing close state, hash, or derived committee is inconsistent".into(),
            ));
        }
        Ok(())
    }

    fn applicable_committee_ids(
        &self,
        signer: ReplicaId,
        election: &ElectionId,
    ) -> Result<Vec<CommitteeId>> {
        let ids = self
            .elections
            .get(election)
            .ok_or_else(|| RaiError::UnknownElection(election.to_string()))?;
        let applicable = ids
            .iter()
            .copied()
            .filter(|committee_id| {
                self.committees
                    .get(committee_id)
                    .map(|committee| committee.contains(signer))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        if applicable.is_empty() {
            return Err(RaiError::InvalidVote(format!(
                "replica {signer} is not a member of any committee for {election}"
            )));
        }
        Ok(applicable)
    }

    fn require_election_committee(
        &self,
        election: &ElectionId,
        committee: CommitteeId,
        signer: ReplicaId,
    ) -> Result<()> {
        let committees = self
            .elections
            .get(election)
            .ok_or_else(|| RaiError::UnknownElection(election.to_string()))?;
        if !committees.contains(&committee) {
            return Err(RaiError::InvalidVote(
                "committee is outside the derived election committee set".into(),
            ));
        }
        let committee = self
            .committees
            .get(&committee)
            .ok_or(RaiError::UnknownCommittee(committee))?;
        if !committee.contains(signer) {
            return Err(RaiError::InvalidVote(format!(
                "replica {signer} is not a committee member"
            )));
        }
        Ok(())
    }

    fn require_received_slot_vote_valid(&self, vote: &SignedVote) -> Result<()> {
        let ElectionId::Slot { slot, .. } = &vote.election else {
            return Ok(());
        };
        let VoteValue::Candidate(block) = vote.value else {
            return Ok(());
        };
        if !self.blocks.admissible_for_slot(*slot, block)? {
            return Err(RaiError::Inadmissible(format!(
                "block {} is not admissible for {}",
                block.short(),
                vote.election
            )));
        }
        self.require_receive_source_epochs(&vote.election, block)?;
        // Unlike stable-prefix and certified-release checks, a receive lock is
        // derived from signed vote history. Rejecting a second unreleased use
        // of the same send prevents that signer's weight from contributing to
        // two competing receive candidates.
        self.require_receive_vote_locks_available(vote.signer, &vote.election, block)
    }

    fn require_admissible_and_safe(
        &self,
        signer: ReplicaId,
        election: &ElectionId,
        value: VoteValue,
        kind: VoteKind,
    ) -> Result<()> {
        match election {
            ElectionId::Slot { slot, .. } => {
                let VoteValue::Candidate(block) = value else {
                    return Ok(());
                };
                if !self.blocks.admissible_for_slot(*slot, block)? {
                    return Err(RaiError::Inadmissible(format!(
                        "block {} is not admissible for {}",
                        block.short(),
                        election
                    )));
                }
                self.require_receive_source_epochs(election, block)?;
                self.require_receive_vote_locks_available(signer, election, block)?;
                if !self.safe_to_vote(signer, election, block)? {
                    return Err(RaiError::UnsafeVote(format!(
                        "stable-prefix, certified-release, or governing-frontier guard rejects {} for {}",
                        block.short(),
                        election
                    )));
                }
            }
            ElectionId::CloseCut { epoch, .. } => {
                let VoteValue::Candidate(hash) = value else {
                    return Ok(());
                };
                let candidate = self.close_cut_candidates.get(&hash).ok_or_else(|| {
                    RaiError::UnknownCandidate(format!(
                        "validated close-cut candidate {}",
                        hash.short()
                    ))
                })?;
                if candidate.epoch != *epoch {
                    return Err(RaiError::Inadmissible(
                        "close-cut candidate epoch does not match the election".into(),
                    ));
                }
                if kind == VoteKind::First && self.preferred_close_value(election)? != Some(hash) {
                    return Err(RaiError::InvalidVote(
                        "close-cut first vote is not for the replica's deterministic preferred value"
                            .into(),
                    ));
                }
            }
            ElectionId::CloseRecord { epoch, .. } => {
                let VoteValue::Candidate(hash) = value else {
                    return Ok(());
                };
                let package = self
                    .close_record_candidates
                    .get(&(*epoch, hash))
                    .ok_or_else(|| {
                        RaiError::UnknownCandidate(format!(
                            "validated close-record package {}",
                            hash.short()
                        ))
                    })?;
                // Stable admissibility: validate the self-contained package
                // opening without merging later local slot evidence.
                let previous_hash = self.previous_close_hash(*epoch)?;
                let previous_state = epoch
                    .checked_sub(1)
                    .and_then(|previous| self.close_states.get(&previous));
                let committees = self.committees_for_epoch(*epoch)?;
                package.validate_with_blocks(
                    previous_hash,
                    previous_state,
                    &committees,
                    &self.crypto,
                    &self.blocks,
                )?;
                self.require_local_obligations_covered(package)?;
                if kind == VoteKind::First && self.preferred_close_value(election)? != Some(hash) {
                    return Err(RaiError::InvalidVote(
                        "close-record first vote is not for the canonically reconstructed record"
                            .into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn preferred_close_value(&self, election: &ElectionId) -> Result<Option<Hash32>> {
        match election {
            ElectionId::CloseCut { epoch, round } => {
                if let Some((_, value)) = self.decided_close_cut(*epoch)? {
                    return Ok(Some(value));
                }
                if *round == 0 {
                    return Ok(Some(hash_close_cut(*epoch, self.visible_elections(*epoch))));
                }
                let previous = ElectionId::CloseCut {
                    epoch: *epoch,
                    round: *round - 1,
                };
                match self.derive_result(&previous)? {
                    Some(GlobalResult::Timeout) => {
                        Ok(Some(hash_close_cut(*epoch, self.visible_elections(*epoch))))
                    }
                    Some(GlobalResult::Converged(value)) => Ok(Some(value)),
                    Some(GlobalResult::Fast(value) | GlobalResult::Final(value)) => Ok(Some(value)),
                    Some(GlobalResult::Notarized(_)) => Err(RaiError::SafetyFault(
                        "close-cut election exposed an unconverted notarized result".into(),
                    )),
                    None => Ok(None),
                }
            }
            ElectionId::CloseRecord { epoch, round } => {
                if let Some((_, value)) = self.decided_close_record(*epoch)? {
                    return Ok(Some(value));
                }

                if *round == 0 {
                    return self.reconstructed_close_record_preference(*epoch);
                }

                let previous = ElectionId::CloseRecord {
                    epoch: *epoch,
                    round: *round - 1,
                };
                match self.derive_result(&previous)? {
                    // A timeout certificate or certificate-level conflict is
                    // positive death evidence, so the successor is unlocked
                    // and uses the current canonical fresh reconstruction.
                    Some(GlobalResult::Timeout) => {
                        self.reconstructed_close_record_preference(*epoch)
                    }
                    // A converged value is a live carry. Additional
                    // evidence may alter fresh preference but cannot override
                    // this carry until the source round is positively dead.
                    Some(GlobalResult::Converged(value)) => Ok(Some(value)),
                    Some(GlobalResult::Fast(value) | GlobalResult::Final(value)) => Ok(Some(value)),
                    Some(GlobalResult::Notarized(_)) => Err(RaiError::SafetyFault(
                        "close-record election exposed an unconverted notarized result".into(),
                    )),
                    // A local timer expiration or an incomplete source round
                    // is not a death proof; do not start the successor.
                    None => Ok(None),
                }
            }
            ElectionId::Slot { .. } => Ok(None),
        }
    }

    fn reconstructed_close_record_preference(&self, epoch: Epoch) -> Result<Option<Hash32>> {
        // Multiple self-contained close packages may remain admissible at the
        // same time. Fresh preference is therefore not selected by scanning
        // cached candidates; it is the canonical package reconstructed from
        // the complete valid evidence currently known to this replica.
        let package = self.build_close_package_from_current_evidence(epoch)?;
        Ok(Some(package.record.hash()))
    }

    fn decided_close_cut(&self, epoch: Epoch) -> Result<Option<(ElectionId, Hash32)>> {
        self.decided_close_instance(epoch, true)
    }

    fn decided_close_record(&self, epoch: Epoch) -> Result<Option<(ElectionId, Hash32)>> {
        self.decided_close_instance(epoch, false)
    }

    fn decided_close_instance(
        &self,
        epoch: Epoch,
        close_cut: bool,
    ) -> Result<Option<(ElectionId, Hash32)>> {
        let mut certified: Option<(ElectionId, Hash32)> = None;
        for election in self.elections.keys() {
            let matches_instance = if close_cut {
                matches!(election, ElectionId::CloseCut { epoch: e, .. } if *e == epoch)
            } else {
                matches!(election, ElectionId::CloseRecord { epoch: e, .. } if *e == epoch)
            };
            if !matches_instance {
                continue;
            }
            let Some(GlobalResult::Fast(value) | GlobalResult::Final(value)) =
                self.derive_result(election)?
            else {
                continue;
            };
            match certified.as_ref() {
                Some((_, existing)) if *existing != value => {
                    let instance = if close_cut {
                        "close-cut"
                    } else {
                        "close-record"
                    };
                    return Err(RaiError::SafetyFault(format!(
                        "{instance} rounds for epoch {epoch} decided conflicting values"
                    )));
                }
                None => certified = Some((election.clone(), value)),
                _ => {}
            }
        }
        Ok(certified)
    }

    fn committees_for(&self, election: &ElectionId) -> Result<Vec<Committee>> {
        let ids = self
            .elections
            .get(election)
            .ok_or_else(|| RaiError::UnknownElection(election.to_string()))?;
        ids.iter()
            .map(|id| {
                self.committees
                    .get(id)
                    .cloned()
                    .ok_or(RaiError::UnknownCommittee(*id))
            })
            .collect()
    }

    fn committees_for_epoch(&self, epoch: Epoch) -> Result<Vec<Committee>> {
        self.expected_committee_ids_readonly(epoch)?
            .into_iter()
            .map(|id| {
                self.committees
                    .get(&id)
                    .cloned()
                    .ok_or(RaiError::UnknownCommittee(id))
            })
            .collect()
    }

    fn expected_committee_ids(&mut self, epoch: Epoch) -> Result<Vec<CommitteeId>> {
        self.freeze_committee_registry()?;
        self.expected_committee_ids_readonly(epoch)
    }

    fn expected_committee_ids_readonly(&self, epoch: Epoch) -> Result<Vec<CommitteeId>> {
        let genesis = self.genesis_committee.ok_or_else(|| {
            RaiError::InvalidCommittee("genesis committee has not been derived".into())
        })?;
        let committee_at = |index: i128| -> Result<CommitteeId> {
            if index < 0 {
                Ok(genesis)
            } else {
                self.derived_committees
                    .get(&(index as u64))
                    .copied()
                    .ok_or_else(|| {
                        RaiError::InvalidCommittee(format!(
                            "committeeAt({index}) is unavailable because its close is not certified"
                        ))
                    })
            }
        };
        let e = epoch as i128;
        let mut ids = vec![committee_at(e - 3)?, committee_at(e - 2)?];
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    fn freeze_committee_registry(&mut self) -> Result<()> {
        if self.committee_registry_frozen {
            return Ok(());
        }
        if self.committees.is_empty() {
            return Err(RaiError::InvalidCommittee(
                "cannot derive genesis committee from an empty registry".into(),
            ));
        }
        let genesis_hash = *self
            .close_hashes
            .get(&u64::MAX)
            .expect("genesis close hash installed");
        self.genesis_committee = Some(self.derive_registry_committee(u64::MAX, genesis_hash)?);
        self.committee_registry_frozen = true;
        Ok(())
    }

    fn derive_balance_committee(
        &self,
        epoch: Epoch,
        certified_close_hash: Hash32,
        accounts: &BTreeMap<u64, AccountState>,
    ) -> Result<Committee> {
        let mut weights = BTreeMap::<ReplicaId, Weight>::new();
        for state in accounts.values() {
            let weight = weights.entry(state.representative).or_default();
            *weight = weight.checked_add(state.balance).ok_or_else(|| {
                RaiError::InvalidCommittee("representative weight overflow".into())
            })?;
        }
        weights.retain(|_, weight| *weight > 0);
        if weights.is_empty() {
            return Err(RaiError::InvalidCommittee(
                "certified account state delegates no positive voting weight".into(),
            ));
        }
        if let Some(existing) = self.committees.values().find(|committee| {
            committee.weights == weights
                && committee.f == self.committee_fault_weight
                && committee.p == self.committee_participation_weight
        }) {
            return Ok(existing.clone());
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"rai-balance-committee-v2-close-hash");
        put_u64(&mut bytes, epoch);
        bytes.extend_from_slice(&certified_close_hash.0);
        let digest = Hash32::digest(&bytes);
        let mut id_bytes = [0u8; 8];
        id_bytes.copy_from_slice(&digest.0[..8]);
        let mut id = u64::from_be_bytes(id_bytes);
        loop {
            let candidate = Committee::weighted(
                id,
                weights.clone(),
                self.committee_fault_weight,
                self.committee_participation_weight,
            )?;
            match self.committees.get(&id) {
                None => return Ok(candidate),
                Some(existing) if existing == &candidate => return Ok(candidate),
                Some(_) => id = id.wrapping_add(1),
            }
        }
    }

    fn derive_registry_committee(&self, epoch: Epoch, close_hash: Hash32) -> Result<CommitteeId> {
        let ids = self.committees.keys().copied().collect::<Vec<_>>();
        if ids.is_empty() {
            return Err(RaiError::InvalidCommittee(
                "cannot derive a committee from an empty registry".into(),
            ));
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"rai-derived-committee-v1");
        put_u64(&mut bytes, epoch);
        bytes.extend_from_slice(&close_hash.0);
        let digest = Hash32::digest(&bytes);
        let mut index_bytes = [0u8; 8];
        index_bytes.copy_from_slice(&digest.0[..8]);
        let index = u64::from_be_bytes(index_bytes) as usize % ids.len();
        Ok(ids[index])
    }

    fn latest_close_cut_round(&self, epoch: Epoch) -> Option<(u32, ElectionId)> {
        self.elections
            .keys()
            .filter_map(|election| match election {
                ElectionId::CloseCut {
                    epoch: election_epoch,
                    round,
                } if *election_epoch == epoch => Some((*round, election.clone())),
                _ => None,
            })
            .max_by_key(|(round, _)| *round)
    }

    fn latest_close_record_round(&self, epoch: Epoch) -> Option<(u32, ElectionId)> {
        self.elections
            .keys()
            .filter_map(|election| match election {
                ElectionId::CloseRecord {
                    epoch: election_epoch,
                    round,
                } if *election_epoch == epoch => Some((*round, election.clone())),
                _ => None,
            })
            .max_by_key(|(round, _)| *round)
    }

    fn known_values(&self, election: &ElectionId) -> BTreeSet<VoteValue> {
        let mut values = BTreeSet::new();
        match election {
            ElectionId::Slot { slot, .. } => {
                values.extend(
                    self.blocks
                        .candidates_at_slot(*slot)
                        .into_iter()
                        .map(VoteValue::Candidate),
                );
            }
            ElectionId::CloseCut { epoch, .. } => {
                values.extend(
                    self.close_cut_candidates
                        .iter()
                        .filter_map(|(hash, candidate)| {
                            (candidate.epoch == *epoch).then_some(VoteValue::Candidate(*hash))
                        }),
                );
            }
            ElectionId::CloseRecord { epoch, .. } => {
                values.extend(self.close_record_candidates.keys().filter_map(
                    |(candidate_epoch, hash)| {
                        (*candidate_epoch == *epoch).then_some(VoteValue::Candidate(*hash))
                    },
                ));
            }
        }
        values
    }

    fn require_certified_predecessor(&self, epoch: Epoch) -> Result<()> {
        if epoch == 0 {
            return self
                .close_hashes
                .contains_key(&u64::MAX)
                .then_some(())
                .ok_or_else(|| {
                    RaiError::InvalidConfiguration(
                        "genesis predecessor close hash is unavailable".into(),
                    )
                });
        }
        let predecessor = epoch - 1;
        if self.epoch_states.get(&predecessor) != Some(&EpochState::Closed) {
            return Err(RaiError::InvalidConfiguration(format!(
                "epoch {predecessor} must be closed before transitioning epoch {epoch}"
            )));
        }
        let state = self.close_states.get(&predecessor).ok_or_else(|| {
            RaiError::InvalidConfiguration(format!(
                "certified close state {predecessor} is unavailable"
            ))
        })?;
        if self.close_hashes.get(&predecessor) != Some(&state.close_hash) {
            return Err(RaiError::SafetyFault(format!(
                "predecessor epoch {predecessor} close state and hash disagree"
            )));
        }
        Ok(())
    }

    fn check_epoch_cardinality(&self) -> Result<()> {
        let open = self
            .epoch_states
            .values()
            .filter(|state| **state == EpochState::Open)
            .count();
        let closing = self
            .epoch_states
            .values()
            .filter(|state| **state == EpochState::Closing)
            .count();
        if open > 1 || closing > 1 {
            return Err(RaiError::SafetyFault(format!(
                "epoch lifecycle invariant violated: open={open}, closing={closing}"
            )));
        }
        Ok(())
    }

    fn previous_close_hash(&self, epoch: Epoch) -> Result<Hash32> {
        if epoch == 0 {
            self.close_hashes
                .get(&u64::MAX)
                .copied()
                .ok_or_else(|| RaiError::InvalidClosePackage("genesis close hash missing".into()))
        } else {
            self.close_hashes
                .get(&(epoch - 1))
                .copied()
                .ok_or_else(|| RaiError::InvalidClosePackage("previous epoch is not closed".into()))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalResultUpdate {
    pub derived: Option<GlobalResult>,
    pub first_observation: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CloseProtocolAction {
    AwaitReports,
    BroadcastCloseCut {
        election: ElectionId,
        candidate: CloseCutCandidate,
        hash: Hash32,
    },
    AwaitCloseCut {
        election: ElectionId,
    },
    DrainCut {
        pending: BTreeSet<ElectionId>,
    },
    BroadcastCloseRecord {
        election: ElectionId,
        package: ClosePackage,
        hash: Hash32,
    },
    AwaitCloseRecord {
        election: ElectionId,
    },
    Closed {
        close_hash: Hash32,
    },
}

#[cfg(test)]
mod conformance_regressions {
    use super::*;
    use crate::block::Block;
    use crate::crypto::AccountKeyStore;

    fn two_committee_engine() -> (RaiEngine, ElectionId, Hash32, Hash32) {
        let crypto = DemoKeyStore::deterministic(1..=6);
        let account_keys = AccountKeyStore::deterministic([1]);
        let mut engine = RaiEngine::with_account_genesis(
            crypto,
            [GenesisAccount::new(
                1,
                1_000,
                1,
                account_keys.public_key(1).unwrap(),
            )],
        )
        .unwrap();
        engine
            .add_committee(Committee::new(10, 1..=6, 1, 1).unwrap())
            .unwrap();
        engine
            .add_committee(Committee::new(11, 1..=6, 1, 1).unwrap())
            .unwrap();
        engine.freeze_committee_registry().unwrap();

        let slot = Slot::new(1, 1);
        let election = ElectionId::Slot { slot, epoch: 0 };
        engine.elections.insert(election.clone(), vec![10, 11]);

        let left = SignedBlock::sign(
            &account_keys,
            Block {
                slot,
                parent: BlockStore::genesis(1),
                balance: 1_000,
                representative: 1,
                sends: vec![crate::block::Send {
                    destination: 1,
                    amount: 1,
                }],
                receives: Vec::new(),
            },
        )
        .unwrap();
        let right = SignedBlock::sign(
            &account_keys,
            Block {
                slot,
                parent: BlockStore::genesis(1),
                balance: 1_000,
                representative: 1,
                sends: vec![crate::block::Send {
                    destination: 1,
                    amount: 2,
                }],
                receives: Vec::new(),
            },
        )
        .unwrap();
        let left_hash = engine.submit_block(left).unwrap();
        let right_hash = engine.submit_block(right).unwrap();
        engine.blocks.mark_complete(left_hash).unwrap();
        engine.blocks.mark_complete(right_hash).unwrap();
        (engine, election, left_hash, right_hash)
    }

    #[test]
    fn final_vote_requires_compatible_support_in_every_applicable_committee() {
        let (mut engine, election, left, right) = two_committee_engine();
        for (committee, value) in [(10, left), (11, right)] {
            let vote = SignedVote::new(
                &engine.crypto,
                1,
                election.clone(),
                committee,
                VoteValue::Candidate(value),
                VoteKind::First,
            )
            .unwrap();
            let descriptor = engine.committees.get(&committee).unwrap().clone();
            engine
                .pool
                .insert(vote, &descriptor, &engine.crypto)
                .unwrap();
        }

        assert!(engine.cast_final_vote(1, &election, 10, left).is_err());
        assert_eq!(engine.pool.final_value(1, 10, &election), None);
        assert_eq!(engine.pool.final_value(1, 11, &election), None);
    }

    #[test]
    fn final_vote_is_atomically_expanded_to_every_applicable_committee() {
        let (mut engine, election, left, _) = two_committee_engine();
        for committee in [10, 11] {
            let vote = SignedVote::new(
                &engine.crypto,
                1,
                election.clone(),
                committee,
                VoteValue::Candidate(left),
                VoteKind::First,
            )
            .unwrap();
            let descriptor = engine.committees.get(&committee).unwrap().clone();
            engine
                .pool
                .insert(vote, &descriptor, &engine.crypto)
                .unwrap();
        }

        engine.cast_final_vote(1, &election, 10, left).unwrap();
        assert_eq!(
            engine.pool.final_value(1, 10, &election),
            Some(VoteValue::Candidate(left))
        );
        assert_eq!(
            engine.pool.final_value(1, 11, &election),
            Some(VoteValue::Candidate(left))
        );
    }

    #[test]
    fn first_vote_is_atomically_expanded_when_actions_agree() {
        let (mut engine, election, left, _) = two_committee_engine();
        engine
            .cast_first_vote(1, &election, 10, VoteValue::Candidate(left))
            .unwrap();
        for committee in [10, 11] {
            assert_eq!(
                engine.pool.first_value(1, committee, &election),
                Some(VoteValue::Candidate(left))
            );
        }
    }

    #[test]
    fn notarization_is_atomically_expanded_when_actions_agree() {
        let (mut engine, election, left, _) = two_committee_engine();
        for signer in 1..=3 {
            engine
                .cast_first_vote(signer, &election, 10, VoteValue::Candidate(left))
                .unwrap();
        }
        engine
            .cast_notarization_vote(1, &election, 10, VoteValue::Candidate(left))
            .unwrap();
        for committee in [10, 11] {
            assert!(engine.pool.has_notarization_vote(
                1,
                committee,
                &election,
                VoteValue::Candidate(left),
            ));
        }
    }

    #[test]
    fn start_closing_report_retains_finished_unreleased_support() {
        let crypto = DemoKeyStore::deterministic(1..=6);
        let account_keys = AccountKeyStore::deterministic([3]);
        let mut engine = RaiEngine::with_account_genesis(
            crypto,
            [GenesisAccount::new(
                3,
                1_000,
                1,
                account_keys.public_key(3).unwrap(),
            )],
        )
        .unwrap();
        engine
            .add_committee(Committee::new(7, 1..=6, 1, 1).unwrap())
            .unwrap();
        let slot = Slot::new(3, 1);
        let election = ElectionId::Slot { slot, epoch: 0 };
        engine.register_derived_election(election.clone()).unwrap();
        let block = SignedBlock::sign(
            &account_keys,
            Block {
                slot,
                parent: BlockStore::genesis(slot.account),
                balance: 1_000,
                representative: 1,
                sends: Vec::new(),
                receives: Vec::new(),
            },
        )
        .unwrap();
        let hash = engine.submit_block(block).unwrap();
        for signer in 1..=4 {
            engine
                .cast_first_vote(signer, &election, 7, VoteValue::Candidate(hash))
                .unwrap();
        }
        assert_eq!(
            engine.derive_result(&election).unwrap(),
            Some(GlobalResult::Notarized(hash))
        );
        let uninvolved = engine.build_signed_report(5, 0).unwrap();
        assert!(!uninvolved.elections.contains(&election));

        let report = engine.start_closing_with_report(0, 1).unwrap();
        assert!(report.elections.contains(&election));
    }

    #[test]
    fn certified_cut_registers_and_starts_report_only_elections() {
        let crypto = DemoKeyStore::deterministic(1..=6);
        let mut engine = RaiEngine::new(crypto, Hash32::digest(b"cut-genesis"));
        engine
            .add_committee(Committee::new(7, 1..=6, 1, 1).unwrap())
            .unwrap();
        engine.start_closing(0).unwrap();
        let included = ElectionId::Slot {
            slot: Slot::new(99, 1),
            epoch: 0,
        };
        for signer in 1..=5 {
            engine
                .submit_report(
                    SignedReport::new(&engine.crypto, signer, 0, [included.clone()]).unwrap(),
                )
                .unwrap();
        }
        let (close_election, _, hash) = engine.prepare_close_cut_round(0, 0, None).unwrap();
        for signer in 1..=5 {
            engine
                .submit_vote(
                    SignedVote::new(
                        &engine.crypto,
                        signer,
                        close_election.clone(),
                        7,
                        VoteValue::Candidate(hash),
                        VoteKind::First,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        engine
            .install_certified_close_cut(&close_election, hash)
            .unwrap();
        assert_eq!(
            engine.election_started_at_ms(&included),
            Some(engine.now_ms())
        );
        assert!(engine.elections.contains_key(&included));
    }

    #[test]
    fn late_earlier_round_certificates_decide_logical_close_instances() {
        let crypto = DemoKeyStore::deterministic(1..=6);
        let mut engine = RaiEngine::new(crypto, Hash32::digest(b"late-close-genesis"));
        engine
            .add_committee(Committee::new(7, 1..=6, 1, 1).unwrap())
            .unwrap();
        engine.start_closing(0).unwrap();
        for signer in 1..=5 {
            engine
                .submit_report(SignedReport::new(&engine.crypto, signer, 0, []).unwrap())
                .unwrap();
        }

        let (cut_round_0, _, cut_hash) = engine.prepare_close_cut_round(0, 0, None).unwrap();
        for signer in 1..=4 {
            engine
                .submit_vote(
                    SignedVote::new(
                        &engine.crypto,
                        signer,
                        cut_round_0.clone(),
                        7,
                        VoteValue::Candidate(cut_hash),
                        VoteKind::First,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(
            engine.derive_result(&cut_round_0).unwrap(),
            Some(GlobalResult::Converged(cut_hash))
        );

        let (cut_round_1, _, carried_cut_hash) = engine
            .prepare_close_cut_round(0, 1, Some(cut_hash))
            .unwrap();
        assert_eq!(carried_cut_hash, cut_hash);

        engine
            .submit_vote(
                SignedVote::new(
                    &engine.crypto,
                    5,
                    cut_round_0.clone(),
                    7,
                    VoteValue::Candidate(cut_hash),
                    VoteKind::First,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            engine.derive_result(&cut_round_0).unwrap(),
            Some(GlobalResult::Fast(cut_hash))
        );

        let (record_round_0, record_hash) = match engine.drive_close_protocol(0).unwrap() {
            CloseProtocolAction::BroadcastCloseRecord { election, hash, .. } => (election, hash),
            action => panic!("late cut certificate was not installed: {action:?}"),
        };
        assert_eq!(engine.certified_close_cut_hashes.get(&0), Some(&cut_hash));
        assert!(engine
            .cast_first_vote(6, &cut_round_1, 7, VoteValue::Candidate(carried_cut_hash),)
            .is_err());

        for signer in 1..=4 {
            engine
                .submit_vote(
                    SignedVote::new(
                        &engine.crypto,
                        signer,
                        record_round_0.clone(),
                        7,
                        VoteValue::Candidate(record_hash),
                        VoteKind::First,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(
            engine.derive_result(&record_round_0).unwrap(),
            Some(GlobalResult::Converged(record_hash))
        );

        let record_round_1 = match engine.drive_close_protocol(0).unwrap() {
            CloseProtocolAction::BroadcastCloseRecord { election, hash, .. } => {
                assert_eq!(hash, record_hash);
                election
            }
            action => panic!("live close-record value was not carried: {action:?}"),
        };

        engine
            .submit_vote(
                SignedVote::new(
                    &engine.crypto,
                    5,
                    record_round_0.clone(),
                    7,
                    VoteValue::Candidate(record_hash),
                    VoteKind::First,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            engine.derive_result(&record_round_0).unwrap(),
            Some(GlobalResult::Fast(record_hash))
        );

        assert_eq!(
            engine.drive_close_protocol(0).unwrap(),
            CloseProtocolAction::Closed {
                close_hash: record_hash
            }
        );
        assert_eq!(engine.epoch_state(0), Some(EpochState::Closed));
        assert!(engine
            .cast_first_vote(6, &record_round_1, 7, VoteValue::Candidate(record_hash),)
            .is_err());
    }
}
