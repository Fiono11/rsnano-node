//! One-instance node election adapter. The owner supplies report validation and
//! drains network effects; node-wide instance routing remains external.
use super::*;
use crate::consensus::{
    election::EpochValue,
    reports::{CheckpointDecision, CheckpointElection},
};
use rsnano_messages::CheckpointFrame;
use rsnano_types::PrivateKey;
use std::collections::VecDeque;

pub struct CheckpointElectionAdapter<P: Ord, V> {
    session: CheckpointSession<P>,
    transport: CheckpointTransport<P>,
    journals: CheckpointSessionJournals,
    validate: V,
    effects: VecDeque<DriverEffect<P>>,
    submitted: bool,
    delivered: bool,
    proof_bytes: usize,
}
impl<P: Ord + Clone, V> CheckpointElectionAdapter<P, V>
where
    V: FnMut(CheckpointCandidate) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
{
    pub fn new(
        context: CheckpointContext,
        key: PrivateKey,
        journals: CheckpointSessionJournals,
        validate: V,
        proof_bytes: usize,
    ) -> Result<Self, CheckpointError> {
        if proof_bytes == 0 {
            return Err(CheckpointError::InvalidSize);
        }
        let transport =
            CheckpointTransport::new(&context, SlowIoLimits::default().max_message_bytes)?;
        Ok(Self {
            session: CheckpointSession::new(context, key)?,
            transport,
            journals,
            validate,
            effects: VecDeque::new(),
            submitted: false,
            delivered: false,
            proof_bytes,
        })
    }
    pub fn receive(&mut self, peer: P, frame: &CheckpointFrame) -> Result<(), CheckpointError> {
        if let Some(message) = self.transport.receive(peer.clone(), frame)? {
            let effects =
                self.session
                    .receive(peer, message, &mut self.journals, &mut self.validate)?;
            self.effects.extend(effects);
        }
        Ok(())
    }
    pub(super) fn receive_payload(&mut self, peer: P, bytes: &[u8]) -> Result<(), CheckpointError> {
        let message = self.transport.decode_payload(bytes)?;
        self.effects.extend(self.session.receive(
            peer,
            message,
            &mut self.journals,
            &mut self.validate,
        )?);
        Ok(())
    }
    pub fn encode(
        &self,
        message: &CheckpointMessage,
    ) -> Result<Vec<CheckpointFrame>, CheckpointError> {
        self.transport.encode(message)
    }
    pub fn drain_effects(&mut self) -> impl Iterator<Item = DriverEffect<P>> + '_ {
        self.effects.drain(..)
    }
    pub fn retry(&mut self) {
        self.effects
            .extend(self.session.retry(&mut self.journals, &mut self.validate));
    }
    pub fn forget_connection(&mut self, peer: &P) {
        self.session.forget_connection(peer);
        self.transport.forget_connection(peer);
        self.effects
            .retain(|effect| !matches!(effect, DriverEffect::Reply(p, _) if p == peer));
    }
    /// Return process-lifetime signing locks when replacing this instance.
    pub fn into_journals(self) -> CheckpointSessionJournals {
        self.journals
    }
}
impl<P: Ord + Clone + Send, V> CheckpointElection for CheckpointElectionAdapter<P, V>
where
    V: FnMut(CheckpointCandidate) -> Result<ValidatedCheckpointCandidate, CheckpointError> + Send,
{
    fn evidence_updated(&mut self) {
        self.effects.extend(
            self.session
                .evidence_updated(&mut self.journals, &mut self.validate),
        );
    }
    fn candidate(&mut self, value: EpochValue) {
        // Ticks may offer a different report selection after the first proposal.
        // Preserve the journaled proposal and avoid replaying it on every tick.
        if self.submitted {
            return;
        }
        match self
            .session
            .propose(value, &mut self.journals, &mut self.validate)
        {
            Ok(effects) => {
                self.submitted = true;
                self.effects.extend(effects);
            }
            Err(error) => self.effects.push_back(DriverEffect::Rejected(error)),
        }
    }
    fn decided(&mut self) -> Option<CheckpointDecision> {
        if self.delivered {
            return None;
        }
        match self
            .session
            .checkpoint_decision(VerificationLimits::default(), self.proof_bytes)
        {
            Ok(Some(decision)) => {
                self.delivered = true;
                Some(decision)
            }
            Ok(None) => None,
            Err(error) => {
                self.effects.push_back(DriverEffect::Rejected(error));
                None
            }
        }
    }
    fn verify(&self, decision: &CheckpointDecision) -> bool {
        self.session
            .verify_decision(decision, VerificationLimits::default(), self.proof_bytes)
            .is_ok()
    }
}
