use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Blake2HashBuilder, BlockHash, ConsensusEpoch, PublicKey, QualifiedRoot, Root, VoteError,
    VoteKind,
};

use crate::consensus::election::{
    CertificateEvidence, Certificates, Committees, Election, ElectionId, ElectionState,
    EpochLedger, EpochValue, LocalSlotState, SlotVotes, TIMEOUT_BLOCK, kudzu_state,
};

/// RAI: the state one consensus epoch decided, `S_e = (L_e, Sigma_e)`, as
/// the epoch's joint election finalized it. Every replica that accepted the
/// decided value derived this same state from the same selected reports.
pub(super) type DecidedStates = BTreeMap<ConsensusEpoch, Arc<EpochLedger>>;

/// RAI: an instance of a decided epoch that notarized a block the decided
/// state does not hold is late: what it decides is discarded instead of
/// recorded. Until the epoch is decided here, nothing of it is late: an
/// instance this node lacks may be part of the state finalized.
pub(super) fn is_late(decided: &DecidedStates, election: &Election) -> bool {
    decided.get(&election.epoch()).is_some_and(|state| {
        let slot =
            crate::consensus::election::AccountSlot::new(election.account(), election.height());
        election.certificates().notar.iter().any(|block| {
            !state.is_finalized(&slot, block) && !state.notarized(&slot).contains(block)
        })
    })
}

/// RAI, "The joint epoch election": the election that closes one consensus
/// epoch. Every round is a Kudzu election slot (Protocol 1) with its own
/// leader. The leader selects `N - f` usable reports of the epoch, derives
/// `BuildState(S_{e-1}, Q_e)` and proposes the value `X = (h_p, Q_e, d_e)`:
/// the reports it selected and the hash of the state they determine, never
/// the state itself.
///
/// A follower does not compare that value with one of its own. It
/// reconstructs the named reports, derives the same state and checks that it
/// hashes to `d_e`; two validators that accept a value therefore hold the
/// same state whatever certificates each of them happened to collect. That
/// is what makes the rule public rather than local: an epoch closes on what
/// `N - f` reports determine, not on what any one replica saw.
///
/// A proposal is valid in a round if it names a parent placement this
/// replica holds a notarization certificate for and every slot in between
/// has shared skip evidence (a timeout certificate, or notarizations of two
/// different values across the two committees). A child of a non-genesis
/// placement copies its parent's `(Q_e, d_e)`, so once a placement is
/// complete no later slot can carry another state; only a child of election
/// genesis introduces a selection of its own.
pub(crate) struct EpochClose {
    epoch: ConsensusEpoch,
    root: QualifiedRoot,
    /// The representatives which took part in the epoch, in public key
    /// order; the leader of round r is the one at (epoch + r) mod n
    leaders: Vec<PublicKey>,
    rounds: Vec<CloseRound>,
    /// The round this replica is in
    current: usize,
    /// RAI: this replica holds the decided predecessor state and enough
    /// usable reports to derive a value. Without both it can neither propose
    /// nor check a proposal, so it takes no part in the election yet.
    ready: bool,
    /// The values this replica validated: it derived `BuildState` from their
    /// selected reports and the hash came out as the one proposed. Only a
    /// validated value is first voted, supported on a second look, or taken
    /// as a round's tree block.
    values: HashMap<BlockHash, EpochValue>,
    /// The state each validated value decides. The one the election
    /// finalizes is `S_e`.
    states: HashMap<BlockHash, Arc<EpochLedger>>,
    /// The value this replica proposed as the leader of a round
    proposed: BTreeMap<u32, BlockHash>,
    /// The round whose certificate closed the epoch, and the value
    closed: Option<(u32, BlockHash)>,
    /// The close was reported: the epoch is decided here and its state
    /// handed over once
    reported: bool,
    /// Δ_timeout of Protocol 1, line 22
    round_timeout: Duration,
    events: Vec<CloseEvent>,
    #[cfg(feature = "rai_protocol")]
    signed_proposals: HashMap<BlockHash, rsnano_messages::EpochProp>,
    #[cfg(feature = "rai_protocol")]
    signed_votes: BTreeMap<u32, Vec<Arc<rsnano_types::Vote>>>,
}

/// One round of the close election: a Kudzu slot
pub(crate) struct CloseRound {
    votes: SlotVotes,
    certificates: Certificates,
    /// The committees of the last count, None before the first vote
    committees: Option<Committees>,
    state: ElectionState,
    slot: LocalSlotState,
    /// The values voted for by someone: the candidates of this round
    candidates: Vec<BlockHash>,
    /// When this replica entered the round (line 3); None until it did
    entered: Option<Timestamp>,
    /// Δ_timeout passed since entering (line 22)
    timed_out: bool,
    last_solicited: Option<Timestamp>,
}

impl Default for CloseRound {
    fn default() -> Self {
        Self {
            votes: SlotVotes::default(),
            certificates: Certificates::default(),
            committees: None,
            state: ElectionState::Active,
            slot: LocalSlotState::default(),
            candidates: Vec::new(),
            entered: None,
            timed_out: false,
            last_solicited: None,
        }
    }
}

/// What happened in the close election, for the log
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // some variants are the RAI epoch decision's
pub(crate) enum CloseEvent {
    /// This replica can take part: it holds the decided predecessor state
    /// and enough usable reports
    Ready,
    /// This replica entered a round led by the given representative
    RoundEntered {
        round: u32,
        leader: Option<PublicKey>,
    },
    /// A value this replica derived and checked for itself
    Validated { value: BlockHash, state: BlockHash },
    /// A certificate finalized the value; the epoch is decided
    Closed {
        round: u32,
        value: BlockHash,
        state: BlockHash,
    },
    /// RAI: the round was abandoned because the two committees of the joint
    /// election certified different values and none of them all (the
    /// cross-committee conflict clause of ET)
    RoundConflict { round: u32 },
}

/// The close election as seen from outside
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochCloseInfo {
    pub epoch: ConsensusEpoch,
    pub ready: bool,
    /// The state hash of the value finalized, if the epoch is decided
    pub value: Option<BlockHash>,
    /// This replica entered round 0
    pub started: bool,
    /// The round this replica is in
    pub round: u32,
    pub closed: Option<(u32, BlockHash)>,
}

impl EpochClose {
    /// A vote for a round this far ahead of the current one is dropped
    const MAX_ROUNDS_AHEAD: usize = 64;
    /// Distinct values one round accepts votes for: a value per
    /// representative at most, from correct ones
    const MAX_CANDIDATES: usize = 64;
    /// Rounds behind the current one which are still solicited
    const SOLICITED_ROUNDS: usize = 3;

    pub fn new(epoch: ConsensusEpoch, leaders: Vec<PublicKey>, round_timeout: Duration) -> Self {
        Self {
            epoch,
            root: Self::root_of(epoch),
            leaders,
            rounds: vec![CloseRound::default()],
            current: 0,
            ready: false,
            values: HashMap::new(),
            states: HashMap::new(),
            proposed: BTreeMap::new(),
            closed: None,
            reported: false,
            round_timeout,
            events: Vec::new(),
            #[cfg(feature = "rai_protocol")]
            signed_proposals: HashMap::new(),
            #[cfg(feature = "rai_protocol")]
            signed_votes: BTreeMap::new(),
        }
    }

    /// The root the close election's votes and requests are keyed by; there
    /// is no block behind it
    pub fn root_of(epoch: ConsensusEpoch) -> QualifiedRoot {
        let root = Blake2HashBuilder::new()
            .update(b"RAI epoch close root")
            .update(epoch.as_u64().to_le_bytes())
            .build();
        QualifiedRoot::new(Root::from(root), BlockHash::ZERO)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn retain_proposal(&mut self, proposal: rsnano_messages::EpochProp, hash: BlockHash) {
        if self.values.contains_key(&hash) {
            self.signed_proposals.entry(hash).or_insert(proposal);
        }
    }

    /// Preserve the original signed object, including all batched hashes.
    #[cfg(feature = "rai_protocol")]
    pub fn retain_vote(&mut self, vote: Arc<rsnano_types::Vote>, round: u32) {
        if !matches!(vote.kind(), VoteKind::First | VoteKind::Final) {
            return;
        }
        let votes = self.signed_votes.entry(round).or_default();
        if votes.len() < 2 * rsnano_messages::CloseProofReply::MAX_VOTES
            && !votes
                .iter()
                .any(|v| v.voter == vote.voter && v.kind() == vote.kind())
        {
            votes.push(vote);
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn close_proof(&self) -> Option<rsnano_messages::CloseProofReply> {
        let (round, hash) = self.closed?;
        let proposal = self.signed_proposals.get(&hash)?.clone();
        let kind = if self.rounds[round as usize].certificates.fast == Some(hash) {
            VoteKind::First
        } else {
            VoteKind::Final
        };
        let votes = self
            .signed_votes
            .get(&round)?
            .iter()
            .filter(|v| v.kind() == kind && v.hashes.contains(&hash))
            .map(|v| v.as_ref().clone())
            .collect();
        let proof = rsnano_messages::CloseProofReply { proposal, votes };
        // Framing is u16-sized. Never hand an oversized object to the sender.
        proof.serialize(&mut Vec::new()).ok()?;
        Some(proof)
    }

    pub fn epoch(&self) -> ConsensusEpoch {
        self.epoch
    }

    pub fn id(&self, round: u32) -> ElectionId {
        ElectionId::new(
            self.root.clone(),
            ConsensusEpoch::close_round(self.epoch, round),
        )
    }

    pub fn leader(&self, round: u32) -> Option<PublicKey> {
        if self.leaders.is_empty() {
            return None;
        }
        let index = (self.epoch.as_u64() + round as u64) % self.leaders.len() as u64;
        Some(self.leaders[index as usize])
    }

    pub fn is_closed(&self) -> bool {
        self.closed.is_some()
    }

    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// The round this replica is in: the slot a leader proposes into now
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn current_round(&self) -> u32 {
        self.current as u32
    }

    /// RAI: `S_e`, the state the finalized value decided. None until the
    /// epoch is closed, or while this replica has not validated the value
    /// finalized - it goes on collecting and will hold it once it has.
    pub fn decided_state(&self) -> Option<&Arc<EpochLedger>> {
        let (_, value) = self.closed.as_ref()?;
        self.states.get(value)
    }

    /// The epoch is closed and this replica holds the state decided
    pub fn is_decided(&self) -> bool {
        self.decided_state().is_some()
    }

    /// RAI: this replica holds the decided predecessor state and enough
    /// usable reports of this epoch to derive a value
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn set_ready(&mut self, ready: bool) {
        if ready && !self.ready {
            self.events.push(CloseEvent::Ready);
        }
        self.ready = ready;
    }

    /// RAI: a value this replica derived `BuildState(S_{e-1}, Q_e)` for and
    /// whose hash came out as the one proposed. Only such a value may be
    /// voted for; the state is kept, because deciding the value decides it.
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn accept_value(&mut self, value: EpochValue, state: Arc<EpochLedger>) -> BlockHash {
        let hash = value.hash();
        if !self.values.contains_key(&hash) {
            self.events.push(CloseEvent::Validated {
                value: hash,
                state: value.state,
            });
            self.values.insert(hash, value);
            self.states.insert(hash, state);
            self.check_closed();
        }
        hash
    }

    /// The state a validated value decides
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn state_of(&self, value: &BlockHash) -> Option<&Arc<EpochLedger>> {
        self.states.get(value)
    }

    /// Whether this replica has already derived and checked a value. A
    /// proposal is repeated while its round stands, and a derivation walks
    /// the whole predecessor state: one check per value is enough.
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn holds_value(&self, value: &BlockHash) -> bool {
        self.values.contains_key(value)
    }

    /// RAI: the value this replica proposed as the leader of a round
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn record_proposal(&mut self, round: u32, value: BlockHash) {
        self.proposed.insert(round, value);
    }

    /// Whether this replica has to build a proposal for the round it is in:
    /// it leads the round, it can derive a value, and it has not proposed
    /// one there yet
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn proposal_due(&self, local_reps: &[PublicKey]) -> Option<u32> {
        if !self.ready || self.closed.is_some() {
            return None;
        }
        let round = self.current as u32;
        if self.proposed.contains_key(&round) {
            return None;
        }
        let leader = self.leader(round)?;
        local_reps.contains(&leader).then_some(round)
    }

    /// RAI, "Leader behavior": the highest joint-complete placement this
    /// replica holds, whose children copy its `(Q_e, d_e)`. None while no
    /// placement is complete, and the leader then extends election genesis
    /// with a selection of its own.
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn parent_for(&self, round: u32) -> Option<&EpochValue> {
        (0..round as usize)
            .rev()
            .find_map(|earlier| self.tree_block(earlier))
            .and_then(|hash| self.values.get(&hash))
    }

    pub fn info(&self) -> EpochCloseInfo {
        EpochCloseInfo {
            epoch: self.epoch,
            ready: self.ready,
            value: self
                .closed
                .and_then(|(_, value)| self.values.get(&value))
                .map(|value| value.state),
            started: self.rounds[0].entered.is_some(),
            round: self.current as u32,
            closed: self.closed,
        }
    }

    pub fn take_events(&mut self) -> Vec<CloseEvent> {
        std::mem::take(&mut self.events)
    }

    /// The representatives seen taking part, as the leaders of the rounds
    /// to come: a representative which stopped voting leads a round which
    /// times out, one seen since leads a round of its own
    pub fn set_leaders(&mut self, leaders: Vec<PublicKey>) {
        self.leaders = leaders;
    }

    /// Protocol 1, lines 2–13: enter the rounds this replica is due in and
    /// leave those which are done
    pub fn tick(&mut self, now: Timestamp) {
        if !self.ready || self.closed.is_some() {
            return;
        }
        loop {
            let round = self.current;
            if self.rounds[round].entered.is_none() {
                self.rounds[round].entered = Some(now);
                self.events.push(CloseEvent::RoundEntered {
                    round: round as u32,
                    leader: self.leader(round as u32),
                });
            }
            // Joint completeness or shared skip evidence: a timeout
            // certificate, or notarizations of two different values across
            // the committees, both let the slot be left
            if self.tree_block(round).is_some()
                || self.rounds[round].certificates.timeout
                || self.rounds[round].certificates.conflict
            {
                self.current += 1;
                self.ensure_round(self.current);
                continue;
            }
            let entered = self.rounds[round].entered.unwrap();
            self.rounds[round].timed_out = entered.elapsed(now) >= self.round_timeout;
            return;
        }
    }

    fn ensure_round(&mut self, round: usize) {
        while self.rounds.len() <= round {
            self.rounds.push(CloseRound::default());
        }
    }

    /// A vote of the given kind by a representative for a value in a round,
    /// counted in the committee of the epoch closed
    pub fn apply_vote(
        &mut self,
        voter: PublicKey,
        value: BlockHash,
        kind: VoteKind,
        round: u32,
        committees: &Committees,
        now: Timestamp,
    ) -> Result<(), VoteError> {
        if self.closed.is_some() {
            return Err(VoteError::Late);
        }
        let round = round as usize;
        if round > self.current + Self::MAX_ROUNDS_AHEAD {
            return Err(VoteError::Ignored);
        }
        self.ensure_round(round);
        let slot = &mut self.rounds[round];
        if !slot.candidates.contains(&value) {
            if slot.candidates.len() >= Self::MAX_CANDIDATES {
                return Err(VoteError::Ignored);
            }
            slot.candidates.push(value);
        }
        slot.votes.add(voter, value, kind)?;
        self.count_round(round, committees, now);
        Ok(())
    }

    /// RAI: count every round again: the committee of the epoch became
    /// known after votes were collected
    pub fn recount(&mut self, committees: &Committees, now: Timestamp) {
        for round in 0..self.rounds.len() {
            if self.closed.is_some() {
                return;
            }
            if self.rounds[round].votes.len() > 0 {
                self.count_round(round, committees, now);
            }
        }
    }

    /// Tallies a round in the committees, collects its certificates and
    /// closes the epoch on a finalized value
    fn count_round(&mut self, round: usize, committees: &Committees, now: Timestamp) {
        let _ = now;
        let conflicted = self.rounds[round].certificates.conflict;
        let slot = &mut self.rounds[round];
        slot.votes.calculate(committees);
        slot.votes.update_certificates(&mut slot.certificates);
        slot.committees = Some(committees.clone());
        slot.state = kudzu_state(
            slot.state,
            &slot.votes,
            &slot.certificates,
            &slot.candidates,
            false,
        );
        if slot.certificates.conflict && !conflicted {
            self.events.push(CloseEvent::RoundConflict {
                round: round as u32,
            });
        }
        if let Some(value) = slot.certificates.finalized() {
            self.closed = Some((round as u32, value));
            self.report_closed();
        }
    }

    /// The epoch is closed: report it once this replica holds the state the
    /// finalized value decided. A replica that had not validated that value
    /// yet goes on deriving and reports when it has.
    fn report_closed(&mut self) {
        if self.reported {
            return;
        }
        let Some((round, value)) = self.closed else {
            return;
        };
        let Some(derived) = self.values.get(&value) else {
            return;
        };
        self.reported = true;
        self.events.push(CloseEvent::Closed {
            round,
            value,
            state: derived.state,
        });
    }

    #[allow(dead_code)] // the RAI epoch decision uses these
    fn check_closed(&mut self) {
        self.report_closed();
    }

    /// Records a vote this replica is about to cast; the value becomes a
    /// candidate of the round like the values the other replicas vote for
    pub fn mark_voted(&mut self, round: u32, value: BlockHash, kind: VoteKind) {
        self.ensure_round(round as usize);
        let slot = &mut self.rounds[round as usize];
        if value != TIMEOUT_BLOCK && !slot.candidates.contains(&value) {
            slot.candidates.push(value);
        }
        slot.slot.mark_voted(value, kind);
    }

    /// Protocol 1 for this replica's representatives: the votes to broadcast
    /// now, cast ones included so that they are re-broadcast until the epoch
    /// is closed. `local_reps` are the representatives this node votes with.
    pub fn votes_due(&self, local_reps: &[PublicKey]) -> Vec<(u32, BlockHash, VoteKind)> {
        let mut due = Vec::new();
        for round in 0..=self.current.min(self.rounds.len() - 1) {
            let slot = &self.rounds[round];
            if slot.entered.is_none() {
                continue;
            }
            let routing = self.routing(round);
            // Lines 9–11: exit with the tree block and a final vote if
            // notarized ⊆ {B}. Cast even after a fast finalization: replicas
            // which missed a first vote depend on it.
            let exit_vote = self.tree_block(round).filter(|value| {
                slot.slot.final_voted.is_none()
                    && slot.slot.notarized_only(value)
                    && slot.certificates.explicit_finalization_possible()
            });
            if let Some((closed_round, _)) = self.closed {
                if closed_round as usize == round
                    && let Some(value) = exit_vote
                {
                    due.push((round as u32, value, VoteKind::Final));
                }
                continue;
            }
            due.extend(
                slot.slot
                    .cast_votes()
                    .map(|(value, kind)| (round as u32, value, kind)),
            );
            if round < self.current {
                // A round this replica left: only its exit vote is still due
                if let Some(value) = exit_vote {
                    due.push((round as u32, value, VoteKind::Final));
                }
                continue;
            }

            if slot.slot.first_voted.is_none() {
                if let Some(value) = self.valid_proposal(round, local_reps) {
                    // Lines 18–21
                    due.push((round as u32, value, VoteKind::First));
                } else if slot.timed_out {
                    // Lines 22–25
                    due.push((round as u32, routing, VoteKind::Abstain));
                }
            }
            if let Some(value) = exit_vote {
                due.push((round as u32, value, VoteKind::Final));
                continue;
            }
            if slot.slot.final_voted.is_some() || slot.slot.first_voted.is_none() {
                continue;
            }
            if slot.committees.is_none() {
                continue;
            }
            // Lines 28–31: a second look at every value with many first votes
            // whose placement is valid here. It is notarized if this replica
            // validated it; otherwise the timeout block is (Protocol 2,
            // lines 7–9).
            let mut timeout = false;
            for value in slot.votes.many_votes() {
                if !self.values.contains_key(&value) {
                    // A value this replica could not derive and check: it
                    // notarizes the timeout block in its place
                    timeout = true;
                    continue;
                }
                if self.chain_valid(round, &value) && !slot.slot.looked_at(&value) {
                    due.push((round as u32, value, VoteKind::Notar));
                }
            }
            // Lines 32–35
            timeout |= !slot.certificates.is_terminated() && slot.votes.should_timeout();
            if timeout && slot.slot.timeout_voted.is_none() {
                due.push((round as u32, routing, VoteKind::Timeout));
            }
        }
        due
    }

    /// The candidate hash a timeout or abstain vote of this round is routed
    /// through: one this replica validated, so that the request reaches the
    /// replicas holding the value; the timeout block when it has none.
    fn routing(&self, round: usize) -> BlockHash {
        self.rounds
            .get(round)
            .into_iter()
            .flat_map(|slot| slot.candidates.iter())
            .find(|value| self.values.contains_key(value))
            .copied()
            .unwrap_or(TIMEOUT_BLOCK)
    }

    /// The valid proposal of the round's leader: its first vote, for a value
    /// this replica has validated and whose placement is valid in the round.
    /// If this node leads the round, the value it proposed there.
    fn valid_proposal(&self, round: usize, local_reps: &[PublicKey]) -> Option<BlockHash> {
        let leader = self.leader(round as u32)?;
        let proposal = if local_reps.contains(&leader) {
            self.proposed.get(&(round as u32)).copied()?
        } else {
            self.rounds[round].votes.rep(&leader)?.first?
        };
        (proposal != TIMEOUT_BLOCK
            && self.values.contains_key(&proposal)
            && self.chain_valid(round, &proposal))
        .then_some(proposal)
    }

    /// RAI: whether a placement may sit in this round. It names a parent
    /// placement, and that parent must be notarized in an earlier round with
    /// shared skip evidence for every slot in between; a child of election
    /// genesis is valid only while no round before it is complete. A
    /// non-genesis child must also copy its parent's `(Q_e, d_e)`, which is
    /// what stops a later slot from carrying another state.
    pub fn chain_valid(&self, round: usize, value: &BlockHash) -> bool {
        let Some(held) = self.values.get(value) else {
            return false;
        };
        if held.slot as usize != round {
            return false;
        }
        match self.round_of(&held.parent) {
            Some(parent_round) => {
                let Some(parent) = self.values.get(&held.parent) else {
                    return false;
                };
                parent_round < round
                    && held.copies(parent)
                    && (parent_round + 1..round).all(|slot| self.skipped(slot))
                    && self.chain_valid(parent_round, &held.parent)
            }
            None => held.extends_genesis() && (0..round).all(|slot| self.skipped(slot)),
        }
    }

    /// The round whose certificates notarize this placement, if this replica
    /// holds one
    fn round_of(&self, value: &BlockHash) -> Option<usize> {
        if value.is_zero() {
            return None;
        }
        self.rounds
            .iter()
            .position(|slot| slot.certificates.is_notarized(value))
    }

    /// Shared skip evidence for a round: a timeout certificate, or the two
    /// committees notarizing different values
    fn skipped(&self, round: usize) -> bool {
        self.rounds
            .get(round)
            .is_some_and(|slot| slot.certificates.timeout || slot.certificates.conflict)
    }

    /// The placement of this round in the complete tree (Section 4.3): a
    /// notarized value this replica validated, whose chain is valid
    fn tree_block(&self, round: usize) -> Option<BlockHash> {
        self.rounds
            .get(round)?
            .certificates
            .notar
            .iter()
            .find(|value| self.values.contains_key(*value) && self.chain_valid(round, value))
            .copied()
    }

    /// This replica's statements in the round, for a replica which asks
    pub fn certificate_evidence(&self, round: u32) -> Option<CertificateEvidence> {
        let slot = self.rounds.get(round as usize)?;
        slot.entered?;
        let routing = slot.slot.timeout_voted.unwrap_or(TIMEOUT_BLOCK);
        let statements = slot.slot.statements_for(&slot.candidates, routing);
        (!statements.is_empty()).then_some(CertificateEvidence {
            statements,
            blocks: Vec::new(),
        })
    }

    /// The rounds whose evidence is to be solicited now, at most once per
    /// interval each: the recent rounds of an epoch not closed yet
    pub fn solicitations(&mut self, now: Timestamp, interval: Duration) -> Vec<(u32, BlockHash)> {
        if self.closed.is_some() || !self.ready {
            return Vec::new();
        }
        let first = self.current.saturating_sub(Self::SOLICITED_ROUNDS);
        let mut result = Vec::new();
        for round in first..=self.current.min(self.rounds.len() - 1) {
            let routing = self.routing(round);
            let slot = &mut self.rounds[round];
            if slot.entered.is_none()
                || slot
                    .last_solicited
                    .is_some_and(|last| last.elapsed(now) < interval)
            {
                continue;
            }
            slot.last_solicited = Some(now);
            result.push((round as u32, routing));
        }
        result
    }

    #[cfg(test)]
    fn round(&self, round: u32) -> &CloseRound {
        &self.rounds[round as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::{Committee, ReportRef};
    use rsnano_types::{Account, Amount};
    use rustc_hash::FxHashMap;

    /// RAI: the leader proposes the value it derived, and a follower votes
    /// for it because it derived the same state from the same reports - not
    /// because the value matches one of its own
    #[test]
    fn a_follower_votes_for_the_value_it_could_derive() {
        let mut close = close_election();
        assert_eq!(close.leader(0), Some(LEADER));
        assert_eq!(close.votes_due(&[FOLLOWER]), vec![]);

        // Not ready: no decided predecessor state or not enough reports
        close.tick(t(0));
        assert!(!close.info().started);

        close.set_ready(true);
        close.tick(t(0));
        assert_eq!(
            close.take_events(),
            vec![
                CloseEvent::Ready,
                CloseEvent::RoundEntered {
                    round: 0,
                    leader: Some(LEADER)
                }
            ]
        );
        // The leader's first vote arrives before the proposal itself: the
        // follower holds no value to check and does not vote
        let proposal = genesis_value(0, 1);
        let hash = proposal.hash();
        vote(&mut close, 0, hash, VoteKind::First, 0).unwrap();
        assert_eq!(close.votes_due(&[FOLLOWER]), vec![]);

        // With the value derived and checked, the follower first votes it
        close.accept_value(proposal, ledger());
        assert_eq!(
            close.votes_due(&[FOLLOWER]),
            vec![(0, hash, VoteKind::First)]
        );
    }

    /// A value this replica could not derive is not one it votes for, even
    /// with many first votes behind it: it times out the round instead
    #[test]
    fn an_underivable_value_is_not_voted_for() {
        let mut close = close_election();
        close.set_ready(true);
        close.tick(t(0));
        let ours = genesis_value(0, 1);
        let theirs = genesis_value(0, 2);
        close.accept_value(ours.clone(), ledger());
        close.mark_voted(0, ours.hash(), VoteKind::First);

        // Three of six first vote a value this replica can not derive: many
        // first votes, and the second look goes to the timeout block
        for rep in 1..4 {
            vote(&mut close, rep, theirs.hash(), VoteKind::First, 0).unwrap();
        }
        let due = close.votes_due(&[FOLLOWER]);
        assert!(
            due.iter().any(|(_, _, kind)| *kind == VoteKind::Timeout),
            "a value it can not check is one it does not notarize: {due:?}"
        );
        assert!(
            !due.iter()
                .any(|(_, value, kind)| { *value == theirs.hash() && *kind == VoteKind::Notar })
        );
    }

    /// RAI: deciding a value decides the state it names. A replica that has
    /// not derived that value yet reports the epoch decided only once it has.
    #[test]
    fn deciding_a_value_decides_the_state_it_names() {
        let mut close = close_election();
        close.set_ready(true);
        close.tick(t(0));
        let proposal = genesis_value(0, 1);
        let hash = proposal.hash();

        // The certificate arrives before this replica derived the value
        for rep in 0..4 {
            vote(&mut close, rep, hash, VoteKind::Final, 0).unwrap();
        }
        assert!(close.is_closed());
        assert!(!close.is_decided());
        assert!(
            !close
                .take_events()
                .iter()
                .any(|event| matches!(event, CloseEvent::Closed { .. })),
            "nothing is decided before the state is derived"
        );

        let state = ledger();
        close.accept_value(proposal.clone(), state.clone());
        assert!(close.is_decided());
        assert_eq!(
            close.decided_state().map(|held| held.state_hash()),
            Some(state.state_hash())
        );
        assert!(close.take_events().iter().any(|event| matches!(
            event,
            CloseEvent::Closed { value, .. } if *value == hash
        )));
    }

    /// RAI: "A child of a non-genesis placement must copy its parent's
    /// (Q_e, d_e)." Once a placement is complete every later slot carries
    /// the same state, so the election decides which slot it finalizes and
    /// never another state.
    #[test]
    fn a_child_of_a_complete_placement_copies_its_payload() {
        let mut close = close_election();
        close.set_ready(true);
        close.tick(t(0));
        let parent = genesis_value(0, 1);
        close.accept_value(parent.clone(), ledger());
        // Round 0 notarizes it, so round 1 has a joint-complete parent
        for rep in 0..4 {
            vote(&mut close, rep, parent.hash(), VoteKind::First, 0).unwrap();
        }
        close.tick(t(1));
        assert_eq!(close.info().round, 1);
        assert_eq!(
            close.parent_for(1).map(|value| value.hash()),
            Some(parent.hash())
        );

        // A child copying the payload is valid in round 1
        let child = parent.extend(1);
        close.accept_value(child.clone(), ledger());
        assert!(close.chain_valid(1, &child.hash()));

        // One introducing another selection is not, however it is chained
        let other = genesis_value(1, 2);
        close.accept_value(other.clone(), ledger());
        assert!(
            !close.chain_valid(1, &other.hash()),
            "only a child of election genesis introduces a selection"
        );
    }

    /// A child of election genesis is valid only while no earlier round is
    /// complete, and every round before it has shared skip evidence
    #[test]
    fn a_genesis_child_needs_every_earlier_round_skipped() {
        let mut close = close_election();
        close.set_ready(true);
        close.tick(t(0));
        let late = genesis_value(1, 1);
        close.accept_value(late.clone(), ledger());
        assert!(
            !close.chain_valid(1, &late.hash()),
            "round 0 has no skip evidence yet"
        );

        // Round 0 times out: the skip evidence is there
        for rep in 0..4 {
            vote(&mut close, rep, TIMEOUT_BLOCK, VoteKind::Timeout, 0).unwrap();
        }
        assert!(close.chain_valid(1, &late.hash()));
    }

    /// A value proposed in another slot than the one it names is not valid
    /// there: the placement hash binds its election slot
    #[test]
    fn a_value_is_valid_only_in_the_slot_it_names() {
        let mut close = close_election();
        close.set_ready(true);
        close.tick(t(0));
        let value = genesis_value(0, 1);
        close.accept_value(value.clone(), ledger());
        assert!(close.chain_valid(0, &value.hash()));
        assert!(!close.chain_valid(1, &value.hash()));
    }

    /// The round times out and the next leader takes over
    #[test]
    fn a_round_that_times_out_moves_to_the_next_leader() {
        let mut close = close_election();
        close.set_ready(true);
        close.tick(t(0));
        close.take_events();
        // Δ_timeout passes without a proposal: this replica abstains
        close.tick(t(6));
        assert_eq!(
            close.votes_due(&[FOLLOWER]),
            vec![(0, TIMEOUT_BLOCK, VoteKind::Abstain)]
        );

        for rep in 0..4 {
            vote(&mut close, rep, TIMEOUT_BLOCK, VoteKind::Timeout, 0).unwrap();
        }
        close.tick(t(6));
        assert_eq!(close.info().round, 1);
        assert_eq!(close.leader(1), Some(key(1)));
    }

    /// RAI: the cross-committee conflict clause of ET. Two committees
    /// notarizing different values skip the slot.
    #[test]
    fn a_cross_committee_conflict_skips_the_round() {
        let mut close = close_election();
        close.set_ready(true);
        close.tick(t(0));
        let one = genesis_value(0, 1);
        let other = genesis_value(0, 2);
        close.accept_value(one.clone(), ledger());
        close.accept_value(other.clone(), ledger());
        // The old committee certifies one value, the new one the other
        for rep in 0..4 {
            close
                .apply_vote(key(rep), one.hash(), VoteKind::First, 0, &joint(), t(0))
                .unwrap();
        }
        for rep in 6..10 {
            close
                .apply_vote(key(rep), other.hash(), VoteKind::First, 0, &joint(), t(0))
                .unwrap();
        }
        assert!(close.round(0).certificates.conflict);
        let child = genesis_value(1, 3);
        close.accept_value(child.clone(), ledger());
        assert!(
            close.chain_valid(1, &child.hash()),
            "a conflicted round is skipped like a timed out one"
        );
    }

    /*
     * Test helpers
     */

    const TIMEOUT: Duration = Duration::from_secs(5);
    const LEADER: PublicKey = key(0);
    const FOLLOWER: PublicKey = key(3);

    const fn key(index: usize) -> PublicKey {
        // Twelve equal representatives in public key order: six in the old
        // committee, six in the new one
        PublicKey::from_bytes(
            [
                [1; 32], [2; 32], [3; 32], [4; 32], [5; 32], [6; 32], [7; 32], [8; 32], [9; 32],
                [10; 32], [11; 32], [12; 32],
            ][index],
        )
    }

    fn keys() -> Vec<PublicKey> {
        (0..6).map(key).collect()
    }

    /// The committee of six equal representatives: four for a certificate
    fn committees() -> Committees {
        Committees::single(Arc::new(committee(0..6)))
    }

    /// The old committee and the new one, which share no member: a value
    /// needs both to certify it
    fn joint() -> Committees {
        Committees::joint(Arc::new(committee(0..6)), Arc::new(committee(6..12)))
    }

    fn committee(members: std::ops::Range<usize>) -> Committee {
        let weights: FxHashMap<PublicKey, Amount> =
            members.map(|i| (key(i), Amount::raw(100))).collect();
        Committee::new(weights)
    }

    fn t(secs: u64) -> Timestamp {
        Timestamp::new_test_instance() + Duration::from_secs(secs)
    }

    /// A value of election genesis in the given slot, with a selection told
    /// apart by `selection`
    fn genesis_value(slot: u32, selection: u64) -> EpochValue {
        EpochValue::from_parts(
            ConsensusEpoch::ZERO,
            slot,
            BlockHash::ZERO,
            vec![ReportRef {
                reporter: PublicKey::from(selection),
                certified: BlockHash::from(selection * 10),
                residual: BlockHash::from(selection * 10 + 1),
            }],
            BlockHash::from(selection * 100),
        )
    }

    /// The state a value decides; the tests do not derive it, they only
    /// check that deciding the value hands it over
    fn ledger() -> Arc<EpochLedger> {
        let mut ledger = EpochLedger::new();
        ledger.finalize_genesis(
            crate::consensus::election::AccountSlot::new(Account::from(1), 1),
            BlockHash::from(1),
        );
        Arc::new(ledger)
    }

    /// Epoch 0: key 0 leads round 0
    fn close_election() -> EpochClose {
        EpochClose::new(ConsensusEpoch::ZERO, keys(), TIMEOUT)
    }

    fn vote(
        close: &mut EpochClose,
        rep: usize,
        value: BlockHash,
        kind: VoteKind,
        round: u32,
    ) -> Result<(), VoteError> {
        close.apply_vote(key(rep), value, kind, round, &committees(), t(0))
    }
}
