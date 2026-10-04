//! Application/consensus integration for one checkpoint and signing identity.
//! Timers, transport, report reconstruction and node installation remain external.
use super::*;
use crate::consensus::election::EpochValue;
use rsnano_types::{BlockHash, PrivateKey, PublicKey};
use std::collections::{BTreeMap, BTreeSet};

/// Keep across session recreation for the process lifetime. No disk persistence.
#[derive(Default)]
pub struct CheckpointSessionJournals {
    first: VolatileFirstVoteJournal,
    endorsements: CandidateEndorsementJournal,
    proposals: BTreeMap<(CheckpointInstance, PublicKey), CheckpointCandidate>,
}

pub struct CheckpointSession<P> {
    context: CheckpointContext,
    key: PrivateKey,
    application: ApplicationExchange,
    driver: CheckpointDriver<P>,
    local_candidate: Option<BlockHash>,
    released: bool,
    initial_r: Option<BlockHash>,
    initial_requested: bool,
    pending: Vec<(P, CheckpointMessage)>,
    requested: BTreeSet<BlockHash>,
    evidence_changed: bool,
}
impl<P: Clone + Eq> CheckpointSession<P> {
    pub fn new(context: CheckpointContext, key: PrivateKey) -> Result<Self, CheckpointError> {
        let copy = || CheckpointContext::new(context.instance(), context.committee.clone());
        Ok(Self {
            application: ApplicationExchange::new(
                copy()?,
                key.clone(),
                SlowIoLimits::default().max_message_bytes,
            )?,
            driver: CheckpointDriver::new(copy()?, key.clone())?,
            context,
            key,
            local_candidate: None,
            released: false,
            initial_r: None,
            initial_requested: false,
            pending: Vec::new(),
            requested: BTreeSet::new(),
            evidence_changed: false,
        })
    }
    pub fn decision(&self) -> Option<BlockHash> {
        self.driver.decision()
    }
    /// Export only after independently verifying the retained proof. The
    /// receiving node must revalidate the application value before installation.
    pub fn checkpoint_decision(
        &self,
        limits: VerificationLimits,
        max_bytes: usize,
    ) -> Result<Option<crate::consensus::reports::CheckpointDecision>, CheckpointError> {
        let Some(value) = self.driver.decision() else {
            return Ok(None);
        };
        let proof = self
            .driver
            .decision_proof(limits.max_objects)?
            .ok_or(CheckpointError::MissingEvidence(value))?;
        if proof.verify(&self.context, self.application.evidence(), limits)? != value {
            return Err(CheckpointError::InvalidEvidence);
        }
        let value = self
            .application
            .evidence()
            .value(value)
            .ok_or(CheckpointError::MissingEvidence(value))?
            .clone();
        Ok(Some(crate::consensus::reports::CheckpointDecision {
            value,
            proof: proof.encode(max_bytes)?,
        }))
    }
    pub fn verify_decision(
        &self,
        decision: &crate::consensus::reports::CheckpointDecision,
        limits: VerificationLimits,
        max_bytes: usize,
    ) -> Result<(), CheckpointError> {
        let value = CheckpointDecisionProof::decode(&decision.proof, max_bytes)?.verify(
            &self.context,
            self.application.evidence(),
            limits,
        )?;
        if decision.value.epoch != self.context.instance().epoch || value != decision.value.hash() {
            return Err(CheckpointError::InvalidValue);
        }
        self.application
            .evidence()
            .validate_value(&self.context.instance(), value)
    }
    pub fn evidence(&self) -> &CandidateEvidence {
        self.application.evidence()
    }
    pub fn forget_connection(&mut self, peer: &P) {
        self.driver.forget_connection(peer);
        self.pending.retain(|(p, _)| p != peer);
    }
    pub fn propose(
        &mut self,
        value: EpochValue,
        journals: &mut CheckpointSessionJournals,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
    ) -> Result<Vec<DriverEffect<P>>, CheckpointError> {
        let slot = (self.context.instance(), self.key.public_key());
        let candidate = if let Some(existing) = journals.proposals.get(&slot) {
            existing.clone()
        } else {
            let candidate = CheckpointCandidate::sign(&self.context, value, &self.key)?;
            let checked = validate(candidate.clone())?;
            if checked.candidate().digest() != candidate.digest() {
                return Err(CheckpointError::InvalidEvidence);
            }
            journals.proposals.insert(slot, candidate.clone());
            candidate
        };
        self.local_candidate = Some(candidate.digest());
        let mut out = vec![DriverEffect::Broadcast(CheckpointMessage::Application(
            ApplicationMessage::Candidate(candidate.clone()),
        ))];
        let effects = self.application.receive(
            ApplicationMessage::Candidate(candidate),
            &mut journals.endorsements,
            validate,
        )?;
        self.application_effects(None, effects, &mut out);
        self.advance(journals, validate, &mut out);
        Ok(out)
    }
    pub fn receive(
        &mut self,
        peer: P,
        message: CheckpointMessage,
        journals: &mut CheckpointSessionJournals,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
    ) -> Result<Vec<DriverEffect<P>>, CheckpointError> {
        let mut out = Vec::new();
        if let CheckpointMessage::Application(message) = message {
            let effects =
                self.application
                    .receive(message, &mut journals.endorsements, validate)?;
            self.application_effects(Some(&peer), effects, &mut out);
            self.advance(journals, validate, &mut out);
        } else {
            // An authenticated initial slow request carries its own RC admission.
            // The consensus verifier still independently checks both objects.
            if let CheckpointMessage::Slow(SlowMessage::Request(request)) = &message {
                if let SlowJustification::Initial(proposal) = &request.justification {
                    request.authenticate(&SlowContext::new(&self.context))?;
                    self.deliver(
                        peer.clone(),
                        CheckpointMessage::Fast(FastMessage::Recovery(proposal.recovery.clone())),
                        &mut out,
                    );
                }
            }
            self.deliver(peer, message, &mut out);
            self.advance(journals, validate, &mut out);
        }
        Ok(out)
    }
    fn deliver(&mut self, peer: P, message: CheckpointMessage, out: &mut Vec<DriverEffect<P>>) {
        // FIRST can be parked inside the pool without a missing-object effect;
        // discover its dependency here so the application fetch is not omitted.
        let precheck = if let CheckpointMessage::Fast(FastMessage::First(vote)) = &message {
            self.context
                .verify_first_vote(0, vote, self.application.evidence())
        } else {
            Ok(())
        };
        let result = precheck.and_then(|_| {
            self.driver
                .receive(peer.clone(), message.clone(), self.application.evidence())
        });
        match result {
            Ok(effects) => self.driver_effects(effects, out),
            Err(CheckpointError::MissingEvidence(id)) => {
                if !self
                    .pending
                    .iter()
                    .any(|(p, m)| p == &peer && m == &message)
                {
                    self.pending.push((peer, message));
                }
                self.fetch(id, out);
            }
            Err(error) => out.push(DriverEffect::Rejected(error)),
        }
    }
    fn fetch(&mut self, id: BlockHash, out: &mut Vec<DriverEffect<P>>) {
        if self.requested.insert(id) {
            out.push(DriverEffect::Broadcast(CheckpointMessage::Application(
                ApplicationMessage::Fetch(id),
            )));
        }
    }
    fn driver_effects(&mut self, effects: Vec<DriverEffect<P>>, out: &mut Vec<DriverEffect<P>>) {
        for effect in effects {
            if let DriverEffect::Broadcast(CheckpointMessage::Slow(SlowMessage::Fetch(id))) =
                &effect
            {
                // Slow proof hashes and application dependency hashes share the
                // existing fetch interface. Query both stores; neither trusts a
                // reply without independent verification.
                self.fetch(*id, out);
            }
            out.push(effect);
        }
    }
    fn application_effects(
        &mut self,
        peer: Option<&P>,
        effects: Vec<ApplicationEffect>,
        out: &mut Vec<DriverEffect<P>>,
    ) {
        for effect in effects {
            match effect {
                ApplicationEffect::Broadcast(message) => out.push(DriverEffect::Broadcast(
                    CheckpointMessage::Application(message),
                )),
                ApplicationEffect::Reply(message) => {
                    if let Some(peer) = peer {
                        out.push(DriverEffect::Reply(
                            peer.clone(),
                            CheckpointMessage::Application(message),
                        ));
                    }
                }
                ApplicationEffect::InitialReady(id) => {
                    let maximum = |proof| {
                        self.application
                            .evidence()
                            .r_maximum(&self.context.instance(), 0, proof)
                            .expect("initial-ready evidence was verified")
                    };
                    if self.initial_r.is_none_or(|old| maximum(id) > maximum(old)) {
                        self.initial_r = Some(id);
                    }
                    self.evidence_changed = true;
                }
                ApplicationEffect::Admitted(_) => self.evidence_changed = true,
                ApplicationEffect::MissingEvidence(_) => {}
                ApplicationEffect::Rejected(error) => out.push(DriverEffect::Rejected(error)),
            }
        }
    }
    fn advance(
        &mut self,
        journals: &mut CheckpointSessionJournals,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
        out: &mut Vec<DriverEffect<P>>,
    ) {
        if let Some(id) = self.local_candidate {
            if !self.released && self.application.evidence().candidate(id).is_some() {
                let value = self.application.evidence().admitted_value(id).unwrap();
                match self.driver.propose(
                    value,
                    Introduction::Proposal(id),
                    &mut journals.first,
                    self.application.evidence(),
                ) {
                    Ok(effects) => {
                        self.released = true;
                        self.driver_effects(effects, out);
                    }
                    Err(error) => out.push(DriverEffect::Rejected(error)),
                }
            }
            if self.released && !self.initial_requested && self.driver.decision().is_none() {
                match InitialRRequest::sign(
                    &self.context,
                    id,
                    &self.key,
                    self.application.evidence(),
                ) {
                    Ok(request) => {
                        self.initial_requested = true;
                        out.push(DriverEffect::Broadcast(CheckpointMessage::Application(
                            ApplicationMessage::InitialRequest(request.clone()),
                        )));
                        match self.application.receive(
                            ApplicationMessage::InitialRequest(request),
                            &mut journals.endorsements,
                            validate,
                        ) {
                            Ok(effects) => self.application_effects(None, effects, out),
                            Err(error) => out.push(DriverEffect::Rejected(error)),
                        }
                    }
                    Err(error) => out.push(DriverEffect::Rejected(error)),
                }
            }
        }
        let pending = std::mem::take(&mut self.pending);
        for (peer, message) in pending {
            self.deliver(peer, message, out);
        }
        if let Some(id) = self.initial_r {
            match self
                .driver
                .try_recovery(Some(id), self.application.evidence())
            {
                Ok(effects) => self.driver_effects(effects, out),
                Err(CheckpointError::MissingEvidence(id)) => self.fetch(id, out),
                Err(error) => out.push(DriverEffect::Rejected(error)),
            }
        }
        if std::mem::take(&mut self.evidence_changed) {
            match self.driver.evidence_updated(self.application.evidence()) {
                Ok(effects) => self.driver_effects(effects, out),
                Err(CheckpointError::MissingEvidence(id)) => self.fetch(id, out),
                Err(error) => out.push(DriverEffect::Rejected(error)),
            }
        }
    }
    /// Wake parked candidate validation and dependent consensus proofs when
    /// local report reconstruction changes, without retransmitting history.
    pub fn evidence_updated(
        &mut self,
        journals: &mut CheckpointSessionJournals,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
    ) -> Vec<DriverEffect<P>> {
        let mut out = Vec::new();
        let effects = self
            .application
            .evidence_updated(&mut journals.endorsements, validate);
        self.application_effects(None, effects, &mut out);
        self.advance(journals, validate, &mut out);
        out
    }
    pub fn retry(
        &mut self,
        journals: &mut CheckpointSessionJournals,
        validate: &mut impl FnMut(
            CheckpointCandidate,
        ) -> Result<ValidatedCheckpointCandidate, CheckpointError>,
    ) -> Vec<DriverEffect<P>> {
        self.requested.clear();
        let mut out = Vec::new();
        let effects = self.application.retry(&mut journals.endorsements, validate);
        self.application_effects(None, effects, &mut out);
        self.advance(journals, validate, &mut out);
        match self.driver.retry(self.application.evidence()) {
            Ok(effects) => self.driver_effects(effects, &mut out),
            Err(CheckpointError::MissingEvidence(id)) => self.fetch(id, &mut out),
            Err(error) => out.push(DriverEffect::Rejected(error)),
        }
        out
    }
}
