use std::collections::{BTreeMap, BTreeSet};

use rsnano_types::BlockHash;
use serde::{Deserialize, Serialize};

use super::{
    CheckpointContext, CheckpointError, CheckpointEvidence, CheckpointInstance, FirstVote,
    FirstVotePool,
};

/// Recomputes Cand(S) after the caller has validated distinct identities.
/// None means fewer than Q or more than N records, not an empty candidate set.
fn candidates(
    context: &CheckpointContext,
    values: impl IntoIterator<Item = BlockHash>,
) -> Option<BTreeSet<BlockHash>> {
    let mut counts = BTreeMap::<BlockHash, u64>::new();
    let mut m = 0;
    for value in values {
        *counts.entry(value).or_default() += 1;
        m += 1;
    }
    let support = context.thresholds().recovery_support(m)?;
    Some(
        counts
            .into_iter()
            .filter_map(|(v, count)| (count >= support).then_some(v))
            .collect(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryResolution {
    Singleton,
    /// Digest of ordinary Q-response R evidence, fetched and verified separately.
    Empty {
        r_evidence: BlockHash,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCertificate {
    pub instance: CheckpointInstance,
    pub rank: u64,
    pub value: BlockHash,
    pub snapshot: Vec<FirstVote>,
    pub resolution: RecoveryResolution,
}

impl RecoveryCertificate {
    pub fn verify(
        &self,
        context: &CheckpointContext,
        rank: u64,
        evidence: &impl CheckpointEvidence,
    ) -> Result<BlockHash, CheckpointError> {
        if self.instance != context.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        if rank != 0 || self.rank != rank {
            return Err(CheckpointError::WrongRank);
        }
        if context
            .thresholds()
            .recovery_support(self.snapshot.len() as u64)
            .is_none()
        {
            return Err(CheckpointError::InvalidSize);
        }
        let mut signers = BTreeSet::new();
        for vote in &self.snapshot {
            if !signers.insert(vote.signer) {
                return Err(CheckpointError::DuplicateSigner);
            }
            context.verify_first_vote(rank, vote, evidence)?;
        }
        let possible = candidates(context, self.snapshot.iter().map(|vote| vote.value))
            .ok_or(CheckpointError::InvalidSize)?;
        match &self.resolution {
            RecoveryResolution::Singleton
                if possible.len() == 1 && possible.contains(&self.value) => {}
            RecoveryResolution::Empty { r_evidence } if possible.is_empty() => {
                let maximum = evidence.r_maximum(&self.instance, rank, *r_evidence)?;
                if maximum != self.value {
                    return Err(CheckpointError::InvalidRecovery);
                }
                evidence.validate_value(&self.instance, maximum)?;
            }
            _ => return Err(CheckpointError::InvalidRecovery),
        }
        Ok(self.value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryProgress {
    NeedVotes,
    Ambiguous(BTreeSet<BlockHash>),
    NeedREvidence,
    Resolved(RecoveryCertificate),
}

impl FirstVotePool {
    /// A resolved certificate imported from a peer can be used directly after
    /// RecoveryCertificate::verify, even while this pool remains ambiguous.
    /// The pool never overwrites an immutable first vote with that preference.
    pub fn resolve(
        &self,
        r_evidence: Option<BlockHash>,
        evidence: &impl CheckpointEvidence,
    ) -> Result<RecoveryProgress, CheckpointError> {
        let snapshot = self.snapshot();
        let Some(possible) = candidates(self.context(), snapshot.iter().map(|vote| vote.value))
        else {
            return Ok(RecoveryProgress::NeedVotes);
        };
        if possible.len() > 1 {
            return Ok(RecoveryProgress::Ambiguous(possible));
        }
        let (value, resolution) = if let Some(value) = possible.first() {
            (*value, RecoveryResolution::Singleton)
        } else {
            let Some(r_evidence) = r_evidence else {
                return Ok(RecoveryProgress::NeedREvidence);
            };
            let value = evidence.r_maximum(&self.context().instance(), self.rank(), r_evidence)?;
            (value, RecoveryResolution::Empty { r_evidence })
        };
        let certificate = RecoveryCertificate {
            instance: self.context().instance(),
            rank: self.rank(),
            value,
            snapshot,
            resolution,
        };
        certificate.verify(self.context(), self.rank(), evidence)?;
        Ok(RecoveryProgress::Resolved(certificate))
    }
}

#[cfg(test)]
pub(super) fn candidate_values(
    context: &CheckpointContext,
    values: &[BlockHash],
) -> Option<BTreeSet<BlockHash>> {
    candidates(context, values.iter().copied())
}
