use crate::MessageVariant;
use bitvec::prelude::BitArray;
use num_traits::FromPrimitive;
use rsnano_types::{
    Account, Blake2Hash, Blake2HashBuilder, Block, BlockHash, DeserializationError, PrivateKey,
    PublicKey, Signature, SnapshotNumber, read_u8, read_u32_be, read_u64_be,
};
use std::{
    collections::BTreeMap,
    io::{Error, ErrorKind, Read, Write},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RaiSlot {
    pub account: Account,
    pub index: u64,
}

impl RaiSlot {
    pub const SERIALIZED_SIZE: usize = Account::SERIALIZED_SIZE + 8;

    pub const fn new(account: Account, index: u64) -> Self {
        Self { account, index }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.account.serialize(writer)?;
        writer.write_all(&self.index.to_be_bytes())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            account: Account::deserialize(reader)?,
            index: read_u64_be(reader)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RaiElectionId {
    pub slot: RaiSlot,
    pub epoch: SnapshotNumber,
    pub context: RaiEpochContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RaiEpochContext {
    pub close_hash: BlockHash,
    pub previous_close_hash: BlockHash,
}

impl RaiEpochContext {
    pub const SERIALIZED_SIZE: usize = BlockHash::SERIALIZED_SIZE * 2;

    pub const fn new(close_hash: BlockHash, previous_close_hash: BlockHash) -> Self {
        Self {
            close_hash,
            previous_close_hash,
        }
    }

    pub const fn bootstrap() -> Self {
        Self::new(BlockHash::ZERO, BlockHash::ZERO)
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.close_hash.serialize(writer)?;
        self.previous_close_hash.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            close_hash: BlockHash::deserialize(reader)?,
            previous_close_hash: BlockHash::deserialize(reader)?,
        })
    }
}

impl RaiElectionId {
    pub const SERIALIZED_SIZE: usize =
        RaiSlot::SERIALIZED_SIZE + 4 + RaiEpochContext::SERIALIZED_SIZE;

    pub const fn new(slot: RaiSlot, epoch: SnapshotNumber) -> Self {
        Self::with_context(slot, epoch, RaiEpochContext::bootstrap())
    }

    pub const fn with_context(
        slot: RaiSlot,
        epoch: SnapshotNumber,
        context: RaiEpochContext,
    ) -> Self {
        Self {
            slot,
            epoch,
            context,
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.slot.serialize(writer)?;
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.context.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            slot: RaiSlot::deserialize(reader)?,
            epoch: read_u32_be(reader)?,
            context: RaiEpochContext::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RaiVoteTarget {
    Proposal(BlockHash),
    Timeout,
}

impl RaiVoteTarget {
    const PROPOSAL_TAG: u8 = 0;
    const TIMEOUT_TAG: u8 = 1;

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match self {
            Self::Proposal(hash) => {
                writer.write_all(&[Self::PROPOSAL_TAG])?;
                hash.serialize(writer)
            }
            Self::Timeout => writer.write_all(&[Self::TIMEOUT_TAG]),
        }
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        match read_u8(reader)? {
            Self::PROPOSAL_TAG => Ok(Self::Proposal(BlockHash::deserialize(reader)?)),
            Self::TIMEOUT_TAG => Ok(Self::Timeout),
            _ => Err(DeserializationError::InvalidData),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RaiTerminalOutcome {
    Proposal(BlockHash),
    Notarized(BlockHash),
    Timeout,
}

impl RaiTerminalOutcome {
    const PROPOSAL_TAG: u8 = 0;
    const TIMEOUT_TAG: u8 = 1;
    const NOTARIZED_TAG: u8 = 2;

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match self {
            Self::Proposal(hash) => write_tagged_hash(writer, Self::PROPOSAL_TAG, hash),
            Self::Notarized(hash) => write_tagged_hash(writer, Self::NOTARIZED_TAG, hash),
            Self::Timeout => writer.write_all(&[Self::TIMEOUT_TAG]),
        }
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        match read_u8(reader)? {
            Self::PROPOSAL_TAG => Ok(Self::Proposal(BlockHash::deserialize(reader)?)),
            Self::NOTARIZED_TAG => Ok(Self::Notarized(BlockHash::deserialize(reader)?)),
            Self::TIMEOUT_TAG => Ok(Self::Timeout),
            _ => Err(DeserializationError::InvalidData),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RaiTerminalRecord {
    pub election: RaiElectionId,
    pub outcome: RaiTerminalOutcome,
}

impl RaiTerminalRecord {
    pub const fn new(election: RaiElectionId, outcome: RaiTerminalOutcome) -> Self {
        Self { election, outcome }
    }

    pub fn hash(&self) -> Blake2Hash {
        object_hash(b"rai:terminal_record", |writer| self.serialize(writer))
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.outcome.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            election: RaiElectionId::deserialize(reader)?,
            outcome: RaiTerminalOutcome::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiProposal {
    pub election: RaiElectionId,
    pub block: Block,
}

impl RaiProposal {
    pub fn new(election: RaiElectionId, block: Block) -> Self {
        Self { election, block }
    }

    pub fn proposal_hash(&self) -> BlockHash {
        self.block.hash()
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.block.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            election: RaiElectionId::deserialize(reader)?,
            block: Block::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RaiVotePhase {
    First,
    Cert,
    Final,
    Timeout,
}

impl RaiVotePhase {
    const FIRST_TAG: u8 = 0;
    const CERT_TAG: u8 = 1;
    const FINAL_TAG: u8 = 2;
    const TIMEOUT_TAG: u8 = 3;

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        let tag = match self {
            Self::First => Self::FIRST_TAG,
            Self::Cert => Self::CERT_TAG,
            Self::Final => Self::FINAL_TAG,
            Self::Timeout => Self::TIMEOUT_TAG,
        };
        writer.write_all(&[tag])
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        match read_u8(reader)? {
            Self::FIRST_TAG => Ok(Self::First),
            Self::CERT_TAG => Ok(Self::Cert),
            Self::FINAL_TAG => Ok(Self::Final),
            Self::TIMEOUT_TAG => Ok(Self::Timeout),
            _ => Err(DeserializationError::InvalidData),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiVote {
    pub phase: RaiVotePhase,
    pub election: RaiElectionId,
    pub target: RaiVoteTarget,
    pub voter: PublicKey,
    pub signature: Signature,
}

impl RaiVote {
    pub fn new(
        phase: RaiVotePhase,
        election: RaiElectionId,
        target: RaiVoteTarget,
        private_key: &PrivateKey,
    ) -> Self {
        debug_assert!(
            matches!(
                (phase, target),
                (RaiVotePhase::Timeout, RaiVoteTarget::Timeout)
                    | (
                        RaiVotePhase::First | RaiVotePhase::Cert | RaiVotePhase::Final,
                        RaiVoteTarget::Proposal(_)
                    )
            ),
            "Rai vote phase and target are inconsistent"
        );

        let mut vote = Self {
            phase,
            election,
            target,
            voter: private_key.public_key(),
            signature: Signature::default(),
        };
        vote.signature = private_key.sign(vote.hash().as_bytes());
        vote
    }

    pub fn proposal(
        phase: RaiVotePhase,
        election: RaiElectionId,
        proposal_hash: BlockHash,
        private_key: &PrivateKey,
    ) -> Self {
        Self::new(
            phase,
            election,
            RaiVoteTarget::Proposal(proposal_hash),
            private_key,
        )
    }

    pub fn timeout(election: RaiElectionId, private_key: &PrivateKey) -> Self {
        Self::new(
            RaiVotePhase::Timeout,
            election,
            RaiVoteTarget::Timeout,
            private_key,
        )
    }

    pub fn hash(&self) -> Blake2Hash {
        signed_hash(b"rai:vote", self.voter, |writer| {
            self.phase.serialize(writer)?;
            self.election.serialize(writer)?;
            self.target.serialize(writer)
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.phase.serialize(writer)?;
        self.election.serialize(writer)?;
        self.target.serialize(writer)?;
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            phase: RaiVotePhase::deserialize(reader)?,
            election: RaiElectionId::deserialize(reader)?,
            target: RaiVoteTarget::deserialize(reader)?,
            voter: PublicKey::deserialize(reader)?,
            signature: Signature::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiVoteSet {
    pub committee_epoch: SnapshotNumber,
    pub votes: Vec<RaiVote>,
}

impl RaiVoteSet {
    pub fn new(committee_epoch: SnapshotNumber, votes: Vec<RaiVote>) -> Self {
        Self {
            committee_epoch,
            votes: canonical_votes(votes),
        }
    }

    pub fn hash(&self) -> Blake2Hash {
        object_hash(b"rai:vote_set", |writer| self.serialize(writer))
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&self.committee_epoch.to_be_bytes())?;
        write_count(writer, self.votes.len())?;
        for vote in &self.votes {
            vote.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let committee_epoch = read_u32_be(reader)?;
        let vote_count = read_u32_be(reader)?;
        let mut votes = Vec::with_capacity(vote_count as usize);
        for _ in 0..vote_count {
            votes.push(RaiVote::deserialize(reader)?);
        }
        Ok(Self::new(committee_epoch, votes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCertificate {
    pub committee_epoch: SnapshotNumber,
    pub election: RaiElectionId,
    pub proposal_hash: BlockHash,
    pub votes: Vec<RaiVote>,
}

impl RaiCertificate {
    pub fn new(
        committee_epoch: SnapshotNumber,
        election: RaiElectionId,
        proposal_hash: BlockHash,
        votes: Vec<RaiVote>,
    ) -> Self {
        Self {
            committee_epoch,
            election,
            proposal_hash,
            votes: canonical_votes(votes),
        }
    }

    pub fn hash(&self) -> Blake2Hash {
        object_hash(b"rai:certificate", |writer| self.serialize(writer))
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&self.committee_epoch.to_be_bytes())?;
        self.election.serialize(writer)?;
        self.proposal_hash.serialize(writer)?;
        write_count(writer, self.votes.len())?;
        for vote in &self.votes {
            vote.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let committee_epoch = read_u32_be(reader)?;
        let election = RaiElectionId::deserialize(reader)?;
        let proposal_hash = BlockHash::deserialize(reader)?;
        let vote_count = read_u32_be(reader)?;
        let mut votes = Vec::with_capacity(vote_count as usize);
        for _ in 0..vote_count {
            votes.push(RaiVote::deserialize(reader)?);
        }
        Ok(Self::new(committee_epoch, election, proposal_hash, votes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiNotarDecision {
    pub election: RaiElectionId,
    pub proposal_hash: BlockHash,
    pub cert_vote_sets: Vec<RaiVoteSet>,
    pub closing_evidence: RaiClosingProposalEvidence,
}

impl RaiNotarDecision {
    pub fn new(
        election: RaiElectionId,
        proposal_hash: BlockHash,
        cert_vote_sets: Vec<RaiVoteSet>,
    ) -> Self {
        Self::new_with_closing_evidence(
            election,
            proposal_hash,
            cert_vote_sets,
            RaiClosingProposalEvidence::new(Vec::new()),
        )
    }

    pub fn new_with_closing_evidence(
        election: RaiElectionId,
        proposal_hash: BlockHash,
        cert_vote_sets: Vec<RaiVoteSet>,
        closing_evidence: RaiClosingProposalEvidence,
    ) -> Self {
        Self {
            election,
            proposal_hash,
            cert_vote_sets: canonical_vote_sets(cert_vote_sets),
            closing_evidence,
        }
    }

    pub fn hash(&self) -> Blake2Hash {
        object_hash(b"rai:notar_decision", |writer| self.serialize(writer))
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.proposal_hash.serialize(writer)?;
        write_count(writer, self.cert_vote_sets.len())?;
        for vote_set in &self.cert_vote_sets {
            vote_set.serialize(writer)?;
        }
        self.closing_evidence.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let election = RaiElectionId::deserialize(reader)?;
        let proposal_hash = BlockHash::deserialize(reader)?;
        let set_count = read_u32_be(reader)?;
        let mut cert_vote_sets = Vec::with_capacity(set_count as usize);
        for _ in 0..set_count {
            cert_vote_sets.push(RaiVoteSet::deserialize(reader)?);
        }
        let closing_evidence = RaiClosingProposalEvidence::deserialize(reader)?;
        Ok(Self::new_with_closing_evidence(
            election,
            proposal_hash,
            cert_vote_sets,
            closing_evidence,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiFastDecision {
    pub election: RaiElectionId,
    pub proposal_hash: BlockHash,
    pub first_vote_sets: Vec<RaiVoteSet>,
}

impl RaiFastDecision {
    pub fn new(
        election: RaiElectionId,
        proposal_hash: BlockHash,
        first_vote_sets: Vec<RaiVoteSet>,
    ) -> Self {
        Self {
            election,
            proposal_hash,
            first_vote_sets: canonical_vote_sets(first_vote_sets),
        }
    }

    pub fn hash(&self) -> Blake2Hash {
        object_hash(b"rai:fast_decision", |writer| self.serialize(writer))
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.proposal_hash.serialize(writer)?;
        write_count(writer, self.first_vote_sets.len())?;
        for vote_set in &self.first_vote_sets {
            vote_set.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let election = RaiElectionId::deserialize(reader)?;
        let proposal_hash = BlockHash::deserialize(reader)?;
        let set_count = read_u32_be(reader)?;
        let mut first_vote_sets = Vec::with_capacity(set_count as usize);
        for _ in 0..set_count {
            first_vote_sets.push(RaiVoteSet::deserialize(reader)?);
        }
        Ok(Self::new(election, proposal_hash, first_vote_sets))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiFinalDecision {
    pub election: RaiElectionId,
    pub proposal_hash: BlockHash,
    pub final_vote_sets: Vec<RaiVoteSet>,
    pub closing_evidence: RaiClosingProposalEvidence,
}

impl RaiFinalDecision {
    pub fn new(
        election: RaiElectionId,
        proposal_hash: BlockHash,
        final_vote_sets: Vec<RaiVoteSet>,
    ) -> Self {
        Self::new_with_closing_evidence(
            election,
            proposal_hash,
            final_vote_sets,
            RaiClosingProposalEvidence::new(Vec::new()),
        )
    }

    pub fn new_with_closing_evidence(
        election: RaiElectionId,
        proposal_hash: BlockHash,
        final_vote_sets: Vec<RaiVoteSet>,
        closing_evidence: RaiClosingProposalEvidence,
    ) -> Self {
        Self {
            election,
            proposal_hash,
            final_vote_sets: canonical_vote_sets(final_vote_sets),
            closing_evidence,
        }
    }

    pub fn hash(&self) -> Blake2Hash {
        object_hash(b"rai:final_decision", |writer| self.serialize(writer))
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.proposal_hash.serialize(writer)?;
        write_count(writer, self.final_vote_sets.len())?;
        for vote_set in &self.final_vote_sets {
            vote_set.serialize(writer)?;
        }
        self.closing_evidence.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let election = RaiElectionId::deserialize(reader)?;
        let proposal_hash = BlockHash::deserialize(reader)?;
        let set_count = read_u32_be(reader)?;
        let mut final_vote_sets = Vec::with_capacity(set_count as usize);
        for _ in 0..set_count {
            final_vote_sets.push(RaiVoteSet::deserialize(reader)?);
        }
        let closing_evidence = RaiClosingProposalEvidence::deserialize(reader)?;
        Ok(Self::new_with_closing_evidence(
            election,
            proposal_hash,
            final_vote_sets,
            closing_evidence,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiClosingProposalEvidence {
    pub items: Vec<RaiCloseEvidence>,
}

impl RaiClosingProposalEvidence {
    pub fn new(items: Vec<RaiCloseEvidence>) -> Self {
        Self {
            items: canonical_close_evidence(items),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        write_count(writer, self.items.len())?;
        for item in &self.items {
            item.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let item_count = read_u32_be(reader)?;
        let mut items = Vec::with_capacity(item_count as usize);
        for _ in 0..item_count {
            items.push(RaiCloseEvidence::deserialize(reader)?);
        }
        Ok(Self::new(items))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiTimeoutDecision {
    pub election: RaiElectionId,
    pub timeout_vote_sets: Vec<RaiVoteSet>,
    pub evidence: RaiTimeoutDecisionEvidence,
}

impl RaiTimeoutDecision {
    pub fn new(election: RaiElectionId, timeout_vote_sets: Vec<RaiVoteSet>) -> Self {
        Self::new_with_evidence(
            election,
            timeout_vote_sets,
            RaiTimeoutDecisionEvidence::None,
        )
    }

    pub fn new_with_evidence(
        election: RaiElectionId,
        timeout_vote_sets: Vec<RaiVoteSet>,
        evidence: RaiTimeoutDecisionEvidence,
    ) -> Self {
        Self {
            election,
            timeout_vote_sets: canonical_vote_sets(timeout_vote_sets),
            evidence,
        }
    }

    pub fn hash(&self) -> Blake2Hash {
        object_hash(b"rai:timeout_decision", |writer| self.serialize(writer))
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        write_count(writer, self.timeout_vote_sets.len())?;
        for vote_set in &self.timeout_vote_sets {
            vote_set.serialize(writer)?;
        }
        self.evidence.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let election = RaiElectionId::deserialize(reader)?;
        let set_count = read_u32_be(reader)?;
        let mut timeout_vote_sets = Vec::with_capacity(set_count as usize);
        for _ in 0..set_count {
            timeout_vote_sets.push(RaiVoteSet::deserialize(reader)?);
        }
        let evidence = RaiTimeoutDecisionEvidence::deserialize(reader)?;
        Ok(Self::new_with_evidence(
            election,
            timeout_vote_sets,
            evidence,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiTimeoutDecisionEvidence {
    None,
    BelowThreshold(RaiBelowThresholdTimeoutEvidence),
    Exclusion(RaiExclusionTimeoutEvidence),
    Closing(RaiClosingTimeoutEvidence),
}

impl RaiTimeoutDecisionEvidence {
    const NONE_TAG: u8 = 0;
    const BELOW_THRESHOLD_TAG: u8 = 1;
    const EXCLUSION_TAG: u8 = 2;
    const CLOSING_TAG: u8 = 3;

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match self {
            Self::None => writer.write_all(&[Self::NONE_TAG]),
            Self::BelowThreshold(evidence) => {
                writer.write_all(&[Self::BELOW_THRESHOLD_TAG])?;
                evidence.serialize(writer)
            }
            Self::Exclusion(evidence) => {
                writer.write_all(&[Self::EXCLUSION_TAG])?;
                evidence.serialize(writer)
            }
            Self::Closing(evidence) => {
                writer.write_all(&[Self::CLOSING_TAG])?;
                evidence.serialize(writer)
            }
        }
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        match read_u8(reader)? {
            Self::NONE_TAG => Ok(Self::None),
            Self::BELOW_THRESHOLD_TAG => Ok(Self::BelowThreshold(
                RaiBelowThresholdTimeoutEvidence::deserialize(reader)?,
            )),
            Self::EXCLUSION_TAG => Ok(Self::Exclusion(RaiExclusionTimeoutEvidence::deserialize(
                reader,
            )?)),
            Self::CLOSING_TAG => Ok(Self::Closing(RaiClosingTimeoutEvidence::deserialize(
                reader,
            )?)),
            _ => Err(DeserializationError::InvalidData),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiBelowThresholdTimeoutEvidence {
    pub reports: Vec<RaiStopReport>,
    pub signer_evidence: Vec<RaiCloseEvidence>,
}

impl RaiBelowThresholdTimeoutEvidence {
    pub fn new(reports: Vec<RaiStopReport>, signer_evidence: Vec<RaiCloseEvidence>) -> Self {
        Self {
            reports: canonical_stop_reports(reports),
            signer_evidence: canonical_close_evidence(signer_evidence),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        write_count(writer, self.reports.len())?;
        write_count(writer, self.signer_evidence.len())?;
        for report in &self.reports {
            report.serialize(writer)?;
        }
        for evidence in &self.signer_evidence {
            evidence.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let report_count = read_u32_be(reader)?;
        let evidence_count = read_u32_be(reader)?;
        let mut reports = Vec::with_capacity(report_count as usize);
        let mut signer_evidence = Vec::with_capacity(evidence_count as usize);
        for _ in 0..report_count {
            reports.push(RaiStopReport::deserialize(reader)?);
        }
        for _ in 0..evidence_count {
            signer_evidence.push(RaiCloseEvidence::deserialize(reader)?);
        }
        Ok(Self::new(reports, signer_evidence))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiExclusionTimeoutEvidence {
    pub items: Vec<RaiCloseEvidence>,
}

impl RaiExclusionTimeoutEvidence {
    pub fn new(items: Vec<RaiCloseEvidence>) -> Self {
        Self {
            items: canonical_close_evidence(items),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        write_count(writer, self.items.len())?;
        for item in &self.items {
            item.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let item_count = read_u32_be(reader)?;
        let mut items = Vec::with_capacity(item_count as usize);
        for _ in 0..item_count {
            items.push(RaiCloseEvidence::deserialize(reader)?);
        }
        Ok(Self::new(items))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiClosingTimeoutEvidence {
    pub items: Vec<RaiCloseEvidence>,
}

impl RaiClosingTimeoutEvidence {
    pub fn new(items: Vec<RaiCloseEvidence>) -> Self {
        Self {
            items: canonical_close_evidence(items),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        write_count(writer, self.items.len())?;
        for item in &self.items {
            item.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let item_count = read_u32_be(reader)?;
        let mut items = Vec::with_capacity(item_count as usize);
        for _ in 0..item_count {
            items.push(RaiCloseEvidence::deserialize(reader)?);
        }
        Ok(Self::new(items))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiStopReport {
    pub epoch: SnapshotNumber,
    pub previous_close_hash: BlockHash,
    pub started_elections: Vec<RaiElectionId>,
    pub signer: PublicKey,
    pub signature: Signature,
}

impl RaiStopReport {
    pub fn new(
        epoch: SnapshotNumber,
        previous_close_hash: BlockHash,
        mut started_elections: Vec<RaiElectionId>,
        private_key: &PrivateKey,
    ) -> Self {
        started_elections.sort();
        started_elections.dedup();

        let mut report = Self {
            epoch,
            previous_close_hash,
            started_elections,
            signer: private_key.public_key(),
            signature: Signature::default(),
        };
        report.signature = private_key.sign(report.hash().as_bytes());
        report
    }

    pub fn hash(&self) -> Blake2Hash {
        signed_hash(b"rai:stop_report", self.signer, |writer| {
            writer.write_all(&self.epoch.to_be_bytes())?;
            self.previous_close_hash.serialize(writer)?;
            write_count(writer, self.started_elections.len())?;
            for election in &self.started_elections {
                election.serialize(writer)?;
            }
            Ok(())
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.previous_close_hash.serialize(writer)?;
        self.signer.serialize(writer)?;
        self.signature.serialize(writer)?;
        write_count(writer, self.started_elections.len())?;
        for election in &self.started_elections {
            election.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let epoch = read_u32_be(reader)?;
        let previous_close_hash = BlockHash::deserialize(reader)?;
        let signer = PublicKey::deserialize(reader)?;
        let signature = Signature::deserialize(reader)?;
        let election_count = read_u32_be(reader)?;
        let mut started_elections = Vec::with_capacity(election_count as usize);
        for _ in 0..election_count {
            started_elections.push(RaiElectionId::deserialize(reader)?);
        }
        started_elections.sort();
        started_elections.dedup();

        Ok(Self {
            epoch,
            previous_close_hash,
            started_elections,
            signer,
            signature,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiEpochCloseVote {
    pub epoch: SnapshotNumber,
    pub previous_close_hash: BlockHash,
    pub decided_proposals_hash: Blake2Hash,
    pub signer: PublicKey,
    pub signature: Signature,
}

impl RaiEpochCloseVote {
    pub fn new(
        epoch: SnapshotNumber,
        previous_close_hash: BlockHash,
        proposal_hashes: &[BlockHash],
        private_key: &PrivateKey,
    ) -> Self {
        let mut vote = Self {
            epoch,
            previous_close_hash,
            decided_proposals_hash: hash_proposal_hashes(proposal_hashes),
            signer: private_key.public_key(),
            signature: Signature::default(),
        };
        vote.signature = private_key.sign(vote.hash().as_bytes());
        vote
    }

    pub fn hash(&self) -> Blake2Hash {
        signed_hash(b"rai:epoch_close_vote", self.signer, |writer| {
            writer.write_all(&self.epoch.to_be_bytes())?;
            self.previous_close_hash.serialize(writer)?;
            self.decided_proposals_hash.serialize(writer)
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.previous_close_hash.serialize(writer)?;
        self.decided_proposals_hash.serialize(writer)?;
        self.signer.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            epoch: read_u32_be(reader)?,
            previous_close_hash: BlockHash::deserialize(reader)?,
            decided_proposals_hash: Blake2Hash::deserialize(reader)?,
            signer: PublicKey::deserialize(reader)?,
            signature: Signature::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiEpochClose {
    pub epoch: SnapshotNumber,
    pub previous_close_hash: BlockHash,
    pub proposal_hashes: Vec<BlockHash>,
    pub votes: Vec<RaiEpochCloseVote>,
}

impl RaiEpochClose {
    pub fn new(
        epoch: SnapshotNumber,
        previous_close_hash: BlockHash,
        proposal_hashes: Vec<BlockHash>,
        votes: Vec<RaiEpochCloseVote>,
    ) -> Self {
        Self {
            epoch,
            previous_close_hash,
            proposal_hashes: canonical_proposal_hashes(proposal_hashes),
            votes: canonical_close_votes(votes),
        }
    }

    pub fn decided_proposals_hash(&self) -> Blake2Hash {
        hash_proposal_hashes(&self.proposal_hashes)
    }

    pub fn hash(&self) -> Blake2Hash {
        object_hash(b"rai:epoch_close", |writer| self.serialize(writer))
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.previous_close_hash.serialize(writer)?;
        write_count(writer, self.proposal_hashes.len())?;
        write_count(writer, self.votes.len())?;
        for proposal_hash in &self.proposal_hashes {
            proposal_hash.serialize(writer)?;
        }
        for vote in &self.votes {
            vote.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let epoch = read_u32_be(reader)?;
        let previous_close_hash = BlockHash::deserialize(reader)?;
        let proposal_count = read_u32_be(reader)?;
        let vote_count = read_u32_be(reader)?;
        let mut proposal_hashes = Vec::with_capacity(proposal_count as usize);
        let mut votes = Vec::with_capacity(vote_count as usize);

        for _ in 0..proposal_count {
            proposal_hashes.push(BlockHash::deserialize(reader)?);
        }
        for _ in 0..vote_count {
            votes.push(RaiEpochCloseVote::deserialize(reader)?);
        }

        Ok(Self::new(
            epoch,
            previous_close_hash,
            proposal_hashes,
            votes,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseUpdateVote {
    pub epoch: SnapshotNumber,
    pub previous_close_hash: BlockHash,
    pub parent_close_hash: BlockHash,
    pub decided_proposals_hash: Blake2Hash,
    pub signer: PublicKey,
    pub signature: Signature,
}

impl RaiCloseUpdateVote {
    pub fn new(
        epoch: SnapshotNumber,
        previous_close_hash: BlockHash,
        parent_close_hash: BlockHash,
        proposal_hashes: &[BlockHash],
        private_key: &PrivateKey,
    ) -> Self {
        let mut vote = Self {
            epoch,
            previous_close_hash,
            parent_close_hash,
            decided_proposals_hash: hash_proposal_hashes(proposal_hashes),
            signer: private_key.public_key(),
            signature: Signature::default(),
        };
        vote.signature = private_key.sign(vote.hash().as_bytes());
        vote
    }

    pub fn hash(&self) -> Blake2Hash {
        signed_hash(b"rai:close_update_vote", self.signer, |writer| {
            writer.write_all(&self.epoch.to_be_bytes())?;
            self.previous_close_hash.serialize(writer)?;
            self.parent_close_hash.serialize(writer)?;
            self.decided_proposals_hash.serialize(writer)
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.previous_close_hash.serialize(writer)?;
        self.parent_close_hash.serialize(writer)?;
        self.decided_proposals_hash.serialize(writer)?;
        self.signer.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            epoch: read_u32_be(reader)?,
            previous_close_hash: BlockHash::deserialize(reader)?,
            parent_close_hash: BlockHash::deserialize(reader)?,
            decided_proposals_hash: Blake2Hash::deserialize(reader)?,
            signer: PublicKey::deserialize(reader)?,
            signature: Signature::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseUpdate {
    pub epoch: SnapshotNumber,
    pub previous_close_hash: BlockHash,
    pub parent_close_hash: BlockHash,
    pub proposal_hashes: Vec<BlockHash>,
    pub votes: Vec<RaiCloseUpdateVote>,
}

impl RaiCloseUpdate {
    pub fn new(
        epoch: SnapshotNumber,
        previous_close_hash: BlockHash,
        parent_close_hash: BlockHash,
        proposal_hashes: Vec<BlockHash>,
        votes: Vec<RaiCloseUpdateVote>,
    ) -> Self {
        Self {
            epoch,
            previous_close_hash,
            parent_close_hash,
            proposal_hashes: canonical_proposal_hashes(proposal_hashes),
            votes: canonical_close_update_votes(votes),
        }
    }

    pub fn decided_proposals_hash(&self) -> Blake2Hash {
        hash_proposal_hashes(&self.proposal_hashes)
    }

    pub fn hash(&self) -> Blake2Hash {
        object_hash(b"rai:close_update", |writer| self.serialize(writer))
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.previous_close_hash.serialize(writer)?;
        self.parent_close_hash.serialize(writer)?;
        write_count(writer, self.proposal_hashes.len())?;
        write_count(writer, self.votes.len())?;
        for proposal_hash in &self.proposal_hashes {
            proposal_hash.serialize(writer)?;
        }
        for vote in &self.votes {
            vote.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let epoch = read_u32_be(reader)?;
        let previous_close_hash = BlockHash::deserialize(reader)?;
        let parent_close_hash = BlockHash::deserialize(reader)?;
        let proposal_count = read_u32_be(reader)?;
        let vote_count = read_u32_be(reader)?;
        let mut proposal_hashes = Vec::with_capacity(proposal_count as usize);
        let mut votes = Vec::with_capacity(vote_count as usize);

        for _ in 0..proposal_count {
            proposal_hashes.push(BlockHash::deserialize(reader)?);
        }
        for _ in 0..vote_count {
            votes.push(RaiCloseUpdateVote::deserialize(reader)?);
        }

        Ok(Self::new(
            epoch,
            previous_close_hash,
            parent_close_hash,
            proposal_hashes,
            votes,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiFirstVote {
    pub election: RaiElectionId,
    pub proposal_hash: BlockHash,
    pub voter: PublicKey,
    pub signature: Signature,
    pub notar_signature: Signature,
}

impl RaiFirstVote {
    pub fn new(
        election: RaiElectionId,
        proposal_hash: BlockHash,
        private_key: &PrivateKey,
    ) -> Self {
        let mut vote = Self {
            election,
            proposal_hash,
            voter: private_key.public_key(),
            signature: Signature::default(),
            notar_signature: Signature::default(),
        };
        vote.signature = private_key.sign(vote.hash().as_bytes());
        vote.notar_signature = private_key.sign(vote.notar_hash().as_bytes());
        vote
    }

    pub fn hash(&self) -> Blake2Hash {
        signed_hash(b"rai:first_vote", self.voter, |writer| {
            self.election.serialize(writer)?;
            self.proposal_hash.serialize(writer)
        })
    }

    pub fn notar_hash(&self) -> Blake2Hash {
        signed_hash(b"rai:notar_vote", self.voter, |writer| {
            self.election.serialize(writer)?;
            RaiVoteTarget::Proposal(self.proposal_hash).serialize(writer)
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.proposal_hash.serialize(writer)?;
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)?;
        self.notar_signature.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            election: RaiElectionId::deserialize(reader)?,
            proposal_hash: BlockHash::deserialize(reader)?,
            voter: PublicKey::deserialize(reader)?,
            signature: Signature::deserialize(reader)?,
            notar_signature: Signature::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiNotarVote {
    pub election: RaiElectionId,
    pub target: RaiVoteTarget,
    pub voter: PublicKey,
    pub signature: Signature,
}

impl RaiNotarVote {
    pub fn new(election: RaiElectionId, target: RaiVoteTarget, private_key: &PrivateKey) -> Self {
        let mut vote = Self {
            election,
            target,
            voter: private_key.public_key(),
            signature: Signature::default(),
        };
        vote.signature = private_key.sign(vote.hash().as_bytes());
        vote
    }

    pub fn hash(&self) -> Blake2Hash {
        signed_hash(b"rai:notar_vote", self.voter, |writer| {
            self.election.serialize(writer)?;
            self.target.serialize(writer)
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.target.serialize(writer)?;
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            election: RaiElectionId::deserialize(reader)?,
            target: RaiVoteTarget::deserialize(reader)?,
            voter: PublicKey::deserialize(reader)?,
            signature: Signature::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiFinalVote {
    pub election: RaiElectionId,
    pub proposal_hash: BlockHash,
    pub voter: PublicKey,
    pub signature: Signature,
}

impl RaiFinalVote {
    pub fn new(
        election: RaiElectionId,
        proposal_hash: BlockHash,
        private_key: &PrivateKey,
    ) -> Self {
        let mut vote = Self {
            election,
            proposal_hash,
            voter: private_key.public_key(),
            signature: Signature::default(),
        };
        vote.signature = private_key.sign(vote.hash().as_bytes());
        vote
    }

    pub fn hash(&self) -> Blake2Hash {
        signed_hash(b"rai:final_vote", self.voter, |writer| {
            self.election.serialize(writer)?;
            self.proposal_hash.serialize(writer)
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.proposal_hash.serialize(writer)?;
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            election: RaiElectionId::deserialize(reader)?,
            proposal_hash: BlockHash::deserialize(reader)?,
            voter: PublicKey::deserialize(reader)?,
            signature: Signature::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiTimeoutVote {
    pub election: RaiElectionId,
    pub voter: PublicKey,
    pub signature: Signature,
}

impl RaiTimeoutVote {
    pub fn new(election: RaiElectionId, private_key: &PrivateKey) -> Self {
        let mut vote = Self {
            election,
            voter: private_key.public_key(),
            signature: Signature::default(),
        };
        vote.signature = private_key.sign(vote.hash().as_bytes());
        vote
    }

    pub fn hash(&self) -> Blake2Hash {
        signed_hash(b"rai:timeout_vote", self.voter, |writer| {
            self.election.serialize(writer)
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            election: RaiElectionId::deserialize(reader)?,
            voter: PublicKey::deserialize(reader)?,
            signature: Signature::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiNilVote {
    pub election: RaiElectionId,
    pub signer: PublicKey,
    pub signature: Signature,
}

impl RaiNilVote {
    pub fn new(election: RaiElectionId, private_key: &PrivateKey) -> Self {
        let mut vote = Self {
            election,
            signer: private_key.public_key(),
            signature: Signature::default(),
        };
        vote.signature = private_key.sign(vote.hash().as_bytes());
        vote
    }

    pub fn hash(&self) -> Blake2Hash {
        signed_hash(b"rai:nil_vote", self.signer, |writer| {
            self.election.serialize(writer)
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.signer.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            election: RaiElectionId::deserialize(reader)?,
            signer: PublicKey::deserialize(reader)?,
            signature: Signature::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiReportOmission {
    pub election: RaiElectionId,
    pub report: RaiStopReport,
}

impl RaiReportOmission {
    pub fn new(election: RaiElectionId, report: RaiStopReport) -> Self {
        Self { election, report }
    }

    pub fn signer(&self) -> PublicKey {
        self.report.signer
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.election.serialize(writer)?;
        self.report.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Ok(Self {
            election: RaiElectionId::deserialize(reader)?,
            report: RaiStopReport::deserialize(reader)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiCloseEvidence {
    FirstVote(RaiFirstVote),
    NilVote(RaiNilVote),
    ReportOmission(RaiReportOmission),
}

impl RaiCloseEvidence {
    const FIRST_VOTE_TAG: u8 = 0;
    const NIL_VOTE_TAG: u8 = 1;
    const REPORT_OMISSION_TAG: u8 = 2;

    pub fn election(&self) -> RaiElectionId {
        match self {
            Self::FirstVote(vote) => vote.election,
            Self::NilVote(vote) => vote.election,
            Self::ReportOmission(omission) => omission.election,
        }
    }

    pub fn signer(&self) -> PublicKey {
        match self {
            Self::FirstVote(vote) => vote.voter,
            Self::NilVote(vote) => vote.signer,
            Self::ReportOmission(omission) => omission.signer(),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match self {
            Self::FirstVote(vote) => {
                writer.write_all(&[Self::FIRST_VOTE_TAG])?;
                vote.serialize(writer)
            }
            Self::NilVote(vote) => {
                writer.write_all(&[Self::NIL_VOTE_TAG])?;
                vote.serialize(writer)
            }
            Self::ReportOmission(omission) => {
                writer.write_all(&[Self::REPORT_OMISSION_TAG])?;
                omission.serialize(writer)
            }
        }
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        match read_u8(reader)? {
            Self::FIRST_VOTE_TAG => Ok(Self::FirstVote(RaiFirstVote::deserialize(reader)?)),
            Self::NIL_VOTE_TAG => Ok(Self::NilVote(RaiNilVote::deserialize(reader)?)),
            Self::REPORT_OMISSION_TAG => Ok(Self::ReportOmission(RaiReportOmission::deserialize(
                reader,
            )?)),
            _ => Err(DeserializationError::InvalidData),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiMessage {
    Proposal(RaiProposal),
    Vote(RaiVote),
    Certificate(RaiCertificate),
    NotarDecision(RaiNotarDecision),
    FastDecision(RaiFastDecision),
    FinalDecision(RaiFinalDecision),
    TimeoutDecision(RaiTimeoutDecision),
    StopReport(RaiStopReport),
    EpochClose(RaiEpochClose),
    CloseUpdate(RaiCloseUpdate),
    LegacyFirstVote(RaiFirstVote),
    LegacyNotarVote(RaiNotarVote),
    LegacyFinalVote(RaiFinalVote),
    LegacyTimeoutVote(RaiTimeoutVote),
}

impl RaiMessage {
    pub fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match self {
            Self::Proposal(message) => {
                RaiMessageKind::Proposal.serialize(writer)?;
                message.serialize(writer)
            }
            Self::Vote(message) => {
                RaiMessageKind::Vote.serialize(writer)?;
                message.serialize(writer)
            }
            Self::Certificate(message) => {
                RaiMessageKind::Certificate.serialize(writer)?;
                message.serialize(writer)
            }
            Self::NotarDecision(message) => {
                RaiMessageKind::NotarDecision.serialize(writer)?;
                message.serialize(writer)
            }
            Self::FastDecision(message) => {
                RaiMessageKind::FastDecision.serialize(writer)?;
                message.serialize(writer)
            }
            Self::FinalDecision(message) => {
                RaiMessageKind::FinalDecision.serialize(writer)?;
                message.serialize(writer)
            }
            Self::TimeoutDecision(message) => {
                RaiMessageKind::TimeoutDecision.serialize(writer)?;
                message.serialize(writer)
            }
            Self::StopReport(message) => {
                RaiMessageKind::StopReport.serialize(writer)?;
                message.serialize(writer)
            }
            Self::EpochClose(message) => {
                RaiMessageKind::EpochClose.serialize(writer)?;
                message.serialize(writer)
            }
            Self::CloseUpdate(message) => {
                RaiMessageKind::CloseUpdate.serialize(writer)?;
                message.serialize(writer)
            }
            Self::LegacyFirstVote(message) => {
                RaiMessageKind::LegacyFirstVote.serialize(writer)?;
                message.serialize(writer)
            }
            Self::LegacyNotarVote(message) => {
                RaiMessageKind::LegacyNotarVote.serialize(writer)?;
                message.serialize(writer)
            }
            Self::LegacyFinalVote(message) => {
                RaiMessageKind::LegacyFinalVote.serialize(writer)?;
                message.serialize(writer)
            }
            Self::LegacyTimeoutVote(message) => {
                RaiMessageKind::LegacyTimeoutVote.serialize(writer)?;
                message.serialize(writer)
            }
        }
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DeserializationError> {
        let mut reader = bytes;
        let kind = RaiMessageKind::deserialize(&mut reader)?;
        let message = match kind {
            RaiMessageKind::Proposal => Self::Proposal(RaiProposal::deserialize(&mut reader)?),
            RaiMessageKind::Vote => Self::Vote(RaiVote::deserialize(&mut reader)?),
            RaiMessageKind::Certificate => {
                Self::Certificate(RaiCertificate::deserialize(&mut reader)?)
            }
            RaiMessageKind::NotarDecision => {
                Self::NotarDecision(RaiNotarDecision::deserialize(&mut reader)?)
            }
            RaiMessageKind::FastDecision => {
                Self::FastDecision(RaiFastDecision::deserialize(&mut reader)?)
            }
            RaiMessageKind::FinalDecision => {
                Self::FinalDecision(RaiFinalDecision::deserialize(&mut reader)?)
            }
            RaiMessageKind::TimeoutDecision => {
                Self::TimeoutDecision(RaiTimeoutDecision::deserialize(&mut reader)?)
            }
            RaiMessageKind::StopReport => {
                Self::StopReport(RaiStopReport::deserialize(&mut reader)?)
            }
            RaiMessageKind::EpochClose => {
                Self::EpochClose(RaiEpochClose::deserialize(&mut reader)?)
            }
            RaiMessageKind::CloseUpdate => {
                Self::CloseUpdate(RaiCloseUpdate::deserialize(&mut reader)?)
            }
            RaiMessageKind::LegacyFirstVote => {
                Self::LegacyFirstVote(RaiFirstVote::deserialize(&mut reader)?)
            }
            RaiMessageKind::LegacyNotarVote => {
                Self::LegacyNotarVote(RaiNotarVote::deserialize(&mut reader)?)
            }
            RaiMessageKind::LegacyFinalVote => {
                Self::LegacyFinalVote(RaiFinalVote::deserialize(&mut reader)?)
            }
            RaiMessageKind::LegacyTimeoutVote => {
                Self::LegacyTimeoutVote(RaiTimeoutVote::deserialize(&mut reader)?)
            }
        };

        ensure_empty(reader)?;
        Ok(message)
    }
}

impl MessageVariant for RaiMessage {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive)]
#[repr(u8)]
enum RaiMessageKind {
    Proposal = 0,
    Vote = 1,
    Certificate = 2,
    FastDecision = 3,
    FinalDecision = 4,
    TimeoutDecision = 5,
    StopReport = 6,
    EpochClose = 7,
    CloseUpdate = 8,
    NotarDecision = 9,
    LegacyFirstVote = 128,
    LegacyNotarVote = 129,
    LegacyFinalVote = 130,
    LegacyTimeoutVote = 131,
}

impl RaiMessageKind {
    fn serialize<T>(self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&[self as u8])
    }

    fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        Self::from_u8(read_u8(reader)?).ok_or(DeserializationError::InvalidData)
    }
}

fn signed_hash(
    domain: &[u8],
    signer: PublicKey,
    write_payload: impl FnOnce(&mut Vec<u8>) -> std::io::Result<()>,
) -> Blake2Hash {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(domain);
    write_payload(&mut bytes).expect("Hash payload serialization to Vec should succeed");
    bytes.extend_from_slice(signer.as_bytes());
    Blake2HashBuilder::default().update(bytes).build()
}

fn object_hash(
    domain: &[u8],
    write_payload: impl FnOnce(&mut Vec<u8>) -> std::io::Result<()>,
) -> Blake2Hash {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(domain);
    write_payload(&mut bytes).expect("Hash payload serialization to Vec should succeed");
    Blake2HashBuilder::default().update(bytes).build()
}

fn write_tagged_hash<T>(writer: &mut T, tag: u8, hash: &BlockHash) -> std::io::Result<()>
where
    T: Write,
{
    writer.write_all(&[tag])?;
    hash.serialize(writer)
}

fn canonical_votes(votes: Vec<RaiVote>) -> Vec<RaiVote> {
    let mut by_signer = BTreeMap::<PublicKey, RaiVote>::new();
    for vote in votes {
        by_signer
            .entry(vote.voter)
            .and_modify(|existing| {
                if vote.hash() < existing.hash() {
                    *existing = vote.clone();
                }
            })
            .or_insert(vote);
    }
    by_signer.into_values().collect()
}

fn canonical_vote_sets(vote_sets: Vec<RaiVoteSet>) -> Vec<RaiVoteSet> {
    let mut by_committee = BTreeMap::<SnapshotNumber, RaiVoteSet>::new();
    for vote_set in vote_sets {
        let vote_set = RaiVoteSet::new(vote_set.committee_epoch, vote_set.votes);
        by_committee
            .entry(vote_set.committee_epoch)
            .and_modify(|existing| {
                if vote_set.hash() < existing.hash() {
                    *existing = vote_set.clone();
                }
            })
            .or_insert(vote_set);
    }
    by_committee.into_values().collect()
}

fn canonical_close_votes(votes: Vec<RaiEpochCloseVote>) -> Vec<RaiEpochCloseVote> {
    let mut by_signer = BTreeMap::<PublicKey, RaiEpochCloseVote>::new();
    for vote in votes {
        by_signer
            .entry(vote.signer)
            .and_modify(|existing| {
                if vote.hash() < existing.hash() {
                    *existing = vote.clone();
                }
            })
            .or_insert(vote);
    }
    by_signer.into_values().collect()
}

fn canonical_close_update_votes(votes: Vec<RaiCloseUpdateVote>) -> Vec<RaiCloseUpdateVote> {
    let mut by_signer = BTreeMap::<PublicKey, RaiCloseUpdateVote>::new();
    for vote in votes {
        by_signer
            .entry(vote.signer)
            .and_modify(|existing| {
                if vote.hash() < existing.hash() {
                    *existing = vote.clone();
                }
            })
            .or_insert(vote);
    }
    by_signer.into_values().collect()
}

fn canonical_proposal_hashes(mut proposal_hashes: Vec<BlockHash>) -> Vec<BlockHash> {
    proposal_hashes.sort();
    proposal_hashes.dedup();
    proposal_hashes
}

fn hash_proposal_hashes(proposal_hashes: &[BlockHash]) -> Blake2Hash {
    let proposal_hashes = canonical_proposal_hashes(proposal_hashes.to_vec());
    object_hash(b"rai:decided_proposals", |writer| {
        write_count(writer, proposal_hashes.len())?;
        for proposal_hash in &proposal_hashes {
            proposal_hash.serialize(writer)?;
        }
        Ok(())
    })
}

fn canonical_stop_reports(reports: Vec<RaiStopReport>) -> Vec<RaiStopReport> {
    let mut by_signer = BTreeMap::<PublicKey, RaiStopReport>::new();
    for report in reports {
        by_signer
            .entry(report.signer)
            .and_modify(|existing| {
                if report.hash() < existing.hash() {
                    *existing = report.clone();
                }
            })
            .or_insert(report);
    }
    by_signer.into_values().collect()
}

fn canonical_close_evidence(evidence: Vec<RaiCloseEvidence>) -> Vec<RaiCloseEvidence> {
    let mut by_signer = BTreeMap::<PublicKey, RaiCloseEvidence>::new();
    for item in evidence {
        by_signer
            .entry(item.signer())
            .and_modify(|existing| {
                if close_evidence_hash(&item) < close_evidence_hash(existing) {
                    *existing = item.clone();
                }
            })
            .or_insert(item);
    }
    by_signer.into_values().collect()
}

fn close_evidence_hash(evidence: &RaiCloseEvidence) -> Blake2Hash {
    object_hash(b"rai:close_evidence", |writer| evidence.serialize(writer))
}

fn write_count<T>(writer: &mut T, count: usize) -> std::io::Result<()>
where
    T: Write,
{
    let count = u32::try_from(count)
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "Rai message vector too large"))?;
    writer.write_all(&count.to_be_bytes())
}

fn ensure_empty(bytes: &[u8]) -> Result<(), DeserializationError> {
    if bytes.is_empty() {
        Ok(())
    } else {
        Err(DeserializationError::TooMuchData)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn rai_messages_are_serializable() {
        let key = PrivateKey::from(42);
        let election = test_election();
        let proposal_hash = BlockHash::from(123);
        let first_vote = RaiVote::proposal(RaiVotePhase::First, election, proposal_hash, &key);
        let cert_vote = RaiVote::proposal(RaiVotePhase::Cert, election, proposal_hash, &key);
        let final_vote = RaiVote::proposal(RaiVotePhase::Final, election, proposal_hash, &key);
        let timeout_vote = RaiVote::timeout(election, &key);
        let nil_vote = RaiNilVote::new(election, &key);
        let first_vote_set = RaiVoteSet::new(election.epoch, vec![first_vote.clone()]);
        let cert_vote_set = RaiVoteSet::new(election.epoch, vec![cert_vote.clone()]);
        let final_vote_set = RaiVoteSet::new(election.epoch, vec![final_vote.clone()]);
        let timeout_vote_set = RaiVoteSet::new(election.epoch, vec![timeout_vote.clone()]);
        let close_proposal_hashes = vec![proposal_hash];
        let epoch_close_vote = RaiEpochCloseVote::new(
            election.epoch,
            BlockHash::from(456),
            &close_proposal_hashes,
            &key,
        );
        let close_update_vote = RaiCloseUpdateVote::new(
            election.epoch,
            BlockHash::from(456),
            BlockHash::from(789),
            &close_proposal_hashes,
            &key,
        );
        let stop_report =
            RaiStopReport::new(election.epoch, BlockHash::from(456), Vec::new(), &key);

        let messages = vec![
            Message::Rai(RaiMessage::Proposal(RaiProposal::new(
                election,
                Block::new_test_instance(),
            ))),
            Message::Rai(RaiMessage::Vote(first_vote.clone())),
            Message::Rai(RaiMessage::Certificate(RaiCertificate::new(
                election.epoch,
                election,
                proposal_hash,
                vec![first_vote.clone(), cert_vote],
            ))),
            Message::Rai(RaiMessage::FastDecision(RaiFastDecision::new(
                election,
                proposal_hash,
                vec![first_vote_set],
            ))),
            Message::Rai(RaiMessage::NotarDecision(RaiNotarDecision::new(
                election,
                proposal_hash,
                vec![cert_vote_set],
            ))),
            Message::Rai(RaiMessage::FinalDecision(RaiFinalDecision::new(
                election,
                proposal_hash,
                vec![final_vote_set],
            ))),
            Message::Rai(RaiMessage::TimeoutDecision(RaiTimeoutDecision::new(
                election,
                vec![timeout_vote_set],
            ))),
            Message::Rai(RaiMessage::StopReport(stop_report.clone())),
            Message::Rai(RaiMessage::EpochClose(RaiEpochClose::new(
                election.epoch,
                BlockHash::from(456),
                close_proposal_hashes,
                vec![epoch_close_vote],
            ))),
            Message::Rai(RaiMessage::CloseUpdate(RaiCloseUpdate::new(
                election.epoch,
                BlockHash::from(456),
                BlockHash::from(789),
                vec![proposal_hash],
                vec![close_update_vote],
            ))),
            Message::Rai(RaiMessage::LegacyFirstVote(RaiFirstVote::new(
                election,
                proposal_hash,
                &key,
            ))),
            Message::Rai(RaiMessage::LegacyNotarVote(RaiNotarVote::new(
                election,
                RaiVoteTarget::Proposal(proposal_hash),
                &key,
            ))),
            Message::Rai(RaiMessage::LegacyFinalVote(RaiFinalVote::new(
                election,
                proposal_hash,
                &key,
            ))),
            Message::Rai(RaiMessage::LegacyTimeoutVote(RaiTimeoutVote::new(
                election, &key,
            ))),
        ];

        for message in messages {
            assert_deserializable(&message);
        }

        assert_deserializable(&Message::Rai(RaiMessage::TimeoutDecision(
            RaiTimeoutDecision::new_with_evidence(
                election,
                vec![RaiVoteSet::new(election.epoch, vec![timeout_vote])],
                RaiTimeoutDecisionEvidence::Closing(RaiClosingTimeoutEvidence::new(vec![
                    RaiCloseEvidence::NilVote(nil_vote),
                ])),
            ),
        )));
    }

    #[test]
    fn election_id_serializes_epoch_context() {
        let election = RaiElectionId::with_context(
            RaiSlot::new(Account::from(1), 2),
            3,
            RaiEpochContext::new(BlockHash::from(4), BlockHash::from(5)),
        );
        let mut bytes = Vec::new();

        election.serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), RaiElectionId::SERIALIZED_SIZE);
        assert_eq!(
            RaiElectionId::deserialize(&mut bytes.as_slice()).unwrap(),
            election
        );
    }

    #[test]
    fn epoch_close_contains_canonical_proposal_identifiers() {
        let key = PrivateKey::from(42);
        let epoch = 7;
        let previous_close_hash = BlockHash::from(456);
        let proposal_a = BlockHash::from(1);
        let proposal_b = BlockHash::from(2);

        let close = RaiEpochClose::new(
            epoch,
            previous_close_hash,
            vec![proposal_b, proposal_a, proposal_b],
            Vec::new(),
        );
        let vote_a = RaiEpochCloseVote::new(
            epoch,
            previous_close_hash,
            &[proposal_b, proposal_a, proposal_b],
            &key,
        );
        let vote_b =
            RaiEpochCloseVote::new(epoch, previous_close_hash, &[proposal_a, proposal_b], &key);

        assert_eq!(close.proposal_hashes, vec![proposal_a, proposal_b]);
        assert_eq!(
            close.decided_proposals_hash(),
            vote_a.decided_proposals_hash
        );
        assert_eq!(vote_a.decided_proposals_hash, vote_b.decided_proposals_hash);
    }

    #[test]
    fn timeout_decision_contains_canonical_attached_evidence() {
        let key_a = PrivateKey::from(1);
        let key_b = PrivateKey::from(2);
        let election = test_election();
        let vote_set = RaiVoteSet::new(election.epoch, vec![RaiVote::timeout(election, &key_a)]);
        let report_a = RaiStopReport::new(election.epoch, BlockHash::from(1), Vec::new(), &key_a);
        let report_b = RaiStopReport::new(election.epoch, BlockHash::from(2), Vec::new(), &key_b);
        let omission_a =
            RaiCloseEvidence::ReportOmission(RaiReportOmission::new(election, report_a.clone()));
        let omission_b =
            RaiCloseEvidence::ReportOmission(RaiReportOmission::new(election, report_b.clone()));

        let evidence = RaiBelowThresholdTimeoutEvidence::new(
            vec![report_b.clone(), report_a.clone(), report_b],
            vec![omission_b.clone(), omission_a.clone(), omission_b],
        );
        let decision = RaiTimeoutDecision::new_with_evidence(
            election,
            vec![vote_set],
            RaiTimeoutDecisionEvidence::BelowThreshold(evidence),
        );

        assert_deserializable(&Message::Rai(RaiMessage::TimeoutDecision(decision.clone())));
        assert!(matches!(
            decision.evidence,
            RaiTimeoutDecisionEvidence::BelowThreshold(ref evidence)
                if evidence.reports.len() == 2 && evidence.signer_evidence.len() == 2
        ));
    }

    #[test]
    fn retired_legacy_certified_message_kinds_are_rejected() {
        for retired_kind in [132, 133, 134, 135, 136, 137] {
            assert!(matches!(
                RaiMessage::deserialize(&[retired_kind]),
                Err(DeserializationError::InvalidData)
            ));
        }
    }

    #[test]
    fn signed_vote_hash_excludes_signature() {
        let key = PrivateKey::from(42);
        let vote = RaiFirstVote::new(test_election(), BlockHash::from(123), &key);

        assert_eq!(
            vote.voter.verify(vote.hash().as_bytes(), &vote.signature),
            Ok(())
        );
        assert_eq!(
            vote.voter
                .verify(vote.notar_hash().as_bytes(), &vote.notar_signature),
            Ok(())
        );
    }

    fn test_election() -> RaiElectionId {
        RaiElectionId::new(RaiSlot::new(Account::from(1), 2), 3)
    }
}
