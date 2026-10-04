//! Transport-independent slow replica and proof relay. There are no clocks here:
//! Per-message admission and response-replay batches are bounded; network
//! scheduling, retry timers, retained-storage bounds and synchronization remain
//! responsibilities of the eventual node service.
use super::slow_io::ResponseReplay;
use super::*;
use rsnano_types::{Blake2HashBuilder, BlockHash, PrivateKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlowMessage {
    Request(SlowRequest),
    Response(SlowResponse),
    Certificate(SlowCertificate),
    Fetch(BlockHash),
    Decision(BlockHash),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlowEffect {
    Broadcast(SlowMessage),
    /// Send to the peer that sent the current Fetch, rather than broadcasting.
    Reply(SlowMessage),
    Decided {
        value: BlockHash,
        certificate: BlockHash,
    },
    /// Local diagnostic, never a network message or a consensus outcome.
    /// VerificationLimit retains pending work for a retry with a larger budget.
    Rejected {
        object: BlockHash,
        reason: CheckpointError,
    },
}

impl SlowResponse {
    pub fn digest(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI slow response object v1.3")
            .update(self.signing_hash().as_bytes())
            .update(self.signature.as_bytes())
            .build()
    }
}

pub struct SlowReplica {
    fast: CheckpointContext,
    slow: SlowContext,
    key: PrivateKey,
    store: SlowProofStore,
    proposer: SlowProposer,
    responder: SlowResponder,
    pending_requests: BTreeSet<BlockHash>,
    wanted_requests: BTreeSet<BlockHash>,
    requested_fetches: BTreeSet<BlockHash>,
    pending_decisions: BTreeSet<BlockHash>,
    relayed_requests: BTreeSet<BlockHash>,
    relayed_responses: BTreeMap<BlockHash, SlowResponse>,
    decision: Option<(BlockHash, BlockHash)>,
    limits: VerificationLimits,
    io_limits: SlowIoLimits,
    replay: ResponseReplay,
}
impl SlowReplica {
    pub fn new(
        fast: CheckpointContext,
        key: PrivateKey,
        proposal: SlowProposal,
        application: &impl CheckpointEvidence,
        limits: VerificationLimits,
    ) -> Result<Self, CheckpointError> {
        Self::new_with_io_limits(
            fast,
            key,
            proposal,
            application,
            limits,
            SlowIoLimits::default(),
        )
    }

    pub fn new_with_io_limits(
        fast: CheckpointContext,
        key: PrivateKey,
        proposal: SlowProposal,
        application: &impl CheckpointEvidence,
        limits: VerificationLimits,
        io_limits: SlowIoLimits,
    ) -> Result<Self, CheckpointError> {
        io_limits.validate_for_replica()?;
        if limits.max_depth == 0 || limits.max_objects == 0 {
            return Err(CheckpointError::VerificationLimit);
        }
        let slow = SlowContext::new(&fast);
        let mut store = SlowProofStore::default();
        let proposer = SlowProposer::start(&slow, &fast, application, proposal, &key, &mut store)?;
        SlowMessage::Request(proposer.current_request().clone())
            .encoded_len(io_limits.max_message_bytes)?;
        let responder = SlowResponder::new(&slow, key.public_key())?;
        let pending_requests = BTreeSet::from([proposer.current_request().digest()]);
        Ok(Self {
            fast,
            slow,
            key,
            store,
            proposer,
            responder,
            pending_requests,
            wanted_requests: BTreeSet::new(),
            requested_fetches: BTreeSet::new(),
            pending_decisions: BTreeSet::new(),
            relayed_requests: BTreeSet::new(),
            relayed_responses: BTreeMap::new(),
            decision: None,
            limits,
            io_limits,
            replay: ResponseReplay::default(),
        })
    }
    /// Local resource policy, independent of consensus rank and signed evidence.
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

    /// Changes local admission and response-replay policy without evicting any
    /// proof or vote. Reject reductions that would strand retained responses.
    pub fn set_io_limits(&mut self, limits: SlowIoLimits) -> Result<(), CheckpointError> {
        limits.validate_for_replica()?;
        SlowMessage::Request(self.proposer.current_request().clone())
            .encoded_len(limits.max_message_bytes)?;
        for response in self.relayed_responses.values() {
            SlowMessage::Response(response.clone()).encoded_len(limits.max_message_bytes)?;
        }
        self.io_limits = limits;
        Ok(())
    }

    pub fn decision(&self) -> Option<(BlockHash, BlockHash)> {
        self.decision
    }
    pub fn current_request(&self) -> &SlowRequest {
        self.proposer.current_request()
    }
    pub fn proof_store(&self) -> &SlowProofStore {
        &self.store
    }

    /// Pump after construction, after payload reconstruction, or on an external
    /// retry timer. Reissues missing evidence fetches; it never fabricates votes.
    pub fn retry(
        &mut self,
        application: &impl CheckpointEvidence,
    ) -> Result<Vec<SlowEffect>, CheckpointError> {
        // Reserve a bounded, fair slice of exact signed response history.
        // The current request/decision and fresh pump effects are separate from
        // this historical-response budget and are not delayed behind old data.
        let mut effects: Vec<_> = self
            .replay
            .batch(&self.relayed_responses, self.io_limits)?
            .into_iter()
            .map(SlowEffect::Broadcast)
            .collect();
        self.requested_fetches.clear();
        if self
            .relayed_requests
            .contains(&self.proposer.current_request().digest())
        {
            effects.push(SlowEffect::Broadcast(SlowMessage::Request(
                self.proposer.current_request().clone(),
            )));
        }
        if let Some((_, certificate)) = self.decision {
            effects.push(SlowEffect::Broadcast(SlowMessage::Certificate(
                self.store.certificate(certificate)?.clone(),
            )));
            effects.push(SlowEffect::Broadcast(SlowMessage::Decision(certificate)));
        }
        self.pump(application, effects)
    }

    /// Revisit pending verification after application evidence arrives without
    /// replaying response history or reopening unrelated lost-message fetches.
    pub fn evidence_updated(
        &mut self,
        application: &impl CheckpointEvidence,
    ) -> Result<Vec<SlowEffect>, CheckpointError> {
        self.pump(application, Vec::new())
    }

    pub fn receive(
        &mut self,
        message: SlowMessage,
        application: &impl CheckpointEvidence,
    ) -> Result<Vec<SlowEffect>, CheckpointError> {
        message.encoded_len(self.io_limits.max_message_bytes)?;
        let mut effects = Vec::new();
        match message {
            SlowMessage::Request(request) => {
                request.authenticate(&self.slow)?;
                let id = self.store.insert_request(request);
                self.pending_requests.insert(id);
            }
            SlowMessage::Response(response) => self.accept_response(response, &mut effects)?,
            SlowMessage::Certificate(certificate) => {
                if certificate.instance != self.slow.instance() {
                    return Err(CheckpointError::WrongInstance);
                }
                self.store.insert_certificate(certificate);
            }
            SlowMessage::Fetch(id) => {
                if let Ok(request) = self.store.request(id) {
                    effects.push(SlowEffect::Reply(SlowMessage::Request(request.clone())));
                } else if let Ok(certificate) = self.store.certificate(id) {
                    effects.push(SlowEffect::Reply(SlowMessage::Certificate(
                        certificate.clone(),
                    )));
                }
                // Unknown hashes may refer to application data. The transport
                // adapter also routes Fetch to the payload/evidence service.
            }
            SlowMessage::Decision(id) => {
                self.pending_decisions.insert(id);
            }
        }
        self.pump(application, effects)
    }

    fn accept_response(
        &mut self,
        response: SlowResponse,
        effects: &mut Vec<SlowEffect>,
    ) -> Result<(), CheckpointError> {
        SlowMessage::Response(response.clone()).encoded_len(self.io_limits.max_message_bytes)?;
        response.authenticate(&self.slow)?;
        let id = response.digest();
        if !self.relayed_responses.contains_key(&id) {
            self.relayed_responses.insert(id, response.clone());
            self.replay.remember(id);
            self.proposer.receive(&self.slow, response.clone())?;
            effects.push(SlowEffect::Broadcast(SlowMessage::Response(
                response.clone(),
            )));
            for request in
                std::iter::once(response.request).chain(response.entries.iter().map(|e| e.origin))
            {
                if self.store.request(request).is_err() {
                    self.wanted_requests.insert(request);
                    effects.push(SlowEffect::Broadcast(SlowMessage::Fetch(request)));
                } else {
                    // Recheck fetched origins too: a response alone is not
                    // authority to mutate responder state or count a quorum.
                    self.pending_requests.insert(request);
                }
            }
        }
        Ok(())
    }

    fn decide(
        &mut self,
        value: BlockHash,
        certificate: BlockHash,
        effects: &mut Vec<SlowEffect>,
    ) -> Result<(), CheckpointError> {
        if let Some((old, _)) = self.decision {
            if old != value {
                return Err(CheckpointError::InvalidEvidence);
            }
            return Ok(());
        }
        self.decision = Some((value, certificate));
        effects.push(SlowEffect::Broadcast(SlowMessage::Decision(certificate)));
        effects.push(SlowEffect::Decided { value, certificate });
        Ok(())
    }

    fn pump(
        &mut self,
        application: &impl CheckpointEvidence,
        mut effects: Vec<SlowEffect>,
    ) -> Result<Vec<SlowEffect>, CheckpointError> {
        for id in self.wanted_requests.iter().copied().collect::<Vec<_>>() {
            if self.store.request(id).is_ok() {
                self.wanted_requests.remove(&id);
                self.pending_requests.insert(id);
            } else {
                effects.push(SlowEffect::Broadcast(SlowMessage::Fetch(id)));
            }
        }
        // The loop normally advances at most one phase. It can run through R/A/B
        // locally when f=0. A bounded pump prevents monopolizing the event loop.
        for _ in 0..self.limits.max_objects {
            let pending: Vec<_> = self.pending_requests.iter().copied().collect();
            for id in pending {
                match self.responder.deliver(
                    &self.slow,
                    &self.fast,
                    application,
                    &self.store,
                    id,
                    &self.key,
                    self.limits,
                ) {
                    Ok(response) => {
                        let value = self.store.request(id)?.value;
                        if value.phase() == SlowPhase::R {
                            self.proposer
                                .merge_verified_r(SlowEntry { value, origin: id });
                        }
                        self.pending_requests.remove(&id);
                        if self.relayed_requests.insert(id) {
                            effects.push(SlowEffect::Broadcast(SlowMessage::Request(
                                self.store.request(id)?.clone(),
                            )));
                        }
                        self.accept_response(response, &mut effects)?;
                        // Self responses may name this same origin. Its input has
                        // just been processed; do not repeatedly redeliver it.
                        self.pending_requests.remove(&id);
                    }
                    Err(CheckpointError::MissingEvidence(missing)) => {
                        effects.push(SlowEffect::Broadcast(SlowMessage::Fetch(missing)))
                    }
                    Err(CheckpointError::VerificationLimit) => {
                        effects.push(SlowEffect::Rejected {
                            object: id,
                            reason: CheckpointError::VerificationLimit,
                        });
                    }
                    Err(reason) => {
                        self.pending_requests.remove(&id);
                        effects.push(SlowEffect::Rejected { object: id, reason });
                    }
                }
            }
            for id in self.pending_decisions.iter().copied().collect::<Vec<_>>() {
                match self
                    .store
                    .verifier(&self.slow, &self.fast, application, self.limits)
                    .verify_certificate(id)
                {
                    Ok(proof) => {
                        self.pending_decisions.remove(&id);
                        match proof.result() {
                            SlowResult::B(BOutcome::Commit(value)) => {
                                self.decide(value, id, &mut effects)?
                            }
                            _ => effects.push(SlowEffect::Rejected {
                                object: id,
                                reason: CheckpointError::InvalidEvidence,
                            }),
                        }
                    }
                    Err(CheckpointError::MissingEvidence(missing)) => {
                        effects.push(SlowEffect::Broadcast(SlowMessage::Fetch(missing)))
                    }
                    Err(CheckpointError::VerificationLimit) => {
                        effects.push(SlowEffect::Rejected {
                            object: id,
                            reason: CheckpointError::VerificationLimit,
                        });
                    }
                    Err(reason) => {
                        self.pending_decisions.remove(&id);
                        effects.push(SlowEffect::Rejected { object: id, reason });
                    }
                }
            }
            if self.decision.is_some() {
                break;
            }
            let progress = self.proposer.poll(
                &self.slow,
                &self.fast,
                application,
                &self.key,
                &mut self.store,
                self.limits,
            );
            let progress = match progress {
                Ok(progress) => progress,
                Err(reason) => {
                    effects.push(SlowEffect::Rejected {
                        object: self.proposer.current_request().digest(),
                        reason,
                    });
                    break;
                }
            };
            match progress {
                ProposerProgress::Waiting => break,
                ProposerProgress::Broadcast(request) => {
                    if let SlowJustification::Previous(id) = request.justification {
                        let certificate = self.store.certificate(id)?;
                        if certificate.phase == SlowPhase::R {
                            let maximum = certificate
                                .responses
                                .iter()
                                .chain(certificate.retained_r.iter())
                                .map(|r| r.entries[0].clone())
                                .max_by_key(|entry| entry.value)
                                .unwrap();
                            self.responder.merge_verified_r(maximum);
                        }
                        effects.push(SlowEffect::Broadcast(SlowMessage::Certificate(
                            certificate.clone(),
                        )));
                    }
                    self.pending_requests.insert(request.digest());
                    // Next iteration validates and serves the new local request.
                }
                ProposerProgress::Decided { value, certificate } => {
                    effects.push(SlowEffect::Broadcast(SlowMessage::Certificate(
                        self.store.certificate(certificate)?.clone(),
                    )));
                    self.decide(value, certificate, &mut effects)?;
                    break;
                }
            }
        }
        // Suppress repeated fetches across inbound events to avoid a gossip
        // feedback loop. Explicit retry() reopens them after loss.
        effects.retain(|effect| match effect {
            SlowEffect::Broadcast(SlowMessage::Fetch(id)) => self.requested_fetches.insert(*id),
            _ => true,
        });
        Ok(effects)
    }
}
