//! Signed slow-phase responses and independently checked quorum certificates.
//! SlowVerifier supplies bounded recursive request ancestry; node integration
//! and the initial application/fast-R evidence adapter are still separate.
use std::collections::{BTreeMap, BTreeSet};

use rsnano_types::{Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

use super::{BOutcome, BValue, CheckpointError, SlowContext, evaluate_a, evaluate_b};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlowPhase {
    R,
    A,
    B,
}

impl SlowPhase {
    pub(super) fn tag(self) -> u8 {
        match self {
            Self::R => 0,
            Self::A => 1,
            Self::B => 2,
        }
    }
}

/// R compares rank before value. A and B never carry an implicit R rank.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SlowValue {
    R { rank: u64, value: BlockHash },
    A(BlockHash),
    B(BValue),
}
impl SlowValue {
    pub(super) fn phase(self) -> SlowPhase {
        match self {
            Self::R { .. } => SlowPhase::R,
            Self::A(_) => SlowPhase::A,
            Self::B(_) => SlowPhase::B,
        }
    }
    pub(super) fn origin_rank(self, response_rank: u64) -> u64 {
        match self {
            Self::R { rank, .. } => rank,
            _ => response_rank,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlowEntry {
    pub value: SlowValue,
    pub origin: BlockHash,
}

/// A historical snapshot remains valid after its signer updates local state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlowResponse {
    pub instance: BlockHash,
    pub rank: u64,
    pub phase: SlowPhase,
    pub request: BlockHash,
    pub signer: PublicKey,
    pub entries: Vec<SlowEntry>,
    pub signature: Signature,
}

impl SlowResponse {
    pub(super) fn signing_hash(&self) -> BlockHash {
        let mut hash = Blake2HashBuilder::new()
            .update(b"RAI checkpoint slow response v1.3")
            .update(self.instance.as_bytes())
            .update(self.rank.to_le_bytes())
            .update([self.phase.tag()])
            .update(self.request.as_bytes())
            .update(self.signer.as_bytes())
            .update((self.entries.len() as u64).to_le_bytes());
        for entry in &self.entries {
            hash = hash.update([entry.value.phase().tag()]);
            hash = match entry.value {
                SlowValue::R { rank, value } => {
                    hash.update(rank.to_le_bytes()).update(value.as_bytes())
                }
                SlowValue::A(value) => hash.update(value.as_bytes()),
                SlowValue::B(value) => hash
                    .update([u8::from(value.flag)])
                    .update(value.value.as_bytes()),
            };
            hash = hash.update(entry.origin.as_bytes());
        }
        hash.build()
    }

    fn check_shape(&self) -> Result<(), CheckpointError> {
        if self.entries.is_empty()
            || self.entries.len() > 2
            || (self.phase == SlowPhase::R && self.entries.len() != 1)
        {
            return Err(CheckpointError::InvalidSize);
        }
        if self.entries.iter().any(|entry| {
            entry.value.phase() != self.phase
                || matches!(entry.value, SlowValue::R { rank, .. } if rank < self.rank)
        }) || self
            .entries
            .windows(2)
            .any(|pair| pair[0].value >= pair[1].value)
        {
            return Err(CheckpointError::InvalidEvidence);
        }
        Ok(())
    }

    /// Component-level signing API. The responder service must validate the
    /// triggering request and every delivered input before choosing this state.
    pub fn sign(
        context: &SlowContext,
        rank: u64,
        phase: SlowPhase,
        request: BlockHash,
        entries: Vec<SlowEntry>,
        key: &PrivateKey,
    ) -> Result<Self, CheckpointError> {
        if !context.members().contains(&key.public_key()) {
            return Err(CheckpointError::NonMember);
        }
        let mut response = Self {
            instance: context.instance(),
            rank,
            phase,
            request,
            signer: key.public_key(),
            entries,
            signature: Signature::default(),
        };
        response.check_shape()?;
        response.signature = key.sign(response.signing_hash().as_bytes());
        Ok(response)
    }

    pub fn authenticate(&self, context: &SlowContext) -> Result<(), CheckpointError> {
        if self.instance != context.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        if !context.members().contains(&self.signer) {
            return Err(CheckpointError::NonMember);
        }
        self.check_shape()?;
        self.signer
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .map_err(|_| CheckpointError::BadSignature)
    }
}

/// Implementations must validate signed requests and complete finite ancestry:
/// initial R -> RC-backed proposal; later R -> preceding slow B;
/// A -> selected slow R rank/value; B -> recomputed slow A flag/value.
/// W-admission and missing-dependency handling belong to that request verifier.
/// SlowVerifier implements this boundary using the content-addressed proof store.
pub trait SlowRequestEvidence {
    fn validate_request(
        &self,
        context: &SlowContext,
        phase: SlowPhase,
        rank: u64,
        request: BlockHash,
    ) -> Result<(), CheckpointError>;
    fn validate_origin(
        &self,
        context: &SlowContext,
        phase: SlowPhase,
        rank: u64,
        request: BlockHash,
        value: SlowValue,
    ) -> Result<(), CheckpointError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginWitnesses {
    pub origin: BlockHash,
    pub responses: Vec<SlowResponse>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlowCertificate {
    pub instance: BlockHash,
    pub rank: u64,
    pub phase: SlowPhase,
    pub request: BlockHash,
    pub responses: Vec<SlowResponse>,
    /// Optional signed local R maximum, in addition to the collected quorum.
    pub retained_r: Option<SlowResponse>,
    pub witnesses: Vec<OriginWitnesses>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlowResult {
    R { rank: u64, value: BlockHash },
    A(BValue),
    B(BOutcome),
}

/// Only certificate verification constructs this token. It is not a decision
/// proof for the node service until its application adapter and wire integration exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSlowCertificate {
    instance: BlockHash,
    rank: u64,
    request: BlockHash,
    result: SlowResult,
}
impl VerifiedSlowCertificate {
    pub fn instance(&self) -> BlockHash {
        self.instance
    }
    pub fn rank(&self) -> u64 {
        self.rank
    }
    pub fn request(&self) -> BlockHash {
        self.request
    }
    pub fn result(&self) -> SlowResult {
        self.result
    }
}

fn authenticate_quorum(
    context: &SlowContext,
    phase: SlowPhase,
    rank: u64,
    request: BlockHash,
    responses: &[SlowResponse],
    threshold: u64,
) -> Result<(), CheckpointError> {
    if responses.len() as u64 != threshold {
        return Err(CheckpointError::InvalidSize);
    }
    let mut signers = BTreeSet::new();
    for response in responses {
        response.authenticate(context)?;
        if response.rank != rank {
            return Err(CheckpointError::WrongRank);
        }
        if response.phase != phase || response.request != request {
            return Err(CheckpointError::InvalidEvidence);
        }
        if !signers.insert(response.signer) {
            return Err(CheckpointError::DuplicateSigner);
        }
    }
    Ok(())
}

impl SlowCertificate {
    pub fn verify(
        &self,
        context: &SlowContext,
        evidence: &impl SlowRequestEvidence,
    ) -> Result<VerifiedSlowCertificate, CheckpointError> {
        self.verify_with_threshold(context, evidence, context.quorum())
    }

    pub(super) fn verify_with_threshold(
        &self,
        context: &SlowContext,
        evidence: &impl SlowRequestEvidence,
        threshold: u64,
    ) -> Result<VerifiedSlowCertificate, CheckpointError> {
        if self.instance != context.instance() {
            return Err(CheckpointError::WrongInstance);
        }
        if self.retained_r.is_some() && self.phase != SlowPhase::R {
            return Err(CheckpointError::InvalidEvidence);
        }
        if let Some(retained) = &self.retained_r {
            authenticate_quorum(
                context,
                self.phase,
                self.rank,
                self.request,
                std::slice::from_ref(retained),
                1,
            )?;
        }
        authenticate_quorum(
            context,
            self.phase,
            self.rank,
            self.request,
            &self.responses,
            context.quorum(),
        )?;
        // At most two originating requests per quorum response. Reject excess
        // attachments before invoking a potentially expensive ancestry verifier.
        if self.witnesses.len()
            > (self.responses.len() + usize::from(self.retained_r.is_some())) * 2
        {
            return Err(CheckpointError::InvalidSize);
        }
        evidence.validate_request(context, self.phase, self.rank, self.request)?;
        let mut witnesses = BTreeMap::new();
        for witness in &self.witnesses {
            if witnesses
                .insert(witness.origin, &witness.responses)
                .is_some()
            {
                return Err(CheckpointError::InvalidEvidence);
            }
        }
        let mut used = BTreeSet::new();
        for response in self.responses.iter().chain(self.retained_r.iter()) {
            for entry in &response.entries {
                let rank = entry.value.origin_rank(self.rank);
                evidence.validate_origin(context, self.phase, rank, entry.origin, entry.value)?;
                if used.insert(entry.origin) {
                    let answers = witnesses
                        .get(&entry.origin)
                        .ok_or(CheckpointError::MissingEvidence(entry.origin))?;
                    // These signatures witness receipt of the origin broadcast.
                    // Their values need not equal that broadcast's input. Do not
                    // recursively require eligibility of witness responses: that
                    // would turn complementing evidence into a circular quorum.
                    if answers.len() as u64 > context.quorum() || (answers.len() as u64) < threshold
                    {
                        return Err(CheckpointError::InvalidSize);
                    }
                    authenticate_quorum(
                        context,
                        self.phase,
                        rank,
                        entry.origin,
                        answers,
                        answers.len() as u64,
                    )?;
                }
            }
        }
        if used.len() != witnesses.len() {
            return Err(CheckpointError::InvalidEvidence);
        }
        let result = match self.phase {
            SlowPhase::R => {
                let (rank, value) = self
                    .responses
                    .iter()
                    .chain(self.retained_r.iter())
                    .map(|r| match r.entries[0].value {
                        SlowValue::R { rank, value } => (rank, value),
                        _ => unreachable!(),
                    })
                    .max()
                    .unwrap();
                SlowResult::R { rank, value }
            }
            SlowPhase::A => {
                let responses: Vec<_> = self
                    .responses
                    .iter()
                    .map(|r| {
                        (
                            r.signer,
                            r.entries
                                .iter()
                                .map(|e| match e.value {
                                    SlowValue::A(v) => v,
                                    _ => unreachable!(),
                                })
                                .collect(),
                        )
                    })
                    .collect();
                SlowResult::A(evaluate_a(context.quorum(), &responses)?)
            }
            SlowPhase::B => {
                let responses: Vec<_> = self
                    .responses
                    .iter()
                    .map(|r| {
                        (
                            r.signer,
                            r.entries
                                .iter()
                                .map(|e| match e.value {
                                    SlowValue::B(v) => v,
                                    _ => unreachable!(),
                                })
                                .collect(),
                        )
                    })
                    .collect();
                SlowResult::B(evaluate_b(context.quorum(), &responses)?)
            }
        };
        Ok(VerifiedSlowCertificate {
            instance: self.instance,
            rank: self.rank,
            request: self.request,
            result,
        })
    }
}

/// The next protocol action follows the verified phase result, including a
/// retained R maximum when supplied with its signed origin evidence. In particular,
/// an RC-backed initial preference cannot override a slow R selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlowAction {
    R { rank: u64, value: BlockHash },
    A { rank: u64, value: BlockHash },
    B { rank: u64, value: BValue },
    Decide(BlockHash),
}
impl VerifiedSlowCertificate {
    pub fn next_action(&self) -> Result<SlowAction, CheckpointError> {
        Ok(match self.result {
            SlowResult::R { rank, value } => SlowAction::A { rank, value },
            SlowResult::A(value) => SlowAction::B {
                rank: self.rank,
                value,
            },
            SlowResult::B(BOutcome::Adopt(value)) => SlowAction::R {
                rank: self.rank.checked_add(1).ok_or(CheckpointError::WrongRank)?,
                value,
            },
            SlowResult::B(BOutcome::Commit(value)) => SlowAction::Decide(value),
        })
    }
}
