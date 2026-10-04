//! Read-only decision observer. No key, proposer or responder is created.
use super::*;
use rsnano_types::BlockHash;
use std::collections::BTreeSet;

pub struct CheckpointObserver {
    fast: CheckpointContext,
    slow: SlowContext,
    store: SlowProofStore,
    pending: BTreeSet<BlockHash>,
    requested: BTreeSet<BlockHash>,
    decision: Option<(BlockHash, BlockHash)>,
    limits: VerificationLimits,
    max_message_bytes: usize,
}
impl CheckpointObserver {
    pub fn new(
        fast: CheckpointContext,
        limits: VerificationLimits,
        max_message_bytes: usize,
    ) -> Result<Self, CheckpointError> {
        if limits.max_depth == 0 || limits.max_objects == 0 {
            return Err(CheckpointError::VerificationLimit);
        }
        if max_message_bytes == 0 {
            return Err(CheckpointError::InvalidSize);
        }
        Ok(Self {
            slow: SlowContext::new(&fast),
            fast,
            store: SlowProofStore::default(),
            pending: BTreeSet::new(),
            requested: BTreeSet::new(),
            decision: None,
            limits,
            max_message_bytes,
        })
    }
    pub fn decision(&self) -> Option<(BlockHash, BlockHash)> {
        self.decision
    }
    pub fn decision_proof(
        &self,
        max_objects: usize,
    ) -> Result<Option<CheckpointDecisionProof>, CheckpointError> {
        self.decision
            .map(|(_, root)| {
                SlowDecisionProof::collect(&self.store, root, max_objects)
                    .map(CheckpointDecisionProof::Slow)
            })
            .transpose()
    }
    pub fn set_verification_limits(
        &mut self,
        limits: VerificationLimits,
    ) -> Result<(), CheckpointError> {
        if limits.max_depth == 0 || limits.max_objects == 0 {
            return Err(CheckpointError::VerificationLimit);
        }
        self.limits = limits;
        Ok(())
    }
    pub fn receive(
        &mut self,
        message: SlowMessage,
        application: &impl CheckpointEvidence,
    ) -> Result<Vec<SlowEffect>, CheckpointError> {
        message.encoded_len(self.max_message_bytes)?;
        let mut effects = Vec::new();
        match message {
            SlowMessage::Request(request) => {
                request.authenticate(&self.slow)?;
                let id = self.store.insert_request(request);
                self.requested.remove(&id);
            }
            SlowMessage::Certificate(certificate) => {
                if certificate.instance != self.slow.instance() {
                    return Err(CheckpointError::WrongInstance);
                }
                let id = self.store.insert_certificate(certificate);
                self.requested.remove(&id);
            }
            SlowMessage::Decision(id) => {
                self.pending.insert(id);
            }
            SlowMessage::Fetch(id) => {
                if let Ok(request) = self.store.request(id) {
                    effects.push(SlowEffect::Reply(SlowMessage::Request(request.clone())));
                } else if let Ok(certificate) = self.store.certificate(id) {
                    effects.push(SlowEffect::Reply(SlowMessage::Certificate(
                        certificate.clone(),
                    )));
                }
            }
            SlowMessage::Response(response) => {
                response.authenticate(&self.slow)?;
            }
        }
        effects.extend(self.pump(application));
        Ok(effects)
    }
    pub fn evidence_updated(&mut self, application: &impl CheckpointEvidence) -> Vec<SlowEffect> {
        self.pump(application)
    }

    /// Explicit retries reopen lost fetches. Proofs are retained after deciding.
    pub fn retry(&mut self, application: &impl CheckpointEvidence) -> Vec<SlowEffect> {
        self.requested.clear();
        let mut effects = self.pump(application);
        if let Some((_, certificate)) = self.decision {
            effects.push(SlowEffect::Broadcast(SlowMessage::Decision(certificate)));
        }
        effects
    }
    fn pump(&mut self, application: &impl CheckpointEvidence) -> Vec<SlowEffect> {
        let mut effects = Vec::new();
        for id in self.pending.iter().copied().collect::<Vec<_>>() {
            let result = self
                .store
                .verifier(&self.slow, &self.fast, application, self.limits)
                .verify_certificate(id);
            match result {
                Ok(proof) => {
                    self.pending.remove(&id);
                    match proof.result() {
                        SlowResult::B(BOutcome::Commit(value)) => match self.decision {
                            Some((previous, _)) if previous != value => {
                                effects.push(SlowEffect::Rejected {
                                    object: id,
                                    reason: CheckpointError::InvalidEvidence,
                                })
                            }
                            Some(_) => {}
                            None => {
                                self.decision = Some((value, id));
                                effects.push(SlowEffect::Decided {
                                    value,
                                    certificate: id,
                                });
                                effects.push(SlowEffect::Broadcast(SlowMessage::Decision(id)));
                            }
                        },
                        _ => effects.push(SlowEffect::Rejected {
                            object: id,
                            reason: CheckpointError::InvalidEvidence,
                        }),
                    }
                }
                Err(CheckpointError::MissingEvidence(missing)) => {
                    if self.requested.insert(missing) {
                        effects.push(SlowEffect::Broadcast(SlowMessage::Fetch(missing)));
                    }
                }
                Err(reason) => {
                    if reason != CheckpointError::VerificationLimit {
                        self.pending.remove(&id);
                    }
                    effects.push(SlowEffect::Rejected { object: id, reason });
                }
            }
        }
        effects
    }
}
