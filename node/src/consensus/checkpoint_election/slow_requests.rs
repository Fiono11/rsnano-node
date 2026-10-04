//! Content-addressed signed requests and bounded recursive slow ancestry checks.
use super::*;
use rsnano_types::{Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlowJustification {
    Initial(SlowProposal),
    Previous(BlockHash),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlowRequest {
    pub instance: BlockHash,
    pub rank: u64,
    pub value: SlowValue,
    pub signer: PublicKey,
    pub justification: SlowJustification,
    pub signature: Signature,
}

impl SlowRequest {
    pub fn phase(&self) -> SlowPhase {
        self.value.phase()
    }
    fn signing_hash(&self) -> BlockHash {
        // Explicitly domain the canonical serde encoding. This is a local proof
        // format; wire serialization/version negotiation remains a later step.
        let bytes = serde_json::to_vec(&(
            self.instance,
            self.rank,
            self.value,
            self.signer,
            &self.justification,
        ))
        .expect("serializing typed proof fields cannot fail");
        Blake2HashBuilder::new()
            .update(b"RAI checkpoint slow request v1.3")
            .update(bytes)
            .build()
    }
    pub fn digest(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI slow request object v1.3")
            .update(self.signing_hash().as_bytes())
            .update(self.signature.as_bytes())
            .build()
    }
    pub fn sign(
        context: &SlowContext,
        rank: u64,
        value: SlowValue,
        justification: SlowJustification,
        key: &PrivateKey,
    ) -> Result<Self, CheckpointError> {
        if !context.members().contains(&key.public_key()) {
            return Err(CheckpointError::NonMember);
        }
        if matches!(value, SlowValue::R { rank: r, .. } if r != rank) {
            return Err(CheckpointError::WrongRank);
        }
        let mut request = Self {
            instance: context.instance(),
            rank,
            value,
            signer: key.public_key(),
            justification,
            signature: Signature::default(),
        };
        request.signature = key.sign(request.signing_hash().as_bytes());
        Ok(request)
    }
    pub fn authenticate(&self, context: &SlowContext) -> Result<(), CheckpointError> {
        if self.instance != context.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        if !context.members().contains(&self.signer) {
            return Err(CheckpointError::NonMember);
        }
        if matches!(self.value, SlowValue::R { rank, .. } if rank != self.rank) {
            return Err(CheckpointError::WrongRank);
        }
        self.signer
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .map_err(|_| CheckpointError::BadSignature)
    }
}

impl SlowCertificate {
    pub fn digest(&self) -> BlockHash {
        let bytes = serde_json::to_vec(self).expect("serializing typed proof fields cannot fail");
        Blake2HashBuilder::new()
            .update(b"RAI slow certificate object v1.3")
            .update(bytes)
            .build()
    }
}

/// Transport limits and lifetime/eviction policy belong to the eventual service.
/// Object insertion alone never marks evidence valid.
#[derive(Default)]
pub struct SlowProofStore {
    requests: BTreeMap<BlockHash, SlowRequest>,
    certificates: BTreeMap<BlockHash, SlowCertificate>,
}
impl SlowProofStore {
    pub fn insert_request(&mut self, request: SlowRequest) -> BlockHash {
        let id = request.digest();
        self.requests.entry(id).or_insert(request);
        id
    }
    pub fn insert_certificate(&mut self, certificate: SlowCertificate) -> BlockHash {
        let id = certificate.digest();
        self.certificates.entry(id).or_insert(certificate);
        id
    }
    pub fn request(&self, id: BlockHash) -> Result<&SlowRequest, CheckpointError> {
        self.requests
            .get(&id)
            .ok_or(CheckpointError::MissingEvidence(id))
    }
    pub fn certificate(&self, id: BlockHash) -> Result<&SlowCertificate, CheckpointError> {
        self.certificates
            .get(&id)
            .ok_or(CheckpointError::MissingEvidence(id))
    }
    pub fn verifier<'a, E: CheckpointEvidence>(
        &'a self,
        slow: &'a SlowContext,
        fast: &'a CheckpointContext,
        application: &'a E,
        limits: VerificationLimits,
    ) -> SlowVerifier<'a, E> {
        SlowVerifier {
            store: self,
            slow,
            fast,
            application,
            limits,
            depth: Cell::new(0),
            visited: Cell::new(0),
            active: RefCell::new(BTreeSet::new()),
            checked_requests: RefCell::new(BTreeSet::new()),
        }
    }
}

/// Exhausting a local budget is retryable; it never means the proof is valid.
#[derive(Clone, Copy, Debug)]
pub struct VerificationLimits {
    pub max_depth: usize,
    pub max_objects: usize,
}
impl Default for VerificationLimits {
    fn default() -> Self {
        Self {
            max_depth: 128,
            max_objects: 4096,
        }
    }
}

pub struct SlowVerifier<'a, E> {
    store: &'a SlowProofStore,
    slow: &'a SlowContext,
    fast: &'a CheckpointContext,
    application: &'a E,
    limits: VerificationLimits,
    depth: Cell<usize>,
    visited: Cell<usize>,
    active: RefCell<BTreeSet<BlockHash>>,
    checked_requests: RefCell<BTreeSet<BlockHash>>,
}
impl<E: CheckpointEvidence> SlowVerifier<'_, E> {
    fn enter<T>(
        &self,
        id: BlockHash,
        action: impl FnOnce() -> Result<T, CheckpointError>,
    ) -> Result<T, CheckpointError> {
        if self.active.borrow().contains(&id) {
            return Err(CheckpointError::CyclicEvidence);
        }
        if self.depth.get() >= self.limits.max_depth
            || self.visited.get() >= self.limits.max_objects
        {
            return Err(CheckpointError::VerificationLimit);
        }
        self.depth.set(self.depth.get() + 1);
        self.visited.set(self.visited.get() + 1);
        self.active.borrow_mut().insert(id);
        let result = action();
        self.active.borrow_mut().remove(&id);
        self.depth.set(self.depth.get() - 1);
        result
    }
    pub fn verify_certificate(
        &self,
        id: BlockHash,
    ) -> Result<VerifiedSlowCertificate, CheckpointError> {
        self.check_certificate(id, self.slow.quorum())
    }
    fn check_certificate(
        &self,
        id: BlockHash,
        witnesses: u64,
    ) -> Result<VerifiedSlowCertificate, CheckpointError> {
        self.enter(id, || {
            self.store
                .certificate(id)?
                .verify_with_threshold(self.slow, self, witnesses)
        })
    }
    pub fn verify_request(&self, id: BlockHash) -> Result<(), CheckpointError> {
        if self.checked_requests.borrow().contains(&id) {
            return Ok(());
        }
        self.enter(id, || {
            let request = self.store.request(id)?;
            request.authenticate(self.slow)?;
            match &request.justification {
                SlowJustification::Initial(proposal) => {
                    let value = proposal.verify(self.slow, self.fast, self.application)?;
                    if request.rank != 0 || request.value != (SlowValue::R { rank: 0, value }) {
                        return Err(CheckpointError::WrongAncestry);
                    }
                }
                SlowJustification::Previous(previous) => {
                    // Requests need W complementing witnesses; a quorum response
                    // needs Q. Never promote a W-checked certificate to Q cache.
                    let proof = self.check_certificate(*previous, self.slow.witness_threshold())?;
                    let expected = match proof.next_action()? {
                        SlowAction::R { rank, value } => (rank, SlowValue::R { rank, value }),
                        SlowAction::A { rank, value } => (rank, SlowValue::A(value)),
                        SlowAction::B { rank, value } => (rank, SlowValue::B(value)),
                        SlowAction::Decide(_) => return Err(CheckpointError::WrongAncestry),
                    };
                    if (request.rank, request.value) != expected {
                        return Err(CheckpointError::WrongAncestry);
                    }
                }
            }
            Ok(())
        })?;
        self.checked_requests.borrow_mut().insert(id);
        Ok(())
    }
}
impl<E: CheckpointEvidence> SlowRequestEvidence for SlowVerifier<'_, E> {
    fn validate_request(
        &self,
        context: &SlowContext,
        phase: SlowPhase,
        rank: u64,
        request: BlockHash,
    ) -> Result<(), CheckpointError> {
        if context.instance() != self.slow.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        self.verify_request(request)?;
        let object = self.store.request(request)?;
        if object.rank != rank {
            return Err(CheckpointError::WrongRank);
        }
        if object.phase() != phase {
            return Err(CheckpointError::WrongPhase);
        }
        Ok(())
    }
    fn validate_origin(
        &self,
        context: &SlowContext,
        phase: SlowPhase,
        rank: u64,
        request: BlockHash,
        value: SlowValue,
    ) -> Result<(), CheckpointError> {
        self.validate_request(context, phase, rank, request)?;
        if self.store.request(request)?.value != value {
            return Err(CheckpointError::InvalidEvidence);
        }
        Ok(())
    }
}
