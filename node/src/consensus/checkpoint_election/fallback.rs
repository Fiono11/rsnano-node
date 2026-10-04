//! External-validity boundary for a fresh ordinary Archipelago instance.
//! This is not the slow R/A/B driver or its recursive certificate verifier.
use std::collections::BTreeSet;

use rsnano_types::{Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

use super::{CheckpointContext, CheckpointError, CheckpointEvidence, RecoveryCertificate};

/// Membership is the first 3f+1 fast members in ascending public-key order.
/// The digest binds that selection and the enclosing v1.3 checkpoint instance.
pub struct SlowContext {
    instance: BlockHash,
    members: BTreeSet<PublicKey>,
    f: u64,
}

impl SlowContext {
    pub fn new(fast: &CheckpointContext) -> Self {
        let f = fast.thresholds().w - 1;
        let members: BTreeSet<_> = fast.committee.weights().keys().copied().collect();
        let members: BTreeSet<_> = members.into_iter().take((3 * f + 1) as usize).collect();
        let mut digest = Blake2HashBuilder::new()
            .update(b"RAI checkpoint slow instance v1.3")
            .update(fast.instance().digest().as_bytes())
            .update(f.to_le_bytes());
        for member in &members {
            digest = digest.update(member.as_bytes());
        }
        Self {
            instance: digest.build(),
            members,
            f,
        }
    }

    pub fn instance(&self) -> BlockHash {
        self.instance
    }
    pub fn members(&self) -> &BTreeSet<PublicKey> {
        &self.members
    }
    pub fn quorum(&self) -> u64 {
        2 * self.f + 1
    }
    pub fn witness_threshold(&self) -> u64 {
        self.f + 1
    }

    /// Admission needs a valid RC0, never a timeout or evidence that fast failed.
    /// Every slow initial request must carry this proposal's external validity.
    pub fn proposal(
        &self,
        fast: &CheckpointContext,
        recovery: RecoveryCertificate,
        key: &PrivateKey,
        evidence: &impl CheckpointEvidence,
    ) -> Result<SlowProposal, CheckpointError> {
        self.validate_recovery(fast, &recovery, evidence)?;
        if !self.members.contains(&key.public_key()) {
            return Err(CheckpointError::NonMember);
        }
        let mut proposal = SlowProposal {
            instance: self.instance,
            signer: key.public_key(),
            recovery,
            signature: Signature::default(),
        };
        proposal.signature = key.sign(proposal.signing_hash().as_bytes());
        Ok(proposal)
    }

    fn validate_recovery(
        &self,
        fast: &CheckpointContext,
        recovery: &RecoveryCertificate,
        evidence: &impl CheckpointEvidence,
    ) -> Result<BlockHash, CheckpointError> {
        if Self::new(fast).instance != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        recovery.verify(fast, 0, evidence)
    }
}

/// A signed initial input to slow R, not an A request and not a slow decision.
/// Alternative witnesses for the same value are interchangeable: the signature
/// binds the value; the attached recovery proof is independently revalidated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlowProposal {
    pub instance: BlockHash,
    pub signer: PublicKey,
    pub recovery: RecoveryCertificate,
    pub signature: Signature,
}

impl SlowProposal {
    fn signing_hash(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI checkpoint slow initial proposal v1.3")
            .update(self.instance.as_bytes())
            .update(self.signer.as_bytes())
            .update(self.recovery.value.as_bytes())
            .build()
    }

    pub fn verify(
        &self,
        slow: &SlowContext,
        fast: &CheckpointContext,
        evidence: &impl CheckpointEvidence,
    ) -> Result<BlockHash, CheckpointError> {
        if self.instance != slow.instance {
            return Err(CheckpointError::WrongInstance);
        }
        if !slow.members.contains(&self.signer) {
            return Err(CheckpointError::NonMember);
        }
        self.signer
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .map_err(|_| CheckpointError::BadSignature)?;
        slow.validate_recovery(fast, &self.recovery, evidence)
    }
}
