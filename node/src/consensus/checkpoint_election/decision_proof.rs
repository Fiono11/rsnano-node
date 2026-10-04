//! Portable consensus decision proofs. Application candidates must still be
//! reconstructed and admitted by the receiver's CheckpointEvidence implementation.
use super::*;
use rsnano_types::BlockHash;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlowDecisionProof {
    pub root: BlockHash,
    pub requests: Vec<SlowRequest>,
    pub certificates: Vec<SlowCertificate>,
}
impl SlowDecisionProof {
    pub fn collect(
        store: &SlowProofStore,
        root: BlockHash,
        max_objects: usize,
    ) -> Result<Self, CheckpointError> {
        let mut requests = BTreeMap::new();
        let mut certificates = BTreeMap::new();
        let mut pending = vec![(true, root)];
        while let Some((is_certificate, id)) = pending.pop() {
            if if is_certificate {
                certificates.contains_key(&id)
            } else {
                requests.contains_key(&id)
            } {
                continue;
            }
            if requests.len() + certificates.len() >= max_objects {
                return Err(CheckpointError::VerificationLimit);
            }
            if is_certificate {
                let certificate = store.certificate(id)?.clone();
                pending.push((false, certificate.request));
                for response in certificate
                    .responses
                    .iter()
                    .chain(certificate.retained_r.iter())
                {
                    for entry in &response.entries {
                        pending.push((false, entry.origin));
                    }
                }
                // Witness response origins are not recursively eligible objects;
                // the verifier uses these signatures only as receipt witnesses.
                certificates.insert(id, certificate);
            } else {
                let request = store.request(id)?.clone();
                if let SlowJustification::Previous(previous) = &request.justification {
                    pending.push((true, *previous));
                }
                requests.insert(id, request);
            }
        }
        Ok(Self {
            root,
            requests: requests.into_values().collect(),
            certificates: certificates.into_values().collect(),
        })
    }
    pub fn verify(
        &self,
        context: &CheckpointContext,
        evidence: &impl CheckpointEvidence,
        limits: VerificationLimits,
    ) -> Result<BlockHash, CheckpointError> {
        if self
            .requests
            .len()
            .checked_add(self.certificates.len())
            .is_none_or(|len| len > limits.max_objects)
        {
            return Err(CheckpointError::VerificationLimit);
        }
        let mut store = SlowProofStore::default();
        let mut seen = BTreeSet::new();
        for request in &self.requests {
            if !seen.insert(request.digest()) {
                return Err(CheckpointError::InvalidEvidence);
            }
            store.insert_request(request.clone());
        }
        for certificate in &self.certificates {
            if !seen.insert(certificate.digest()) {
                return Err(CheckpointError::InvalidEvidence);
            }
            store.insert_certificate(certificate.clone());
        }
        let slow = SlowContext::new(context);
        let proof = store
            .verifier(&slow, context, evidence, limits)
            .verify_certificate(self.root)?;
        // Reject unrelated attachments rather than accepting an unverified tail.
        let closure = Self::collect(&store, self.root, limits.max_objects)?;
        if closure.requests.len() != self.requests.len()
            || closure.certificates.len() != self.certificates.len()
        {
            return Err(CheckpointError::InvalidEvidence);
        }
        match proof.result() {
            SlowResult::B(BOutcome::Commit(value)) => Ok(value),
            _ => Err(CheckpointError::InvalidEvidence),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointDecisionProof {
    Fast(FastCertificate),
    Slow(SlowDecisionProof),
}
impl CheckpointDecisionProof {
    const MAGIC: &'static [u8; 8] = b"RAIDECI\x01";
    pub fn verify(
        &self,
        context: &CheckpointContext,
        evidence: &impl CheckpointEvidence,
        limits: VerificationLimits,
    ) -> Result<BlockHash, CheckpointError> {
        match self {
            Self::Fast(certificate) => certificate.verify(context, 0, evidence),
            Self::Slow(proof) => proof.verify(context, evidence, limits),
        }
    }
    pub fn encode(&self, max_bytes: usize) -> Result<Vec<u8>, CheckpointError> {
        let budget = max_bytes
            .checked_sub(Self::MAGIC.len())
            .ok_or(CheckpointError::InvalidSize)?;
        let len = super::slow_io::encoded_len(self, budget)?;
        let mut bytes = Vec::with_capacity(Self::MAGIC.len() + len);
        bytes.extend_from_slice(Self::MAGIC);
        serde_json::to_writer(&mut bytes, self).map_err(|_| CheckpointError::InvalidEvidence)?;
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], max_bytes: usize) -> Result<Self, CheckpointError> {
        if bytes.len() > max_bytes || bytes.len() <= Self::MAGIC.len() {
            return Err(CheckpointError::InvalidSize);
        }
        if !bytes.starts_with(Self::MAGIC) {
            return Err(CheckpointError::InvalidEvidence);
        }
        serde_json::from_slice(&bytes[Self::MAGIC.len()..])
            .map_err(|_| CheckpointError::InvalidEvidence)
    }
}
