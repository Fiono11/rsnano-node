use rsnano_messages::{
    RaiBelowThresholdTimeoutEvidence, RaiCertificate, RaiCloseEvidence, RaiCloseUpdate,
    RaiCloseUpdateVote, RaiClosingProposalEvidence, RaiClosingTimeoutEvidence, RaiElectionId,
    RaiEpochClose, RaiEpochCloseVote, RaiEpochContext, RaiExclusionTimeoutEvidence,
    RaiFastDecision, RaiFinalDecision, RaiFinalVote, RaiFirstVote, RaiMessage, RaiNilVote,
    RaiNotarDecision, RaiNotarVote, RaiProposal, RaiSlot, RaiStopReport, RaiTerminalOutcome,
    RaiTerminalRecord, RaiTimeoutDecision, RaiTimeoutDecisionEvidence, RaiTimeoutVote, RaiVote,
    RaiVotePhase, RaiVoteSet, RaiVoteTarget,
};
use rsnano_types::{
    Amount, Blake2HashBuilder, Block, BlockHash, PrivateKey, PublicKey, Signature, SnapshotNumber,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

pub struct RaiService {
    state: Mutex<RaiState>,
}

impl RaiService {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RaiState::new()),
        }
    }

    pub fn new_null() -> Self {
        Self::new()
    }

    pub fn current_open_epoch(&self) -> SnapshotNumber {
        self.state.lock().unwrap().current_open_epoch
    }

    pub fn current_epoch_context(&self) -> RaiEpochContext {
        self.state.lock().unwrap().current_epoch_context()
    }

    pub fn election_id(&self, slot: RaiSlot) -> RaiElectionId {
        self.state.lock().unwrap().election_id(slot)
    }

    pub fn snapshot(&self) -> RaiStateSnapshot {
        self.state.lock().unwrap().snapshot()
    }

    pub fn install_committee(&self, committee: RaiCommittee) {
        self.state.lock().unwrap().install_committee(committee);
    }

    pub fn start_election(&self, election: RaiElectionId) -> bool {
        self.state.lock().unwrap().start_election(election)
    }

    pub fn add_proposal(&self, election: RaiElectionId, proposal_hash: BlockHash) -> bool {
        self.state
            .lock()
            .unwrap()
            .add_proposal(election, proposal_hash)
    }

    pub fn election(&self, election: &RaiElectionId) -> Option<RaiElectionState> {
        self.state.lock().unwrap().election(election).cloned()
    }

    pub fn proposal_block(
        &self,
        election: &RaiElectionId,
        proposal_hash: &BlockHash,
    ) -> Option<Block> {
        self.state
            .lock()
            .unwrap()
            .proposal_block(election, proposal_hash)
            .cloned()
    }

    pub fn started_elections(&self, epoch: SnapshotNumber) -> Vec<RaiElectionId> {
        self.state.lock().unwrap().started_elections(epoch)
    }

    pub fn terminal_records(&self, epoch: SnapshotNumber) -> Vec<RaiTerminalRecord> {
        self.state.lock().unwrap().terminal_records(epoch)
    }

    pub fn election_terminal_record(&self, election: &RaiElectionId) -> Option<RaiTerminalRecord> {
        self.state
            .lock()
            .unwrap()
            .election_terminal_record(election)
    }

    pub fn handle_block_conflict(
        &self,
        election: RaiElectionId,
        fork_hash: BlockHash,
        existing_successor: Option<BlockHash>,
    ) -> bool {
        self.state
            .lock()
            .unwrap()
            .handle_block_conflict(election, fork_hash, existing_successor)
    }

    pub fn process_block_conflict(
        &self,
        election: RaiElectionId,
        fork_hash: BlockHash,
        existing_successor: Option<BlockHash>,
        private_key: Option<&PrivateKey>,
    ) -> RaiOpenElectionOutput {
        self.state.lock().unwrap().process_block_conflict(
            election,
            fork_hash,
            existing_successor,
            private_key,
        )
    }

    pub fn process_message(
        &self,
        message: RaiMessage,
        private_key: Option<&PrivateKey>,
    ) -> RaiOpenElectionOutput {
        self.state
            .lock()
            .unwrap()
            .process_message(message, private_key)
    }

    pub fn complete_slot(&self, record: RaiTerminalRecord) -> bool {
        self.state.lock().unwrap().complete_slot(record)
    }

    pub fn close_current_epoch(&self) -> SnapshotNumber {
        self.state.lock().unwrap().close_current_epoch()
    }

    pub fn open_next_epoch(&self) -> SnapshotNumber {
        self.state.lock().unwrap().open_next_epoch()
    }

    pub fn open_next_epoch_with_close_head(&self, close_head: BlockHash) -> SnapshotNumber {
        self.state
            .lock()
            .unwrap()
            .open_next_epoch_with_close_head(close_head)
    }

    pub fn mark_carryover(&self, election: RaiElectionId) {
        self.state.lock().unwrap().mark_carryover(election);
    }

    pub fn try_lock_first_vote(
        &self,
        election: RaiElectionId,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        self.state
            .lock()
            .unwrap()
            .try_lock_first_vote(election, signer, proposal_hash)
    }

    pub fn try_lock_cert_vote(
        &self,
        election: RaiElectionId,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        self.state
            .lock()
            .unwrap()
            .try_lock_cert_vote(election, signer, proposal_hash)
    }

    pub fn try_lock_final_vote(
        &self,
        election: RaiElectionId,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        self.state
            .lock()
            .unwrap()
            .try_lock_final_vote(election, signer, proposal_hash)
    }

    pub fn try_lock_timeout_vote(&self, election: RaiElectionId, signer: PublicKey) -> bool {
        self.state
            .lock()
            .unwrap()
            .try_lock_timeout_vote(election, signer)
    }

    pub fn try_lock_nil_vote(&self, election: RaiElectionId, signer: PublicKey) -> bool {
        self.state
            .lock()
            .unwrap()
            .try_lock_nil_vote(election, signer)
    }
}

impl Default for RaiService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RaiOpenElectionOutput {
    pub messages: Vec<RaiMessage>,
    pub terminal_record: Option<RaiTerminalRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiState {
    current_open_epoch: SnapshotNumber,
    epoch_phases: BTreeMap<SnapshotNumber, RaiEpochPhase>,
    epoch_contexts: BTreeMap<SnapshotNumber, RaiEpochContext>,
    close_heads: BTreeMap<SnapshotNumber, BlockHash>,
    certified_close_heads: BTreeMap<BlockHash, RaiCertifiedCloseHead>,
    committees: BTreeMap<SnapshotNumber, RaiCommittee>,
    elections: BTreeMap<RaiElectionId, RaiElectionState>,
    terminal_elections: BTreeMap<RaiElectionId, RaiTerminalRecord>,
    notarized_elections: BTreeMap<RaiElectionId, RaiTerminalRecord>,
    completed_slots: BTreeMap<RaiSlot, RaiTerminalRecord>,
    notarized_slots: BTreeMap<RaiSlot, BTreeSet<BlockHash>>,
    slot_cert_votes: BTreeMap<RaiSlot, BTreeMap<PublicKey, BTreeSet<BlockHash>>>,
    slot_final_votes: BTreeMap<RaiSlot, BTreeMap<PublicKey, BlockHash>>,
    carryover: BTreeMap<SnapshotNumber, BTreeSet<RaiElectionId>>,
    stop_reports: BTreeMap<SnapshotNumber, BTreeMap<PublicKey, RaiStopReport>>,
    stop_report_sets: BTreeMap<BlockHash, RaiStopReportSet>,
    preservation_witnesses: BTreeMap<RaiElectionId, RaiPreservationWitness>,
    close_evidence: BTreeMap<RaiElectionId, BTreeMap<PublicKey, RaiCloseEvidence>>,
    emitted_certificates: BTreeSet<RaiCertificateKey>,
    emitted_decisions: BTreeSet<RaiDecisionKey>,
    signing_locks: BTreeMap<RaiSigner, RaiSigningLock>,
}

impl RaiState {
    pub fn new() -> Self {
        let mut epoch_phases = BTreeMap::new();
        epoch_phases.insert(0, RaiEpochPhase::Open);
        let mut epoch_contexts = BTreeMap::new();
        epoch_contexts.insert(0, RaiEpochContext::bootstrap());

        Self {
            current_open_epoch: 0,
            epoch_phases,
            epoch_contexts,
            close_heads: BTreeMap::new(),
            certified_close_heads: BTreeMap::new(),
            committees: BTreeMap::new(),
            elections: BTreeMap::new(),
            terminal_elections: BTreeMap::new(),
            notarized_elections: BTreeMap::new(),
            completed_slots: BTreeMap::new(),
            notarized_slots: BTreeMap::new(),
            slot_cert_votes: BTreeMap::new(),
            slot_final_votes: BTreeMap::new(),
            carryover: BTreeMap::new(),
            stop_reports: BTreeMap::new(),
            stop_report_sets: BTreeMap::new(),
            preservation_witnesses: BTreeMap::new(),
            close_evidence: BTreeMap::new(),
            emitted_certificates: BTreeSet::new(),
            emitted_decisions: BTreeSet::new(),
            signing_locks: BTreeMap::new(),
        }
    }

    pub fn snapshot(&self) -> RaiStateSnapshot {
        RaiStateSnapshot {
            current_open_epoch: self.current_open_epoch,
            open_epochs: self.count_phase(RaiEpochPhase::Open),
            closing_epochs: self.count_phase(RaiEpochPhase::Closing),
            closed_epochs: self.count_phase(RaiEpochPhase::Closed),
            committees: self.committees.len(),
            elections: self.elections.len(),
            terminal_elections: self.terminal_elections.len() + self.notarized_elections.len(),
            completed_slots: self.completed_slots.len(),
            carryover_elections: self.carryover.values().map(BTreeSet::len).sum(),
            stop_reports: self.stop_reports.values().map(BTreeMap::len).sum(),
            stop_report_sets: self.stop_report_sets.len(),
            preservation_witnesses: self.preservation_witnesses.len(),
            close_evidence_items: self.close_evidence.values().map(BTreeMap::len).sum(),
            signing_locks: self.signing_locks.len(),
        }
    }

    pub fn epoch_phase(&self, epoch: SnapshotNumber) -> Option<RaiEpochPhase> {
        self.epoch_phases.get(&epoch).copied()
    }

    pub fn epoch_context(&self, epoch: SnapshotNumber) -> Option<RaiEpochContext> {
        self.epoch_contexts.get(&epoch).copied()
    }

    pub fn current_epoch_context(&self) -> RaiEpochContext {
        self.epoch_context(self.current_open_epoch)
            .expect("current open Rai epoch should always have a context")
    }

    pub fn election_id(&self, slot: RaiSlot) -> RaiElectionId {
        RaiElectionId::with_context(slot, self.current_open_epoch, self.current_epoch_context())
    }

    pub fn committee(&self, epoch: SnapshotNumber) -> Option<&RaiCommittee> {
        self.committees.get(&epoch)
    }

    pub fn election(&self, election: &RaiElectionId) -> Option<&RaiElectionState> {
        self.elections.get(election)
    }

    pub fn proposal_block(
        &self,
        election: &RaiElectionId,
        proposal_hash: &BlockHash,
    ) -> Option<&Block> {
        self.election(election)
            .and_then(|election| election.proposal_blocks.get(proposal_hash))
    }

    pub fn terminal_record(&self, slot: &RaiSlot) -> Option<RaiTerminalRecord> {
        self.completed_slots.get(slot).copied()
    }

    pub fn election_terminal_record(&self, election: &RaiElectionId) -> Option<RaiTerminalRecord> {
        self.terminal_elections
            .get(election)
            .or_else(|| self.notarized_elections.get(election))
            .copied()
    }

    pub fn started_elections(&self, epoch: SnapshotNumber) -> Vec<RaiElectionId> {
        self.elections
            .keys()
            .filter(|election| election.epoch == epoch)
            .copied()
            .collect()
    }

    pub fn terminal_records(&self, epoch: SnapshotNumber) -> Vec<RaiTerminalRecord> {
        self.terminal_elections
            .values()
            .chain(self.notarized_elections.values())
            .filter(|record| record.election.epoch == epoch)
            .copied()
            .collect()
    }

    pub fn preservation_witness(
        &self,
        election: &RaiElectionId,
    ) -> Option<&RaiPreservationWitness> {
        self.preservation_witnesses.get(election)
    }

    pub fn close_evidence(
        &self,
        election: &RaiElectionId,
        signer: &PublicKey,
    ) -> Option<&RaiCloseEvidence> {
        self.close_evidence
            .get(election)
            .and_then(|evidence| evidence.get(signer))
    }

    pub fn install_committee(&mut self, committee: RaiCommittee) {
        self.committees.insert(committee.epoch, committee);
    }

    pub fn start_election(&mut self, election: RaiElectionId) -> bool {
        if self.is_election_closed(&election)
            || self.completed_slots.contains_key(&election.slot)
            || !self.valid_election_context(election)
            || self.has_live_election_for_slot_except(&election.slot, Some(election))
            || self.has_carryover(&election.slot)
        {
            return false;
        }

        if election.epoch != self.current_open_epoch
            || self.epoch_phase(election.epoch) != Some(RaiEpochPhase::Open)
        {
            return false;
        }

        self.elections
            .insert(election, RaiElectionState::new(election))
            .is_none()
    }

    pub fn add_proposal(&mut self, election: RaiElectionId, proposal_hash: BlockHash) -> bool {
        if self.is_election_closed(&election)
            || self.completed_slots.contains_key(&election.slot)
            || !self.valid_election_context(election)
        {
            return false;
        }

        if !self.elections.contains_key(&election)
            && (election.epoch != self.current_open_epoch
                || self.epoch_phase(election.epoch) != Some(RaiEpochPhase::Open)
                || self.has_live_election_for_slot_except(&election.slot, None)
                || self.has_carryover(&election.slot))
        {
            return false;
        }

        let election_state = self
            .elections
            .entry(election)
            .or_insert_with(|| RaiElectionState::new(election));
        election_state.proposals.insert(proposal_hash)
    }

    pub fn handle_block_conflict(
        &mut self,
        election: RaiElectionId,
        fork_hash: BlockHash,
        existing_successor: Option<BlockHash>,
    ) -> bool {
        if self.completed_slots.contains_key(&election.slot) {
            return false;
        }

        if self.is_election_closed(&election) {
            return false;
        }

        if !self.elections.contains_key(&election) && !self.start_election(election) {
            return false;
        }

        let mut changed = false;
        if let Some(existing_successor) = existing_successor {
            changed |= self.add_proposal(election, existing_successor);
        }
        changed |= self.add_proposal(election, fork_hash);
        changed
    }

    pub fn process_block_conflict(
        &mut self,
        election: RaiElectionId,
        fork_hash: BlockHash,
        existing_successor: Option<BlockHash>,
        private_key: Option<&PrivateKey>,
    ) -> RaiOpenElectionOutput {
        let mut output = RaiOpenElectionOutput::default();
        if !self.handle_block_conflict(election, fork_hash, existing_successor) {
            return output;
        }

        self.try_create_first_vote_for_open_election(election, private_key, &mut output);
        self.advance_open_election(election, private_key, &mut output);
        output
    }

    pub fn process_message(
        &mut self,
        message: RaiMessage,
        private_key: Option<&PrivateKey>,
    ) -> RaiOpenElectionOutput {
        let mut output = RaiOpenElectionOutput::default();

        let election = match message {
            RaiMessage::Proposal(proposal) => {
                self.process_proposal_message(proposal, private_key, &mut output)
            }
            RaiMessage::Vote(vote) => self.process_vote_message(vote, private_key, &mut output),
            RaiMessage::LegacyFirstVote(vote) => {
                self.process_first_vote_message(vote, private_key, &mut output)
            }
            RaiMessage::LegacyNotarVote(vote) => {
                self.process_notar_vote_message(vote, private_key, &mut output)
            }
            RaiMessage::LegacyFinalVote(vote) => {
                self.process_final_vote_message(vote, private_key, &mut output)
            }
            RaiMessage::LegacyTimeoutVote(vote) => {
                self.process_timeout_vote_message(vote, private_key, &mut output)
            }
            RaiMessage::StopReport(report) => {
                self.process_stop_report_message(report, private_key);
                None
            }
            RaiMessage::Certificate(certificate) => self.process_certificate_message(certificate),
            RaiMessage::NotarDecision(decision) => {
                self.process_notar_decision_message(decision, &mut output);
                None
            }
            RaiMessage::FastDecision(decision) => {
                self.process_fast_decision_message(decision, &mut output);
                None
            }
            RaiMessage::FinalDecision(decision) => {
                self.process_final_decision_message(decision, &mut output);
                None
            }
            RaiMessage::TimeoutDecision(decision) => {
                self.process_timeout_decision_message(decision, &mut output);
                None
            }
            RaiMessage::EpochClose(close) => {
                self.process_epoch_close_message(close);
                None
            }
            RaiMessage::CloseUpdate(update) => {
                self.process_close_update_message(update);
                None
            }
        };

        if let Some(election) = election {
            self.advance_open_election(election, private_key, &mut output);
        }

        output
    }

    pub fn complete_slot(&mut self, record: RaiTerminalRecord) -> bool {
        if matches!(record.outcome, RaiTerminalOutcome::Notarized(_)) {
            return self.complete_notarized_attempt(record);
        }

        if let Some(existing) = self.terminal_elections.get(&record.election) {
            return *existing == record;
        }

        if let Some(existing) = self.completed_slots.get(&record.election.slot)
            && *existing != record
        {
            return false;
        }

        if matches!(record.outcome, RaiTerminalOutcome::Proposal(_)) {
            self.completed_slots.insert(record.election.slot, record);
        }

        self.terminal_elections.insert(record.election, record);

        if let Some(election) = self.elections.get_mut(&record.election) {
            election.terminal_record = Some(record);
        }

        for elections in self.carryover.values_mut() {
            elections.remove(&record.election);
        }

        true
    }

    fn complete_notarized_attempt(&mut self, record: RaiTerminalRecord) -> bool {
        if let Some(existing) = self.notarized_elections.get(&record.election) {
            return *existing == record;
        }

        if let Some(existing) = self.completed_slots.get(&record.election.slot)
            && *existing != record
        {
            return false;
        }

        let RaiTerminalOutcome::Notarized(proposal_hash) = record.outcome else {
            return false;
        };

        self.notarized_elections.insert(record.election, record);
        self.notarized_slots
            .entry(record.election.slot)
            .or_default()
            .insert(proposal_hash);

        if let Some(election) = self.elections.get_mut(&record.election) {
            election.terminal_record = Some(record);
        }

        for elections in self.carryover.values_mut() {
            elections.remove(&record.election);
        }

        true
    }

    pub fn close_current_epoch(&mut self) -> SnapshotNumber {
        let closing_epoch = self.current_open_epoch;
        self.epoch_phases
            .insert(closing_epoch, RaiEpochPhase::Closing);

        let carryover: Vec<_> = self
            .elections
            .keys()
            .copied()
            .filter(|id| id.epoch == closing_epoch && !self.terminal_elections.contains_key(id))
            .filter(|id| !self.notarized_elections.contains_key(id))
            .collect();

        for election in carryover {
            self.mark_carryover(election);
        }

        closing_epoch
    }

    pub fn open_next_epoch(&mut self) -> SnapshotNumber {
        self.open_next_epoch_with_close_head(BlockHash::ZERO)
    }

    pub fn open_next_epoch_with_close_head(&mut self, close_head: BlockHash) -> SnapshotNumber {
        self.close_current_epoch();
        self.close_heads.insert(self.current_open_epoch, close_head);
        self.current_open_epoch += 1;
        let context = self.context_for_epoch(self.current_open_epoch);
        self.epoch_contexts.insert(self.current_open_epoch, context);
        self.epoch_phases
            .insert(self.current_open_epoch, RaiEpochPhase::Open);
        self.current_open_epoch
    }

    pub fn close_epoch(&mut self, epoch: SnapshotNumber) -> bool {
        if self.epoch_phase(epoch) != Some(RaiEpochPhase::Closing) {
            return false;
        }

        if self
            .carryover
            .get(&epoch)
            .is_some_and(|elections| !elections.is_empty())
        {
            return false;
        }

        self.epoch_phases.insert(epoch, RaiEpochPhase::Closed);
        true
    }

    pub fn mark_carryover(&mut self, election: RaiElectionId) {
        self.elections
            .entry(election)
            .or_insert_with(|| RaiElectionState::new(election))
            .is_carryover = true;
        self.carryover
            .entry(election.epoch)
            .or_default()
            .insert(election);
    }

    pub fn try_lock_first_vote(
        &mut self,
        election: RaiElectionId,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        let lock = self.signing_lock(election, signer);
        if lock.nil_locked || lock.timeout_locked || lock.first_vote.is_some() {
            return lock.first_vote == Some(proposal_hash);
        }

        lock.first_vote = Some(proposal_hash);
        lock.cert_votes.insert(proposal_hash);
        true
    }

    pub fn try_lock_cert_vote(
        &mut self,
        election: RaiElectionId,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        if self.signer_final_voted_different_slot(election.slot, signer, proposal_hash) {
            return false;
        }

        let lock = self.signing_lock(election, signer);
        if lock.nil_locked {
            return false;
        }

        if lock
            .final_vote
            .is_some_and(|existing| existing != proposal_hash)
        {
            return false;
        }

        lock.cert_votes.insert(proposal_hash)
    }

    pub fn try_lock_final_vote(
        &mut self,
        election: RaiElectionId,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        if self.signer_has_conflicting_slot_cert_vote(election.slot, signer, proposal_hash) {
            return false;
        }

        let lock = self.signing_lock(election, signer);
        if lock.nil_locked || lock.timeout_locked {
            return false;
        }

        if lock
            .cert_votes
            .iter()
            .any(|certified| *certified != proposal_hash)
        {
            return false;
        }

        if let Some(existing) = lock.final_vote {
            return existing == proposal_hash;
        }

        lock.final_vote = Some(proposal_hash);
        true
    }

    pub fn try_lock_timeout_vote(&mut self, election: RaiElectionId, signer: PublicKey) -> bool {
        let lock = self.signing_lock(election, signer);
        if lock.nil_locked {
            return false;
        }

        if lock.final_vote.is_some() {
            return false;
        }

        if lock.timeout_vote {
            return false;
        }

        lock.timeout_locked = true;
        lock.timeout_vote = true;
        true
    }

    pub fn try_lock_nil_vote(&mut self, election: RaiElectionId, signer: PublicKey) -> bool {
        let lock = self.signing_lock(election, signer);
        if lock.first_vote.is_some()
            || !lock.cert_votes.is_empty()
            || lock.final_vote.is_some()
            || lock.timeout_vote
        {
            return false;
        }

        if lock.nil_locked {
            return false;
        }

        lock.nil_locked = true;
        true
    }

    fn has_carryover(&self, slot: &RaiSlot) -> bool {
        self.carryover
            .values()
            .any(|elections| elections.iter().any(|election| &election.slot == slot))
    }

    fn has_live_election_for_slot_except(
        &self,
        slot: &RaiSlot,
        except: Option<RaiElectionId>,
    ) -> bool {
        self.elections.keys().any(|election| {
            &election.slot == slot
                && Some(*election) != except
                && !self.is_election_closed(election)
        })
    }

    fn is_election_closed(&self, election: &RaiElectionId) -> bool {
        self.terminal_elections.contains_key(election)
            || self.notarized_elections.contains_key(election)
    }

    fn valid_election_context(&self, election: RaiElectionId) -> bool {
        self.epoch_context(election.epoch) == Some(election.context)
    }

    fn context_for_epoch(&self, epoch: SnapshotNumber) -> RaiEpochContext {
        RaiEpochContext::new(
            self.close_head_for_context(epoch, 2),
            self.close_head_for_context(epoch, 3),
        )
    }

    fn close_head_for_context(&self, epoch: SnapshotNumber, distance: SnapshotNumber) -> BlockHash {
        if epoch < distance {
            BlockHash::ZERO
        } else {
            self.close_heads
                .get(&(epoch - distance))
                .copied()
                .unwrap_or(BlockHash::ZERO)
        }
    }

    fn count_phase(&self, phase: RaiEpochPhase) -> usize {
        self.epoch_phases
            .values()
            .filter(|candidate| **candidate == phase)
            .count()
    }

    fn signing_lock(&mut self, election: RaiElectionId, signer: PublicKey) -> &mut RaiSigningLock {
        self.signing_locks
            .entry(RaiSigner { election, signer })
            .or_default()
    }

    fn process_proposal_message(
        &mut self,
        proposal: RaiProposal,
        private_key: Option<&PrivateKey>,
        output: &mut RaiOpenElectionOutput,
    ) -> Option<RaiElectionId> {
        let election = proposal.election;
        let proposal_hash = proposal.proposal_hash();
        let proposal_block = proposal.block;

        if let Some(record) = self.election_terminal_record(&election) {
            if record.election == election
                && record.outcome == RaiTerminalOutcome::Proposal(proposal_hash)
            {
                self.record_proposal_block(election, proposal_block);
                output.terminal_record = Some(record);
            }
            return None;
        }

        if self.terminal_record(&election.slot).is_some() {
            return None;
        }

        if !self.is_open_election_accepting_votes(election) {
            return None;
        }

        self.record_proposal_block(election, proposal_block);
        self.try_create_first_vote(election, proposal_hash, private_key, output);
        Some(election)
    }

    fn process_vote_message(
        &mut self,
        vote: RaiVote,
        private_key: Option<&PrivateKey>,
        output: &mut RaiOpenElectionOutput,
    ) -> Option<RaiElectionId> {
        if !self.valid_open_member_signature(
            vote.election,
            vote.voter,
            vote.hash().as_bytes(),
            &vote.signature,
        ) {
            return None;
        }

        let accepted = match (vote.phase, vote.target) {
            (RaiVotePhase::First, RaiVoteTarget::Proposal(proposal_hash)) => self
                .accept_first_vote(
                    vote.election,
                    vote.voter,
                    proposal_hash,
                    private_key,
                    output,
                ),
            (RaiVotePhase::Cert, RaiVoteTarget::Proposal(proposal_hash)) => {
                self.accept_cert_vote(vote.election, vote.voter, proposal_hash)
            }
            (RaiVotePhase::Final, RaiVoteTarget::Proposal(proposal_hash)) => {
                self.accept_final_vote(vote.election, vote.voter, proposal_hash)
            }
            (RaiVotePhase::Timeout, RaiVoteTarget::Timeout) => {
                self.accept_timeout_vote(vote.election, vote.voter)
            }
            _ => None,
        };

        if accepted.is_some() {
            self.record_certified_vote(vote);
        }

        accepted
    }

    fn process_first_vote_message(
        &mut self,
        vote: RaiFirstVote,
        private_key: Option<&PrivateKey>,
        output: &mut RaiOpenElectionOutput,
    ) -> Option<RaiElectionId> {
        let election = vote.election;
        if !self.valid_open_member_signature(
            vote.election,
            vote.voter,
            vote.hash().as_bytes(),
            &vote.signature,
        ) || !self.valid_open_member_signature(
            vote.election,
            vote.voter,
            vote.notar_hash().as_bytes(),
            &vote.notar_signature,
        ) {
            return None;
        }

        self.accept_first_vote(
            election,
            vote.voter,
            vote.proposal_hash,
            private_key,
            output,
        )
    }

    fn process_notar_vote_message(
        &mut self,
        vote: RaiNotarVote,
        _private_key: Option<&PrivateKey>,
        _output: &mut RaiOpenElectionOutput,
    ) -> Option<RaiElectionId> {
        if !self.valid_open_member_signature(
            vote.election,
            vote.voter,
            vote.hash().as_bytes(),
            &vote.signature,
        ) {
            return None;
        }

        match vote.target {
            RaiVoteTarget::Proposal(proposal_hash) => {
                self.accept_cert_vote(vote.election, vote.voter, proposal_hash)
            }
            RaiVoteTarget::Timeout => self.accept_timeout_vote(vote.election, vote.voter),
        }
    }

    fn process_final_vote_message(
        &mut self,
        vote: RaiFinalVote,
        _private_key: Option<&PrivateKey>,
        _output: &mut RaiOpenElectionOutput,
    ) -> Option<RaiElectionId> {
        if !self.valid_open_member_signature(
            vote.election,
            vote.voter,
            vote.hash().as_bytes(),
            &vote.signature,
        ) {
            return None;
        }

        self.accept_final_vote(vote.election, vote.voter, vote.proposal_hash)
    }

    fn process_timeout_vote_message(
        &mut self,
        vote: RaiTimeoutVote,
        _private_key: Option<&PrivateKey>,
        _output: &mut RaiOpenElectionOutput,
    ) -> Option<RaiElectionId> {
        if !self.valid_open_member_signature(
            vote.election,
            vote.voter,
            vote.hash().as_bytes(),
            &vote.signature,
        ) {
            return None;
        }

        self.accept_timeout_vote(vote.election, vote.voter)
    }

    fn process_certificate_message(
        &mut self,
        certificate: RaiCertificate,
    ) -> Option<RaiElectionId> {
        if !self.valid_certificate(&certificate) {
            return None;
        }

        self.add_proposal(certificate.election, certificate.proposal_hash);
        for vote in certificate.votes {
            self.record_certified_vote(vote);
        }
        Some(certificate.election)
    }

    fn process_fast_decision_message(
        &mut self,
        decision: RaiFastDecision,
        output: &mut RaiOpenElectionOutput,
    ) {
        if !self.valid_fast_decision(&decision) {
            return;
        }

        for vote_set in decision.first_vote_sets {
            for vote in vote_set.votes {
                self.record_certified_vote(vote);
            }
        }

        self.complete_open_decision(
            RaiDecisionKind::Fast,
            decision.election,
            RaiTerminalOutcome::Proposal(decision.proposal_hash),
            output,
        );
    }

    fn process_notar_decision_message(
        &mut self,
        decision: RaiNotarDecision,
        output: &mut RaiOpenElectionOutput,
    ) {
        if !self.valid_notar_decision(&decision) {
            return;
        }

        for vote_set in decision.cert_vote_sets {
            for vote in vote_set.votes {
                self.record_certified_vote(vote);
            }
        }
        for item in decision.closing_evidence.items {
            self.record_close_evidence(item);
        }

        self.complete_open_decision(
            RaiDecisionKind::Notar,
            decision.election,
            RaiTerminalOutcome::Notarized(decision.proposal_hash),
            output,
        );
    }

    fn process_final_decision_message(
        &mut self,
        decision: RaiFinalDecision,
        output: &mut RaiOpenElectionOutput,
    ) {
        if !self.valid_final_decision(&decision) {
            return;
        }

        for vote_set in decision.final_vote_sets {
            for vote in vote_set.votes {
                self.record_certified_vote(vote);
            }
        }
        for item in decision.closing_evidence.items {
            self.record_close_evidence(item);
        }

        self.complete_open_decision(
            RaiDecisionKind::Final,
            decision.election,
            RaiTerminalOutcome::Proposal(decision.proposal_hash),
            output,
        );
    }

    fn process_timeout_decision_message(
        &mut self,
        decision: RaiTimeoutDecision,
        output: &mut RaiOpenElectionOutput,
    ) {
        if !self.valid_timeout_decision(&decision) {
            return;
        }

        self.record_timeout_decision_evidence(&decision.evidence);

        for vote_set in decision.timeout_vote_sets {
            for vote in vote_set.votes {
                self.record_certified_vote(vote);
            }
        }

        self.complete_open_decision(
            RaiDecisionKind::Timeout,
            decision.election,
            RaiTerminalOutcome::Timeout,
            output,
        );
    }

    fn process_epoch_close_message(&mut self, close: RaiEpochClose) {
        if !self.valid_epoch_close(&close) {
            return;
        }

        let hash = close.hash();
        self.certified_close_heads
            .entry(hash)
            .or_insert_with(|| RaiCertifiedCloseHead {
                epoch: close.epoch,
                previous_close_hash: close.previous_close_hash,
                proposal_hashes: close.proposal_hashes.clone(),
            });
        self.close_heads.insert(close.epoch, hash);
    }

    fn process_close_update_message(&mut self, update: RaiCloseUpdate) {
        if !self.valid_close_update(&update) {
            return;
        }

        let hash = update.hash();
        self.certified_close_heads
            .entry(hash)
            .or_insert_with(|| RaiCertifiedCloseHead {
                epoch: update.epoch,
                previous_close_hash: update.previous_close_hash,
                proposal_hashes: update.proposal_hashes.clone(),
            });
        self.close_heads.insert(update.epoch, hash);
    }

    fn accept_first_vote(
        &mut self,
        election: RaiElectionId,
        voter: PublicKey,
        proposal_hash: BlockHash,
        private_key: Option<&PrivateKey>,
        output: &mut RaiOpenElectionOutput,
    ) -> Option<RaiElectionId> {
        self.add_proposal(election, proposal_hash);
        if self.record_first_vote(election, voter, proposal_hash) {
            self.record_cert_vote(election, voter, proposal_hash);
        }
        self.try_create_first_vote_for_open_election(election, private_key, output);
        Some(election)
    }

    fn accept_cert_vote(
        &mut self,
        election: RaiElectionId,
        voter: PublicKey,
        proposal_hash: BlockHash,
    ) -> Option<RaiElectionId> {
        if !self.is_admissible_cert_vote(election, voter, proposal_hash) {
            return None;
        }

        self.record_cert_vote(election, voter, proposal_hash);
        Some(election)
    }

    fn accept_final_vote(
        &mut self,
        election: RaiElectionId,
        voter: PublicKey,
        proposal_hash: BlockHash,
    ) -> Option<RaiElectionId> {
        if !self.is_admissible_final_vote(election, voter, proposal_hash) {
            return None;
        }

        self.record_final_vote(election, voter, proposal_hash);
        Some(election)
    }

    fn accept_timeout_vote(
        &mut self,
        election: RaiElectionId,
        voter: PublicKey,
    ) -> Option<RaiElectionId> {
        if !self.is_admissible_timeout_vote(election, voter) {
            return None;
        }

        self.record_timeout_vote(election, voter);
        Some(election)
    }

    fn process_stop_report_message(
        &mut self,
        report: RaiStopReport,
        private_key: Option<&PrivateKey>,
    ) {
        let epoch = report.epoch;
        if !self.valid_stop_report(&report) || !self.record_stop_report(report) {
            return;
        }

        self.try_assemble_stop_report_set(epoch, private_key);
    }

    fn is_open_election_accepting_votes(&self, election: RaiElectionId) -> bool {
        election.epoch == self.current_open_epoch
            && self.epoch_phase(election.epoch) == Some(RaiEpochPhase::Open)
            && self.valid_election_context(election)
            && !self.is_election_closed(&election)
            && !self.completed_slots.contains_key(&election.slot)
            && !self.has_carryover(&election.slot)
    }

    fn active_open_committees(&self, election: RaiElectionId) -> Vec<RaiCommittee> {
        if !self.is_open_election_accepting_votes(election) {
            return Vec::new();
        }

        self.open_decision_committees(election)
    }

    fn open_decision_committees(&self, election: RaiElectionId) -> Vec<RaiCommittee> {
        if !self.valid_election_context(election)
            || !matches!(
                self.epoch_phase(election.epoch),
                Some(RaiEpochPhase::Open | RaiEpochPhase::Closing)
            )
        {
            return Vec::new();
        }

        let Some(current) = self.committee(election.epoch).cloned() else {
            return Vec::new();
        };

        if election.epoch == 0 {
            return vec![current];
        }

        let Some(previous) = self.committee(election.epoch - 1).cloned() else {
            return Vec::new();
        };

        vec![current, previous]
    }

    fn valid_open_member_signature(
        &self,
        election: RaiElectionId,
        signer: PublicKey,
        message: &[u8],
        signature: &Signature,
    ) -> bool {
        self.active_open_committees(election)
            .iter()
            .any(|committee| committee.valid_member_signature(signer, message, signature))
    }

    fn valid_certificate(&self, certificate: &RaiCertificate) -> bool {
        let active_committees = self.active_open_committees(certificate.election);
        let Some(committee) = active_committees
            .iter()
            .find(|committee| committee.epoch == certificate.committee_epoch)
        else {
            return false;
        };

        self.valid_vote_collection(
            committee,
            &certificate.votes,
            committee.quorum_threshold,
            |vote| {
                vote.election == certificate.election
                    && vote.target == RaiVoteTarget::Proposal(certificate.proposal_hash)
                    && matches!(vote.phase, RaiVotePhase::First | RaiVotePhase::Cert)
            },
        )
    }

    fn valid_fast_decision(&self, decision: &RaiFastDecision) -> bool {
        self.valid_open_decision_vote_sets(
            decision.election,
            &decision.first_vote_sets,
            |committee| committee.fast_threshold,
            |vote| {
                vote.election == decision.election
                    && vote.phase == RaiVotePhase::First
                    && vote.target == RaiVoteTarget::Proposal(decision.proposal_hash)
            },
        )
    }

    fn valid_notar_decision(&self, decision: &RaiNotarDecision) -> bool {
        (decision.closing_evidence.items.is_empty()
            && self.valid_open_decision_vote_sets(
                decision.election,
                &decision.cert_vote_sets,
                |committee| committee.quorum_threshold,
                |vote| {
                    vote.election == decision.election
                        && vote.target == RaiVoteTarget::Proposal(decision.proposal_hash)
                        && matches!(vote.phase, RaiVotePhase::First | RaiVotePhase::Cert)
                },
            ))
            || (self.valid_closing_decision_vote_set(
                decision.election,
                &decision.cert_vote_sets,
                |vote| {
                    vote.election == decision.election
                        && vote.target == RaiVoteTarget::Proposal(decision.proposal_hash)
                        && matches!(vote.phase, RaiVotePhase::First | RaiVotePhase::Cert)
                },
            ) && self.valid_closing_proposal_evidence(
                decision.election,
                decision.proposal_hash,
                &decision.closing_evidence,
            ))
    }

    fn valid_final_decision(&self, decision: &RaiFinalDecision) -> bool {
        (decision.closing_evidence.items.is_empty()
            && self.valid_open_decision_vote_sets(
                decision.election,
                &decision.final_vote_sets,
                |committee| committee.quorum_threshold,
                |vote| {
                    vote.election == decision.election
                        && vote.phase == RaiVotePhase::Final
                        && vote.target == RaiVoteTarget::Proposal(decision.proposal_hash)
                },
            ))
            || (self.valid_closing_decision_vote_set(
                decision.election,
                &decision.final_vote_sets,
                |vote| {
                    vote.election == decision.election
                        && vote.phase == RaiVotePhase::Final
                        && vote.target == RaiVoteTarget::Proposal(decision.proposal_hash)
                },
            ) && self.valid_closing_proposal_evidence(
                decision.election,
                decision.proposal_hash,
                &decision.closing_evidence,
            ))
    }

    fn valid_timeout_decision(&self, decision: &RaiTimeoutDecision) -> bool {
        match &decision.evidence {
            RaiTimeoutDecisionEvidence::None => self.valid_open_decision_vote_sets(
                decision.election,
                &decision.timeout_vote_sets,
                |committee| committee.quorum_threshold,
                |vote| {
                    vote.election == decision.election
                        && vote.phase == RaiVotePhase::Timeout
                        && vote.target == RaiVoteTarget::Timeout
                },
            ),
            RaiTimeoutDecisionEvidence::BelowThreshold(evidence) => {
                self.valid_closing_decision_vote_set(
                    decision.election,
                    &decision.timeout_vote_sets,
                    |vote| {
                        vote.election == decision.election
                            && vote.phase == RaiVotePhase::Timeout
                            && vote.target == RaiVoteTarget::Timeout
                    },
                ) && self.valid_below_threshold_timeout_evidence(decision, evidence)
            }
            RaiTimeoutDecisionEvidence::Exclusion(evidence) => {
                self.valid_closing_decision_vote_set(
                    decision.election,
                    &decision.timeout_vote_sets,
                    |vote| {
                        vote.election == decision.election
                            && vote.phase == RaiVotePhase::Timeout
                            && vote.target == RaiVoteTarget::Timeout
                    },
                ) && self.valid_exclusion_timeout_evidence(decision, evidence)
            }
            RaiTimeoutDecisionEvidence::Closing(evidence) => {
                self.valid_closing_decision_vote_set(
                    decision.election,
                    &decision.timeout_vote_sets,
                    |vote| {
                        vote.election == decision.election
                            && vote.phase == RaiVotePhase::Timeout
                            && vote.target == RaiVoteTarget::Timeout
                    },
                ) && self.valid_closing_timeout_evidence(decision.election, evidence)
            }
        }
    }

    fn valid_epoch_close(&self, close: &RaiEpochClose) -> bool {
        let Some(committee) = self.committee(close.epoch) else {
            return false;
        };
        let decided_hash = close.decided_proposals_hash();

        self.valid_epoch_close_votes(
            committee,
            &close.votes,
            committee.quorum_threshold,
            |vote| {
                vote.epoch == close.epoch
                    && vote.previous_close_hash == close.previous_close_hash
                    && vote.decided_proposals_hash == decided_hash
            },
        )
    }

    fn valid_close_update(&self, update: &RaiCloseUpdate) -> bool {
        let Some(committee) = self.committee(update.epoch) else {
            return false;
        };
        let Some(parent) = self.certified_close_heads.get(&update.parent_close_hash) else {
            return false;
        };
        if parent.epoch != update.epoch || parent.previous_close_hash != update.previous_close_hash
        {
            return false;
        }
        if !parent
            .proposal_hashes
            .iter()
            .all(|proposal| update.proposal_hashes.contains(proposal))
        {
            return false;
        }

        let decided_hash = update.decided_proposals_hash();
        self.valid_close_update_votes(
            committee,
            &update.votes,
            committee.quorum_threshold,
            |vote| {
                vote.epoch == update.epoch
                    && vote.previous_close_hash == update.previous_close_hash
                    && vote.parent_close_hash == update.parent_close_hash
                    && vote.decided_proposals_hash == decided_hash
            },
        )
    }

    fn valid_epoch_close_votes(
        &self,
        committee: &RaiCommittee,
        votes: &[RaiEpochCloseVote],
        threshold: usize,
        vote_predicate: impl Fn(&RaiEpochCloseVote) -> bool,
    ) -> bool {
        let mut voters = BTreeSet::new();
        votes.iter().all(|vote| {
            voters.insert(vote.signer)
                && committee.valid_member_signature(
                    vote.signer,
                    vote.hash().as_bytes(),
                    &vote.signature,
                )
                && vote_predicate(vote)
        }) && voters.len() >= threshold
    }

    fn valid_close_update_votes(
        &self,
        committee: &RaiCommittee,
        votes: &[RaiCloseUpdateVote],
        threshold: usize,
        vote_predicate: impl Fn(&RaiCloseUpdateVote) -> bool,
    ) -> bool {
        let mut voters = BTreeSet::new();
        votes.iter().all(|vote| {
            voters.insert(vote.signer)
                && committee.valid_member_signature(
                    vote.signer,
                    vote.hash().as_bytes(),
                    &vote.signature,
                )
                && vote_predicate(vote)
        }) && voters.len() >= threshold
    }

    fn valid_closing_decision_vote_set(
        &self,
        election: RaiElectionId,
        vote_sets: &[RaiVoteSet],
        vote_predicate: impl Fn(&RaiVote) -> bool,
    ) -> bool {
        if self.epoch_phase(election.epoch) != Some(RaiEpochPhase::Closing) {
            return false;
        }

        if !self.valid_election_context(election) {
            return false;
        }

        let Some(committee) = self.committee(election.epoch) else {
            return false;
        };

        if vote_sets.len() != 1 {
            return false;
        }

        let vote_set = &vote_sets[0];
        vote_set.committee_epoch == committee.epoch
            && self.valid_vote_collection(
                committee,
                &vote_set.votes,
                committee.quorum_threshold,
                vote_predicate,
            )
    }

    fn valid_below_threshold_timeout_evidence(
        &self,
        decision: &RaiTimeoutDecision,
        evidence: &RaiBelowThresholdTimeoutEvidence,
    ) -> bool {
        let Some(committee) = self.committee(decision.election.epoch) else {
            return false;
        };
        let report_set = RaiStopReportSet::new(decision.election.epoch, evidence.reports.clone());
        if !self.valid_stop_report_set(&report_set, committee)
            || report_set.preserves(decision.election, committee.preservation_threshold)
        {
            return false;
        }

        let voters = self.timeout_decision_voters(decision);
        let evidence_by_signer = evidence
            .signer_evidence
            .iter()
            .map(|item| (item.signer(), item))
            .collect::<BTreeMap<_, _>>();
        if evidence_by_signer.len() != evidence.signer_evidence.len()
            || evidence_by_signer.len() != voters.len()
        {
            return false;
        }

        voters.iter().all(|voter| {
            evidence_by_signer.get(voter).is_some_and(|item| {
                item.election() == decision.election
                    && self.valid_close_evidence(item)
                    && self.close_evidence_matches_report_set(item, &report_set)
            })
        })
    }

    fn valid_exclusion_timeout_evidence(
        &self,
        decision: &RaiTimeoutDecision,
        evidence: &RaiExclusionTimeoutEvidence,
    ) -> bool {
        let Some(committee) = self.committee(decision.election.epoch) else {
            return false;
        };

        let mut votes_by_proposal = BTreeMap::<BlockHash, usize>::new();
        let mut signers = BTreeSet::new();
        for item in &evidence.items {
            if item.election() != decision.election
                || !committee.contains(&item.signer())
                || !self.valid_close_evidence(item)
                || !signers.insert(item.signer())
            {
                return false;
            }

            if let RaiCloseEvidence::FirstVote(vote) = item {
                *votes_by_proposal.entry(vote.proposal_hash).or_default() += 1;
            }
        }

        signers.len() >= committee.fast_threshold
            && votes_by_proposal
                .values()
                .all(|votes| signers.len() - *votes >= committee.fast_threshold)
    }

    fn valid_closing_proposal_evidence(
        &self,
        election: RaiElectionId,
        proposal_hash: BlockHash,
        evidence: &RaiClosingProposalEvidence,
    ) -> bool {
        let Some(committee) = self.committee(election.epoch) else {
            return false;
        };
        let Some(stats) = self.close_proof_stats(election, committee, &evidence.items) else {
            return false;
        };

        stats.preferred_eligible_proposal(committee.support_threshold) == Some(proposal_hash)
    }

    fn valid_closing_timeout_evidence(
        &self,
        election: RaiElectionId,
        evidence: &RaiClosingTimeoutEvidence,
    ) -> bool {
        let Some(committee) = self.committee(election.epoch) else {
            return false;
        };
        let Some(stats) = self.close_proof_stats(election, committee, &evidence.items) else {
            return false;
        };

        stats.max_proposal_votes() < committee.support_threshold
            && stats.item_count - stats.max_proposal_votes() >= committee.support_threshold
    }

    fn close_proof_stats(
        &self,
        election: RaiElectionId,
        committee: &RaiCommittee,
        items: &[RaiCloseEvidence],
    ) -> Option<RaiCloseProofStats> {
        let mut signers = BTreeSet::new();
        let mut votes_by_proposal = BTreeMap::<BlockHash, usize>::new();
        for item in items {
            if item.election() != election
                || !committee.contains(&item.signer())
                || !self.valid_close_evidence(item)
                || !signers.insert(item.signer())
            {
                return None;
            }

            if let RaiCloseEvidence::FirstVote(vote) = item {
                *votes_by_proposal.entry(vote.proposal_hash).or_default() += 1;
            }
        }

        Some(RaiCloseProofStats {
            item_count: signers.len(),
            votes_by_proposal,
        })
    }

    fn timeout_decision_voters(&self, decision: &RaiTimeoutDecision) -> BTreeSet<PublicKey> {
        decision
            .timeout_vote_sets
            .iter()
            .flat_map(|vote_set| vote_set.votes.iter().map(|vote| vote.voter))
            .collect()
    }

    fn close_evidence_matches_report_set(
        &self,
        evidence: &RaiCloseEvidence,
        report_set: &RaiStopReportSet,
    ) -> bool {
        match evidence {
            RaiCloseEvidence::FirstVote(_) => true,
            RaiCloseEvidence::NilVote(_) => true,
            RaiCloseEvidence::ReportOmission(omission) => report_set
                .omitted_report(omission.election, omission.signer())
                .is_some_and(|report| report == omission.report),
        }
    }

    fn valid_open_decision_vote_sets(
        &self,
        election: RaiElectionId,
        vote_sets: &[RaiVoteSet],
        threshold: impl Fn(&RaiCommittee) -> usize,
        vote_predicate: impl Fn(&RaiVote) -> bool,
    ) -> bool {
        let active_committees = self.open_decision_committees(election);
        if active_committees.is_empty() || vote_sets.len() != active_committees.len() {
            return false;
        }

        let vote_sets_by_epoch = vote_sets
            .iter()
            .map(|vote_set| (vote_set.committee_epoch, vote_set))
            .collect::<BTreeMap<_, _>>();
        if vote_sets_by_epoch.len() != vote_sets.len() {
            return false;
        }

        active_committees.iter().all(|committee| {
            vote_sets_by_epoch
                .get(&committee.epoch)
                .is_some_and(|vote_set| {
                    self.valid_vote_collection(
                        committee,
                        &vote_set.votes,
                        threshold(committee),
                        &vote_predicate,
                    )
                })
        })
    }

    fn valid_vote_collection(
        &self,
        committee: &RaiCommittee,
        votes: &[RaiVote],
        threshold: usize,
        vote_predicate: impl Fn(&RaiVote) -> bool,
    ) -> bool {
        let mut voters = BTreeSet::new();
        votes.iter().all(|vote| {
            voters.insert(vote.voter)
                && committee.valid_member_signature(
                    vote.voter,
                    vote.hash().as_bytes(),
                    &vote.signature,
                )
                && vote_predicate(vote)
        }) && voters.len() >= threshold
    }

    fn is_active_open_member(&self, election: RaiElectionId, signer: &PublicKey) -> bool {
        self.active_open_committees(election)
            .iter()
            .any(|committee| committee.contains(signer))
    }

    fn valid_stop_report(&self, report: &RaiStopReport) -> bool {
        if self.epoch_phase(report.epoch) != Some(RaiEpochPhase::Closing) {
            return false;
        }

        let Some(committee) = self.committee(report.epoch) else {
            return false;
        };

        committee.valid_member_signature(report.signer, report.hash().as_bytes(), &report.signature)
            && report.started_elections.iter().all(|election| {
                election.epoch == report.epoch && self.valid_election_context(*election)
            })
    }

    fn valid_close_evidence(&self, evidence: &RaiCloseEvidence) -> bool {
        let election = evidence.election();
        if self.epoch_phase(election.epoch) != Some(RaiEpochPhase::Closing) {
            return false;
        }

        if !self.valid_election_context(election) {
            return false;
        }

        let Some(committee) = self.committee(election.epoch) else {
            return false;
        };

        match evidence {
            RaiCloseEvidence::FirstVote(vote) => {
                vote.election == election
                    && committee.valid_member_signature(
                        vote.voter,
                        vote.hash().as_bytes(),
                        &vote.signature,
                    )
                    && committee.valid_member_signature(
                        vote.voter,
                        vote.notar_hash().as_bytes(),
                        &vote.notar_signature,
                    )
            }
            RaiCloseEvidence::NilVote(vote) => {
                vote.election == election
                    && committee.valid_member_signature(
                        vote.signer,
                        vote.hash().as_bytes(),
                        &vote.signature,
                    )
            }
            RaiCloseEvidence::ReportOmission(omission) => {
                omission.election == election
                    && omission.report.epoch == election.epoch
                    && !omission.report.started_elections.contains(&election)
                    && self.valid_stop_report(&omission.report)
            }
        }
    }

    fn valid_stop_report_set(
        &self,
        report_set: &RaiStopReportSet,
        committee: &RaiCommittee,
    ) -> bool {
        report_set.epoch == committee.epoch
            && report_set.reports.len() >= committee.report_threshold
            && report_set.reports.first().is_some_and(|first| {
                report_set
                    .reports
                    .iter()
                    .all(|report| report.previous_close_hash == first.previous_close_hash)
            })
            && report_set
                .reports
                .iter()
                .all(|report| self.valid_stop_report(report))
    }

    fn signer_participated(&self, election_state: &RaiElectionState, signer: PublicKey) -> bool {
        election_state.first_votes.contains_key(&signer)
            || election_state.cert_votes.contains_key(&signer)
            || election_state.final_votes.contains_key(&signer)
            || election_state.timeout_votes.contains(&signer)
    }

    fn record_first_vote(
        &mut self,
        election: RaiElectionId,
        voter: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        let election_state = self
            .elections
            .entry(election)
            .or_insert_with(|| RaiElectionState::new(election));

        if election_state.nil_votes.contains(&voter) {
            return false;
        }

        if let Some(existing) = election_state.first_votes.get(&voter) {
            return *existing == proposal_hash;
        }

        election_state.first_votes.insert(voter, proposal_hash);
        true
    }

    fn record_proposal_block(&mut self, election: RaiElectionId, block: Block) -> bool {
        let proposal_hash = block.hash();
        let election_state = self
            .elections
            .entry(election)
            .or_insert_with(|| RaiElectionState::new(election));

        let changed = election_state.proposals.insert(proposal_hash);
        election_state
            .proposal_blocks
            .insert(proposal_hash, block)
            .is_none()
            || changed
    }

    fn record_cert_vote(
        &mut self,
        election: RaiElectionId,
        voter: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        let election_state = self
            .elections
            .entry(election)
            .or_insert_with(|| RaiElectionState::new(election));

        if election_state.nil_votes.contains(&voter) {
            return false;
        }

        let changed = election_state
            .cert_votes
            .entry(voter)
            .or_default()
            .insert(proposal_hash);
        self.record_slot_cert_support(election.slot, voter, proposal_hash);
        changed
    }

    fn record_final_vote(
        &mut self,
        election: RaiElectionId,
        voter: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        let election_state = self
            .elections
            .entry(election)
            .or_insert_with(|| RaiElectionState::new(election));

        if election_state.nil_votes.contains(&voter) {
            return false;
        }

        if election_state.timeout_votes.contains(&voter) {
            return false;
        }

        if let Some(existing) = election_state.final_votes.get(&voter) {
            return *existing == proposal_hash;
        }

        election_state.final_votes.insert(voter, proposal_hash);
        self.record_slot_final_vote(election.slot, voter, proposal_hash);
        true
    }

    fn record_timeout_vote(&mut self, election: RaiElectionId, voter: PublicKey) -> bool {
        let election_state = self
            .elections
            .entry(election)
            .or_insert_with(|| RaiElectionState::new(election));

        if election_state.nil_votes.contains(&voter) {
            return false;
        }

        if election_state.final_votes.contains_key(&voter) {
            return false;
        }

        election_state.timeout_votes.insert(voter)
    }

    fn record_certified_vote(&mut self, vote: RaiVote) -> bool {
        let election = vote.election;
        let voter = vote.voter;
        let phase = vote.phase;
        let target = vote.target;
        let election_state = self
            .elections
            .entry(election)
            .or_insert_with(|| RaiElectionState::new(election));

        if election_state.nil_votes.contains(&voter) {
            return false;
        }

        match (phase, target) {
            (RaiVotePhase::First, RaiVoteTarget::Proposal(proposal_hash)) => {
                if election_state.timeout_votes.contains(&voter) {
                    return false;
                }
                if election_state
                    .first_votes
                    .get(&voter)
                    .is_some_and(|existing| *existing != proposal_hash)
                    || election_state
                        .first_vote_objects
                        .get(&voter)
                        .is_some_and(|existing| existing != &vote)
                {
                    return false;
                }

                election_state.first_votes.insert(voter, proposal_hash);
                election_state
                    .cert_votes
                    .entry(voter)
                    .or_default()
                    .insert(proposal_hash);
                election_state.first_vote_objects.insert(voter, vote);
                self.record_slot_cert_support(election.slot, voter, proposal_hash);
                true
            }
            (RaiVotePhase::Cert, RaiVoteTarget::Proposal(proposal_hash)) => {
                if election_state
                    .cert_vote_objects
                    .get(&voter)
                    .and_then(|votes| votes.get(&proposal_hash))
                    .is_some_and(|existing| existing != &vote)
                {
                    return false;
                }

                election_state
                    .cert_votes
                    .entry(voter)
                    .or_default()
                    .insert(proposal_hash);
                election_state
                    .cert_vote_objects
                    .entry(voter)
                    .or_default()
                    .insert(proposal_hash, vote);
                self.record_slot_cert_support(election.slot, voter, proposal_hash);
                true
            }
            (RaiVotePhase::Final, RaiVoteTarget::Proposal(proposal_hash)) => {
                if election_state.timeout_votes.contains(&voter)
                    || election_state
                        .final_votes
                        .get(&voter)
                        .is_some_and(|existing| *existing != proposal_hash)
                    || election_state
                        .final_vote_objects
                        .get(&voter)
                        .is_some_and(|existing| existing != &vote)
                {
                    return false;
                }

                election_state.final_votes.insert(voter, proposal_hash);
                election_state.final_vote_objects.insert(voter, vote);
                self.record_slot_final_vote(election.slot, voter, proposal_hash);
                true
            }
            (RaiVotePhase::Timeout, RaiVoteTarget::Timeout) => {
                if election_state.final_votes.contains_key(&voter)
                    || election_state
                        .timeout_vote_objects
                        .get(&voter)
                        .is_some_and(|existing| existing != &vote)
                {
                    return false;
                }

                election_state.timeout_votes.insert(voter);
                election_state.timeout_vote_objects.insert(voter, vote);
                true
            }
            _ => false,
        }
    }

    fn record_stop_report(&mut self, report: RaiStopReport) -> bool {
        let reports = self.stop_reports.entry(report.epoch).or_default();
        if reports.contains_key(&report.signer) {
            return false;
        }

        reports.insert(report.signer, report);
        true
    }

    fn record_close_evidence(&mut self, evidence: RaiCloseEvidence) -> bool {
        if !self.valid_close_evidence(&evidence) {
            return false;
        }

        let election = evidence.election();
        let signer = evidence.signer();
        if !self.elections.contains_key(&election)
            && self.has_live_election_for_slot_except(&election.slot, None)
        {
            return false;
        }

        if let Some(existing) = self
            .close_evidence
            .get(&election)
            .and_then(|by_signer| by_signer.get(&signer))
        {
            return existing == &evidence;
        }

        match &evidence {
            RaiCloseEvidence::FirstVote(vote) => {
                if !self.record_first_vote(election, signer, vote.proposal_hash) {
                    return false;
                }
            }
            RaiCloseEvidence::NilVote(vote) => {
                if vote.signer != signer {
                    return false;
                }
                if self
                    .election(&election)
                    .is_some_and(|state| self.signer_participated(state, signer))
                {
                    return false;
                }
                let election_state = self
                    .elections
                    .entry(election)
                    .or_insert_with(|| RaiElectionState::new(election));
                election_state.nil_votes.insert(signer);
                election_state.nil_vote_objects.insert(signer, vote.clone());
            }
            RaiCloseEvidence::ReportOmission(_) => {
                if self
                    .election(&election)
                    .is_some_and(|state| self.signer_participated(state, signer))
                {
                    return false;
                }
            }
        }

        if self.epoch_phase(election.epoch) == Some(RaiEpochPhase::Closing)
            && !self.completed_slots.contains_key(&election.slot)
        {
            self.mark_carryover(election);
        }

        self.close_evidence
            .entry(election)
            .or_default()
            .insert(signer, evidence)
            .is_none()
    }

    fn record_timeout_decision_evidence(&mut self, evidence: &RaiTimeoutDecisionEvidence) {
        match evidence {
            RaiTimeoutDecisionEvidence::None => {}
            RaiTimeoutDecisionEvidence::BelowThreshold(evidence) => {
                for report in &evidence.reports {
                    self.record_stop_report(report.clone());
                }
                for item in &evidence.signer_evidence {
                    self.record_close_evidence(item.clone());
                }
            }
            RaiTimeoutDecisionEvidence::Exclusion(evidence) => {
                for item in &evidence.items {
                    self.record_close_evidence(item.clone());
                }
            }
            RaiTimeoutDecisionEvidence::Closing(evidence) => {
                for item in &evidence.items {
                    self.record_close_evidence(item.clone());
                }
            }
        }
    }

    fn try_create_first_vote_for_open_election(
        &mut self,
        election: RaiElectionId,
        private_key: Option<&PrivateKey>,
        output: &mut RaiOpenElectionOutput,
    ) {
        let Some(proposal_hash) = self
            .election(&election)
            .and_then(|election_state| election_state.proposals.iter().next().copied())
        else {
            return;
        };

        self.try_create_first_vote(election, proposal_hash, private_key, output);
    }

    fn try_create_first_vote(
        &mut self,
        election: RaiElectionId,
        proposal_hash: BlockHash,
        private_key: Option<&PrivateKey>,
        output: &mut RaiOpenElectionOutput,
    ) {
        let Some(private_key) = private_key else {
            return;
        };

        let signer = private_key.public_key();
        if !self.is_active_open_member(election, &signer)
            || !self.try_lock_first_vote(election, signer, proposal_hash)
        {
            return;
        }

        let vote = RaiVote::proposal(RaiVotePhase::First, election, proposal_hash, private_key);
        self.record_certified_vote(vote.clone());
        output.messages.push(RaiMessage::Vote(vote));
    }

    fn advance_open_election(
        &mut self,
        election: RaiElectionId,
        private_key: Option<&PrivateKey>,
        output: &mut RaiOpenElectionOutput,
    ) {
        let committees = self.active_open_committees(election);
        if committees.is_empty() {
            return;
        }

        self.try_assemble_open_certificates(election, &committees, output);
        self.try_assemble_open_decisions(election, &committees, output);
        if output.terminal_record.is_some() {
            return;
        }

        self.try_complete_open_election(election, &committees, output);
        if output.terminal_record.is_some() {
            return;
        }

        self.try_create_open_epoch_votes(election, &committees, private_key, output);
        self.try_assemble_open_certificates(election, &committees, output);
        self.try_assemble_open_decisions(election, &committees, output);
        if output.terminal_record.is_some() {
            return;
        }

        self.try_complete_open_election(election, &committees, output);
    }

    fn try_assemble_open_certificates(
        &mut self,
        election: RaiElectionId,
        committees: &[RaiCommittee],
        output: &mut RaiOpenElectionOutput,
    ) {
        let Some(election_state) = self.election(&election).cloned() else {
            return;
        };
        let proposals = election_state.proposals.iter().copied().collect::<Vec<_>>();

        for committee in committees {
            for proposal_hash in &proposals {
                let key = RaiCertificateKey {
                    committee_epoch: committee.epoch,
                    election,
                    proposal_hash: *proposal_hash,
                };
                if self.emitted_certificates.contains(&key) {
                    continue;
                }

                let certificate = RaiCertificate::new(
                    committee.epoch,
                    election,
                    *proposal_hash,
                    self.certificate_votes_for(&election_state, committee, *proposal_hash),
                );
                if certificate.votes.len() < committee.quorum_threshold {
                    continue;
                }

                self.emitted_certificates.insert(key);
                output.messages.push(RaiMessage::Certificate(certificate));
            }
        }
    }

    fn try_assemble_open_decisions(
        &mut self,
        election: RaiElectionId,
        committees: &[RaiCommittee],
        output: &mut RaiOpenElectionOutput,
    ) {
        if output.terminal_record.is_some() {
            return;
        }

        let Some(election_state) = self.election(&election).cloned() else {
            return;
        };
        let proposals = election_state.proposals.iter().copied().collect::<Vec<_>>();

        for proposal_hash in &proposals {
            let Some(first_vote_sets) = self.proposal_vote_sets_for(
                &election_state,
                committees,
                *proposal_hash,
                RaiVotePhase::First,
                |committee| committee.fast_threshold,
            ) else {
                continue;
            };
            let outcome = RaiTerminalOutcome::Proposal(*proposal_hash);
            let key = RaiDecisionKey {
                kind: RaiDecisionKind::Fast,
                election,
                outcome,
            };
            if self.emitted_decisions.insert(key) {
                output
                    .messages
                    .push(RaiMessage::FastDecision(RaiFastDecision::new(
                        election,
                        *proposal_hash,
                        first_vote_sets,
                    )));
            }
            self.complete_open_decision(RaiDecisionKind::Fast, election, outcome, output);
            return;
        }

        for proposal_hash in &proposals {
            let Some(cert_vote_sets) =
                self.certificate_vote_sets_for(&election_state, committees, *proposal_hash)
            else {
                continue;
            };
            let outcome = RaiTerminalOutcome::Notarized(*proposal_hash);
            let key = RaiDecisionKey {
                kind: RaiDecisionKind::Notar,
                election,
                outcome,
            };
            if self.emitted_decisions.insert(key) {
                output
                    .messages
                    .push(RaiMessage::NotarDecision(RaiNotarDecision::new(
                        election,
                        *proposal_hash,
                        cert_vote_sets,
                    )));
            }
            self.complete_open_decision(RaiDecisionKind::Notar, election, outcome, output);
            return;
        }

        for proposal_hash in &proposals {
            let Some(final_vote_sets) = self.proposal_vote_sets_for(
                &election_state,
                committees,
                *proposal_hash,
                RaiVotePhase::Final,
                |committee| committee.quorum_threshold,
            ) else {
                continue;
            };
            let outcome = RaiTerminalOutcome::Proposal(*proposal_hash);
            let key = RaiDecisionKey {
                kind: RaiDecisionKind::Final,
                election,
                outcome,
            };
            if self.emitted_decisions.insert(key) {
                output
                    .messages
                    .push(RaiMessage::FinalDecision(RaiFinalDecision::new(
                        election,
                        *proposal_hash,
                        final_vote_sets,
                    )));
            }
            self.complete_open_decision(RaiDecisionKind::Final, election, outcome, output);
            return;
        }

        let Some(timeout_vote_sets) =
            self.timeout_vote_sets_for(&election_state, committees, |committee| {
                committee.quorum_threshold
            })
        else {
            return;
        };
        let outcome = RaiTerminalOutcome::Timeout;
        let key = RaiDecisionKey {
            kind: RaiDecisionKind::Timeout,
            election,
            outcome,
        };
        if self.emitted_decisions.insert(key) {
            output
                .messages
                .push(RaiMessage::TimeoutDecision(RaiTimeoutDecision::new(
                    election,
                    timeout_vote_sets,
                )));
        }
        self.complete_open_decision(RaiDecisionKind::Timeout, election, outcome, output);
    }

    fn certificate_votes_for(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        proposal_hash: BlockHash,
    ) -> Vec<RaiVote> {
        let mut votes = Vec::new();
        votes.extend(
            election_state
                .first_vote_objects
                .values()
                .filter(|vote| {
                    committee.contains(&vote.voter)
                        && vote.phase == RaiVotePhase::First
                        && vote.target == RaiVoteTarget::Proposal(proposal_hash)
                })
                .cloned(),
        );
        votes.extend(
            election_state
                .cert_vote_objects
                .values()
                .filter_map(|votes| votes.get(&proposal_hash))
                .filter(|vote| committee.contains(&vote.voter))
                .filter(|vote| vote.phase == RaiVotePhase::Cert)
                .cloned(),
        );
        votes
    }

    fn certificate_vote_sets_for(
        &self,
        election_state: &RaiElectionState,
        committees: &[RaiCommittee],
        proposal_hash: BlockHash,
    ) -> Option<Vec<RaiVoteSet>> {
        let mut vote_sets = Vec::new();
        for committee in committees {
            let vote_set = RaiVoteSet::new(
                committee.epoch,
                self.certificate_votes_for(election_state, committee, proposal_hash),
            );
            if vote_set.votes.len() < committee.quorum_threshold {
                return None;
            }
            vote_sets.push(vote_set);
        }

        Some(vote_sets)
    }

    fn proposal_vote_sets_for(
        &self,
        election_state: &RaiElectionState,
        committees: &[RaiCommittee],
        proposal_hash: BlockHash,
        phase: RaiVotePhase,
        threshold: impl Fn(&RaiCommittee) -> usize,
    ) -> Option<Vec<RaiVoteSet>> {
        let mut vote_sets = Vec::new();
        for committee in committees {
            let votes = match phase {
                RaiVotePhase::First => election_state
                    .first_vote_objects
                    .values()
                    .filter(|vote| {
                        committee.contains(&vote.voter)
                            && vote.phase == RaiVotePhase::First
                            && vote.target == RaiVoteTarget::Proposal(proposal_hash)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
                RaiVotePhase::Final => election_state
                    .final_vote_objects
                    .values()
                    .filter(|vote| {
                        committee.contains(&vote.voter)
                            && vote.phase == RaiVotePhase::Final
                            && vote.target == RaiVoteTarget::Proposal(proposal_hash)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
                _ => return None,
            };

            let vote_set = RaiVoteSet::new(committee.epoch, votes);
            if vote_set.votes.len() < threshold(committee) {
                return None;
            }
            vote_sets.push(vote_set);
        }

        Some(vote_sets)
    }

    fn timeout_vote_sets_for(
        &self,
        election_state: &RaiElectionState,
        committees: &[RaiCommittee],
        threshold: impl Fn(&RaiCommittee) -> usize,
    ) -> Option<Vec<RaiVoteSet>> {
        let mut vote_sets = Vec::new();
        for committee in committees {
            let vote_set = RaiVoteSet::new(
                committee.epoch,
                election_state
                    .timeout_vote_objects
                    .values()
                    .filter(|vote| {
                        committee.contains(&vote.voter)
                            && vote.phase == RaiVotePhase::Timeout
                            && vote.target == RaiVoteTarget::Timeout
                    })
                    .cloned()
                    .collect(),
            );
            if vote_set.votes.len() < threshold(committee) {
                return None;
            }
            vote_sets.push(vote_set);
        }

        Some(vote_sets)
    }

    fn try_create_open_epoch_votes(
        &mut self,
        election: RaiElectionId,
        committees: &[RaiCommittee],
        private_key: Option<&PrivateKey>,
        output: &mut RaiOpenElectionOutput,
    ) {
        let Some(private_key) = private_key else {
            return;
        };

        let signer = private_key.public_key();
        let signing_committees: Vec<_> = committees
            .iter()
            .filter(|committee| committee.contains(&signer))
            .collect();
        if signing_committees.is_empty() {
            return;
        }

        let blocking_proposals: BTreeSet<_> = signing_committees
            .iter()
            .flat_map(|committee| self.blocking_proposals(election, committee))
            .collect();
        for proposal_hash in blocking_proposals {
            if !self.try_lock_cert_vote(election, signer, proposal_hash) {
                continue;
            }

            let vote = RaiVote::proposal(RaiVotePhase::Cert, election, proposal_hash, private_key);
            self.record_certified_vote(vote.clone());
            output.messages.push(RaiMessage::Vote(vote));
        }

        let finalizable_proposals: BTreeSet<_> = signing_committees
            .iter()
            .flat_map(|committee| self.finalizable_proposals(election, committee, signer))
            .collect();
        for proposal_hash in finalizable_proposals {
            if !self.try_lock_final_vote(election, signer, proposal_hash) {
                continue;
            }

            let vote = RaiVote::proposal(RaiVotePhase::Final, election, proposal_hash, private_key);
            self.record_certified_vote(vote.clone());
            output.messages.push(RaiMessage::Vote(vote));
            break;
        }

        if self.has_local_first_vote(election, signer)
            && self.is_admissible_timeout_vote(election, signer)
            && self.try_lock_timeout_vote(election, signer)
        {
            let vote = RaiVote::timeout(election, private_key);
            self.record_certified_vote(vote.clone());
            output.messages.push(RaiMessage::Vote(vote));
        }
    }

    fn try_assemble_stop_report_set(
        &mut self,
        epoch: SnapshotNumber,
        private_key: Option<&PrivateKey>,
    ) {
        let Some(committee) = self.committee(epoch).cloned() else {
            return;
        };
        let Some(reports) = self.stop_reports.get(&epoch) else {
            return;
        };

        if reports.len() < committee.report_threshold {
            return;
        }

        let report_set =
            RaiStopReportSet::new(epoch, reports.values().cloned().collect::<Vec<_>>());
        if !self.valid_stop_report_set(&report_set, &committee) {
            return;
        }

        let report_set_hash = report_set.hash;
        self.stop_report_sets
            .entry(report_set_hash)
            .or_insert_with(|| report_set.clone());

        self.record_stop_preservation_witnesses(&report_set, &committee);
        self.try_create_stop_work_votes(&report_set, &committee, private_key);
    }

    fn record_stop_preservation_witnesses(
        &mut self,
        report_set: &RaiStopReportSet,
        committee: &RaiCommittee,
    ) {
        for election in report_set.started_elections() {
            if report_set.started_count(election) < committee.preservation_threshold {
                continue;
            }

            self.preservation_witnesses
                .entry(election)
                .or_insert_with(|| RaiPreservationWitness {
                    election,
                    report_set_hash: report_set.hash,
                    reporters: report_set.reporters_for(election),
                });

            if !self.completed_slots.contains_key(&election.slot) {
                self.mark_carryover(election);
            }
        }
    }

    fn try_create_stop_work_votes(
        &mut self,
        report_set: &RaiStopReportSet,
        committee: &RaiCommittee,
        private_key: Option<&PrivateKey>,
    ) {
        let Some(private_key) = private_key else {
            return;
        };
        let signer = private_key.public_key();
        if !committee.contains(&signer) {
            return;
        }

        for election in self.stop_candidate_elections(report_set) {
            if self.preservation_witnesses.contains_key(&election) {
                self.record_stop_report_nil(election, private_key, report_set);
            }
        }
    }

    fn stop_candidate_elections(&self, report_set: &RaiStopReportSet) -> BTreeSet<RaiElectionId> {
        let mut elections = report_set
            .started_elections()
            .into_iter()
            .collect::<BTreeSet<_>>();
        if let Some(carryover) = self.carryover.get(&report_set.epoch) {
            elections.extend(carryover.iter().copied());
        }
        elections
    }

    fn record_stop_report_nil(
        &mut self,
        election: RaiElectionId,
        private_key: &PrivateKey,
        report_set: &RaiStopReportSet,
    ) -> bool {
        let signer = private_key.public_key();
        if self
            .election(&election)
            .is_some_and(|state| self.signer_participated(state, signer))
            || self
                .close_evidence
                .get(&election)
                .is_some_and(|evidence| evidence.contains_key(&signer))
        {
            return false;
        }

        if report_set.omitted_report(election, signer).is_none() {
            return false;
        }

        if !self.try_lock_nil_vote(election, signer) {
            return false;
        }

        self.record_close_evidence(RaiCloseEvidence::NilVote(RaiNilVote::new(
            election,
            private_key,
        )))
    }

    fn blocking_proposals(
        &self,
        election: RaiElectionId,
        committee: &RaiCommittee,
    ) -> Vec<BlockHash> {
        self.election(&election)
            .map(|election_state| {
                election_state
                    .proposals
                    .iter()
                    .copied()
                    .filter(|hash| {
                        self.support_witness(election_state, committee, *hash)
                            .is_some()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn finalizable_proposals(
        &self,
        election: RaiElectionId,
        committee: &RaiCommittee,
        signer: PublicKey,
    ) -> Vec<BlockHash> {
        self.election(&election)
            .map(|election_state| {
                election_state
                    .proposals
                    .iter()
                    .copied()
                    .filter(|hash| {
                        self.can_project_final_vote(election_state, committee, signer, *hash)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn is_admissible_cert_vote(
        &self,
        election: RaiElectionId,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        self.election(&election).is_some_and(|election_state| {
            self.active_open_committees(election)
                .iter()
                .any(|committee| {
                    committee.contains(&signer)
                        && self.can_project_proposal_cert_vote(
                            election_state,
                            committee,
                            signer,
                            proposal_hash,
                        )
                })
        })
    }

    fn is_admissible_final_vote(
        &self,
        election: RaiElectionId,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        self.election(&election).is_some_and(|election_state| {
            self.active_open_committees(election)
                .iter()
                .any(|committee| {
                    committee.contains(&signer)
                        && self.can_project_final_vote(
                            election_state,
                            committee,
                            signer,
                            proposal_hash,
                        )
                })
        })
    }

    fn is_admissible_timeout_vote(&self, election: RaiElectionId, signer: PublicKey) -> bool {
        self.election(&election).is_some_and(|election_state| {
            self.active_open_committees(election)
                .iter()
                .any(|committee| {
                    committee.contains(&signer)
                        && self.can_project_timeout_vote(election_state, committee, signer)
                })
        })
    }

    fn can_project_proposal_cert_vote(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        election_state.first_votes.get(&signer) == Some(&proposal_hash)
            || self
                .support_witness(election_state, committee, proposal_hash)
                .is_some()
    }

    fn can_project_final_vote(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        !election_state.timeout_votes.contains(&signer)
            && !self.signer_has_conflicting_cert_vote(election_state, signer, proposal_hash)
            && !self.signer_has_conflicting_slot_cert_vote(
                election_state.election.slot,
                signer,
                proposal_hash,
            )
            && !self.has_conflicting_support_witness(election_state, committee, proposal_hash)
            && election_state.proposal_blocks.contains_key(&proposal_hash)
            && self
                .support_witness(election_state, committee, proposal_hash)
                .is_some()
            && self.cert_vote_count(election_state, committee, proposal_hash)
                >= committee.quorum_threshold
    }

    fn signer_has_conflicting_cert_vote(
        &self,
        election_state: &RaiElectionState,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        election_state
            .cert_votes
            .get(&signer)
            .is_some_and(|votes| votes.iter().any(|vote| *vote != proposal_hash))
    }

    fn signer_has_conflicting_slot_cert_vote(
        &self,
        slot: RaiSlot,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        self.slot_cert_votes
            .get(&slot)
            .and_then(|votes| votes.get(&signer))
            .is_some_and(|votes| votes.iter().any(|vote| *vote != proposal_hash))
    }

    fn signer_final_voted_different_slot(
        &self,
        slot: RaiSlot,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) -> bool {
        self.slot_final_votes
            .get(&slot)
            .and_then(|votes| votes.get(&signer))
            .is_some_and(|vote| *vote != proposal_hash)
    }

    fn record_slot_cert_support(
        &mut self,
        slot: RaiSlot,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) {
        self.slot_cert_votes
            .entry(slot)
            .or_default()
            .entry(signer)
            .or_default()
            .insert(proposal_hash);
    }

    fn record_slot_final_vote(
        &mut self,
        slot: RaiSlot,
        signer: PublicKey,
        proposal_hash: BlockHash,
    ) {
        self.slot_final_votes
            .entry(slot)
            .or_default()
            .entry(signer)
            .or_insert(proposal_hash);
    }

    fn has_conflicting_support_witness(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        proposal_hash: BlockHash,
    ) -> bool {
        election_state.proposals.iter().any(|candidate| {
            *candidate != proposal_hash
                && self
                    .support_witness(election_state, committee, *candidate)
                    .is_some()
        })
    }

    fn can_project_timeout_vote(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        signer: PublicKey,
    ) -> bool {
        election_state.first_votes.contains_key(&signer)
            && !election_state.final_votes.contains_key(&signer)
            && self.has_conflicting_block_proof(election_state, committee)
    }

    fn has_fast_component(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        proposal_hash: BlockHash,
    ) -> bool {
        self.first_vote_count(election_state, committee, proposal_hash) >= committee.fast_threshold
            && self.cert_vote_count(election_state, committee, proposal_hash)
                >= committee.quorum_threshold
    }

    fn has_final_component(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        proposal_hash: BlockHash,
    ) -> bool {
        self.final_vote_count(election_state, committee, proposal_hash)
            >= committee.quorum_threshold
    }

    fn has_timeout_component(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
    ) -> bool {
        self.timeout_vote_count(election_state, committee) >= committee.quorum_threshold
    }

    fn has_conflicting_block_proof(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
    ) -> bool {
        let first_vote_count = election_state
            .first_votes
            .iter()
            .filter(|(voter, _)| committee.contains(voter))
            .count();
        let max_proposal_count = election_state
            .proposals
            .iter()
            .map(|proposal_hash| self.first_vote_count(election_state, committee, *proposal_hash))
            .max()
            .unwrap_or_default();

        first_vote_count - max_proposal_count >= committee.support_threshold
    }

    fn has_local_first_vote(&self, election: RaiElectionId, signer: PublicKey) -> bool {
        self.signing_locks
            .get(&RaiSigner { election, signer })
            .is_some_and(|lock| lock.first_vote.is_some())
    }

    fn complete_open_decision(
        &mut self,
        _kind: RaiDecisionKind,
        election: RaiElectionId,
        outcome: RaiTerminalOutcome,
        output: &mut RaiOpenElectionOutput,
    ) -> bool {
        if output.terminal_record.is_some() {
            return false;
        }

        let record = RaiTerminalRecord::new(election, outcome);
        if !self.complete_slot(record) {
            return false;
        }

        output.terminal_record = Some(record);
        true
    }

    fn try_complete_open_election(
        &mut self,
        election: RaiElectionId,
        committees: &[RaiCommittee],
        output: &mut RaiOpenElectionOutput,
    ) {
        if output.terminal_record.is_some() {
            return;
        }

        let Some(election_state) = self.election(&election) else {
            return;
        };

        let fast_proposal = election_state.proposals.iter().copied().find(|hash| {
            committees
                .iter()
                .all(|committee| self.has_fast_component(election_state, committee, *hash))
        });
        let final_proposal = election_state.proposals.iter().copied().find(|hash| {
            committees
                .iter()
                .all(|committee| self.has_final_component(election_state, committee, *hash))
        });
        let timeout_certified = committees
            .iter()
            .all(|committee| self.has_timeout_component(election_state, committee));

        let outcome = fast_proposal
            .or(final_proposal)
            .map(RaiTerminalOutcome::Proposal)
            .or_else(|| timeout_certified.then_some(RaiTerminalOutcome::Timeout));

        let Some(outcome) = outcome else {
            return;
        };

        let record = RaiTerminalRecord::new(election, outcome);
        if self.complete_slot(record) {
            output.terminal_record = Some(record);
        }
    }

    fn support_witness(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        proposal_hash: BlockHash,
    ) -> Option<RaiSupportWitness> {
        let voters: BTreeSet<_> = election_state
            .first_votes
            .iter()
            .filter(|(_, voted_hash)| **voted_hash == proposal_hash)
            .map(|(voter, _)| *voter)
            .filter(|voter| committee.contains(voter))
            .collect();

        (voters.len() >= committee.support_threshold).then_some(RaiSupportWitness {
            committee_epoch: committee.epoch,
            proposal_hash,
            voters,
        })
    }

    fn first_vote_count(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        proposal_hash: BlockHash,
    ) -> usize {
        election_state
            .first_votes
            .iter()
            .filter(|(_, voted_hash)| **voted_hash == proposal_hash)
            .filter(|(voter, _)| committee.contains(voter))
            .count()
    }

    fn cert_vote_count(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        proposal_hash: BlockHash,
    ) -> usize {
        committee
            .members
            .keys()
            .filter(|voter| {
                election_state.first_votes.get(voter) == Some(&proposal_hash)
                    || election_state
                        .cert_votes
                        .get(voter)
                        .is_some_and(|votes| votes.contains(&proposal_hash))
            })
            .count()
    }

    fn final_vote_count(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
        proposal_hash: BlockHash,
    ) -> usize {
        election_state
            .final_votes
            .iter()
            .filter(|(_, voted_hash)| **voted_hash == proposal_hash)
            .filter(|(voter, _)| committee.contains(voter))
            .count()
    }

    fn timeout_vote_count(
        &self,
        election_state: &RaiElectionState,
        committee: &RaiCommittee,
    ) -> usize {
        election_state
            .timeout_votes
            .iter()
            .filter(|voter| committee.contains(voter))
            .count()
    }
}

impl Default for RaiState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiEpochPhase {
    Open,
    Closing,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCommittee {
    pub epoch: SnapshotNumber,
    pub members: BTreeMap<PublicKey, Amount>,
    pub fault_tolerance: usize,
    pub fast_path_slack: usize,
    pub quorum_threshold: usize,
    pub fast_threshold: usize,
    pub report_threshold: usize,
    pub preservation_threshold: usize,
    pub support_threshold: usize,
}

impl RaiCommittee {
    pub fn new(
        epoch: SnapshotNumber,
        members: impl IntoIterator<Item = (PublicKey, Amount)>,
        fault_tolerance: usize,
        fast_path_slack: usize,
    ) -> Self {
        let members = members.into_iter().collect::<BTreeMap<_, _>>();
        let n = members.len();
        let f = fault_tolerance;
        let p = fast_path_slack;

        assert!(p >= f, "Rai requires p >= f");
        assert!(
            n >= 3 * f + 2 * p + 1,
            "Rai committee size must satisfy n >= 3f + 2p + 1"
        );

        Self {
            epoch,
            members,
            fault_tolerance: f,
            fast_path_slack: p,
            quorum_threshold: n - f - p,
            fast_threshold: n - p,
            report_threshold: n - f,
            preservation_threshold: f + 1,
            support_threshold: f + p + 1,
        }
    }

    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    pub fn contains(&self, signer: &PublicKey) -> bool {
        self.members.contains_key(signer)
    }

    fn valid_member_signature(
        &self,
        signer: PublicKey,
        message: &[u8],
        signature: &Signature,
    ) -> bool {
        self.contains(&signer) && signer.verify(message, signature).is_ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiSupportWitness {
    pub committee_epoch: SnapshotNumber,
    pub proposal_hash: BlockHash,
    pub voters: BTreeSet<PublicKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RaiCloseProofStats {
    item_count: usize,
    votes_by_proposal: BTreeMap<BlockHash, usize>,
}

impl RaiCloseProofStats {
    fn max_proposal_votes(&self) -> usize {
        self.votes_by_proposal
            .values()
            .copied()
            .max()
            .unwrap_or_default()
    }

    fn preferred_eligible_proposal(&self, support_threshold: usize) -> Option<BlockHash> {
        self.votes_by_proposal
            .iter()
            .filter(|(_, votes)| **votes >= support_threshold)
            .map(|(proposal, _)| *proposal)
            .min()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiElectionState {
    pub election: RaiElectionId,
    pub proposals: BTreeSet<BlockHash>,
    pub proposal_blocks: BTreeMap<BlockHash, Block>,
    pub first_votes: BTreeMap<PublicKey, BlockHash>,
    pub cert_votes: BTreeMap<PublicKey, BTreeSet<BlockHash>>,
    pub final_votes: BTreeMap<PublicKey, BlockHash>,
    pub timeout_votes: BTreeSet<PublicKey>,
    pub nil_votes: BTreeSet<PublicKey>,
    pub first_vote_objects: BTreeMap<PublicKey, RaiVote>,
    pub cert_vote_objects: BTreeMap<PublicKey, BTreeMap<BlockHash, RaiVote>>,
    pub final_vote_objects: BTreeMap<PublicKey, RaiVote>,
    pub timeout_vote_objects: BTreeMap<PublicKey, RaiVote>,
    pub nil_vote_objects: BTreeMap<PublicKey, RaiNilVote>,
    pub terminal_record: Option<RaiTerminalRecord>,
    pub is_carryover: bool,
}

impl RaiElectionState {
    pub fn new(election: RaiElectionId) -> Self {
        Self {
            election,
            proposals: BTreeSet::new(),
            proposal_blocks: BTreeMap::new(),
            first_votes: BTreeMap::new(),
            cert_votes: BTreeMap::new(),
            final_votes: BTreeMap::new(),
            timeout_votes: BTreeSet::new(),
            nil_votes: BTreeSet::new(),
            first_vote_objects: BTreeMap::new(),
            cert_vote_objects: BTreeMap::new(),
            final_vote_objects: BTreeMap::new(),
            timeout_vote_objects: BTreeMap::new(),
            nil_vote_objects: BTreeMap::new(),
            terminal_record: None,
            is_carryover: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiStopReportSet {
    pub epoch: SnapshotNumber,
    pub reports: Vec<RaiStopReport>,
    pub hash: BlockHash,
}

impl RaiStopReportSet {
    pub fn new(epoch: SnapshotNumber, reports: Vec<RaiStopReport>) -> Self {
        let reports = reports
            .into_iter()
            .map(|report| (report.signer, report))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect::<Vec<_>>();
        let hash = Self::hash_reports(epoch, &reports);
        Self {
            epoch,
            reports,
            hash,
        }
    }

    pub fn preserves(&self, election: RaiElectionId, preservation_threshold: usize) -> bool {
        self.started_count(election) >= preservation_threshold
    }

    pub fn started_count(&self, election: RaiElectionId) -> usize {
        self.reports
            .iter()
            .filter(|report| report.started_elections.contains(&election))
            .count()
    }

    pub fn started_elections(&self) -> Vec<RaiElectionId> {
        self.reports
            .iter()
            .flat_map(|report| report.started_elections.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn reporters_for(&self, election: RaiElectionId) -> BTreeSet<PublicKey> {
        self.reports
            .iter()
            .filter(|report| report.started_elections.contains(&election))
            .map(|report| report.signer)
            .collect()
    }

    pub fn omitted_report(
        &self,
        election: RaiElectionId,
        signer: PublicKey,
    ) -> Option<RaiStopReport> {
        self.reports
            .iter()
            .find(|report| report.signer == signer && !report.started_elections.contains(&election))
            .cloned()
    }

    fn hash_reports(epoch: SnapshotNumber, reports: &[RaiStopReport]) -> BlockHash {
        let mut builder = Blake2HashBuilder::new()
            .update(b"rai:stop_report_set")
            .update(&epoch.to_be_bytes());
        for report in reports {
            builder = builder.update(report.hash().as_bytes());
        }
        builder.build()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiPreservationWitness {
    pub election: RaiElectionId,
    pub report_set_hash: BlockHash,
    pub reporters: BTreeSet<PublicKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RaiCertifiedCloseHead {
    epoch: SnapshotNumber,
    previous_close_hash: BlockHash,
    proposal_hashes: Vec<BlockHash>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiStateSnapshot {
    pub current_open_epoch: SnapshotNumber,
    pub open_epochs: usize,
    pub closing_epochs: usize,
    pub closed_epochs: usize,
    pub committees: usize,
    pub elections: usize,
    pub terminal_elections: usize,
    pub completed_slots: usize,
    pub carryover_elections: usize,
    pub stop_reports: usize,
    pub stop_report_sets: usize,
    pub preservation_witnesses: usize,
    pub close_evidence_items: usize,
    pub signing_locks: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RaiSigner {
    election: RaiElectionId,
    signer: PublicKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RaiCertificateKey {
    committee_epoch: SnapshotNumber,
    election: RaiElectionId,
    proposal_hash: BlockHash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RaiDecisionKind {
    Fast,
    Notar,
    Final,
    Timeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RaiDecisionKey {
    kind: RaiDecisionKind,
    election: RaiElectionId,
    outcome: RaiTerminalOutcome,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RaiSigningLock {
    first_vote: Option<BlockHash>,
    cert_votes: BTreeSet<BlockHash>,
    final_vote: Option<BlockHash>,
    timeout_vote: bool,
    timeout_locked: bool,
    nil_locked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_messages::RaiTerminalOutcome;
    use rsnano_types::{Amount, Block};

    fn is_proposal_vote(
        message: &RaiMessage,
        phase: RaiVotePhase,
        election: RaiElectionId,
        proposal_hash: BlockHash,
        voter: PublicKey,
    ) -> bool {
        matches!(
            message,
            RaiMessage::Vote(vote)
                if vote.phase == phase
                    && vote.election == election
                    && vote.target == RaiVoteTarget::Proposal(proposal_hash)
                    && vote.voter == voter
        )
    }

    fn is_timeout_vote(message: &RaiMessage, election: RaiElectionId, voter: PublicKey) -> bool {
        matches!(
            message,
            RaiMessage::Vote(vote)
                if vote.phase == RaiVotePhase::Timeout
                    && vote.election == election
                    && vote.target == RaiVoteTarget::Timeout
                    && vote.voter == voter
        )
    }

    fn is_certificate(
        message: &RaiMessage,
        committee_epoch: SnapshotNumber,
        election: RaiElectionId,
        proposal_hash: BlockHash,
    ) -> bool {
        matches!(
            message,
            RaiMessage::Certificate(certificate)
                if certificate.committee_epoch == committee_epoch
                    && certificate.election == election
                    && certificate.proposal_hash == proposal_hash
                    && !certificate.votes.is_empty()
        )
    }

    fn is_fast_decision(
        message: &RaiMessage,
        election: RaiElectionId,
        proposal_hash: BlockHash,
    ) -> bool {
        matches!(
            message,
            RaiMessage::FastDecision(decision)
                if decision.election == election
                    && decision.proposal_hash == proposal_hash
                    && !decision.first_vote_sets.is_empty()
        )
    }

    #[test]
    fn state_starts_with_epoch_zero_open() {
        let state = RaiState::new();

        assert_eq!(state.snapshot().current_open_epoch, 0);
        assert_eq!(state.epoch_phase(0), Some(RaiEpochPhase::Open));
        assert_eq!(state.current_epoch_context(), RaiEpochContext::bootstrap());
        assert_eq!(state.snapshot().open_epochs, 1);
    }

    #[test]
    fn epoch_context_is_derived_from_prior_close_heads() {
        let mut state = RaiState::new();
        let close_0 = BlockHash::from(10);
        let close_1 = BlockHash::from(11);

        state.open_next_epoch_with_close_head(close_0);

        assert_eq!(state.current_open_epoch, 1);
        assert_eq!(state.current_epoch_context(), RaiEpochContext::bootstrap());

        state.open_next_epoch_with_close_head(close_1);

        assert_eq!(state.current_open_epoch, 2);
        assert_eq!(
            state.current_epoch_context(),
            RaiEpochContext::new(close_0, BlockHash::ZERO)
        );
        assert_eq!(state.election_id(test_slot()).context.close_hash, close_0);
    }

    #[test]
    fn wrong_epoch_context_election_is_rejected() {
        let mut state = RaiState::new();
        let election = RaiElectionId::with_context(
            test_slot(),
            0,
            RaiEpochContext::new(BlockHash::from(10), BlockHash::ZERO),
        );

        assert!(!state.start_election(election));
        assert!(state.election(&election).is_none());
    }

    #[test]
    fn wrong_epoch_context_proposal_is_ignored() {
        let mut state = RaiState::new();
        let local_key = PrivateKey::from(1);
        let election = RaiElectionId::with_context(
            test_slot(),
            0,
            RaiEpochContext::new(BlockHash::from(10), BlockHash::ZERO),
        );

        state.install_committee(RaiCommittee::new(
            0,
            [(local_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        let output = state.process_message(
            RaiMessage::Proposal(RaiProposal::new(
                election,
                Block::new_test_instance_with_key(1000),
            )),
            Some(&local_key),
        );

        assert!(output.messages.is_empty());
        assert_eq!(output.terminal_record, None);
        assert!(state.election(&election).is_none());
    }

    #[test]
    fn opening_next_epoch_keeps_previous_epoch_closing() {
        let mut state = RaiState::new();
        let election = election(0);
        assert!(state.start_election(election));

        let opened = state.open_next_epoch();

        assert_eq!(opened, 1);
        assert_eq!(state.epoch_phase(0), Some(RaiEpochPhase::Closing));
        assert_eq!(state.epoch_phase(1), Some(RaiEpochPhase::Open));
        assert_eq!(state.snapshot().carryover_elections, 1);
        assert!(state.election(&election).unwrap().is_carryover);
    }

    #[test]
    fn proposal_completed_slots_are_not_reopened() {
        let mut state = RaiState::new();
        let election = election(0);
        let proposal_hash = BlockHash::from(10);

        assert!(state.complete_slot(RaiTerminalRecord::new(
            election,
            RaiTerminalOutcome::Proposal(proposal_hash)
        )));

        assert!(!state.start_election(election));
        assert_eq!(
            state.terminal_record(&election.slot),
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Proposal(proposal_hash)
            ))
        );
    }

    #[test]
    fn timeout_completed_elections_do_not_close_slots() {
        let mut state = RaiState::new();
        let first_epoch = election(0);
        let next_epoch = RaiElectionId::new(first_epoch.slot, 1);
        let timeout = RaiTerminalRecord::new(first_epoch, RaiTerminalOutcome::Timeout);

        assert!(state.start_election(first_epoch));
        assert!(state.complete_slot(timeout));

        assert_eq!(state.terminal_record(&first_epoch.slot), None);
        assert_eq!(state.election_terminal_record(&first_epoch), Some(timeout));
        assert!(!state.start_election(first_epoch));

        state.open_next_epoch();

        assert!(state.start_election(next_epoch));
    }

    #[test]
    fn unresolved_carryover_is_not_reopened_in_later_epoch() {
        let mut state = RaiState::new();
        let first_epoch = election(0);
        let next_epoch = RaiElectionId::new(first_epoch.slot, 1);

        assert!(state.start_election(first_epoch));
        state.open_next_epoch();

        assert!(!state.start_election(next_epoch));
    }

    #[test]
    fn stop_reports_create_preservation_witness_and_nil_vote() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let preserved = election(0);

        state.install_committee(close_committee(&keys));
        state.open_next_epoch();

        let reports = [
            RaiStopReport::new(0, BlockHash::from(10), Vec::new(), &keys[0]),
            RaiStopReport::new(0, BlockHash::from(10), vec![preserved], &keys[1]),
            RaiStopReport::new(0, BlockHash::from(10), vec![preserved], &keys[2]),
            RaiStopReport::new(0, BlockHash::from(10), Vec::new(), &keys[3]),
            RaiStopReport::new(0, BlockHash::from(10), Vec::new(), &keys[4]),
        ];

        let mut output = RaiOpenElectionOutput::default();
        for report in reports {
            output = state.process_message(RaiMessage::StopReport(report), Some(&keys[0]));
        }

        let snapshot = state.snapshot();
        assert_eq!(snapshot.stop_reports, 5);
        assert_eq!(snapshot.stop_report_sets, 1);
        assert_eq!(snapshot.preservation_witnesses, 1);
        assert_eq!(snapshot.close_evidence_items, 1);
        assert!(state.preservation_witness(&preserved).is_some());
        assert!(output.messages.is_empty());
        assert!(matches!(
            state
                .close_evidence(&preserved, &keys[0].public_key()),
            Some(RaiCloseEvidence::NilVote(vote))
                if vote.election == preserved
                    && vote.signer == keys[0].public_key()
        ));
    }

    #[test]
    fn closing_timeout_decision_accepts_below_threshold_evidence() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let election = election(0);

        state.install_committee(close_committee(&keys));
        state.open_next_epoch();

        let reports = keys
            .iter()
            .take(5)
            .map(|key| RaiStopReport::new(0, BlockHash::from(10), Vec::new(), key))
            .collect::<Vec<_>>();
        let votes = keys
            .iter()
            .take(4)
            .map(|key| RaiVote::timeout(election, key))
            .collect::<Vec<_>>();
        let signer_evidence = keys
            .iter()
            .take(4)
            .map(|key| RaiCloseEvidence::NilVote(RaiNilVote::new(election, key)))
            .collect::<Vec<_>>();

        let decision = RaiTimeoutDecision::new_with_evidence(
            election,
            vec![RaiVoteSet::new(0, votes)],
            RaiTimeoutDecisionEvidence::BelowThreshold(RaiBelowThresholdTimeoutEvidence::new(
                reports,
                signer_evidence,
            )),
        );

        let output = state.process_message(RaiMessage::TimeoutDecision(decision), None);

        let timeout = RaiTerminalRecord::new(election, RaiTerminalOutcome::Timeout);
        assert_eq!(output.terminal_record, Some(timeout));
        assert_eq!(state.election_terminal_record(&election), Some(timeout));
        assert_eq!(state.terminal_record(&election.slot), None);
        assert_eq!(state.snapshot().close_evidence_items, 4);
    }

    #[test]
    fn open_timeout_decision_is_adopted_after_epoch_enters_closing() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let election = election(0);

        state.install_committee(close_committee(&keys));
        state.open_next_epoch();

        let votes = keys
            .iter()
            .take(4)
            .map(|key| RaiVote::timeout(election, key))
            .collect::<Vec<_>>();

        let output = state.process_message(
            RaiMessage::TimeoutDecision(RaiTimeoutDecision::new(
                election,
                vec![RaiVoteSet::new(0, votes)],
            )),
            None,
        );

        let timeout = RaiTerminalRecord::new(election, RaiTerminalOutcome::Timeout);
        assert_eq!(output.terminal_record, Some(timeout));
        assert_eq!(state.election_terminal_record(&election), Some(timeout));
    }

    #[test]
    fn closing_timeout_decision_accepts_closing_proof_evidence() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let election = election(0);

        state.install_committee(close_committee(&keys));
        state.open_next_epoch();

        let votes = keys
            .iter()
            .take(4)
            .map(|key| RaiVote::timeout(election, key))
            .collect::<Vec<_>>();
        let evidence = RaiClosingTimeoutEvidence::new(
            keys.iter()
                .take(3)
                .map(|key| RaiCloseEvidence::NilVote(RaiNilVote::new(election, key)))
                .collect(),
        );

        let output = state.process_message(
            RaiMessage::TimeoutDecision(RaiTimeoutDecision::new_with_evidence(
                election,
                vec![RaiVoteSet::new(0, votes)],
                RaiTimeoutDecisionEvidence::Closing(evidence),
            )),
            None,
        );

        let timeout = RaiTerminalRecord::new(election, RaiTerminalOutcome::Timeout);
        assert_eq!(output.terminal_record, Some(timeout));
        assert_eq!(state.election_terminal_record(&election), Some(timeout));
    }

    #[test]
    fn closing_timeout_decision_rejects_proposal_eligible_evidence() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let election = election(0);
        let proposal_hash = BlockHash::from(10);

        state.install_committee(close_committee(&keys));
        state.open_next_epoch();

        let votes = keys
            .iter()
            .take(4)
            .map(|key| RaiVote::timeout(election, key))
            .collect::<Vec<_>>();
        let evidence = RaiClosingTimeoutEvidence::new(
            keys.iter()
                .take(3)
                .map(|key| {
                    RaiCloseEvidence::FirstVote(RaiFirstVote::new(election, proposal_hash, key))
                })
                .collect(),
        );

        let output = state.process_message(
            RaiMessage::TimeoutDecision(RaiTimeoutDecision::new_with_evidence(
                election,
                vec![RaiVoteSet::new(0, votes)],
                RaiTimeoutDecisionEvidence::Closing(evidence),
            )),
            None,
        );

        assert_eq!(output.terminal_record, None);
        assert_eq!(state.election_terminal_record(&election), None);
    }

    #[test]
    fn open_final_decision_is_adopted_after_epoch_enters_closing() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let election = election(0);
        let proposal_hash = BlockHash::from(10);

        state.install_committee(close_committee(&keys));
        state.open_next_epoch();

        let votes = keys
            .iter()
            .take(4)
            .map(|key| RaiVote::proposal(RaiVotePhase::Final, election, proposal_hash, key))
            .collect::<Vec<_>>();

        let output = state.process_message(
            RaiMessage::FinalDecision(RaiFinalDecision::new(
                election,
                proposal_hash,
                vec![RaiVoteSet::new(0, votes)],
            )),
            None,
        );

        let terminal =
            RaiTerminalRecord::new(election, RaiTerminalOutcome::Proposal(proposal_hash));
        assert_eq!(output.terminal_record, Some(terminal));
        assert_eq!(state.terminal_record(&election.slot), Some(terminal));
    }

    #[test]
    fn closing_final_decision_accepts_preferred_closing_evidence() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let election = election(0);
        let proposal_hash = BlockHash::from(10);

        state.install_committee(close_committee(&keys));
        state.open_next_epoch();

        let votes = keys
            .iter()
            .take(4)
            .map(|key| RaiVote::proposal(RaiVotePhase::Final, election, proposal_hash, key))
            .collect::<Vec<_>>();
        let closing_evidence = RaiClosingProposalEvidence::new(
            keys.iter()
                .take(3)
                .map(|key| {
                    RaiCloseEvidence::FirstVote(RaiFirstVote::new(election, proposal_hash, key))
                })
                .collect(),
        );

        let output = state.process_message(
            RaiMessage::FinalDecision(RaiFinalDecision::new_with_closing_evidence(
                election,
                proposal_hash,
                vec![RaiVoteSet::new(0, votes)],
                closing_evidence,
            )),
            None,
        );

        let terminal =
            RaiTerminalRecord::new(election, RaiTerminalOutcome::Proposal(proposal_hash));
        assert_eq!(output.terminal_record, Some(terminal));
        assert_eq!(state.terminal_record(&election.slot), Some(terminal));
    }

    #[test]
    fn final_vote_blocks_timeout_vote() {
        let mut state = RaiState::new();
        let election = election(0);
        let signer = PublicKey::from(1);

        assert!(state.try_lock_final_vote(election, signer, BlockHash::from(10)));

        assert!(!state.try_lock_timeout_vote(election, signer));
    }

    #[test]
    fn final_vote_requires_no_conflicting_local_cert_votes() {
        let mut state = RaiState::new();
        let election = election(0);
        let signer = PublicKey::from(1);
        let other_signer = PublicKey::from(2);
        let proposal_a = BlockHash::from(10);
        let proposal_b = BlockHash::from(11);

        assert!(state.try_lock_cert_vote(election, signer, proposal_a));
        assert!(state.try_lock_cert_vote(election, signer, proposal_b));

        assert!(!state.try_lock_final_vote(election, signer, proposal_a));
        assert!(!state.try_lock_final_vote(election, signer, proposal_b));

        assert!(state.try_lock_cert_vote(election, other_signer, proposal_a));
        assert!(state.try_lock_final_vote(election, other_signer, proposal_a));
        assert!(!state.try_lock_cert_vote(election, other_signer, proposal_b));
    }

    #[test]
    fn timeout_vote_blocks_later_first_and_final_signing() {
        let mut state = RaiState::new();
        let election = election(0);
        let signer = PublicKey::from(1);

        assert!(state.try_lock_timeout_vote(election, signer));

        assert!(!state.try_lock_first_vote(election, signer, BlockHash::from(10)));
        assert!(!state.try_lock_final_vote(election, signer, BlockHash::from(10)));
        assert!(state.try_lock_cert_vote(election, signer, BlockHash::from(10)));
    }

    #[test]
    fn timeout_vote_is_single_use_and_allows_later_cert_signing() {
        let mut state = RaiState::new();
        let election = election(0);
        let signer = PublicKey::from(1);

        assert!(state.try_lock_timeout_vote(election, signer));

        assert!(!state.try_lock_first_vote(election, signer, BlockHash::from(10)));
        assert!(!state.try_lock_final_vote(election, signer, BlockHash::from(10)));
        assert!(state.try_lock_cert_vote(election, signer, BlockHash::from(10)));
        assert!(!state.try_lock_timeout_vote(election, signer));
    }

    #[test]
    fn nil_vote_locks_later_participation() {
        let mut state = RaiState::new();
        let election = election(0);
        let signer = PublicKey::from(1);

        assert!(state.try_lock_nil_vote(election, signer));

        assert!(!state.try_lock_first_vote(election, signer, BlockHash::from(10)));
        assert!(!state.try_lock_cert_vote(election, signer, BlockHash::from(10)));
        assert!(!state.try_lock_final_vote(election, signer, BlockHash::from(10)));
        assert!(!state.try_lock_timeout_vote(election, signer));
        assert!(!state.try_lock_nil_vote(election, signer));
    }

    #[test]
    fn proposal_does_not_create_election_in_future_epoch() {
        let mut state = RaiState::new();
        let future = election(1);

        assert!(!state.add_proposal(future, BlockHash::from(10)));
    }

    #[test]
    fn conflict_creates_election_with_both_proposals() {
        let mut state = RaiState::new();
        let election = election(0);
        let existing = BlockHash::from(10);
        let fork = BlockHash::from(11);

        assert!(state.handle_block_conflict(election, fork, Some(existing)));

        let election_state = state.election(&election).unwrap();
        assert_eq!(
            election_state.proposals,
            [existing, fork].into_iter().collect()
        );
    }

    #[test]
    fn conflict_does_not_reopen_completed_slot() {
        let mut state = RaiState::new();
        let election = election(0);
        let proposal_hash = BlockHash::from(10);
        state.complete_slot(RaiTerminalRecord::new(
            election,
            RaiTerminalOutcome::Proposal(proposal_hash),
        ));

        assert!(!state.handle_block_conflict(election, BlockHash::from(11), Some(proposal_hash)));
    }

    #[test]
    fn first_vote_is_single_value_per_signer_and_election() {
        let mut state = RaiState::new();
        let election = election(0);
        let signer = PublicKey::from(1);

        assert!(state.try_lock_first_vote(election, signer, BlockHash::from(10)));
        assert!(state.try_lock_first_vote(election, signer, BlockHash::from(10)));
        assert!(!state.try_lock_first_vote(election, signer, BlockHash::from(11)));
    }

    #[test]
    fn committee_is_stored_by_epoch() {
        let mut state = RaiState::new();
        let key = PublicKey::from(1);

        state.install_committee(RaiCommittee::new(0, [(key, Amount::nano(1))], 0, 0));

        let committee = state.committee(0).unwrap();
        assert_eq!(committee.member_count(), 1);
        assert!(committee.contains(&key));
        assert_eq!(committee.fault_tolerance, 0);
        assert_eq!(committee.fast_path_slack, 0);
        assert_eq!(committee.quorum_threshold, 1);
        assert_eq!(committee.fast_threshold, 1);
        assert_eq!(committee.report_threshold, 1);
        assert_eq!(committee.preservation_threshold, 1);
        assert_eq!(committee.support_threshold, 1);
    }

    #[test]
    fn close_update_extends_parent_and_carries_forward_proposals() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let previous_close_hash = BlockHash::from(100);
        let proposal_a = BlockHash::from(10);
        let proposal_b = BlockHash::from(11);

        state.install_committee(close_committee(&keys));

        let close_votes = keys
            .iter()
            .take(4)
            .map(|key| RaiEpochCloseVote::new(0, previous_close_hash, &[proposal_a], key))
            .collect::<Vec<_>>();
        let close = RaiEpochClose::new(0, previous_close_hash, vec![proposal_a], close_votes);
        let parent_hash = close.hash();
        state.process_message(RaiMessage::EpochClose(close), None);

        assert!(state.certified_close_heads.contains_key(&parent_hash));
        assert_eq!(state.close_heads.get(&0), Some(&parent_hash));

        let update_votes = keys
            .iter()
            .take(4)
            .map(|key| {
                RaiCloseUpdateVote::new(
                    0,
                    previous_close_hash,
                    parent_hash,
                    &[proposal_a, proposal_b],
                    key,
                )
            })
            .collect::<Vec<_>>();
        let update = RaiCloseUpdate::new(
            0,
            previous_close_hash,
            parent_hash,
            vec![proposal_b, proposal_a],
            update_votes,
        );
        let update_hash = update.hash();
        state.process_message(RaiMessage::CloseUpdate(update), None);

        assert!(state.certified_close_heads.contains_key(&update_hash));
        assert_eq!(state.close_heads.get(&0), Some(&update_hash));
        assert_eq!(
            state
                .certified_close_heads
                .get(&update_hash)
                .unwrap()
                .proposal_hashes,
            vec![proposal_a, proposal_b]
        );
    }

    #[test]
    fn close_update_that_drops_parent_proposal_is_rejected() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let previous_close_hash = BlockHash::from(100);
        let proposal_a = BlockHash::from(10);
        let proposal_b = BlockHash::from(11);

        state.install_committee(close_committee(&keys));

        let close_votes = keys
            .iter()
            .take(4)
            .map(|key| {
                RaiEpochCloseVote::new(0, previous_close_hash, &[proposal_a, proposal_b], key)
            })
            .collect::<Vec<_>>();
        let close = RaiEpochClose::new(
            0,
            previous_close_hash,
            vec![proposal_a, proposal_b],
            close_votes,
        );
        let parent_hash = close.hash();
        state.process_message(RaiMessage::EpochClose(close), None);

        let update_votes = keys
            .iter()
            .take(4)
            .map(|key| {
                RaiCloseUpdateVote::new(0, previous_close_hash, parent_hash, &[proposal_a], key)
            })
            .collect::<Vec<_>>();
        let update = RaiCloseUpdate::new(
            0,
            previous_close_hash,
            parent_hash,
            vec![proposal_a],
            update_votes,
        );
        let update_hash = update.hash();
        state.process_message(RaiMessage::CloseUpdate(update), None);

        assert!(!state.certified_close_heads.contains_key(&update_hash));
        assert_eq!(state.close_heads.get(&0), Some(&parent_hash));
    }

    #[test]
    fn proposal_message_creates_first_vote_and_fast_terminal_record() {
        let mut state = RaiState::new();
        let local_key = PrivateKey::from(1);
        let election = election(0);
        let block = Block::new_test_instance_with_key(1000);
        let proposal_hash = block.hash();

        state.install_committee(RaiCommittee::new(
            0,
            [(local_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        let output = state.process_message(
            RaiMessage::Proposal(RaiProposal::new(election, block)),
            Some(&local_key),
        );

        assert!(output.messages.iter().any(|message| matches!(
            message,
            _ if is_proposal_vote(
                message,
                RaiVotePhase::First,
                election,
                proposal_hash,
                local_key.public_key()
            )
        )));
        assert!(output.messages.iter().any(|message| is_certificate(
            message,
            0,
            election,
            proposal_hash
        )));
        assert!(output.messages.iter().any(|message| is_fast_decision(
            message,
            election,
            proposal_hash
        )));
        assert_eq!(
            output.terminal_record,
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Proposal(proposal_hash)
            ))
        );
        assert_eq!(
            state.terminal_record(&election.slot),
            output.terminal_record
        );
    }

    #[test]
    fn fast_decision_message_completes_slot_from_vote_set() {
        let mut state = RaiState::new();
        let voter = PrivateKey::from(2);
        let election = election(0);
        let proposal_hash = BlockHash::from(10);
        let vote = RaiVote::proposal(RaiVotePhase::First, election, proposal_hash, &voter);

        state.install_committee(RaiCommittee::new(
            0,
            [(voter.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        let output = state.process_message(
            RaiMessage::FastDecision(RaiFastDecision::new(
                election,
                proposal_hash,
                vec![RaiVoteSet::new(0, vec![vote])],
            )),
            None,
        );

        assert_eq!(
            output.terminal_record,
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Proposal(proposal_hash)
            ))
        );
        assert_eq!(
            state.terminal_record(&election.slot),
            output.terminal_record
        );
    }

    #[test]
    fn first_vote_fast_certificate_completes_without_local_key() {
        let mut state = RaiState::new();
        let voter = PrivateKey::from(2);
        let election = election(0);
        let proposal_hash = BlockHash::from(10);

        state.install_committee(RaiCommittee::new(
            0,
            [(voter.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        let output = state.process_message(
            RaiMessage::LegacyFirstVote(RaiFirstVote::new(election, proposal_hash, &voter)),
            None,
        );

        assert!(output.messages.is_empty());
        assert_eq!(
            output.terminal_record,
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Proposal(proposal_hash)
            ))
        );
    }

    #[test]
    fn open_epoch_after_bootstrap_requires_joint_fast_components() {
        let mut state = RaiState::new();
        let current_key = PrivateKey::from(1);
        let previous_key = PrivateKey::from(2);
        let election = election(1);
        let block = Block::new_test_instance_with_key(1000);
        let proposal_hash = block.hash();

        state.install_committee(RaiCommittee::new(
            0,
            [(previous_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));
        state.install_committee(RaiCommittee::new(
            1,
            [(current_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));
        state.open_next_epoch();

        let output = state.process_message(
            RaiMessage::Proposal(RaiProposal::new(election, block)),
            Some(&current_key),
        );

        assert_eq!(output.terminal_record, None);
        assert_eq!(state.terminal_record(&election.slot), None);

        let output = state.process_message(
            RaiMessage::LegacyFirstVote(RaiFirstVote::new(election, proposal_hash, &previous_key)),
            None,
        );

        assert_eq!(
            output.terminal_record,
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Proposal(proposal_hash)
            ))
        );
    }

    #[test]
    fn open_epoch_after_bootstrap_accepts_previous_committee_votes() {
        let mut state = RaiState::new();
        let current_key = PrivateKey::from(1);
        let previous_key = PrivateKey::from(2);
        let election = election(1);
        let proposal_hash = BlockHash::from(10);

        state.install_committee(RaiCommittee::new(
            0,
            [(previous_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));
        state.install_committee(RaiCommittee::new(
            1,
            [(current_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));
        state.open_next_epoch();

        let output = state.process_message(
            RaiMessage::LegacyFirstVote(RaiFirstVote::new(election, proposal_hash, &previous_key)),
            None,
        );

        assert!(output.messages.is_empty());
        assert_eq!(output.terminal_record, None);
        assert!(
            state
                .election(&election)
                .unwrap()
                .first_votes
                .contains_key(&previous_key.public_key())
        );
    }

    #[test]
    fn block_proof_and_cert_vote_create_local_final_vote() {
        let mut state = RaiState::new();
        let local_key = PrivateKey::from(1);
        let remote_key_1 = PrivateKey::from(2);
        let remote_key_2 = PrivateKey::from(3);
        let remote_key_3 = PrivateKey::from(4);
        let election = election(0);
        let block = Block::new_test_instance_with_key(1000);
        let proposal_hash = block.hash();

        state.install_committee(RaiCommittee::new(
            0,
            [
                (local_key.public_key(), Amount::nano(1)),
                (remote_key_1.public_key(), Amount::nano(1)),
                (remote_key_2.public_key(), Amount::nano(1)),
                (remote_key_3.public_key(), Amount::nano(1)),
                (PrivateKey::from(5).public_key(), Amount::nano(1)),
                (PrivateKey::from(6).public_key(), Amount::nano(1)),
            ],
            1,
            1,
        ));

        let output = state.process_message(
            RaiMessage::Proposal(RaiProposal::new(election, block)),
            Some(&local_key),
        );
        assert!(output.messages.iter().any(|message| matches!(
            message,
            _ if is_proposal_vote(
                message,
                RaiVotePhase::First,
                election,
                proposal_hash,
                local_key.public_key()
            )
        )));
        assert!(!output.messages.iter().any(|message| matches!(
            message,
            _ if is_proposal_vote(
                message,
                RaiVotePhase::Final,
                election,
                proposal_hash,
                local_key.public_key()
            )
        )));
        assert_eq!(output.terminal_record, None);

        let output = state.process_message(
            RaiMessage::LegacyFirstVote(RaiFirstVote::new(election, proposal_hash, &remote_key_1)),
            Some(&local_key),
        );
        assert!(!output.messages.iter().any(|message| matches!(
            message,
            _ if is_proposal_vote(
                message,
                RaiVotePhase::Final,
                election,
                proposal_hash,
                local_key.public_key()
            )
        )));
        assert_eq!(output.terminal_record, None);

        let output = state.process_message(
            RaiMessage::LegacyFirstVote(RaiFirstVote::new(election, proposal_hash, &remote_key_2)),
            Some(&local_key),
        );
        assert!(!output.messages.iter().any(|message| matches!(
            message,
            _ if is_proposal_vote(
                message,
                RaiVotePhase::Final,
                election,
                proposal_hash,
                local_key.public_key()
            )
        )));
        assert_eq!(output.terminal_record, None);

        let output = state.process_message(
            RaiMessage::LegacyFirstVote(RaiFirstVote::new(election, proposal_hash, &remote_key_3)),
            Some(&local_key),
        );
        assert!(output.messages.iter().any(|message| matches!(
            message,
            _ if is_proposal_vote(
                message,
                RaiVotePhase::Final,
                election,
                proposal_hash,
                local_key.public_key()
            )
        )));
        assert_eq!(output.terminal_record, None);

        state.process_message(
            RaiMessage::LegacyFinalVote(RaiFinalVote::new(election, proposal_hash, &remote_key_1)),
            Some(&local_key),
        );
        state.process_message(
            RaiMessage::LegacyFinalVote(RaiFinalVote::new(election, proposal_hash, &remote_key_2)),
            Some(&local_key),
        );
        let output = state.process_message(
            RaiMessage::LegacyFinalVote(RaiFinalVote::new(election, proposal_hash, &remote_key_3)),
            Some(&local_key),
        );

        assert_eq!(
            output.terminal_record,
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Proposal(proposal_hash)
            ))
        );
    }

    #[test]
    fn final_vote_without_support_witness_is_ignored() {
        let mut state = RaiState::new();
        let local_key = PrivateKey::from(1);
        let remote_key = PrivateKey::from(2);
        let election = election(0);
        let block = Block::new_test_instance_with_key(1000);
        let proposal_hash = block.hash();

        state.install_committee(RaiCommittee::new(
            0,
            [
                (local_key.public_key(), Amount::nano(1)),
                (remote_key.public_key(), Amount::nano(1)),
                (PrivateKey::from(3).public_key(), Amount::nano(1)),
                (PrivateKey::from(4).public_key(), Amount::nano(1)),
                (PrivateKey::from(5).public_key(), Amount::nano(1)),
                (PrivateKey::from(6).public_key(), Amount::nano(1)),
            ],
            1,
            1,
        ));

        state.process_message(
            RaiMessage::Proposal(RaiProposal::new(election, block)),
            Some(&local_key),
        );

        let output = state.process_message(
            RaiMessage::LegacyFinalVote(RaiFinalVote::new(election, proposal_hash, &remote_key)),
            Some(&local_key),
        );

        assert_eq!(output.terminal_record, None);
        assert!(
            !state
                .election(&election)
                .unwrap()
                .final_votes
                .contains_key(&remote_key.public_key())
        );
    }

    #[test]
    fn final_vote_without_full_proposal_is_ignored() {
        let mut state = RaiState::new();
        let voters = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
            PrivateKey::from(5),
            PrivateKey::from(6),
        ];
        let election = election(0);
        let proposal_hash = Block::new_test_instance_with_key(1000).hash();

        state.install_committee(RaiCommittee::new(
            0,
            voters.iter().map(|key| (key.public_key(), Amount::nano(1))),
            1,
            1,
        ));

        for voter in voters.iter().take(4) {
            state.process_message(
                RaiMessage::LegacyFirstVote(RaiFirstVote::new(election, proposal_hash, voter)),
                None,
            );
        }
        let output = state.process_message(
            RaiMessage::LegacyFinalVote(RaiFinalVote::new(election, proposal_hash, &voters[0])),
            None,
        );

        assert_eq!(output.terminal_record, None);
        assert!(state.election(&election).unwrap().final_votes.is_empty());
    }

    #[test]
    fn final_vote_after_conflicting_cert_vote_is_ignored() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let election = election(0);
        let target_block = Block::new_test_instance_with_key(1000);
        let target_hash = target_block.hash();
        let conflicting_hash = BlockHash::from(11);
        let signer = keys[0].public_key();

        state.install_committee(close_committee(&keys));
        assert!(state.start_election(election));
        state.record_proposal_block(election, target_block);
        state.record_cert_vote(election, signer, conflicting_hash);

        for key in keys.iter().skip(1).take(3) {
            state.record_first_vote(election, key.public_key(), target_hash);
            state.record_cert_vote(election, key.public_key(), target_hash);
        }
        state.record_cert_vote(election, keys[4].public_key(), target_hash);

        let output = state.process_message(
            RaiMessage::Vote(RaiVote::proposal(
                RaiVotePhase::Final,
                election,
                target_hash,
                &keys[0],
            )),
            None,
        );

        assert_eq!(output.terminal_record, None);
        assert!(
            !state
                .election(&election)
                .unwrap()
                .final_votes
                .contains_key(&signer)
        );
    }

    #[test]
    fn final_vote_with_conflicting_many_votes_is_ignored() {
        let mut state = RaiState::new();
        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
            PrivateKey::from(5),
            PrivateKey::from(6),
            PrivateKey::from(7),
        ];
        let election = election(0);
        let target_block = Block::new_test_instance_with_key(1000);
        let target_hash = target_block.hash();
        let conflicting_hash = BlockHash::from(11);
        let signer = keys[0].public_key();

        state.install_committee(RaiCommittee::new(
            0,
            keys.iter().map(|key| (key.public_key(), Amount::nano(1))),
            1,
            1,
        ));
        assert!(state.start_election(election));
        state.record_proposal_block(election, target_block);
        state.add_proposal(election, conflicting_hash);

        for key in keys.iter().skip(1).take(3) {
            state.record_first_vote(election, key.public_key(), target_hash);
            state.record_cert_vote(election, key.public_key(), target_hash);
        }
        for key in keys.iter().skip(4).take(3) {
            state.record_first_vote(election, key.public_key(), conflicting_hash);
        }
        for key in keys.iter().skip(4).take(2) {
            state.record_cert_vote(election, key.public_key(), target_hash);
        }

        let output = state.process_message(
            RaiMessage::Vote(RaiVote::proposal(
                RaiVotePhase::Final,
                election,
                target_hash,
                &keys[0],
            )),
            None,
        );

        assert_eq!(output.terminal_record, None);
        assert!(
            !state
                .election(&election)
                .unwrap()
                .final_votes
                .contains_key(&signer)
        );
    }

    #[test]
    fn conflicting_first_votes_create_timeout_vote() {
        let mut state = RaiState::new();
        let local_key = PrivateKey::from(1);
        let remote_key = PrivateKey::from(2);
        let other_remote_key = PrivateKey::from(3);
        let election = election(0);
        let local_block = Block::new_test_instance_with_key(1000);
        let local_proposal = local_block.hash();
        let conflicting_proposal = BlockHash::from(11);

        state.install_committee(RaiCommittee::new(
            0,
            [
                (local_key.public_key(), Amount::nano(1)),
                (remote_key.public_key(), Amount::nano(1)),
                (other_remote_key.public_key(), Amount::nano(1)),
            ],
            0,
            0,
        ));

        let output = state.process_message(
            RaiMessage::Proposal(RaiProposal::new(election, local_block)),
            Some(&local_key),
        );
        assert!(output.terminal_record.is_none());
        assert!(
            output.messages.len() == 1
                && is_proposal_vote(
                    &output.messages[0],
                    RaiVotePhase::First,
                    election,
                    local_proposal,
                    local_key.public_key()
                )
        );

        let output = state.process_message(
            RaiMessage::LegacyFirstVote(RaiFirstVote::new(
                election,
                conflicting_proposal,
                &remote_key,
            )),
            Some(&local_key),
        );

        assert!(output.messages.iter().any(|message| matches!(
            message,
            _ if is_timeout_vote(message, election, local_key.public_key())
        )));
        assert_eq!(output.terminal_record, None);

        state.process_message(
            RaiMessage::LegacyFirstVote(RaiFirstVote::new(
                election,
                BlockHash::from(12),
                &other_remote_key,
            )),
            Some(&local_key),
        );
        state.process_message(
            RaiMessage::Vote(RaiVote::timeout(election, &remote_key)),
            None,
        );
        let output = state.process_message(
            RaiMessage::Vote(RaiVote::timeout(election, &other_remote_key)),
            None,
        );

        assert_eq!(
            output.terminal_record,
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Timeout
            ))
        );
        assert_ne!(local_proposal, conflicting_proposal);
    }

    #[test]
    fn timeout_vote_without_first_vote_is_ignored() {
        let mut state = RaiState::new();
        let local_key = PrivateKey::from(1);
        let remote_key = PrivateKey::from(2);
        let idle_key = PrivateKey::from(3);
        let election = election(0);
        let local_block = Block::new_test_instance_with_key(1000);

        state.install_committee(RaiCommittee::new(
            0,
            [
                (local_key.public_key(), Amount::nano(1)),
                (remote_key.public_key(), Amount::nano(1)),
                (idle_key.public_key(), Amount::nano(1)),
            ],
            0,
            0,
        ));

        state.process_message(
            RaiMessage::Proposal(RaiProposal::new(election, local_block)),
            Some(&local_key),
        );
        state.process_message(
            RaiMessage::LegacyFirstVote(RaiFirstVote::new(
                election,
                BlockHash::from(11),
                &remote_key,
            )),
            Some(&local_key),
        );
        let output = state.process_message(
            RaiMessage::Vote(RaiVote::timeout(election, &idle_key)),
            None,
        );

        assert_eq!(output.terminal_record, None);
        assert!(
            !state
                .election(&election)
                .unwrap()
                .timeout_votes
                .contains(&idle_key.public_key())
        );
    }

    #[test]
    fn proposal_cert_vote_after_timeout_is_accepted_for_second_look() {
        let mut state = RaiState::new();
        let local_key = PrivateKey::from(1);
        let remote_key = PrivateKey::from(2);
        let other_remote_key = PrivateKey::from(3);
        let election = election(0);
        let local_proposal = BlockHash::from(10);
        let conflicting_proposal = BlockHash::from(11);

        state.install_committee(RaiCommittee::new(
            0,
            [
                (local_key.public_key(), Amount::nano(1)),
                (remote_key.public_key(), Amount::nano(1)),
                (other_remote_key.public_key(), Amount::nano(1)),
            ],
            0,
            0,
        ));

        state.process_message(
            RaiMessage::Vote(RaiVote::proposal(
                RaiVotePhase::First,
                election,
                local_proposal,
                &local_key,
            )),
            None,
        );
        state.process_message(
            RaiMessage::Vote(RaiVote::proposal(
                RaiVotePhase::First,
                election,
                conflicting_proposal,
                &remote_key,
            )),
            None,
        );
        state.process_message(
            RaiMessage::Vote(RaiVote::proposal(
                RaiVotePhase::First,
                election,
                conflicting_proposal,
                &other_remote_key,
            )),
            None,
        );

        state.process_message(
            RaiMessage::Vote(RaiVote::timeout(election, &local_key)),
            None,
        );
        let output = state.process_message(
            RaiMessage::Vote(RaiVote::proposal(
                RaiVotePhase::Cert,
                election,
                conflicting_proposal,
                &local_key,
            )),
            None,
        );

        assert!(
            state
                .election(&election)
                .unwrap()
                .cert_votes
                .get(&local_key.public_key())
                .is_some_and(|votes| { votes.contains(&conflicting_proposal) })
        );
        assert_eq!(
            output.terminal_record,
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Notarized(conflicting_proposal)
            ))
        );
    }

    #[test]
    fn invalid_first_vote_signature_is_ignored() {
        let mut state = RaiState::new();
        let voter = PrivateKey::from(2);
        let election = election(0);
        let mut vote = RaiFirstVote::new(election, BlockHash::from(10), &voter);
        vote.signature = Signature::default();

        state.install_committee(RaiCommittee::new(
            0,
            [(voter.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        let output = state.process_message(RaiMessage::LegacyFirstVote(vote), None);

        assert!(output.messages.is_empty());
        assert_eq!(output.terminal_record, None);
        assert!(state.election(&election).is_none());
    }

    #[test]
    fn notar_decision_closes_attempt_without_completing_slot() {
        let mut state = RaiState::new();
        let voter = PrivateKey::from(1);
        let election = election(0);
        let retry = RaiElectionId::new(test_slot(), 1);
        let proposal_hash = BlockHash::from(10);
        let cert_vote = RaiVote::proposal(RaiVotePhase::Cert, election, proposal_hash, &voter);

        state.install_committee(RaiCommittee::new(
            0,
            [(voter.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        let output = state.process_message(
            RaiMessage::NotarDecision(RaiNotarDecision::new(
                election,
                proposal_hash,
                vec![RaiVoteSet::new(0, vec![cert_vote])],
            )),
            None,
        );

        assert_eq!(
            output.terminal_record,
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Notarized(proposal_hash)
            ))
        );
        assert_eq!(state.terminal_record(&election.slot), None);

        state.open_next_epoch();
        assert!(state.start_election(retry));
    }

    #[test]
    fn open_fast_decision_is_adopted_after_epoch_enters_closing() {
        let mut state = RaiState::new();
        let voter = PrivateKey::from(1);
        let election = election(0);
        let proposal_hash = BlockHash::from(10);
        let vote = RaiVote::proposal(RaiVotePhase::First, election, proposal_hash, &voter);

        state.install_committee(RaiCommittee::new(
            0,
            [(voter.public_key(), Amount::nano(1))],
            0,
            0,
        ));
        assert!(state.start_election(election));
        state.close_current_epoch();

        let output = state.process_message(
            RaiMessage::FastDecision(RaiFastDecision::new(
                election,
                proposal_hash,
                vec![RaiVoteSet::new(0, vec![vote])],
            )),
            None,
        );

        assert_eq!(
            output.terminal_record,
            Some(RaiTerminalRecord::new(
                election,
                RaiTerminalOutcome::Proposal(proposal_hash)
            ))
        );
    }

    #[test]
    fn stop_report_set_rejects_mixed_report_heads() {
        let mut state = RaiState::new();
        let keys = six_keys();
        let preserved = election(0);
        state.install_committee(close_committee(&keys));
        state.open_next_epoch();

        let mut reports = keys
            .iter()
            .take(5)
            .map(|key| RaiStopReport::new(0, BlockHash::from(10), vec![preserved], key))
            .collect::<Vec<_>>();
        reports[4] = RaiStopReport::new(0, BlockHash::from(11), vec![preserved], &keys[4]);

        for report in reports {
            state.process_message(RaiMessage::StopReport(report), Some(&keys[0]));
        }

        assert_eq!(state.snapshot().stop_report_sets, 0);
        assert_eq!(state.preservation_witness(&preserved), None);
    }

    #[test]
    fn future_epoch_rai_message_is_ignored() {
        let mut state = RaiState::new();
        let local_key = PrivateKey::from(1);
        let future = election(1);

        state.install_committee(RaiCommittee::new(
            1,
            [(local_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        let output = state.process_message(
            RaiMessage::Proposal(RaiProposal::new(
                future,
                Block::new_test_instance_with_key(1000),
            )),
            Some(&local_key),
        );

        assert!(output.messages.is_empty());
        assert_eq!(output.terminal_record, None);
        assert!(state.election(&future).is_none());
    }

    fn election(epoch: SnapshotNumber) -> RaiElectionId {
        RaiElectionId::new(test_slot(), epoch)
    }

    fn test_slot() -> RaiSlot {
        RaiSlot::new(PublicKey::from(100).as_account(), 1)
    }

    fn six_keys() -> [PrivateKey; 6] {
        [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
            PrivateKey::from(5),
            PrivateKey::from(6),
        ]
    }

    fn close_committee(keys: &[PrivateKey; 6]) -> RaiCommittee {
        RaiCommittee::new(
            0,
            keys.iter().map(|key| (key.public_key(), Amount::nano(1))),
            1,
            1,
        )
    }
}
