//! In-memory application evidence dissemination. The caller validates candidate
//! payloads against reconstructed reports and owns the endorsement journal.
use super::*;
use rsnano_types::{BlockHash, PrivateKey, PublicKey};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplicationEffect {
    Broadcast(ApplicationMessage),
    /// Reply to the sender of the Fetch passed to receive, never a pending peer.
    Reply(ApplicationMessage),
    Admitted(BlockHash),
    InitialReady(BlockHash),
    MissingEvidence(BlockHash),
    Rejected(CheckpointError),
}

pub struct ApplicationExchange {
    context: CheckpointContext,
    key: PrivateKey,
    evidence: CandidateEvidence,
    candidates: BTreeMap<BlockHash, CheckpointCandidate>,
    endorsements: BTreeMap<BlockHash, BTreeMap<PublicKey, CandidateEndorsement>>,
    requests: BTreeMap<BlockHash, InitialRRequest>,
    responses: BTreeMap<BlockHash, BTreeMap<PublicKey, InitialRResponse>>,
    completed: BTreeSet<BlockHash>,
    certificates: BTreeSet<BlockHash>,
    responder: InitialRResponder,
    pending: Vec<ApplicationMessage>,
    requested: BTreeSet<BlockHash>,
    max_bytes: usize,
}
impl ApplicationExchange {
    pub fn new(
        context: CheckpointContext,
        key: PrivateKey,
        max_bytes: usize,
    ) -> Result<Self, CheckpointError> {
        if context.committee.weight(&key.public_key()).is_zero() {
            return Err(CheckpointError::NonMember);
        }
        if max_bytes == 0 {
            return Err(CheckpointError::InvalidSize);
        }
        Ok(Self {
            evidence: CandidateEvidence::new(context.instance()),
            responder: InitialRResponder::new(context.instance()),
            context,
            key,
            candidates: Default::default(),
            endorsements: Default::default(),
            requests: Default::default(),
            responses: Default::default(),
            completed: Default::default(),
            certificates: Default::default(),
            pending: Vec::new(),
            requested: Default::default(),
            max_bytes,
        })
    }
    pub fn evidence(&self) -> &CandidateEvidence {
        &self.evidence
    }
    pub fn receive(
        &mut self,
        message: ApplicationMessage,
        journal: &mut CandidateEndorsementJournal,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
    ) -> Result<Vec<ApplicationEffect>, CheckpointError> {
        super::slow_io::encoded_len(&message, self.max_bytes)?;
        // Fetches have no deferred reply destination: respond immediately only.
        if let ApplicationMessage::Fetch(id) = message {
            let reply = if let Some(candidate) = self
                .candidates
                .get(&id)
                .or_else(|| {
                    self.candidates.values().find(|c| {
                        c.value.hash() == id && self.evidence.admission(c.digest()).is_some()
                    })
                })
                .or_else(|| self.candidates.values().find(|c| c.value.hash() == id))
            {
                let cid = candidate.digest();
                Some(if let Some(admission) = self.evidence.admission(cid) {
                    ApplicationMessage::Admission {
                        candidate: candidate.clone(),
                        admission: admission.clone(),
                    }
                } else {
                    ApplicationMessage::Candidate(candidate.clone())
                })
            } else if let Some(request) = self.requests.get(&id) {
                Some(ApplicationMessage::InitialRequest(request.clone()))
            } else {
                self.evidence
                    .initial_r(id)
                    .cloned()
                    .map(ApplicationMessage::InitialCertificate)
            };
            return Ok(reply.into_iter().map(ApplicationEffect::Reply).collect());
        }
        if !self.pending.contains(&message) {
            self.pending.push(message);
        }
        Ok(self.pump(journal, validate))
    }
    /// Recheck parked application objects after local report reconstruction.
    /// Unlike timer retry, this does not replay history or reopen fetches.
    pub fn evidence_updated(
        &mut self,
        journal: &mut CandidateEndorsementJournal,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
    ) -> Vec<ApplicationEffect> {
        self.pump(journal, validate)
    }
    pub fn retry(
        &mut self,
        journal: &mut CandidateEndorsementJournal,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
    ) -> Vec<ApplicationEffect> {
        self.requested.clear();
        let mut effects = self.pump(journal, validate);
        for candidate in self.candidates.values() {
            if let Some(admission) = self.evidence.admission(candidate.digest()) {
                effects.push(ApplicationEffect::Broadcast(
                    ApplicationMessage::Admission {
                        candidate: candidate.clone(),
                        admission: admission.clone(),
                    },
                ));
            } else if candidate.proposer == self.key.public_key() {
                effects.push(ApplicationEffect::Broadcast(ApplicationMessage::Candidate(
                    candidate.clone(),
                )));
            }
        }
        for request in self
            .requests
            .values()
            .filter(|r| r.requester == self.key.public_key())
        {
            effects.push(ApplicationEffect::Broadcast(
                ApplicationMessage::InitialRequest(request.clone()),
            ));
        }
        for id in &self.certificates {
            if let Some(certificate) = self.evidence.initial_r(*id) {
                effects.push(ApplicationEffect::Broadcast(
                    ApplicationMessage::InitialCertificate(certificate.clone()),
                ));
            }
        }
        for endorsements in self.endorsements.values() {
            if let Some(endorsement) = endorsements.get(&self.key.public_key()) {
                effects.push(ApplicationEffect::Broadcast(
                    ApplicationMessage::Endorsement(endorsement.clone()),
                ));
            }
        }
        effects
    }
    fn pump(
        &mut self,
        journal: &mut CandidateEndorsementJournal,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
    ) -> Vec<ApplicationEffect> {
        let mut effects = Vec::new();
        loop {
            let work = std::mem::take(&mut self.pending);
            let mut progressed = false;
            for message in work {
                match self.process(message.clone(), journal, validate, &mut effects) {
                    Ok(()) => progressed = true,
                    Err(CheckpointError::MissingEvidence(id)) => {
                        if !self.pending.contains(&message) {
                            self.pending.push(message.clone());
                        }
                        if self.requested.insert(id) {
                            effects.push(ApplicationEffect::MissingEvidence(id));
                            // A cached candidate whose reports are unavailable
                            // needs application reconstruction, not self-fetch.
                            if !self.candidates.contains_key(&id) {
                                effects.push(ApplicationEffect::Broadcast(
                                    ApplicationMessage::Fetch(id),
                                ));
                            }
                        }
                    }
                    Err(error) => effects.push(ApplicationEffect::Rejected(error)),
                }
            }
            if !progressed || self.pending.is_empty() {
                break;
            }
        }
        effects
    }
    fn process(
        &mut self,
        message: ApplicationMessage,
        journal: &mut CandidateEndorsementJournal,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
        effects: &mut Vec<ApplicationEffect>,
    ) -> Result<(), CheckpointError> {
        match message {
            ApplicationMessage::Candidate(candidate) => {
                candidate.authenticate(&self.context)?;
                let id = candidate.digest();
                self.candidates
                    .entry(id)
                    .or_insert_with(|| candidate.clone());
                let validated = validate(candidate)?;
                // Bind the callback's result to the object it was asked to check.
                if validated.candidate().digest() != id {
                    return Err(CheckpointError::InvalidEvidence);
                }
                let endorsement = journal.endorse(&validated, &self.context, &self.key)?;
                let held = self.endorsements.entry(endorsement.candidate).or_default();
                if !held.contains_key(&endorsement.endorser) {
                    effects.push(ApplicationEffect::Broadcast(
                        ApplicationMessage::Endorsement(endorsement.clone()),
                    ));
                    self.pending
                        .push(ApplicationMessage::Endorsement(endorsement));
                }
                self.maybe_admission(id);
            }
            ApplicationMessage::Endorsement(endorsement) => {
                endorsement.authenticate(&self.context)?;
                let id = endorsement.candidate;
                let candidate = self
                    .candidates
                    .get(&id)
                    .ok_or(CheckpointError::MissingEvidence(id))?;
                if endorsement.proposer != candidate.proposer {
                    return Err(CheckpointError::InvalidEvidence);
                }
                self.endorsements
                    .entry(id)
                    .or_default()
                    .entry(endorsement.endorser)
                    .or_insert(endorsement);
                self.maybe_admission(id);
            }
            ApplicationMessage::Admission {
                candidate,
                admission,
            } => {
                admission.verify(&candidate, &self.context)?;
                let id = candidate.digest();
                if self.evidence.candidate(id).is_some() {
                    return Ok(());
                }
                self.candidates
                    .entry(id)
                    .or_insert_with(|| candidate.clone());
                let validated = validate(candidate.clone())?;
                if validated.candidate().digest() != id {
                    return Err(CheckpointError::InvalidEvidence);
                }
                self.evidence.insert(validated, &admission, &self.context)?;
                effects.push(ApplicationEffect::Admitted(id));
                effects.push(ApplicationEffect::Broadcast(
                    ApplicationMessage::Admission {
                        candidate,
                        admission,
                    },
                ));
            }
            ApplicationMessage::InitialRequest(request) => {
                request.verify(&self.context, &self.evidence)?;
                let id = request.digest();
                self.requests.entry(id).or_insert_with(|| request.clone());
                let response =
                    self.responder
                        .receive(&request, &self.context, &self.evidence, &self.key)?;
                effects.push(ApplicationEffect::Broadcast(
                    ApplicationMessage::InitialResponse(response.clone()),
                ));
                self.pending
                    .push(ApplicationMessage::InitialResponse(response));
            }
            ApplicationMessage::InitialResponse(response) => {
                response.authenticate(&self.context)?;
                let request = self
                    .requests
                    .get(&response.request)
                    .ok_or(CheckpointError::MissingEvidence(response.request))?;
                response.verify(&self.context, request, &self.evidence)?;
                let id = response.request;
                self.responses
                    .entry(id)
                    .or_default()
                    .entry(response.responder)
                    .or_insert(response);
                if !self.completed.contains(&id)
                    && self.responses[&id].len() as u64 >= self.context.thresholds().q
                {
                    let certificate = InitialRCertificate {
                        request: request.clone(),
                        responses: self.responses[&id]
                            .values()
                            .take(self.context.thresholds().q as usize)
                            .cloned()
                            .collect(),
                    };
                    self.pending
                        .push(ApplicationMessage::InitialCertificate(certificate));
                    self.completed.insert(id);
                }
            }
            ApplicationMessage::InitialCertificate(certificate) => {
                let id = certificate.digest();
                if self.evidence.initial_r(id).is_some() {
                    return Ok(());
                }
                self.evidence
                    .insert_initial_r(certificate.clone(), &self.context)?;
                self.requests
                    .entry(certificate.request.digest())
                    .or_insert_with(|| certificate.request.clone());
                self.certificates.insert(id);
                effects.push(ApplicationEffect::InitialReady(id));
                effects.push(ApplicationEffect::Broadcast(
                    ApplicationMessage::InitialCertificate(certificate),
                ));
            }
            ApplicationMessage::Fetch(_) => unreachable!(),
        }
        Ok(())
    }
    fn maybe_admission(&mut self, id: BlockHash) {
        if self.evidence.candidate(id).is_some() {
            return;
        }
        if let (Some(candidate), Some(endorsements)) =
            (self.candidates.get(&id), self.endorsements.get(&id))
        {
            if endorsements.len() as u64 >= self.context.thresholds().q {
                let message = ApplicationMessage::Admission {
                    candidate: candidate.clone(),
                    admission: CandidateAdmission {
                        endorsements: endorsements
                            .values()
                            .take(self.context.thresholds().q as usize)
                            .cloned()
                            .collect(),
                    },
                };
                if !self.pending.contains(&message) {
                    self.pending.push(message);
                }
            }
        }
    }
}
