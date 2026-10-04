//! In-memory proposer loop. Transport and synchronized request/response release
//! are external: this component never treats a timeout as quorum evidence.
use super::*;
use rsnano_types::{BlockHash, PrivateKey, PublicKey};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProposerProgress {
    Waiting,
    Broadcast(SlowRequest),
    Decided {
        value: BlockHash,
        certificate: BlockHash,
    },
}

pub struct SlowProposer {
    instance: BlockHash,
    signer: PublicKey,
    current: SlowRequest,
    retained_r: SlowEntry,
    responses: BTreeMap<BlockHash, BTreeMap<PublicKey, Vec<SlowResponse>>>,
    decision: Option<(BlockHash, BlockHash)>,
}
impl SlowProposer {
    pub fn start(
        slow: &SlowContext,
        fast: &CheckpointContext,
        evidence: &impl CheckpointEvidence,
        proposal: SlowProposal,
        key: &PrivateKey,
        store: &mut SlowProofStore,
    ) -> Result<Self, CheckpointError> {
        let value = proposal.verify(slow, fast, evidence)?;
        let current = SlowRequest::sign(
            slow,
            0,
            SlowValue::R { rank: 0, value },
            SlowJustification::Initial(proposal),
            key,
        )?;
        let origin = store.insert_request(current.clone());
        Ok(Self {
            instance: slow.instance(),
            signer: key.public_key(),
            retained_r: SlowEntry {
                value: current.value,
                origin,
            },
            current,
            responses: BTreeMap::new(),
            decision: None,
        })
    }
    pub fn current_request(&self) -> &SlowRequest {
        &self.current
    }
    pub fn decision(&self) -> Option<(BlockHash, BlockHash)> {
        self.decision
    }

    /// Called for a delivered, fully justified R request. A future responder
    /// service also updates its own R/A/B state and broadcasts a signed response.
    pub fn observe_request(
        &mut self,
        slow: &SlowContext,
        fast: &CheckpointContext,
        application: &impl CheckpointEvidence,
        store: &SlowProofStore,
        id: BlockHash,
        limits: VerificationLimits,
    ) -> Result<(), CheckpointError> {
        if slow.instance() != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        store
            .verifier(slow, fast, application, limits)
            .verify_request(id)?;
        let request = store.request(id)?;
        if request.phase() == SlowPhase::R {
            self.merge_verified_r(SlowEntry {
                value: request.value,
                origin: id,
            });
        }
        Ok(())
    }

    pub(super) fn merge_verified_r(&mut self, entry: SlowEntry) {
        debug_assert!(matches!(entry.value, SlowValue::R { .. }));
        if entry.value > self.retained_r.value {
            self.retained_r = entry;
        }
    }

    /// Retain authenticated snapshots even when ancestry has not arrived yet.
    /// Duplicates do not count; historical snapshots are not overwritten by late
    /// delivery of an older state. Service-level storage bounds remain required.
    pub fn receive(
        &mut self,
        context: &SlowContext,
        response: SlowResponse,
    ) -> Result<(), CheckpointError> {
        if context.instance() != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        response.authenticate(context)?;
        let history = self
            .responses
            .entry(response.request)
            .or_default()
            .entry(response.signer)
            .or_default();
        if !history.contains(&response) {
            history.push(response);
        }
        Ok(())
    }

    fn witnesses(
        &self,
        slow: &SlowContext,
        phase: SlowPhase,
        rank: u64,
        origin: BlockHash,
    ) -> Option<Vec<SlowResponse>> {
        let answers: Vec<_> = self
            .responses
            .get(&origin)?
            .values()
            .filter_map(|history| {
                history
                    .iter()
                    .rev()
                    .find(|r| r.rank == rank && r.phase == phase)
                    .cloned()
            })
            .take(slow.quorum() as usize)
            .collect();
        (answers.len() as u64 == slow.quorum()).then_some(answers)
    }

    pub fn poll(
        &mut self,
        slow: &SlowContext,
        fast: &CheckpointContext,
        application: &impl CheckpointEvidence,
        key: &PrivateKey,
        store: &mut SlowProofStore,
        limits: VerificationLimits,
    ) -> Result<ProposerProgress, CheckpointError> {
        if slow.instance() != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        if key.public_key() != self.signer {
            return Err(CheckpointError::NonMember);
        }
        if let Some((value, certificate)) = self.decision {
            return Ok(ProposerProgress::Decided { value, certificate });
        }
        let id = self.current.digest();
        let verifier = store.verifier(slow, fast, application, limits);
        let mut responses = Vec::new();
        let mut witnesses = BTreeMap::new();
        if let Some(histories) = self.responses.get(&id) {
            for history in histories.values() {
                for response in history.iter().rev() {
                    if response.phase != self.current.phase() || response.rank != self.current.rank
                    {
                        continue;
                    }
                    let mut attachments = BTreeMap::new();
                    let mut eligible = true;
                    for entry in &response.entries {
                        let rank = entry.value.origin_rank(response.rank);
                        match verifier.validate_origin(
                            slow,
                            response.phase,
                            rank,
                            entry.origin,
                            entry.value,
                        ) {
                            Ok(()) => {}
                            Err(CheckpointError::VerificationLimit) => {
                                return Err(CheckpointError::VerificationLimit);
                            }
                            Err(_) => {
                                eligible = false;
                                break;
                            }
                        }
                        let Some(answers) =
                            self.witnesses(slow, response.phase, rank, entry.origin)
                        else {
                            eligible = false;
                            break;
                        };
                        attachments.insert(entry.origin, answers);
                    }
                    if eligible {
                        responses.push(response.clone());
                        witnesses.extend(attachments);
                        break;
                    }
                }
                if responses.len() as u64 == slow.quorum() {
                    break;
                }
            }
        }
        if responses.len() as u64 != slow.quorum() {
            return Ok(ProposerProgress::Waiting);
        }
        let mut retained_r = None;
        if self.current.phase() == SlowPhase::R {
            let quorum_max = responses.iter().map(|r| r.entries[0].value).max().unwrap();
            if self.retained_r.value > quorum_max {
                let rank = self.retained_r.value.origin_rank(self.current.rank);
                let Some(answers) =
                    self.witnesses(slow, SlowPhase::R, rank, self.retained_r.origin)
                else {
                    return Ok(ProposerProgress::Waiting);
                };
                witnesses.insert(self.retained_r.origin, answers);
                retained_r = Some(SlowResponse::sign(
                    slow,
                    self.current.rank,
                    SlowPhase::R,
                    id,
                    vec![self.retained_r.clone()],
                    key,
                )?);
            }
        }
        let certificate = SlowCertificate {
            instance: self.instance,
            rank: self.current.rank,
            phase: self.current.phase(),
            request: id,
            responses,
            retained_r,
            witnesses: witnesses
                .into_iter()
                .map(|(origin, responses)| OriginWitnesses { origin, responses })
                .collect(),
        };
        let verified = certificate.verify(slow, &verifier)?;
        let next = verified.next_action()?;
        let selected_r = (certificate.phase == SlowPhase::R).then(|| {
            certificate
                .responses
                .iter()
                .chain(certificate.retained_r.iter())
                .map(|r| r.entries[0].clone())
                .max_by_key(|entry| entry.value)
                .unwrap()
        });
        let certificate_id = store.insert_certificate(certificate);
        let (rank, value) = match next {
            SlowAction::Decide(value) => {
                self.decision = Some((value, certificate_id));
                return Ok(ProposerProgress::Decided {
                    value,
                    certificate: certificate_id,
                });
            }
            SlowAction::R { rank, value } => (rank, SlowValue::R { rank, value }),
            SlowAction::A { rank, value } => (rank, SlowValue::A(value)),
            SlowAction::B { rank, value } => (rank, SlowValue::B(value)),
        };
        let request = SlowRequest::sign(
            slow,
            rank,
            value,
            SlowJustification::Previous(certificate_id),
            key,
        )?;
        let id = store.insert_request(request.clone());
        // The next request is derived solely from the certificate just verified.
        // Do not fail after publishing it to the store due to a second traversal
        // budget: that would leave the proposer retrying an already signed step.
        if let Some(selected) = selected_r {
            if selected.value > self.retained_r.value {
                self.retained_r = selected;
            }
        }
        if value.phase() == SlowPhase::R && value > self.retained_r.value {
            self.retained_r = SlowEntry { value, origin: id };
        }
        self.current = request.clone();
        Ok(ProposerProgress::Broadcast(request))
    }
}
