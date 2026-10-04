//! Role selection and connection-aware delivery. Scheduling and application
//! evidence are supplied by the caller; no node service is installed here.
use super::*;
use rsnano_types::{BlockHash, PrivateKey};
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriverEffect<P> {
    Broadcast(CheckpointMessage),
    Reply(P, CheckpointMessage),
    Decided(BlockHash),
    Rejected(CheckpointError),
}

pub struct CheckpointDriver<P> {
    key: PrivateKey,
    local_first: Option<FirstVote>,
    participant: Option<CheckpointParticipant>,
    observer: Option<CheckpointObserver>,
    pool: FirstVotePool,
    slow: SlowContext,
    early: VecDeque<(P, SlowMessage)>,
    decision: Option<BlockHash>,
    fast_certificate: Option<FastCertificate>,
    recovery_certificate: Option<RecoveryCertificate>,
}
impl<P: Clone + Eq> CheckpointDriver<P> {
    pub fn new(context: CheckpointContext, key: PrivateKey) -> Result<Self, CheckpointError> {
        if context.committee.weight(&key.public_key()).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        let slow = SlowContext::new(&context);
        let copy = CheckpointContext::new(context.instance, context.committee.clone())?;
        let selected = slow.members().contains(&key.public_key());
        let (participant, observer) = if selected {
            (Some(CheckpointParticipant::new(copy, key.clone())?), None)
        } else {
            (
                None,
                Some(CheckpointObserver::new(
                    copy,
                    VerificationLimits::default(),
                    SlowIoLimits::default().max_message_bytes,
                )?),
            )
        };
        Ok(Self {
            key,
            local_first: None,
            participant,
            observer,
            pool: FirstVotePool::new(context),
            slow,
            early: VecDeque::new(),
            decision: None,
            fast_certificate: None,
            recovery_certificate: None,
        })
    }
    /// Release exactly the process-journal FIRST record, even if a later local
    /// candidate differs. Persist the record before making it observable.
    pub fn propose(
        &mut self,
        value: BlockHash,
        introduction: Introduction,
        journal: &mut impl FirstVoteJournal,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<DriverEffect<P>>, CheckpointError> {
        let vote =
            self.pool
                .context()
                .first_vote(0, value, introduction, &self.key, evidence, journal)?;
        self.local_first = Some(vote.clone());
        let mut effects = vec![DriverEffect::Broadcast(CheckpointMessage::Fast(
            FastMessage::First(vote.clone()),
        ))];
        if let Some(participant) = self.participant.as_mut() {
            match participant.receive_fast(FastMessage::First(vote), evidence) {
                Ok(outgoing) => self.participant_effects(None, outgoing, &mut effects),
                Err(error) => effects.push(DriverEffect::Rejected(error)),
            }
        } else {
            self.pool.receive(vote.clone(), evidence)?;
            if let Some(fc) = self.pool.fast_certificate(vote.value) {
                self.accept_fast(fc, evidence, &mut effects)?;
            }
        }
        self.drain_early(evidence, &mut effects);
        Ok(effects)
    }

    pub fn try_recovery(
        &mut self,
        initial_r: Option<BlockHash>,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<DriverEffect<P>>, CheckpointError> {
        let mut effects = Vec::new();
        if let Some(participant) = self.participant.as_mut() {
            let outgoing = participant.try_recovery(initial_r, evidence)?;
            self.participant_effects(None, outgoing, &mut effects);
        } else if self.recovery_certificate.is_none()
            && let RecoveryProgress::Resolved(rc) = self.pool.resolve(initial_r, evidence)?
        {
            self.recovery_certificate = Some(rc.clone());
            effects.push(DriverEffect::Broadcast(CheckpointMessage::Fast(
                FastMessage::Recovery(rc),
            )));
        }
        self.drain_early(evidence, &mut effects);
        Ok(effects)
    }

    pub fn decision(&self) -> Option<BlockHash> {
        self.decision
    }
    pub fn decision_proof(
        &self,
        max_objects: usize,
    ) -> Result<Option<CheckpointDecisionProof>, CheckpointError> {
        if let Some(participant) = &self.participant {
            return participant.decision_proof(max_objects);
        }
        if let Some(certificate) = &self.fast_certificate {
            return Ok(Some(CheckpointDecisionProof::Fast(certificate.clone())));
        }
        self.observer.as_ref().unwrap().decision_proof(max_objects)
    }
    pub fn is_slow_participant(&self) -> bool {
        self.participant.is_some()
    }
    pub fn pending_early(&self) -> usize {
        self.early.len()
    }
    pub fn forget_connection(&mut self, peer: &P) {
        self.early.retain(|(p, _)| p != peer);
    }

    fn decide(&mut self, value: BlockHash, out: &mut Vec<DriverEffect<P>>) {
        match self.decision {
            None => {
                self.decision = Some(value);
                out.push(DriverEffect::Decided(value));
            }
            Some(previous) if previous != value => {
                out.push(DriverEffect::Rejected(CheckpointError::InvalidEvidence))
            }
            _ => {}
        }
    }
    fn slow_effects(
        &mut self,
        peer: Option<&P>,
        effects: Vec<SlowEffect>,
        out: &mut Vec<DriverEffect<P>>,
    ) {
        for effect in effects {
            match effect {
                SlowEffect::Broadcast(m) => {
                    out.push(DriverEffect::Broadcast(CheckpointMessage::Slow(m)))
                }
                SlowEffect::Reply(m) => {
                    if let Some(peer) = peer {
                        out.push(DriverEffect::Reply(
                            peer.clone(),
                            CheckpointMessage::Slow(m),
                        ));
                    }
                }
                SlowEffect::Decided { value, .. } => self.decide(value, out),
                SlowEffect::Rejected { reason, .. } => out.push(DriverEffect::Rejected(reason)),
            }
        }
    }
    fn participant_effects(
        &mut self,
        peer: Option<&P>,
        effects: Vec<ParticipantEffect>,
        out: &mut Vec<DriverEffect<P>>,
    ) {
        for effect in effects {
            match effect {
                ParticipantEffect::Fast(m) => {
                    out.push(DriverEffect::Broadcast(CheckpointMessage::Fast(m)))
                }
                ParticipantEffect::Slow(e) => self.slow_effects(peer, vec![e], out),
                ParticipantEffect::Decided(v) => self.decide(v, out),
            }
        }
    }
    pub fn receive(
        &mut self,
        peer: P,
        message: CheckpointMessage,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<DriverEffect<P>>, CheckpointError> {
        let mut out = Vec::new();
        match message {
            CheckpointMessage::Application(_) => return Err(CheckpointError::WrongPhase),
            CheckpointMessage::Fast(message) => {
                super::slow_io::encoded_len(&message, SlowIoLimits::default().max_message_bytes)?;
                if let Some(participant) = self.participant.as_mut() {
                    let effects = participant.receive_fast(message, evidence)?;
                    self.participant_effects(Some(&peer), effects, &mut out);
                } else {
                    match message {
                        FastMessage::First(vote) => {
                            let value = vote.value;
                            self.pool.receive(vote, evidence)?;
                            if let Some(fc) = self.pool.fast_certificate(value) {
                                self.accept_fast(fc, evidence, &mut out)?;
                            }
                        }
                        FastMessage::Certificate(fc) => self.accept_fast(fc, evidence, &mut out)?,
                        FastMessage::Recovery(rc) => {
                            rc.verify(self.pool.context(), 0, evidence)?;
                        }
                    }
                }
            }
            CheckpointMessage::Slow(message) => {
                message.encoded_len(SlowIoLimits::default().max_message_bytes)?;
                if let Some(participant) = self.participant.as_mut() {
                    if !participant.fallback_started() {
                        // Authenticate before retaining signed traffic. Proof
                        // ancestry is checked by the replica after admission.
                        match &message {
                            SlowMessage::Request(r) => r.authenticate(&self.slow)?,
                            SlowMessage::Response(r) => r.authenticate(&self.slow)?,
                            SlowMessage::Certificate(c) if c.instance != self.slow.instance() => {
                                return Err(CheckpointError::WrongInstance);
                            }
                            _ => {}
                        }
                        if !self.early.iter().any(|(p, m)| p == &peer && m == &message) {
                            self.early.push_back((peer, message));
                        }
                        return Ok(out);
                    }
                    let effects = participant.receive_slow(message, evidence)?;
                    self.participant_effects(Some(&peer), effects, &mut out);
                } else {
                    let effects = self.observer.as_mut().unwrap().receive(message, evidence)?;
                    self.slow_effects(Some(&peer), effects, &mut out);
                }
            }
        }
        self.drain_early(evidence, &mut out);
        Ok(out)
    }
    fn accept_fast(
        &mut self,
        fc: FastCertificate,
        evidence: &impl CheckpointEvidence,
        out: &mut Vec<DriverEffect<P>>,
    ) -> Result<(), CheckpointError> {
        let value = fc.verify(self.pool.context(), 0, evidence)?;
        if self.decision.is_some_and(|v| v != value) {
            return Err(CheckpointError::InvalidEvidence);
        }
        self.decide(value, out);
        if self.fast_certificate.is_none() {
            self.fast_certificate = Some(fc.clone());
            out.push(DriverEffect::Broadcast(CheckpointMessage::Fast(
                FastMessage::Certificate(fc),
            )));
        }
        Ok(())
    }
    fn drain_early(&mut self, evidence: &impl CheckpointEvidence, out: &mut Vec<DriverEffect<P>>) {
        if !self
            .participant
            .as_ref()
            .is_some_and(|p| p.fallback_started())
        {
            return;
        }
        while let Some((peer, message)) = self.early.pop_front() {
            match self
                .participant
                .as_mut()
                .unwrap()
                .receive_slow(message, evidence)
            {
                Ok(effects) => self.participant_effects(Some(&peer), effects, out),
                Err(error) => out.push(DriverEffect::Rejected(error)),
            }
        }
    }
    pub fn evidence_updated(
        &mut self,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<DriverEffect<P>>, CheckpointError> {
        let mut out = Vec::new();
        if let Some(participant) = self.participant.as_mut() {
            let effects = participant.evidence_updated(evidence)?;
            self.participant_effects(None, effects, &mut out);
        } else {
            self.pool.retry_pending(evidence);
            let values: std::collections::BTreeSet<_> =
                self.pool.snapshot().iter().map(|v| v.value).collect();
            for value in values {
                if let Some(fc) = self.pool.fast_certificate(value) {
                    self.accept_fast(fc, evidence, &mut out)?;
                }
            }
            let effects = self.observer.as_mut().unwrap().evidence_updated(evidence);
            self.slow_effects(None, effects, &mut out);
        }
        self.drain_early(evidence, &mut out);
        Ok(out)
    }

    pub fn retry(
        &mut self,
        evidence: &impl CheckpointEvidence,
    ) -> Result<Vec<DriverEffect<P>>, CheckpointError> {
        let mut out = Vec::new();
        if let Some(vote) = &self.local_first {
            out.push(DriverEffect::Broadcast(CheckpointMessage::Fast(
                FastMessage::First(vote.clone()),
            )));
        }
        if let Some(participant) = self.participant.as_mut() {
            let effects = participant.retry(evidence)?;
            self.participant_effects(None, effects, &mut out);
        } else {
            self.pool.retry_pending(evidence);
            let values: std::collections::BTreeSet<_> =
                self.pool.snapshot().iter().map(|v| v.value).collect();
            for value in values {
                if let Some(fc) = self.pool.fast_certificate(value) {
                    self.accept_fast(fc, evidence, &mut out)?;
                }
            }
            let effects = self.observer.as_mut().unwrap().retry(evidence);
            self.slow_effects(None, effects, &mut out);
            if let Some(rc) = &self.recovery_certificate {
                out.push(DriverEffect::Broadcast(CheckpointMessage::Fast(
                    FastMessage::Recovery(rc.clone()),
                )));
            }
            if let Some(fc) = &self.fast_certificate {
                out.push(DriverEffect::Broadcast(CheckpointMessage::Fast(
                    FastMessage::Certificate(fc.clone()),
                )));
            }
        }
        self.drain_early(evidence, &mut out);
        Ok(out)
    }
}
