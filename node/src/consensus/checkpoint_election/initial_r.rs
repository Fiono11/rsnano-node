//! Initial R evidence for empty-candidate recovery. Candidate admission quorums
//! witness each initial value; these messages never reuse FIRST signatures.
use super::*;
use rsnano_types::{Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

fn hash(domain: &[u8], value: &impl Serialize) -> BlockHash {
    Blake2HashBuilder::new()
        .update(domain)
        .update(serde_json::to_vec(value).expect("typed initial R fields"))
        .build()
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialRRequest {
    pub instance: CheckpointInstance,
    pub requester: PublicKey,
    pub candidate: BlockHash,
    pub signature: Signature,
}
impl InitialRRequest {
    fn signing_hash(&self) -> BlockHash {
        hash(
            b"RAI initial R request v1.3",
            &(self.instance, self.requester, self.candidate),
        )
    }
    pub fn digest(&self) -> BlockHash {
        hash(b"RAI initial R request object v1.3", self)
    }
    pub fn sign(
        context: &CheckpointContext,
        candidate: BlockHash,
        key: &PrivateKey,
        evidence: &CandidateEvidence,
    ) -> Result<Self, CheckpointError> {
        let mut request = Self {
            instance: context.instance(),
            requester: key.public_key(),
            candidate,
            signature: Signature::default(),
        };
        if context.committee.weight(&key.public_key()).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        evidence.admitted_value(candidate)?;
        request.signature = key.sign(request.signing_hash().as_bytes());
        Ok(request)
    }
    pub fn verify(
        &self,
        context: &CheckpointContext,
        evidence: &CandidateEvidence,
    ) -> Result<BlockHash, CheckpointError> {
        if self.instance != context.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        if context.committee.weight(&self.requester).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        self.requester
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .map_err(|_| CheckpointError::BadSignature)?;
        let value = evidence.admitted_value(self.candidate)?;
        evidence.validate_introduction(
            &self.instance,
            0,
            value,
            &Introduction::Proposal(self.candidate),
        )?;
        Ok(value)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialRResponse {
    pub instance: CheckpointInstance,
    pub request: BlockHash,
    pub responder: PublicKey,
    pub candidate: BlockHash,
    pub signature: Signature,
}
impl InitialRResponse {
    fn signing_hash(&self) -> BlockHash {
        hash(
            b"RAI initial R response v1.3",
            &(self.instance, self.request, self.responder, self.candidate),
        )
    }
    pub fn authenticate(&self, context: &CheckpointContext) -> Result<(), CheckpointError> {
        if self.instance != context.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        if context.committee.weight(&self.responder).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        self.responder
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .map_err(|_| CheckpointError::BadSignature)
    }
    pub(super) fn verify(
        &self,
        context: &CheckpointContext,
        request: &InitialRRequest,
        evidence: &CandidateEvidence,
    ) -> Result<BlockHash, CheckpointError> {
        if self.instance != context.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        if self.request != request.digest() {
            return Err(CheckpointError::InvalidEvidence);
        }
        if context.committee.weight(&self.responder).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        self.responder
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .map_err(|_| CheckpointError::BadSignature)?;
        let value = evidence.admitted_value(self.candidate)?;
        evidence.validate_introduction(
            &self.instance,
            0,
            value,
            &Introduction::Proposal(self.candidate),
        )?;
        Ok(value)
    }
}
pub struct InitialRResponder {
    instance: CheckpointInstance,
    maximum: Option<(BlockHash, BlockHash)>,
}
impl InitialRResponder {
    pub fn new(instance: CheckpointInstance) -> Self {
        Self {
            instance,
            maximum: None,
        }
    }
    pub fn receive(
        &mut self,
        request: &InitialRRequest,
        context: &CheckpointContext,
        evidence: &CandidateEvidence,
        key: &PrivateKey,
    ) -> Result<InitialRResponse, CheckpointError> {
        if self.instance != context.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        let value = request.verify(context, evidence)?;
        if context.committee.weight(&key.public_key()).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        self.maximum = Some(self.maximum.map_or((value, request.candidate), |old| {
            old.max((value, request.candidate))
        }));
        let mut response = InitialRResponse {
            instance: self.instance,
            request: request.digest(),
            responder: key.public_key(),
            candidate: self.maximum.unwrap().1,
            signature: Signature::default(),
        };
        response.signature = key.sign(response.signing_hash().as_bytes());
        Ok(response)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitialRCertificate {
    pub request: InitialRRequest,
    pub responses: Vec<InitialRResponse>,
}
impl InitialRCertificate {
    pub fn digest(&self) -> BlockHash {
        hash(b"RAI initial R certificate v1.3", self)
    }
    pub fn verify(
        &self,
        context: &CheckpointContext,
        evidence: &CandidateEvidence,
    ) -> Result<BlockHash, CheckpointError> {
        let requested = self.request.verify(context, evidence)?;
        if self.responses.len() as u64 != context.thresholds().q {
            return Err(CheckpointError::InvalidSize);
        }
        let mut signers = BTreeSet::new();
        let mut maximum = requested;
        for response in &self.responses {
            let value = response.verify(context, &self.request, evidence)?;
            if !signers.insert(response.responder) {
                return Err(CheckpointError::DuplicateSigner);
            }
            // Every correct responder incorporates the triggering R write.
            if value < requested {
                return Err(CheckpointError::InvalidEvidence);
            }
            maximum = maximum.max(value);
        }
        Ok(maximum)
    }
}
