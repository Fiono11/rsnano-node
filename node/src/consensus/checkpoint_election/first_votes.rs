use std::collections::BTreeMap;

use rsnano_types::{Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature};
use serde::{Deserialize, Serialize};

use super::{
    CheckpointContext, CheckpointError, CheckpointEvidence, CheckpointInstance, Introduction,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstVote {
    pub instance: CheckpointInstance,
    pub rank: u64,
    pub signer: PublicKey,
    pub value: BlockHash,
    pub introduction: Introduction,
    pub signature: Signature,
}

impl FirstVote {
    fn signing_hash(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI checkpoint FIRST v1.3")
            .update(self.instance.digest().as_bytes())
            .update(self.rank.to_le_bytes())
            .update(self.signer.as_bytes())
            .update(self.value.as_bytes())
            .update(self.introduction.digest().as_bytes())
            .build()
    }

    // Only the journal-backed signing API releases locally created votes.
    fn sign(
        instance: CheckpointInstance,
        rank: u64,
        value: BlockHash,
        introduction: Introduction,
        key: &PrivateKey,
    ) -> Self {
        let mut vote = Self {
            instance,
            rank,
            signer: key.public_key(),
            value,
            introduction,
            signature: Signature::default(),
        };
        vote.signature = key.sign(vote.signing_hash().as_bytes());
        vote
    }
}

/// Atomic insert-if-absent by (instance, rank, signer); fast rank is always zero.
/// If the slot exists,
/// return its original record without overwriting it. Invoke `create` only
/// once while holding exclusive access to an absent slot. A durable backend
/// must also commit before success and recover uncertain writes on retry.
/// Disk persistence is deferred; the volatile implementation retains locks
/// only for this process lifetime and makes no restart-safety guarantee.
pub trait FirstVoteJournal {
    fn get_or_insert(
        &mut self,
        instance: CheckpointInstance,
        rank: u64,
        signer: PublicKey,
        create: impl FnOnce() -> Result<FirstVote, CheckpointError>,
    ) -> Result<FirstVote, CheckpointError>;
}

/// Interim backend explicitly selected while disk persistence is deferred.
/// Keep one journal for the whole process/session, including after decisions.
#[derive(Default)]
pub struct VolatileFirstVoteJournal {
    records: BTreeMap<(CheckpointInstance, u64, PublicKey), FirstVote>,
}

impl FirstVoteJournal for VolatileFirstVoteJournal {
    fn get_or_insert(
        &mut self,
        instance: CheckpointInstance,
        rank: u64,
        signer: PublicKey,
        create: impl FnOnce() -> Result<FirstVote, CheckpointError>,
    ) -> Result<FirstVote, CheckpointError> {
        let key = (instance, rank, signer);
        if let Some(vote) = self.records.get(&key) {
            return Ok(vote.clone());
        }
        let vote = create()?;
        if (vote.instance, vote.rank, vote.signer) != key {
            return Err(CheckpointError::InvalidJournalRecord);
        }
        self.records.insert(key, vote.clone());
        Ok(vote)
    }
}

impl CheckpointContext {
    pub fn first_vote(
        &self,
        rank: u64,
        value: BlockHash,
        introduction: Introduction,
        key: &PrivateKey,
        evidence: &impl CheckpointEvidence,
        journal: &mut impl FirstVoteJournal,
    ) -> Result<FirstVote, CheckpointError> {
        if rank != 0 {
            return Err(CheckpointError::WrongRank);
        }
        if self.committee.weight(&key.public_key()).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        let saved = journal.get_or_insert(self.instance, rank, key.public_key(), || {
            self.verify_introduction(rank, value, &introduction, evidence)?;
            Ok(FirstVote::sign(
                self.instance,
                rank,
                value,
                introduction,
                key,
            ))
        })?;
        if saved.signer != key.public_key() {
            return Err(CheckpointError::InvalidJournalRecord);
        }
        // A prior vote may differ from the current R maximum. Return that
        // exact record, including its original ancestry, never sign anew.
        self.verify_first_vote(rank, &saved, evidence)?;
        Ok(saved)
    }

    pub fn verify_first_vote(
        &self,
        rank: u64,
        vote: &FirstVote,
        evidence: &impl CheckpointEvidence,
    ) -> Result<(), CheckpointError> {
        self.authenticate_first_vote(rank, vote)?;
        self.verify_introduction(rank, vote.value, &vote.introduction, evidence)
    }

    fn authenticate_first_vote(&self, rank: u64, vote: &FirstVote) -> Result<(), CheckpointError> {
        if vote.instance != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        if rank != 0 || vote.rank != rank {
            return Err(CheckpointError::WrongRank);
        }
        if self.committee.weight(&vote.signer).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        vote.signer
            .verify(vote.signing_hash().as_bytes(), &vote.signature)
            .map_err(|_| CheckpointError::BadSignature)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FastCertificate {
    pub instance: CheckpointInstance,
    pub rank: u64,
    pub value: BlockHash,
    pub votes: Vec<FirstVote>,
}

impl FastCertificate {
    /// Verify independently of the receiver's pool. An equivocator's earlier
    /// local vote does not invalidate a correctly signed certificate for v.
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
        if self.votes.len() as u64 != context.thresholds().f_fast {
            return Err(CheckpointError::InvalidSize);
        }
        let mut signers = std::collections::BTreeSet::new();
        for vote in &self.votes {
            if !signers.insert(vote.signer) {
                return Err(CheckpointError::DuplicateSigner);
            }
            if vote.value != self.value {
                return Err(CheckpointError::InvalidEvidence);
            }
            context.verify_first_vote(rank, vote, evidence)?;
        }
        Ok(self.value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoteAdmission {
    Counted,
    Duplicate,
    Pending,
}

/// One pool per checkpoint instance; the only fast attempt is rank zero.
/// Recovery keeps the first validated record per identity; fast detection keeps a record per (identity,value). This
/// preserves fast evidence despite Byzantine equivocation.
pub struct FirstVotePool {
    context: CheckpointContext,
    rank: u64,
    recovery: BTreeMap<PublicKey, FirstVote>,
    fast: BTreeMap<(BlockHash, PublicKey), FirstVote>,
    pending: Vec<FirstVote>,
}

impl FirstVotePool {
    pub fn new(context: CheckpointContext) -> Self {
        Self {
            context,
            rank: 0,
            recovery: BTreeMap::new(),
            fast: BTreeMap::new(),
            pending: Vec::new(),
        }
    }

    pub fn context(&self) -> &CheckpointContext {
        &self.context
    }
    pub fn rank(&self) -> u64 {
        self.rank
    }
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
    pub fn snapshot(&self) -> Vec<FirstVote> {
        self.recovery.values().cloned().collect()
    }

    pub fn receive(
        &mut self,
        vote: FirstVote,
        evidence: &impl CheckpointEvidence,
    ) -> Result<VoteAdmission, CheckpointError> {
        // Never park unauthenticated or wrong-instance messages.
        self.context.authenticate_first_vote(self.rank, &vote)?;
        match self
            .context
            .verify_introduction(self.rank, vote.value, &vote.introduction, evidence)
        {
            Ok(()) => {
                self.pending.retain(|held| held != &vote);
                let key = (vote.value, vote.signer);
                if self.fast.contains_key(&key) {
                    return Ok(VoteAdmission::Duplicate);
                }
                self.recovery
                    .entry(vote.signer)
                    .or_insert_with(|| vote.clone());
                self.fast.insert(key, vote);
                Ok(VoteAdmission::Counted)
            }
            Err(CheckpointError::MissingEvidence(_)) => {
                if !self.pending.contains(&vote) {
                    self.pending.push(vote);
                }
                Ok(VoteAdmission::Pending)
            }
            Err(error) => Err(error),
        }
    }

    /// Recheck when payloads or proof dependencies arrive. Still-missing
    /// records stay pending; evidence proven invalid is discarded.
    pub fn retry_pending(
        &mut self,
        evidence: &impl CheckpointEvidence,
    ) -> Vec<Result<VoteAdmission, CheckpointError>> {
        let pending = std::mem::take(&mut self.pending);
        pending
            .into_iter()
            .map(|vote| self.receive(vote, evidence))
            .collect()
    }

    pub fn fast_certificate(&self, value: BlockHash) -> Option<FastCertificate> {
        let votes: Vec<_> = self
            .fast
            .iter()
            .filter(|((v, _), _)| *v == value)
            .map(|(_, vote)| vote.clone())
            .take(self.context.thresholds().f_fast as usize)
            .collect();
        (votes.len() as u64 == self.context.thresholds().f_fast).then_some(FastCertificate {
            instance: self.context.instance(),
            rank: self.rank,
            value,
            votes,
        })
    }
}
