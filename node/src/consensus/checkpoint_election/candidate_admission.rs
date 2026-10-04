//! Finite initial-input admission: at most one quorum-certified candidate per
//! proposer. Correct endorsers lock once per (instance, proposer, endorser).
use super::*;
use rsnano_types::{Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateEndorsement {
    pub instance: CheckpointInstance,
    pub proposer: PublicKey,
    pub candidate: BlockHash,
    pub endorser: PublicKey,
    pub signature: Signature,
}
impl CandidateEndorsement {
    fn signing_hash(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI candidate admission v1.3")
            .update(self.instance.digest().as_bytes())
            .update(self.proposer.as_bytes())
            .update(self.candidate.as_bytes())
            .update(self.endorser.as_bytes())
            .build()
    }
    pub fn authenticate(&self, context: &CheckpointContext) -> Result<(), CheckpointError> {
        if self.instance != context.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        if context.committee.weight(&self.endorser).is_zero()
            || context.committee.weight(&self.proposer).is_zero()
        {
            return Err(CheckpointError::NonMember);
        }
        self.endorser
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .map_err(|_| CheckpointError::BadSignature)
    }
}

/// Process-lifetime locks; no disk/restart guarantees. Keep this alongside the
/// FIRST journal for the entire session, not per retry or candidate arrival.
#[derive(Default)]
pub struct CandidateEndorsementJournal {
    records: BTreeMap<(CheckpointInstance, PublicKey, PublicKey), CandidateEndorsement>,
}
impl CandidateEndorsementJournal {
    pub fn endorse(
        &mut self,
        candidate: &ValidatedCheckpointCandidate,
        context: &CheckpointContext,
        key: &PrivateKey,
    ) -> Result<CandidateEndorsement, CheckpointError> {
        let candidate = candidate.candidate();
        candidate.authenticate(context)?;
        if context.committee.weight(&key.public_key()).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        let slot = (context.instance(), candidate.proposer, key.public_key());
        if let Some(existing) = self.records.get(&slot) {
            return Ok(existing.clone());
        }
        let mut record = CandidateEndorsement {
            instance: context.instance(),
            proposer: candidate.proposer,
            candidate: candidate.digest(),
            endorser: key.public_key(),
            signature: Signature::default(),
        };
        record.signature = key.sign(record.signing_hash().as_bytes());
        self.records.insert(slot, record.clone());
        Ok(record)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateAdmission {
    pub endorsements: Vec<CandidateEndorsement>,
}
impl CandidateAdmission {
    pub fn verify(
        &self,
        candidate: &CheckpointCandidate,
        context: &CheckpointContext,
    ) -> Result<(), CheckpointError> {
        candidate.authenticate(context)?;
        if self.endorsements.len() as u64 != context.thresholds().q {
            return Err(CheckpointError::InvalidSize);
        }
        let mut signers = BTreeSet::new();
        for endorsement in &self.endorsements {
            endorsement.authenticate(context)?;
            if endorsement.proposer != candidate.proposer
                || endorsement.candidate != candidate.digest()
            {
                return Err(CheckpointError::InvalidEvidence);
            }
            if !signers.insert(endorsement.endorser) {
                return Err(CheckpointError::DuplicateSigner);
            }
        }
        Ok(())
    }
}
