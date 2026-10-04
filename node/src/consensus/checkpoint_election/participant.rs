//! Composition driver for a selected slow-committee participant. The caller
//! supplies application evidence and schedules retries; this is not a synchronizer
//! or the observer service for fast members outside the slow committee.
use super::*;
use rsnano_types::{BlockHash, PrivateKey};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParticipantEffect {
    Fast(FastMessage),
    Slow(SlowEffect),
    Decided(BlockHash),
}

pub struct CheckpointParticipant {
    pool: FirstVotePool,
    key: PrivateKey,
    slow: Option<SlowReplica>,
    recovery: Option<RecoveryCertificate>,
    fast_decision: Option<FastCertificate>,
    decision: Option<BlockHash>,
}
impl CheckpointParticipant {
    pub fn new(context: CheckpointContext, key: PrivateKey) -> Result<Self, CheckpointError> {
        if !SlowContext::new(&context)
            .members()
            .contains(&key.public_key())
        {
            return Err(CheckpointError::NonMember);
        }
        Ok(Self {
            pool: FirstVotePool::new(context),
            key,
            slow: None,
            recovery: None,
            fast_decision: None,
            decision: None,
        })
    }
    pub fn decision(&self) -> Option<BlockHash> {
        self.decision
    }
    pub fn decision_proof(
        &self,
        max_objects: usize,
    ) -> Result<Option<CheckpointDecisionProof>, CheckpointError> {
        if let Some(certificate) = &self.fast_decision {
            return Ok(Some(CheckpointDecisionProof::Fast(certificate.clone())));
        }
        if let Some(slow) = &self.slow
            && let Some((_, root)) = slow.decision()
        {
            return Ok(Some(CheckpointDecisionProof::Slow(
                SlowDecisionProof::collect(slow.proof_store(), root, max_objects)?,
            )));
        }
        Ok(None)
    }
    pub fn fallback_started(&self) -> bool {
        self.slow.is_some()
    }

    fn decide(
        &mut self,
        value: BlockHash,
        effects: &mut Vec<ParticipantEffect>,
    ) -> Result<(), CheckpointError> {
        match self.decision {
            Some(previous) if previous != value => Err(CheckpointError::InvalidEvidence),
            Some(_) => Ok(()),
            None => {
                self.decision = Some(value);
                effects.push(ParticipantEffect::Decided(value));
                Ok(())
            }
        }
    }
    fn slow_effects(
        &mut self,
        outgoing: Vec<SlowEffect>,
    ) -> Result<Vec<ParticipantEffect>, CheckpointError> {
        let mut effects = Vec::new();
        for effect in outgoing {
            if let SlowEffect::Decided { value, .. } = &effect {
                self.decide(*value, &mut effects)?;
            }
            effects.push(ParticipantEffect::Slow(effect));
        }
        Ok(effects)
    }

    /// Import verified RC0 once. Later preferences never replace the initial
    /// slow proposal. No timeout or proof that the fast attempt failed is needed.
    pub fn admit_recovery(
        &mut self,
        rc: RecoveryCertificate,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<ParticipantEffect>, CheckpointError> {
        rc.verify(self.pool.context(), 0, evidence)?;
        if self.slow.is_some() {
            return Ok(Vec::new());
        }
        let context = self.pool.context();
        let proposal =
            SlowContext::new(context).proposal(context, rc.clone(), &self.key, evidence)?;
        let copy = CheckpointContext::new(context.instance, context.committee.clone())?;
        let replica = SlowReplica::new(
            copy,
            self.key.clone(),
            proposal,
            evidence,
            VerificationLimits::default(),
        )?;
        self.slow = Some(replica);
        self.recovery = Some(rc.clone());
        let mut effects = vec![ParticipantEffect::Fast(FastMessage::Recovery(rc))];
        let outgoing = self.slow.as_mut().unwrap().retry(evidence)?;
        effects.extend(self.slow_effects(outgoing)?);
        Ok(effects)
    }

    pub fn receive_fast(
        &mut self,
        message: FastMessage,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<ParticipantEffect>, CheckpointError> {
        match message {
            FastMessage::Recovery(rc) => self.admit_recovery(rc, evidence),
            FastMessage::Certificate(fc) => {
                let value = fc.verify(self.pool.context(), 0, evidence)?;
                let mut effects = Vec::new();
                self.decide(value, &mut effects)?;
                if self.fast_decision.is_none() {
                    self.fast_decision = Some(fc.clone());
                    effects.push(ParticipantEffect::Fast(FastMessage::Certificate(fc)));
                }
                Ok(effects)
            }
            FastMessage::First(vote) => {
                let value = vote.value;
                self.pool.receive(vote, evidence)?;
                if let Some(fc) = self.pool.fast_certificate(value) {
                    return self.receive_fast(FastMessage::Certificate(fc), evidence);
                }
                self.try_recovery(None, evidence)
            }
        }
    }

    /// Empty-candidate recovery needs externally verified initial-R evidence.
    pub fn try_recovery(
        &mut self,
        initial_r: Option<BlockHash>,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<ParticipantEffect>, CheckpointError> {
        if self.slow.is_some() {
            return Ok(Vec::new());
        }
        match self.pool.resolve(initial_r, evidence)? {
            RecoveryProgress::Resolved(rc) => self.admit_recovery(rc, evidence),
            _ => Ok(Vec::new()),
        }
    }

    /// Before RC admission, the caller must retain and retry slow traffic. Reply
    /// effects belong to the connection that supplied this specific message.
    pub fn receive_slow(
        &mut self,
        message: SlowMessage,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<ParticipantEffect>, CheckpointError> {
        let outgoing = self
            .slow
            .as_mut()
            .ok_or(CheckpointError::WrongPhase)?
            .receive(message, evidence)?;
        self.slow_effects(outgoing)
    }

    pub fn evidence_updated(
        &mut self,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<ParticipantEffect>, CheckpointError> {
        self.pool.retry_pending(evidence);
        let mut effects = Vec::new();
        let values: std::collections::BTreeSet<_> =
            self.pool.snapshot().iter().map(|v| v.value).collect();
        for value in values {
            if let Some(fc) = self.pool.fast_certificate(value) {
                effects.extend(self.receive_fast(FastMessage::Certificate(fc), evidence)?);
            }
        }
        effects.extend(self.try_recovery(None, evidence)?);
        if let Some(slow) = self.slow.as_mut() {
            let outgoing = slow.evidence_updated(evidence)?;
            effects.extend(self.slow_effects(outgoing)?);
        }
        Ok(effects)
    }

    /// Keeps serving slow proofs after either path decides. FIRST release still
    /// uses the caller's process-lifetime FirstVoteJournal, never this driver.
    pub fn retry(
        &mut self,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<ParticipantEffect>, CheckpointError> {
        self.pool.retry_pending(evidence);
        let mut effects = Vec::new();
        let values: std::collections::BTreeSet<_> =
            self.pool.snapshot().iter().map(|v| v.value).collect();
        for value in values {
            if let Some(fc) = self.pool.fast_certificate(value) {
                effects.extend(self.receive_fast(FastMessage::Certificate(fc), evidence)?);
            }
        }
        effects.extend(self.try_recovery(None, evidence)?);
        if let Some(fc) = &self.fast_decision {
            effects.push(ParticipantEffect::Fast(FastMessage::Certificate(
                fc.clone(),
            )));
        }
        if let Some(rc) = &self.recovery {
            effects.push(ParticipantEffect::Fast(FastMessage::Recovery(rc.clone())));
        }
        if let Some(slow) = self.slow.as_mut() {
            let outgoing = slow.retry(evidence)?;
            effects.extend(self.slow_effects(outgoing)?);
        }
        Ok(effects)
    }
}
