//! Authenticated application candidates. Signatures establish origin; only
//! successful RAI BuildState validation produces a validated candidate token.
use super::*;
use crate::consensus::election::{
    BlockIndex, EpochLedger, EpochValue, EpochValueError, ReportSource,
};
use rsnano_types::{Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointCandidate {
    pub instance: CheckpointInstance,
    pub proposer: PublicKey,
    pub value: EpochValue,
    pub signature: Signature,
}
impl CheckpointCandidate {
    fn signing_hash(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI checkpoint candidate v1.3")
            .update(self.instance.digest().as_bytes())
            .update(self.proposer.as_bytes())
            .update(self.value.hash().as_bytes())
            .build()
    }
    pub fn digest(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI checkpoint candidate object v1.3")
            .update(self.signing_hash().as_bytes())
            .update(self.signature.as_bytes())
            .build()
    }
    /// Authenticate a candidate object. CandidateAdmission subsequently enforces
    /// the finite proposal universe; signing alone does not admit a value.
    pub fn sign(
        context: &CheckpointContext,
        value: EpochValue,
        key: &PrivateKey,
    ) -> Result<Self, CheckpointError> {
        if context.committee.weight(&key.public_key()).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        if value.epoch != context.instance().epoch {
            return Err(CheckpointError::WrongInstance);
        }
        let mut candidate = Self {
            instance: context.instance(),
            proposer: key.public_key(),
            value,
            signature: Signature::default(),
        };
        candidate.signature = key.sign(candidate.signing_hash().as_bytes());
        Ok(candidate)
    }
    pub fn authenticate(&self, context: &CheckpointContext) -> Result<(), CheckpointError> {
        if self.instance != context.instance() || self.value.epoch != self.instance.epoch {
            return Err(CheckpointError::WrongInstance);
        }
        if context.committee.weight(&self.proposer).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        self.proposer
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .map_err(|_| CheckpointError::BadSignature)
    }
    pub fn validate(
        self,
        context: &CheckpointContext,
        previous: &EpochLedger,
        reports: &impl ReportSource,
        index: &dyn BlockIndex,
    ) -> Result<ValidatedCheckpointCandidate, CheckpointError> {
        self.authenticate(context)?;
        if previous.state_hash() != self.instance.predecessor {
            return Err(CheckpointError::WrongInstance);
        }
        if self.value.reports().len() as u64 != context.thresholds().p_recovery {
            return Err(CheckpointError::InvalidSize);
        }
        // ReportSource is the already-reconstructed/signed-report boundary.
        // Do not let a source substitute reporter identities or voting weights.
        for reference in self.value.reports() {
            let report = reports
                .report(reference)
                .ok_or(CheckpointError::MissingEvidence(self.digest()))?;
            if report.reporter != reference.reporter
                || context.committee.weight(&reference.reporter).is_zero()
                || report.weight != context.committee.weight(&reference.reporter)
                || report.certified.root() != reference.certified
                || report.residual.root() != reference.residual
            {
                return Err(CheckpointError::InvalidEvidence);
            }
        }
        let state = self
            .value
            .validate(
                previous,
                reports,
                index,
                context.committee.thresholds().report,
                context.committee.thresholds().many,
            )
            .map_err(|error| match error {
                EpochValueError::NotReconstructed { .. } => {
                    CheckpointError::MissingEvidence(self.digest())
                }
                _ => CheckpointError::InvalidValue,
            })?;
        Ok(ValidatedCheckpointCandidate {
            candidate: self,
            state,
        })
    }
}

pub struct ValidatedCheckpointCandidate {
    candidate: CheckpointCandidate,
    state: EpochLedger,
}
impl ValidatedCheckpointCandidate {
    pub fn candidate(&self) -> &CheckpointCandidate {
        &self.candidate
    }
    pub fn state(&self) -> &EpochLedger {
        &self.state
    }
}

/// Only RAI-validated, quorum-admitted tokens and independently verified initial
/// R certificates enter this cache. No snapshot-max shortcut is accepted.
pub struct CandidateEvidence {
    instance: CheckpointInstance,
    candidates: std::collections::BTreeMap<BlockHash, ValidatedCheckpointCandidate>,
    initial_r: std::collections::BTreeMap<BlockHash, (InitialRCertificate, BlockHash)>,
    admissions: std::collections::BTreeMap<BlockHash, CandidateAdmission>,
}
impl CandidateEvidence {
    pub fn new(instance: CheckpointInstance) -> Self {
        Self {
            instance,
            candidates: Default::default(),
            initial_r: Default::default(),
            admissions: Default::default(),
        }
    }
    pub fn insert(
        &mut self,
        candidate: ValidatedCheckpointCandidate,
        admission: &CandidateAdmission,
        context: &CheckpointContext,
    ) -> Result<BlockHash, CheckpointError> {
        if candidate.candidate.instance != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        admission.verify(&candidate.candidate, context)?;
        let id = candidate.candidate.digest();
        self.candidates.entry(id).or_insert(candidate);
        self.admissions
            .entry(id)
            .or_insert_with(|| admission.clone());
        Ok(id)
    }
    pub fn admission(&self, id: BlockHash) -> Option<&CandidateAdmission> {
        self.admissions.get(&id)
    }
    pub fn value(&self, hash: BlockHash) -> Option<&crate::consensus::election::EpochValue> {
        self.candidates
            .values()
            .map(|c| &c.candidate.value)
            .find(|value| value.hash() == hash)
    }
    pub fn admitted_value(&self, id: BlockHash) -> Result<BlockHash, CheckpointError> {
        self.candidates
            .get(&id)
            .map(|c| c.candidate.value.hash())
            .ok_or(CheckpointError::MissingEvidence(id))
    }
    pub fn insert_initial_r(
        &mut self,
        certificate: InitialRCertificate,
        context: &CheckpointContext,
    ) -> Result<BlockHash, CheckpointError> {
        if context.instance() != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        let maximum = certificate.verify(context, self)?;
        let id = certificate.digest();
        self.initial_r.entry(id).or_insert((certificate, maximum));
        Ok(id)
    }
    pub fn initial_r(&self, id: BlockHash) -> Option<&InitialRCertificate> {
        self.initial_r.get(&id).map(|(certificate, _)| certificate)
    }
    pub fn candidate(&self, id: BlockHash) -> Option<&ValidatedCheckpointCandidate> {
        self.candidates.get(&id)
    }
}
impl CheckpointEvidence for CandidateEvidence {
    fn validate_value(
        &self,
        instance: &CheckpointInstance,
        value: BlockHash,
    ) -> Result<(), CheckpointError> {
        if instance != &self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        if self
            .candidates
            .values()
            .any(|c| c.candidate.value.hash() == value)
        {
            Ok(())
        } else {
            Err(CheckpointError::MissingEvidence(value))
        }
    }
    fn validate_introduction(
        &self,
        instance: &CheckpointInstance,
        rank: u64,
        value: BlockHash,
        introduction: &Introduction,
    ) -> Result<(), CheckpointError> {
        if instance != &self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        if rank != 0 {
            return Err(CheckpointError::WrongRank);
        }
        let Introduction::Proposal(id) = introduction else {
            return Err(CheckpointError::InvalidEvidence);
        };
        let candidate = self
            .candidates
            .get(id)
            .ok_or(CheckpointError::MissingEvidence(*id))?;
        if candidate.candidate.value.hash() != value {
            return Err(CheckpointError::InvalidEvidence);
        }
        Ok(())
    }
    fn r_maximum(
        &self,
        instance: &CheckpointInstance,
        rank: u64,
        evidence: BlockHash,
    ) -> Result<BlockHash, CheckpointError> {
        if instance != &self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        if rank != 0 {
            return Err(CheckpointError::WrongRank);
        }
        self.initial_r
            .get(&evidence)
            .map(|(_, value)| *value)
            .ok_or(CheckpointError::MissingEvidence(evidence))
    }
}
