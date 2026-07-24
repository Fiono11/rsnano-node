use std::collections::{BTreeMap, BTreeSet};

use crate::block::{hash_ledger_frontiers, AccountState, BlockStore, SendId, SignedBlock};
use crate::certificate::{
    conflicting_notarizations, derive_global_result, derive_local_result, GlobalResult,
    LocalResult, TimeoutReason,
};
use crate::committee::Committee;
use crate::crypto::{CryptoProvider, Signature};
use crate::error::{RaiError, Result};
use crate::types::{
    put_u64, AccountId, CommitteeId, ElectionId, Epoch, Hash32, ReplicaId, Slot, VoteValue,
};
use crate::vote::{SignedVote, VoteKind, VotePool};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedReport {
    pub signer: ReplicaId,
    pub epoch: Epoch,
    pub elections: BTreeSet<ElectionId>,
    pub signature: Signature,
}

impl SignedReport {
    pub fn new(
        crypto: &impl CryptoProvider,
        signer: ReplicaId,
        epoch: Epoch,
        elections: impl IntoIterator<Item = ElectionId>,
    ) -> Result<Self> {
        let elections = elections.into_iter().collect::<BTreeSet<_>>();
        let bytes = Self::signing_bytes_for(signer, epoch, &elections);
        let signature = crypto
            .sign(signer, &bytes)
            .ok_or(RaiError::InvalidSignature)?;
        Ok(Self {
            signer,
            epoch,
            elections,
            signature,
        })
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        Self::signing_bytes_for(self.signer, self.epoch, &self.elections)
    }

    pub fn verify(&self, crypto: &impl CryptoProvider) -> bool {
        crypto.verify(self.signer, &self.signing_bytes(), &self.signature)
    }

    fn signing_bytes_for(
        signer: ReplicaId,
        epoch: Epoch,
        elections: &BTreeSet<ElectionId>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"rai-close-report-v2-ed25519");
        put_u64(&mut out, signer);
        put_u64(&mut out, epoch);
        put_u64(&mut out, elections.len() as u64);
        for election in elections {
            election.encode(&mut out);
        }
        out
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JointReportProof {
    pub reports: BTreeMap<CommitteeId, Vec<SignedReport>>,
}

impl JointReportProof {
    pub fn validate(
        &self,
        epoch: Epoch,
        committees: &[Committee],
        crypto: &impl CryptoProvider,
    ) -> Result<BTreeMap<ReplicaId, SignedReport>> {
        if committees.is_empty() {
            return Err(RaiError::InvalidClosePackage(
                "joint report proof has no close committees".into(),
            ));
        }
        let expected = committees
            .iter()
            .map(|committee| committee.id)
            .collect::<BTreeSet<_>>();
        if self.reports.keys().copied().collect::<BTreeSet<_>>() != expected {
            return Err(RaiError::InvalidClosePackage(
                "joint report proof committee set does not match the close election".into(),
            ));
        }

        let mut accepted = BTreeMap::<ReplicaId, SignedReport>::new();
        for committee in committees {
            let reports = self
                .reports
                .get(&committee.id)
                .expect("expected report bucket");
            let mut signers = BTreeSet::new();
            for report in reports {
                if report.epoch != epoch
                    || !committee.contains(report.signer)
                    || !report.verify(crypto)
                    || !signers.insert(report.signer)
                {
                    return Err(RaiError::InvalidClosePackage(format!(
                        "invalid or duplicate report in committee {} for epoch {epoch}",
                        committee.id
                    )));
                }
                if report.elections.iter().any(|election| {
                    !matches!(election, ElectionId::Slot { epoch: report_epoch, .. } if *report_epoch == epoch)
                }) {
                    return Err(RaiError::InvalidClosePackage(
                        "report contains a non-slot or wrong-epoch election".into(),
                    ));
                }
                match accepted.get(&report.signer) {
                    Some(existing) if existing != report => {
                        return Err(RaiError::InvalidClosePackage(format!(
                            "replica {} supplied conflicting reports for epoch {epoch}",
                            report.signer
                        )));
                    }
                    None => {
                        accepted.insert(report.signer, report.clone());
                    }
                    _ => {}
                }
            }
            let report_weight = committee.weight_of_signers(signers.iter().copied());
            if report_weight < committee.report_threshold() {
                return Err(RaiError::InvalidClosePackage(format!(
                    "committee {} report proof has weight {}, requires {}",
                    committee.id,
                    report_weight,
                    committee.report_threshold()
                )));
            }
        }
        Ok(accepted)
    }

    pub fn report_visible(&self, epoch: Epoch, committees: &[Committee]) -> BTreeSet<ElectionId> {
        let candidates = self
            .reports
            .values()
            .flatten()
            .flat_map(|report| report.elections.iter().cloned())
            .filter(|election| election.epoch() == epoch)
            .collect::<BTreeSet<_>>();
        candidates
            .into_iter()
            .filter(|election| {
                committees.iter().all(|committee| {
                    committee.weight_of_signers(
                        self.reports
                            .get(&committee.id)
                            .into_iter()
                            .flatten()
                            .filter(|report| report.elections.contains(election))
                            .map(|report| report.signer),
                    ) >= committee.visibility_threshold()
                })
            })
            .collect()
    }

    pub fn report_count(&self, committee: CommitteeId, election: &ElectionId) -> usize {
        self.reports
            .get(&committee)
            .into_iter()
            .flatten()
            .filter(|report| report.elections.contains(election))
            .map(|report| report.signer)
            .collect::<BTreeSet<_>>()
            .len()
    }

    pub fn report_weight(&self, committee: &Committee, election: &ElectionId) -> u128 {
        committee.weight_of_signers(
            self.reports
                .get(&committee.id)
                .into_iter()
                .flatten()
                .filter(|report| report.elections.contains(election))
                .map(|report| report.signer),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseCutCandidate {
    pub epoch: Epoch,
    pub elections: BTreeSet<ElectionId>,
    pub report_proof: JointReportProof,
    /// First-vote witnesses supplied for elections whose start is not already
    /// witnessed by f+1 reports in every election committee.
    pub start_witness_votes: BTreeMap<ElectionId, Vec<SignedVote>>,
}

impl CloseCutCandidate {
    pub fn hash(&self) -> Hash32 {
        hash_close_cut(self.epoch, self.elections.iter().cloned())
    }

    pub fn validate(
        &self,
        committees: &[Committee],
        crypto: &impl CryptoProvider,
    ) -> Result<Vec<SignedVote>> {
        if self.elections.iter().any(
            |election| !matches!(election, ElectionId::Slot { epoch, .. } if *epoch == self.epoch),
        ) {
            return Err(RaiError::InvalidClosePackage(
                "close cut contains a non-slot or wrong-epoch election".into(),
            ));
        }
        self.report_proof.validate(self.epoch, committees, crypto)?;
        let forced = self.report_proof.report_visible(self.epoch, committees);
        if !forced.is_subset(&self.elections) {
            return Err(RaiError::InvalidClosePackage(
                "close cut omits a slot forced by its joint report proof".into(),
            ));
        }

        let mut validated_votes = Vec::new();
        for election in &self.elections {
            let report_witness = committees.iter().all(|committee| {
                self.report_proof.report_weight(committee, election)
                    >= committee.visibility_threshold()
            });
            let mut vote_witness = false;
            for vote in self.start_witness_votes.get(election).into_iter().flatten() {
                validate_start_vote(vote, election, committees, crypto)?;
                vote_witness = true;
                validated_votes.push(vote.clone());
            }
            if !report_witness && !vote_witness {
                return Err(RaiError::InvalidClosePackage(format!(
                    "close-cut election {election} has no valid start witness"
                )));
            }
        }
        if self
            .start_witness_votes
            .keys()
            .any(|election| !self.elections.contains(election))
        {
            return Err(RaiError::InvalidClosePackage(
                "start witness supplied for an election outside the close cut".into(),
            ));
        }
        Ok(validated_votes)
    }
}

pub fn hash_close_cut(epoch: Epoch, elections: impl IntoIterator<Item = ElectionId>) -> Hash32 {
    let canonical = elections.into_iter().collect::<BTreeSet<_>>();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RAI/CloseCut");
    put_u64(&mut bytes, epoch);
    put_u64(&mut bytes, canonical.len() as u64);
    for election in canonical {
        election.encode(&mut bytes);
    }
    Hash32::digest(&bytes)
}

fn validate_start_vote(
    vote: &SignedVote,
    election: &ElectionId,
    committees: &[Committee],
    crypto: &impl CryptoProvider,
) -> Result<()> {
    if vote.election != *election || vote.kind != VoteKind::First {
        return Err(RaiError::InvalidClosePackage(
            "start witness is not a first vote for the referenced election".into(),
        ));
    }
    let committee = committees
        .iter()
        .find(|committee| committee.id == vote.committee)
        .ok_or_else(|| {
            RaiError::InvalidClosePackage(
                "start witness vote is scoped outside the election committees".into(),
            )
        })?;
    if !committee.contains(vote.signer)
        || !crypto.verify(vote.signer, &vote.signing_bytes(), &vote.signature)
    {
        return Err(RaiError::InvalidSignature);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElectionCut {
    pub election: ElectionId,
    pub committee_ids: Vec<u64>,
    pub votes: Vec<SignedVote>,
}

impl ElectionCut {
    pub(crate) fn ordered_votes(&self) -> Vec<SignedVote> {
        let mut votes = self.votes.clone();
        votes.sort_by_key(|vote| (vote.kind, vote.committee, vote.value, vote.signer));
        votes
    }

    pub fn resolve(
        &self,
        election_committees: &[Committee],
        crypto: &impl CryptoProvider,
    ) -> Result<SlotStatus> {
        self.resolve_with_local_evidence(election_committees, crypto, &VotePool::default())
    }

    /// Resolves the cut against the union of package evidence and all
    /// relevant valid evidence already present in the local vote pool.
    pub fn resolve_with_local_evidence(
        &self,
        election_committees: &[Committee],
        crypto: &impl CryptoProvider,
        local_pool: &VotePool,
    ) -> Result<SlotStatus> {
        let ElectionId::Slot { .. } = &self.election else {
            return Err(RaiError::InvalidClosePackage(
                "close cuts may only contain slot elections".into(),
            ));
        };
        let expected = election_committees
            .iter()
            .map(|committee| committee.id)
            .collect::<BTreeSet<_>>();
        let supplied = self.committee_ids.iter().copied().collect::<BTreeSet<_>>();
        if supplied != expected {
            return Err(RaiError::InvalidClosePackage(
                "election cut committee set differs from the derived committee set".into(),
            ));
        }
        let committee_map = election_committees
            .iter()
            .map(|committee| (committee.id, committee))
            .collect::<BTreeMap<_, _>>();
        let mut pool = VotePool::default();
        for vote in self.ordered_votes() {
            if vote.election != self.election {
                return Err(RaiError::InvalidClosePackage(
                    "vote belongs to a different election".into(),
                ));
            }
            let committee = committee_map
                .get(&vote.committee)
                .ok_or(RaiError::UnknownCommittee(vote.committee))?;
            pool.insert(vote, committee, crypto)?;
        }
        for vote in local_pool.all_votes_for_election(&self.election) {
            let Some(committee) = committee_map.get(&vote.committee) else {
                continue;
            };
            pool.insert(vote, committee, crypto)?;
        }

        let result =
            derive_global_result(&pool, election_committees, &self.election)?.ok_or_else(|| {
                RaiError::InvalidClosePackage(format!(
                    "certificate-incomplete cut for {}",
                    self.election
                ))
            })?;
        match result {
            GlobalResult::Fast(block) => Ok(SlotStatus::Finalized {
                election: self.election.clone(),
                block,
                via: FinalityEvidence::Fast,
            }),
            GlobalResult::Final(block) => Ok(SlotStatus::Finalized {
                election: self.election.clone(),
                block,
                via: FinalityEvidence::Final,
            }),
            GlobalResult::Notarized(block) => Ok(SlotStatus::Selected {
                election: self.election.clone(),
                block,
            }),
            GlobalResult::Converged(_) => Err(RaiError::InvalidClosePackage(
                "slot election unexpectedly produced a close-election convergence result".into(),
            )),
            GlobalResult::Timeout => {
                let mut timeout_certificate = false;
                let mut conflict = None;
                for committee in election_committees {
                    match derive_local_result(&pool, committee, &self.election)? {
                        Some(LocalResult::Timeout(TimeoutReason::Certificate)) => {
                            timeout_certificate = true;
                        }
                        Some(LocalResult::Timeout(TimeoutReason::Conflict { left, right })) => {
                            conflict.get_or_insert((left, right));
                        }
                        _ => {
                            if conflict.is_none() {
                                conflict =
                                    conflicting_notarizations(&pool, committee, &self.election);
                            }
                        }
                    }
                }
                let reason = if timeout_certificate {
                    ReleaseEvidence::Timeout
                } else if let Some((left, right)) = conflict {
                    ReleaseEvidence::Conflict { left, right }
                } else {
                    ReleaseEvidence::MergedIncompatibility
                };
                Ok(SlotStatus::Released {
                    election: self.election.clone(),
                    reason,
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalityEvidence {
    Fast,
    Final,
    Ancestor,
    CloseRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseEvidence {
    Timeout,
    Conflict { left: Hash32, right: Hash32 },
    MergedIncompatibility,
    CloseExclusion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlotStatus {
    Finalized {
        election: ElectionId,
        block: Hash32,
        via: FinalityEvidence,
    },
    Selected {
        election: ElectionId,
        block: Hash32,
    },
    Released {
        election: ElectionId,
        reason: ReleaseEvidence,
    },
}

impl SlotStatus {
    pub fn election(&self) -> &ElectionId {
        match self {
            Self::Finalized { election, .. }
            | Self::Selected { election, .. }
            | Self::Released { election, .. } => election,
        }
    }

    pub fn block(&self) -> Option<Hash32> {
        match self {
            Self::Finalized { block, .. } | Self::Selected { block, .. } => Some(*block),
            Self::Released { .. } => None,
        }
    }

    pub fn slot(&self) -> Slot {
        self.election()
            .slot()
            .expect("slot status has slot election")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseRecord {
    pub epoch: Epoch,
    pub previous_close_hash: Hash32,
    pub frontiers: BTreeMap<AccountId, Hash32>,
}

impl CloseRecord {
    pub fn hash(&self) -> Hash32 {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RAI/CloseRecord");
        put_u64(&mut bytes, self.epoch);
        bytes.extend_from_slice(&self.previous_close_hash.0);
        put_u64(&mut bytes, self.frontiers.len() as u64);
        for (account, frontier) in &self.frontiers {
            put_u64(&mut bytes, *account);
            bytes.extend_from_slice(&frontier.0);
        }
        Hash32::digest(&bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosePackage {
    pub epoch: Epoch,
    pub record: CloseRecord,
    pub close_cut_election: ElectionId,
    pub close_cut: CloseCutCandidate,
    pub close_cut_votes: Vec<SignedVote>,
    pub cuts: BTreeMap<ElectionId, ElectionCut>,
    pub excluded: BTreeSet<ElectionId>,
    pub exclusion_witness_votes: BTreeMap<ElectionId, Vec<SignedVote>>,
    pub blocks: Vec<SignedBlock>,
    /// Canonically ordered by account through the BTreeMap representation.
    /// Each hash transitively commits to the complete account chain.
    pub frontiers: BTreeMap<AccountId, Hash32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedCloseState {
    pub epoch: Epoch,
    pub close_hash: Hash32,
    pub statuses: BTreeMap<ElectionId, SlotStatus>,
    pub frontier: BTreeMap<AccountId, Hash32>,
    pub accounts: BTreeMap<AccountId, AccountState>,
    pub consumed_sends: BTreeMap<SendId, Hash32>,
    pub ledger_root: Hash32,
}

impl CertifiedCloseState {
    pub fn released(&self, election: &ElectionId) -> bool {
        matches!(
            self.statuses.get(election),
            Some(SlotStatus::Released { .. })
        )
    }
}

impl ClosePackage {
    #[allow(clippy::too_many_arguments)]
    pub fn build_certified(
        epoch: Epoch,
        previous_close_hash: Hash32,
        close_cut_election: ElectionId,
        close_cut: CloseCutCandidate,
        close_cut_votes: Vec<SignedVote>,
        cuts: BTreeMap<ElectionId, ElectionCut>,
        excluded: BTreeSet<ElectionId>,
        exclusion_witness_votes: BTreeMap<ElectionId, Vec<SignedVote>>,
        package_blocks: Vec<SignedBlock>,
        previous_state: Option<&CertifiedCloseState>,
        election_committees: &[Committee],
        crypto: &impl CryptoProvider,
        base_blocks: &BlockStore,
    ) -> Result<Self> {
        validate_close_cut_certificate(
            epoch,
            &close_cut_election,
            &close_cut,
            &close_cut_votes,
            election_committees,
            crypto,
        )?;
        let staged_blocks = stage_package_blocks(base_blocks, &package_blocks)?;
        validate_package_membership(
            epoch,
            &close_cut,
            &cuts,
            &excluded,
            &exclusion_witness_votes,
            election_committees,
            crypto,
        )?;
        let (statuses, resolved_frontier) = resolve_all(
            epoch,
            &cuts,
            &excluded,
            previous_state,
            election_committees,
            crypto,
            &VotePool::default(),
            &staged_blocks,
        )?;
        require_bundled_paths(&statuses, &package_blocks, &staged_blocks)?;

        // Fresh construction derives the proposed ledger from the package-local
        // ordinary-final, selected, and release classifications. Selected is a
        // pre-decision classification only; the certified frontier map is the
        // consensus commitment.
        let close_baseline = staged_blocks.certified_baseline(
            previous_state.map(|state| &state.accounts),
            previous_state.map(|state| &state.consumed_sends),
        )?;
        let proposed_ledger = close_baseline
            .validate_finalization_set(statuses.values().filter_map(SlotStatus::block))?;
        let frontiers = proposed_ledger.frontier_map()?;
        if resolved_frontier != frontiers {
            return Err(RaiError::InvalidClosePackage(
                "resolver frontier differs from the replayed ledger frontier map".into(),
            ));
        }
        let record = CloseRecord {
            epoch,
            previous_close_hash,
            frontiers: frontiers.clone(),
        };
        let package = Self {
            epoch,
            record,
            close_cut_election,
            close_cut,
            close_cut_votes,
            cuts,
            excluded,
            exclusion_witness_votes,
            blocks: package_blocks,
            frontiers,
        };
        package.validate_with_blocks(
            previous_close_hash,
            previous_state,
            election_committees,
            crypto,
            base_blocks,
        )?;
        Ok(package)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn validate_with_blocks(
        &self,
        expected_previous_hash: Hash32,
        previous_state: Option<&CertifiedCloseState>,
        election_committees: &[Committee],
        crypto: &impl CryptoProvider,
        base_blocks: &BlockStore,
    ) -> Result<(CertifiedCloseState, BlockStore)> {
        self.validate_with_local_evidence(
            expected_previous_hash,
            previous_state,
            election_committees,
            crypto,
            base_blocks,
            &VotePool::default(),
        )
    }

    /// Compatibility entry point. Package admissibility is deliberately based
    /// only on the self-contained package evidence. Additional local evidence
    /// may alter fresh preference, but cannot invalidate a package that already
    /// opens a valid close-record commitment.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_with_local_evidence(
        &self,
        expected_previous_hash: Hash32,
        previous_state: Option<&CertifiedCloseState>,
        election_committees: &[Committee],
        crypto: &impl CryptoProvider,
        base_blocks: &BlockStore,
        _local_pool: &VotePool,
    ) -> Result<(CertifiedCloseState, BlockStore)> {
        if self.record.previous_close_hash != expected_previous_hash {
            return Err(RaiError::InvalidClosePackage(
                "close record does not extend the certified previous close hash".into(),
            ));
        }
        if self.record.epoch != self.epoch || self.record.frontiers != self.frontiers {
            return Err(RaiError::InvalidClosePackage(
                "close record does not open the supplied epoch and frontier map".into(),
            ));
        }
        if let Some(previous) = previous_state {
            if previous.close_hash != expected_previous_hash {
                return Err(RaiError::InvalidClosePackage(
                    "supplied previous close state does not open the expected predecessor hash"
                        .into(),
                ));
            }
        }
        validate_close_cut_certificate(
            self.epoch,
            &self.close_cut_election,
            &self.close_cut,
            &self.close_cut_votes,
            election_committees,
            crypto,
        )?;
        validate_package_membership(
            self.epoch,
            &self.close_cut,
            &self.cuts,
            &self.excluded,
            &self.exclusion_witness_votes,
            election_committees,
            crypto,
        )?;
        let staged_blocks = stage_package_blocks(base_blocks, &self.blocks)?;
        let (package_statuses, resolved_frontier) = resolve_all(
            self.epoch,
            &self.cuts,
            &self.excluded,
            previous_state,
            election_committees,
            crypto,
            &VotePool::default(),
            &staged_blocks,
        )?;
        require_bundled_paths(&package_statuses, &self.blocks, &staged_blocks)?;

        if resolved_frontier != self.frontiers {
            return Err(RaiError::InvalidClosePackage(
                "package frontier map differs from deterministic resolver output".into(),
            ));
        }
        let ledger_root = hash_ledger_frontiers(&self.frontiers);
        if let Some(previous) = previous_state {
            for (account, previous_frontier) in &previous.frontier {
                let current = self.frontiers.get(account).ok_or_else(|| {
                    RaiError::InvalidClosePackage(format!(
                        "ledger frontier map omits previously certified account {account}"
                    ))
                })?;
                if current != previous_frontier
                    && !staged_blocks.descends_from(*current, *previous_frontier)?
                {
                    return Err(RaiError::SafetyFault(format!(
                        "ledger frontier for account {account} does not extend its previous certified frontier"
                    )));
                }
            }
        }

        // Reconstruct the entire ledger from genesis through every frontier.
        // This validates all predecessor links and ledger transitions and does
        // not trust cached balances, representatives, or consumed-send data.
        let certified_blocks = staged_blocks.validate_ledger_frontiers(&self.frontiers)?;
        let statuses = durable_statuses_from_ledger(&package_statuses, &certified_blocks)?;
        let accounts = certified_blocks.account_states()?;
        let consumed_sends = certified_blocks.consumed_sends().clone();
        Ok((
            CertifiedCloseState {
                epoch: self.epoch,
                close_hash: self.record.hash(),
                statuses,
                frontier: self.frontiers.clone(),
                accounts,
                consumed_sends,
                ledger_root,
            },
            certified_blocks,
        ))
    }
}

fn durable_statuses_from_ledger(
    package_statuses: &BTreeMap<ElectionId, SlotStatus>,
    ledger: &BlockStore,
) -> Result<BTreeMap<ElectionId, SlotStatus>> {
    let mut durable = BTreeMap::new();
    for (election, status) in package_statuses {
        let slot = status.slot();
        let ledger_block = ledger.finalized(slot);
        let resolved = match status {
            SlotStatus::Finalized { block, via, .. } => {
                if ledger_block != Some(*block) {
                    return Err(RaiError::InvalidClosePackage(format!(
                        "ordinary-finalized block {} for {election} is absent from the certified ledger",
                        block.short()
                    )));
                }
                SlotStatus::Finalized {
                    election: election.clone(),
                    block: *block,
                    via: via.clone(),
                }
            }
            SlotStatus::Selected { block, .. } => {
                if ledger_block != Some(*block) {
                    return Err(RaiError::InvalidClosePackage(format!(
                        "package-selected block {} for {election} is absent from the certified ledger",
                        block.short()
                    )));
                }
                SlotStatus::Finalized {
                    election: election.clone(),
                    block: *block,
                    via: FinalityEvidence::CloseRecord,
                }
            }
            SlotStatus::Released { reason, .. } => {
                if ledger_block.is_some() {
                    return Err(RaiError::InvalidClosePackage(format!(
                        "released election {election} has a candidate on the certified ledger"
                    )));
                }
                SlotStatus::Released {
                    election: election.clone(),
                    reason: reason.clone(),
                }
            }
        };
        durable.insert(election.clone(), resolved);
    }
    Ok(durable)
}

fn validate_close_cut_certificate(
    epoch: Epoch,
    close_cut_election: &ElectionId,
    candidate: &CloseCutCandidate,
    votes: &[SignedVote],
    committees: &[Committee],
    crypto: &impl CryptoProvider,
) -> Result<()> {
    if !matches!(close_cut_election, ElectionId::CloseCut { epoch: cut_epoch, .. } if *cut_epoch == epoch)
        || candidate.epoch != epoch
    {
        return Err(RaiError::InvalidClosePackage(
            "close package references the wrong close-cut election".into(),
        ));
    }
    candidate.validate(committees, crypto)?;
    let hash = candidate.hash();
    let committee_map = committees
        .iter()
        .map(|committee| (committee.id, committee))
        .collect::<BTreeMap<_, _>>();
    let mut pool = VotePool::default();
    for vote in votes {
        if vote.election != *close_cut_election
            || vote.value != VoteValue::Candidate(hash)
            || !matches!(vote.kind, VoteKind::First | VoteKind::Final)
        {
            return Err(RaiError::InvalidClosePackage(
                "close-cut certificate contains a vote for another value or election".into(),
            ));
        }
        let committee = committee_map
            .get(&vote.committee)
            .ok_or(RaiError::UnknownCommittee(vote.committee))?;
        pool.insert(vote.clone(), committee, crypto)?;
    }
    if !matches!(
        derive_global_result(&pool, committees, close_cut_election)?,
        Some(GlobalResult::Fast(value) | GlobalResult::Final(value)) if value == hash
    ) {
        return Err(RaiError::InvalidClosePackage(
            "close cut is not backed by a joint fast or final certificate".into(),
        ));
    }
    Ok(())
}

fn validate_package_membership(
    epoch: Epoch,
    close_cut: &CloseCutCandidate,
    cuts: &BTreeMap<ElectionId, ElectionCut>,
    excluded: &BTreeSet<ElectionId>,
    exclusion_witness_votes: &BTreeMap<ElectionId, Vec<SignedVote>>,
    committees: &[Committee],
    crypto: &impl CryptoProvider,
) -> Result<()> {
    let cut_keys = cuts.keys().cloned().collect::<BTreeSet<_>>();
    if cut_keys != close_cut.elections {
        return Err(RaiError::InvalidClosePackage(
            "certificate-complete slot cuts do not exactly match the certified close cut".into(),
        ));
    }
    for election in excluded {
        if !matches!(election, ElectionId::Slot { epoch: excluded_epoch, .. } if *excluded_epoch == epoch)
            || close_cut.elections.contains(election)
        {
            return Err(RaiError::InvalidClosePackage(
                "invalid or overlapping close-exclusion entry".into(),
            ));
        }
        let votes = exclusion_witness_votes.get(election).ok_or_else(|| {
            RaiError::InvalidClosePackage(format!(
                "excluded election {election} has no signed start/obligation witness"
            ))
        })?;
        if votes.is_empty() {
            return Err(RaiError::InvalidClosePackage(
                "empty close-exclusion witness".into(),
            ));
        }
        for vote in votes {
            validate_start_vote(vote, election, committees, crypto)?;
        }
    }
    if exclusion_witness_votes
        .keys()
        .any(|election| !excluded.contains(election))
    {
        return Err(RaiError::InvalidClosePackage(
            "exclusion witness supplied for a non-excluded election".into(),
        ));
    }
    Ok(())
}

fn stage_package_blocks(base: &BlockStore, supplied: &[SignedBlock]) -> Result<BlockStore> {
    let mut staged = base.clone();
    for block in supplied {
        staged.stage_candidate_for_replay(block.clone())?;
    }
    let mut ordered = supplied.to_vec();
    ordered.sort_by_key(|signed| {
        (
            signed.block.slot.account,
            signed.block.slot.sequence,
            signed.hash(),
        )
    });
    let mut pending = ordered;
    while !pending.is_empty() {
        let mut progressed = false;
        let mut next = Vec::new();
        for signed in pending {
            let hash = signed.hash();
            if staged.is_complete(hash) {
                progressed = true;
                continue;
            }
            if staged.parent_complete(&signed.block) {
                staged.mark_complete_for_replay(hash)?;
                progressed = true;
            } else {
                next.push(signed);
            }
        }
        if !progressed {
            return Err(RaiError::InvalidClosePackage(
                "bundled block data is missing a complete parent path".into(),
            ));
        }
        pending = next;
    }
    Ok(staged)
}

fn require_bundled_paths(
    statuses: &BTreeMap<ElectionId, SlotStatus>,
    _supplied: &[SignedBlock],
    blocks: &BlockStore,
) -> Result<()> {
    // Blocks already present in the validator's authenticated store need not be
    // retransmitted. Every committed path must nevertheless be locally
    // available and complete before the package is accepted.
    for status in statuses.values() {
        let Some(target) = status.block() else {
            continue;
        };
        for hash in blocks.chain_to_genesis(target)? {
            if !blocks.is_complete(hash) {
                return Err(RaiError::InvalidClosePackage(format!(
                    "close package cannot open complete account path through block {}",
                    hash.short()
                )));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_all(
    epoch: Epoch,
    cuts: &BTreeMap<ElectionId, ElectionCut>,
    excluded: &BTreeSet<ElectionId>,
    previous_state: Option<&CertifiedCloseState>,
    election_committees: &[Committee],
    crypto: &impl CryptoProvider,
    _local_pool: &VotePool,
    blocks: &BlockStore,
) -> Result<(
    BTreeMap<ElectionId, SlotStatus>,
    BTreeMap<AccountId, Hash32>,
)> {
    match (epoch, previous_state) {
        (0, None) => {}
        (0, Some(_)) => {
            return Err(RaiError::InvalidClosePackage(
                "genesis close package must not reference a previous close state".into(),
            ));
        }
        (current, Some(previous)) if previous.epoch + 1 == current => {}
        _ => {
            return Err(RaiError::InvalidClosePackage(
                "close package must extend the immediately preceding certified state".into(),
            ));
        }
    }

    let mut statuses = BTreeMap::new();
    let mut slot_owner = BTreeMap::<Slot, ElectionId>::new();

    if let Some(previous) = previous_state {
        if previous
            .statuses
            .values()
            .any(|status| matches!(status, SlotStatus::Selected { .. }))
        {
            return Err(RaiError::SafetyFault(
                "certified predecessor state contains an unpromoted selected entry".into(),
            ));
        }
        for (election, status) in &previous.statuses {
            if matches!(status, SlotStatus::Finalized { .. }) {
                let slot = status.slot();
                slot_owner.insert(slot, election.clone());
                statuses.insert(election.clone(), status.clone());
            }
        }
    }

    for (election, cut) in cuts {
        if election != &cut.election {
            return Err(RaiError::InvalidClosePackage(
                "close cut map key does not match embedded election id".into(),
            ));
        }
        let ElectionId::Slot {
            slot,
            epoch: election_epoch,
        } = election
        else {
            return Err(RaiError::InvalidClosePackage(
                "non-slot election included in close cut".into(),
            ));
        };
        let resolved = cut.resolve(election_committees, crypto)?;
        if *election_epoch != epoch {
            return Err(RaiError::InvalidClosePackage(format!(
                "close cut for epoch {epoch} contains election {election} from another epoch"
            )));
        }
        if let Some(previous) = slot_owner.insert(*slot, election.clone()) {
            return Err(RaiError::InvalidClosePackage(format!(
                "logical slot {slot} appears in both {previous} and {election}"
            )));
        }
        statuses.insert(election.clone(), resolved);
    }

    for election in excluded {
        let ElectionId::Slot {
            slot,
            epoch: election_epoch,
        } = election
        else {
            return Err(RaiError::InvalidClosePackage(
                "only slot elections can be released by close exclusion".into(),
            ));
        };
        if *election_epoch != epoch || cuts.contains_key(election) {
            return Err(RaiError::InvalidClosePackage(
                "invalid or overlapping close-exclusion proof".into(),
            ));
        }
        if let Some(previous) = slot_owner.insert(*slot, election.clone()) {
            return Err(RaiError::InvalidClosePackage(format!(
                "logical slot {slot} appears in both {previous} and {election}"
            )));
        }
        statuses.insert(
            election.clone(),
            SlotStatus::Released {
                election: election.clone(),
                reason: ReleaseEvidence::CloseExclusion,
            },
        );
    }

    let finalized_targets = statuses
        .values()
        .filter_map(|status| match status {
            SlotStatus::Finalized { block, .. } => Some(*block),
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut ancestor_by_slot = BTreeMap::<Slot, Hash32>::new();
    for target in finalized_targets {
        if !blocks.is_complete(target) {
            return Err(RaiError::InvalidClosePackage(format!(
                "finalized target {} is not complete",
                target.short()
            )));
        }
        for hash in blocks.chain_to_genesis(target)? {
            let block = &blocks
                .candidate(hash)
                .ok_or_else(|| RaiError::UnknownCandidate(hash.to_string()))?
                .block;
            if let Some(existing) = ancestor_by_slot.insert(block.slot, hash) {
                if existing != hash {
                    return Err(RaiError::SafetyFault(format!(
                        "ancestor closure selects two blocks for slot {}",
                        block.slot
                    )));
                }
            }
        }
    }

    for (slot, block) in ancestor_by_slot {
        let election = slot_owner.get(&slot).cloned().ok_or_else(|| {
            RaiError::InvalidClosePackage(format!(
                "finalized descendant has an ancestor slot {slot} absent from the certified close state"
            ))
        })?;
        match statuses.get(&election) {
            Some(SlotStatus::Released { .. }) => {
                return Err(RaiError::SafetyFault(format!(
                    "finalized descendant contains released ancestor election {election}"
                )));
            }
            Some(SlotStatus::Selected {
                block: selected, ..
            }) if *selected != block => {
                return Err(RaiError::SafetyFault(format!(
                    "ancestor closure conflicts with selected block for {election}"
                )));
            }
            Some(SlotStatus::Finalized {
                block: finalized, ..
            }) if *finalized != block => {
                return Err(RaiError::SafetyFault(format!(
                    "ancestor closure conflicts with finalized block for {election}"
                )));
            }
            _ => {}
        }
        statuses.insert(
            election.clone(),
            SlotStatus::Finalized {
                election,
                block,
                via: FinalityEvidence::Ancestor,
            },
        );
    }

    let finalized_by_slot = statuses
        .values()
        .filter_map(|status| match status {
            SlotStatus::Finalized {
                election, block, ..
            } => Some((election.slot().expect("finalized slot election"), *block)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    for status in statuses.values() {
        let SlotStatus::Selected { election, block } = status else {
            continue;
        };
        let chain = blocks.chain_to_genesis(*block)?;
        for ancestor in chain.iter().take(chain.len().saturating_sub(1)) {
            let signed = blocks
                .candidate(*ancestor)
                .ok_or_else(|| RaiError::UnknownCandidate(ancestor.to_string()))?;
            let already_finalized = blocks.finalized(signed.block.slot) == Some(*ancestor)
                || finalized_by_slot.get(&signed.block.slot) == Some(ancestor);
            if !already_finalized {
                return Err(RaiError::InvalidClosePackage(format!(
                    "selected block {} for {election} has non-finalized strict ancestor {}",
                    block.short(),
                    ancestor.short()
                )));
            }
        }
    }

    let mut frontier = previous_state
        .map(|state| state.frontier.clone())
        .unwrap_or_else(|| blocks.genesis_frontiers());
    let mut account_chains = BTreeMap::<AccountId, Vec<(Slot, Hash32)>>::new();
    for status in statuses.values() {
        let Some(block_hash) = status.block() else {
            continue;
        };
        let signed = blocks
            .candidate(block_hash)
            .ok_or_else(|| RaiError::UnknownCandidate(block_hash.to_string()))?;
        if signed.block.slot != status.slot() || !blocks.is_complete(block_hash) {
            return Err(RaiError::InvalidClosePackage(format!(
                "resolved block {} does not open a complete candidate for logical slot {}",
                block_hash.short(),
                status.slot()
            )));
        }
        account_chains
            .entry(signed.block.slot.account)
            .or_default()
            .push((signed.block.slot, block_hash));
    }

    for (account, chain) in &mut account_chains {
        chain.sort_unstable_by_key(|(slot, hash)| (*slot, *hash));
        for pair in chain.windows(2) {
            let (older_slot, older) = pair[0];
            let (newer_slot, newer) = pair[1];
            if newer_slot <= older_slot || !blocks.descends_from(newer, older)? {
                return Err(RaiError::InvalidClosePackage(format!(
                    "resolved blocks for account {account} do not form one parent chain"
                )));
            }
        }
        if let Some((_, tip)) = chain.last() {
            if let Some(existing) = frontier.get(account).copied() {
                if existing != *tip && !blocks.descends_from(*tip, existing)? {
                    return Err(RaiError::InvalidClosePackage(format!(
                        "resolved frontier for account {account} does not extend the prior certified frontier"
                    )));
                }
            }
            frontier.insert(*account, *tip);
        }
    }

    Ok((statuses, frontier))
}
