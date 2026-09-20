use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, Blake2HashBuilder, BlockHash, ConsensusEpoch, PublicKey, QualifiedRoot, Root,
    VoteError, VoteKind,
};

use crate::consensus::election::{
    CertificateEvidence, Certificates, Committees, Election, ElectionId, ElectionState, EpochState,
    LocalSlotState, SlotVotes, TIMEOUT_BLOCK, kudzu_state,
};

/// RAI: the content of an epoch's value as this node agreed on it: every
/// (account, height, block) the value hashed. The same on every replica
/// that agreed, unlike the instant the certificate was seen here.
pub(super) type AgreedContent = BTreeMap<ConsensusEpoch, HashSet<(Account, u64, BlockHash)>>;

/// RAI: an instance of an agreed epoch that notarized a block the agreed
/// value does not hold is late: what it decides is discarded instead of
/// recorded. Until this node agrees, nothing of the epoch is late: an
/// instance it lacks may be part of the value finalized.
pub(super) fn is_late(agreed: &AgreedContent, election: &Election) -> bool {
    agreed.get(&election.epoch()).is_some_and(|content| {
        election
            .certificates()
            .notar
            .iter()
            .any(|block| !content.contains(&(election.account(), election.height(), *block)))
    })
}

/// RAI: the close election of one consensus epoch, a multi-round Kudzu
/// instance on the epoch's final state. Every round is a Kudzu slot
/// (Protocol 1) with its own leader; the leader's first vote is its proposal,
/// the value it attests: the hash of the epoch's state as it sees it. A
/// replica first votes the proposal if its own state hashes to the same value
/// and abstains otherwise; the round ends with a notarization or a timeout
/// certificate, and the next round has the next leader. The epoch is closed
/// once some round finalizes a value.
///
/// A proposal is valid in round r if, for the rounds before r, the replica
/// holds a timeout certificate or a notarization certificate for the same
/// value (Section 4.6, with the same value standing in for the parent block):
/// once a round finalizes a value no round can have a timeout certificate
/// (Lemma 5.7), so no other value can ever be proposed validly again.
pub(crate) struct EpochClose {
    epoch: ConsensusEpoch,
    root: QualifiedRoot,
    /// The representatives which took part in the epoch, in public key
    /// order; the leader of round r is the one at (epoch + r) mod n
    leaders: Vec<PublicKey>,
    rounds: Vec<CloseRound>,
    /// The round this replica is in
    current: usize,
    /// Every instance of the epoch has settled on this replica: it attests
    /// `own`. Readiness is lost again while an instance opens late.
    ready: bool,
    /// The value this replica attests: the hash of the epoch's state as it
    /// stands, kept while not ready so that its statements stay routable
    own: Option<BlockHash>,
    /// The hash of the epoch's state with every instance counted, the ones
    /// opened after the close was seen here included: a replica that lacked
    /// blocks at the close agrees with the value finalized once they came
    own_all: Option<BlockHash>,
    /// Every instance of the epoch has settled here: the own value can not
    /// change any more (but for a late instance). An instance terminated by
    /// a timeout certificate is notarized later on, and a value proposed
    /// before that sees the followers abstain once theirs moved on: a round
    /// timeout lost. Settlement can take long, though (the stragglers'
    /// first votes are solicited), and the epoch after can not be left
    /// before the close: a leader whose instances are not settled proposes
    /// once its value has stood for `PROPOSAL_DELAY`, the followers' values
    /// catch up in that time as a rule.
    settled: bool,
    /// The own value as of the last tick and since when it stands
    own_since: Option<(BlockHash, Timestamp)>,
    /// The own value has stood for `PROPOSAL_DELAY`
    own_stable: bool,
    /// The values which were this replica's own at some point: the payloads
    /// it validated (Section 4.3, the correctness predicate of the tree)
    validated: Vec<BlockHash>,
    /// The round whose certificate closed the epoch, and the value
    closed: Option<(u32, BlockHash)>,
    /// When the certificate was seen here: the value attested counts the
    /// instances opened before, so that it stands still and can agree
    closed_at: Option<Timestamp>,
    /// This replica's own value is the finalized one: the epoch's state is
    /// decided here, the instances without a block in it can be discarded
    agreed: bool,
    /// Δ_timeout of Protocol 1, line 22
    round_timeout: Duration,
    events: Vec<CloseEvent>,
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
pub(crate) enum CloseEvent {
    /// Every instance of the epoch terminated, this replica attests the value
    Ready(BlockHash),
    /// This replica entered a round led by the given representative
    RoundEntered {
        round: u32,
        leader: Option<PublicKey>,
    },
    /// A certificate finalized the value; whether it is this replica's own
    Closed {
        round: u32,
        value: BlockHash,
        own: bool,
    },
    /// This replica's own value is the finalized one, at the close or once
    /// its instances settled later: the epoch is decided here
    Agreed(BlockHash),
}

/// The close election as seen from outside
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochCloseInfo {
    pub epoch: ConsensusEpoch,
    pub ready: bool,
    /// The value this replica attests
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
    /// How long a leader's own value must stand before it proposes it while
    /// its instances are not settled
    pub const PROPOSAL_DELAY: Duration = Duration::from_secs(1);

    pub fn new(epoch: ConsensusEpoch, leaders: Vec<PublicKey>, round_timeout: Duration) -> Self {
        Self {
            epoch,
            root: Self::root_of(epoch),
            leaders,
            rounds: vec![CloseRound::default()],
            current: 0,
            ready: false,
            own: None,
            own_all: None,
            settled: false,
            own_since: None,
            own_stable: false,
            validated: Vec::new(),
            closed: None,
            closed_at: None,
            agreed: false,
            round_timeout,
            events: Vec::new(),
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

    /// The finalized value is this replica's own
    pub fn is_agreed(&self) -> bool {
        self.agreed
    }

    /// When the certificate closing the epoch was seen here
    pub fn closed_at(&self) -> Option<Timestamp> {
        self.closed_at
    }

    /// The finalized value is one of this replica's: the one it attests, or
    /// the state with every instance counted
    fn check_agreed(&mut self) {
        if self.agreed {
            return;
        }
        if let Some((_, value)) = self.closed
            && (self.own == Some(value) || self.own_all == Some(value))
        {
            self.agreed = true;
            self.events.push(CloseEvent::Agreed(value));
        }
    }

    /// The hash of the epoch's state with every instance counted, the
    /// alternative this replica may agree with
    pub fn set_alternative(&mut self, value: BlockHash) {
        self.own_all = Some(value);
        self.check_agreed();
    }

    pub fn info(&self) -> EpochCloseInfo {
        EpochCloseInfo {
            epoch: self.epoch,
            ready: self.ready,
            value: self.own,
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

    /// The epoch's state as this replica sees it now: the blocks notarized
    /// or finalized in the epoch's instances. It takes part once every
    /// instance of the epoch has terminated, and its value follows the state
    /// while instances of the epoch still open and terminate, before and
    /// after the close: the value finalized may differ from the final one.
    /// Once the epoch is closed, instances still without a certificate do
    /// not matter: if the state without them is the value finalized, this
    /// replica agrees.
    pub fn set_state(&mut self, state: &EpochState) {
        let value = state.close_value(self.epoch);
        self.ready = state.is_terminated();
        self.settled = state.is_settled();
        if !self.ready && self.closed.is_none_or(|(_, closed)| closed != value) {
            return;
        }
        if self.own != Some(value) {
            self.own = Some(value);
            self.events.push(CloseEvent::Ready(value));
        }
        if !self.validated.contains(&value) {
            self.validated.push(value);
        }
        self.check_agreed();
    }

    /// Protocol 1, lines 2–13: enter the rounds this replica is due in and
    /// leave those which are done
    pub fn tick(&mut self, now: Timestamp) {
        if !self.ready || self.closed.is_some() {
            return;
        }
        if let Some(own) = self.own {
            let since = match self.own_since {
                Some((value, since)) if value == own => since,
                _ => now,
            };
            self.own_since = Some((own, since));
            self.own_stable = since.elapsed(now) >= Self::PROPOSAL_DELAY;
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
            if self.tree_block(round).is_some() || self.rounds[round].certificates.timeout {
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
        let slot = &mut self.rounds[round];
        slot.votes.calculate(committees);
        slot.votes.update_certificates(&mut slot.certificates);
        slot.committees = Some(committees.clone());
        slot.state = kudzu_state(
            slot.state,
            &slot.votes,
            &slot.certificates,
            &slot.candidates,
        );
        if let Some(value) = slot.certificates.finalized() {
            self.closed = Some((round as u32, value));
            self.closed_at = Some(now);
            self.events.push(CloseEvent::Closed {
                round: round as u32,
                value,
                own: self.own == Some(value),
            });
            self.check_agreed();
        }
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
            let routing = self.own.unwrap_or(TIMEOUT_BLOCK);
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
            // whose parent is in the tree. It is notarized if it is this
            // replica's own value; otherwise the timeout block is (Protocol 2,
            // lines 7–9).
            let mut timeout = false;
            for value in slot.votes.many_votes() {
                if !self.chain_valid(round, &value) {
                    continue;
                }
                if self.validated.contains(&value) {
                    if !slot.slot.looked_at(&value) {
                        due.push((round as u32, value, VoteKind::Notar));
                    }
                } else {
                    timeout = true;
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

    /// The valid proposal of the round's leader (Section 4.6): its first
    /// vote, for this replica's own value, chained to the earlier rounds.
    /// If this node leads the round, its own value is the proposal, once
    /// its instances settled or the value stood for `PROPOSAL_DELAY`.
    fn valid_proposal(&self, round: usize, local_reps: &[PublicKey]) -> Option<BlockHash> {
        let own = self.own?;
        let leader = self.leader(round as u32)?;
        let proposal = if local_reps.contains(&leader) {
            if !self.settled && !self.own_stable {
                return None;
            }
            own
        } else {
            self.rounds[round].votes.rep(&leader)?.first?
        };
        (proposal == own && proposal != TIMEOUT_BLOCK && self.chain_valid(round, &own))
            .then_some(proposal)
    }

    /// Whether a block for the value may exist in the round (Section 4.6):
    /// every earlier round back to one with a notarization certificate for
    /// the same value, which must be valid there in turn, has a timeout
    /// certificate
    fn chain_valid(&self, round: usize, value: &BlockHash) -> bool {
        for earlier in (0..round).rev() {
            let certs = &self.rounds[earlier].certificates;
            if certs.is_notarized(value) {
                return self.chain_valid(earlier, value);
            }
            if !certs.timeout {
                return false;
            }
        }
        true
    }

    /// The block for this round in the complete block tree (Section 4.3): a
    /// notarized value this replica validated, chained to the earlier rounds
    fn tree_block(&self, round: usize) -> Option<BlockHash> {
        self.rounds[round]
            .certificates
            .notar
            .iter()
            .find(|value| self.validated.contains(value) && self.chain_valid(round, value))
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
        let routing = self.own.unwrap_or(TIMEOUT_BLOCK);
        let mut result = Vec::new();
        for round in first..=self.current.min(self.rounds.len() - 1) {
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
    use crate::consensus::election::{Committee, SlotOutcome};
    use rsnano_types::{Account, Amount};
    use rustc_hash::FxHashMap;
    use std::sync::Arc;

    #[test]
    fn leader_proposes_its_value_once_ready() {
        let mut close = close_election();
        assert_eq!(close.votes_due(&[LEADER]), vec![]);
        assert_eq!(close.leader(0), Some(LEADER));
        assert_eq!(close.leader(1), Some(keys()[1]));

        close.set_state(&state(1));
        assert_eq!(close.votes_due(&[LEADER]), vec![]);
        close.tick(t(0));
        assert_eq!(
            close.take_events(),
            vec![
                CloseEvent::Ready(value(1)),
                CloseEvent::RoundEntered {
                    round: 0,
                    leader: Some(LEADER)
                }
            ]
        );
        assert_eq!(
            close.votes_due(&[LEADER]),
            vec![(0, value(1), VoteKind::First)]
        );
        close.mark_voted(0, value(1), VoteKind::First);
        // The cast vote is re-broadcast, nothing else is due
        assert_eq!(
            close.votes_due(&[LEADER]),
            vec![(0, value(1), VoteKind::First)]
        );
        assert_eq!(close.info().round, 0);
        assert!(close.info().ready);
        assert!(close.info().started);
    }

    /// A terminated instance can still be notarized: a leader whose
    /// instances are not settled waits for its value to stand for a while,
    /// the followers' values catch up in the meantime
    #[test]
    fn unsettled_leader_proposes_once_its_value_stood_for_a_while() {
        let mut close = close_election();
        let mut unsettled = state(1);
        unsettled.add(SlotOutcome::Pending);
        close.set_state(&unsettled);
        close.tick(t(0));
        assert!(close.info().started);
        assert_eq!(close.votes_due(&[LEADER]), vec![]);

        // The value changed: the wait starts over
        let mut changed = state(2);
        changed.add(SlotOutcome::Pending);
        close.set_state(&changed);
        close.tick(t(0) + EpochClose::PROPOSAL_DELAY);
        assert_eq!(close.votes_due(&[LEADER]), vec![]);
        close.tick(t(0) + EpochClose::PROPOSAL_DELAY * 2);
        assert_eq!(
            close.votes_due(&[LEADER]),
            vec![(0, value(2), VoteKind::First)]
        );
    }

    #[test]
    fn unsettled_leader_proposes_at_once_when_its_instances_settle() {
        let mut close = close_election();
        let mut unsettled = state(1);
        unsettled.add(SlotOutcome::Pending);
        close.set_state(&unsettled);
        close.tick(t(0));
        assert_eq!(close.votes_due(&[LEADER]), vec![]);

        close.set_state(&state(1));
        close.tick(t(0) + Duration::from_millis(1));
        assert_eq!(
            close.votes_due(&[LEADER]),
            vec![(0, value(1), VoteKind::First)]
        );
    }

    #[test]
    fn follower_first_votes_the_leaders_proposal_if_it_is_its_own_value() {
        let mut close = close_election();
        close.set_state(&state(1));
        close.tick(t(0));
        assert_eq!(close.votes_due(&[FOLLOWER]), vec![]);

        // Another representative's first vote is not a proposal
        vote(&mut close, 2, value(1), VoteKind::First, 0).unwrap();
        assert_eq!(close.votes_due(&[FOLLOWER]), vec![]);
        vote(&mut close, 0, value(1), VoteKind::First, 0).unwrap();
        assert_eq!(
            close.votes_due(&[FOLLOWER]),
            vec![(0, value(1), VoteKind::First)]
        );
    }

    #[test]
    fn follower_abstains_at_the_timeout_without_a_valid_proposal() {
        let mut close = close_election();
        close.set_state(&state(2));
        close.tick(t(0));
        vote(&mut close, 0, value(1), VoteKind::First, 0).unwrap();
        assert_eq!(close.votes_due(&[FOLLOWER]), vec![]);
        close.tick(t(4));
        assert_eq!(close.votes_due(&[FOLLOWER]), vec![]);
        close.tick(t(5));
        assert_eq!(
            close.votes_due(&[FOLLOWER]),
            vec![(0, value(2), VoteKind::Abstain)]
        );
        close.mark_voted(0, value(2), VoteKind::Abstain);
        // Too late to first vote the proposal now
        assert_eq!(
            close.votes_due(&[FOLLOWER]),
            vec![(0, value(2), VoteKind::Abstain)]
        );
    }

    #[test]
    fn readiness_starts_the_round_timer_not_the_epoch_end() {
        let mut close = close_election();
        close.tick(t(0));
        assert_eq!(close.take_events(), vec![]);
        close.set_state(&state(1));
        close.tick(t(10));
        close.tick(t(14));
        assert!(!close.round(0).timed_out);
        close.tick(t(15));
        assert!(close.round(0).timed_out);
    }

    #[test]
    fn fast_finalization_closes_the_epoch_and_leaves_the_exit_vote() {
        let mut close = close_election();
        close.set_state(&state(1));
        close.tick(t(0));
        vote(&mut close, 0, value(1), VoteKind::First, 0).unwrap();
        close.mark_voted(0, value(1), VoteKind::First);
        for rep in 1..5 {
            vote(&mut close, rep, value(1), VoteKind::First, 0).unwrap();
        }
        assert_eq!(close.closed, Some((0, value(1))));
        let events = close.take_events();
        assert!(events.contains(&CloseEvent::Closed {
            round: 0,
            value: value(1),
            own: true
        }));
        assert!(events.contains(&CloseEvent::Agreed(value(1))));
        assert!(close.is_agreed());
        assert_eq!(
            close.votes_due(&[FOLLOWER]),
            vec![(0, value(1), VoteKind::Final)]
        );
        close.mark_voted(0, value(1), VoteKind::Final);
        assert_eq!(close.votes_due(&[FOLLOWER]), vec![]);
        assert_eq!(
            vote(&mut close, 5, value(1), VoteKind::First, 0),
            Err(VoteError::Late)
        );
        // No further round is entered
        close.tick(t(100));
        assert_eq!(close.info().round, 0);
    }

    #[test]
    fn notarization_certificate_ends_the_round_with_a_final_vote_and_enters_the_next() {
        let mut close = close_election();
        close.set_state(&state(1));
        close.tick(t(0));
        close.take_events();
        vote(&mut close, 0, value(1), VoteKind::First, 0).unwrap();
        close.mark_voted(0, value(1), VoteKind::First);
        // Three first votes plus one second look: a notarization certificate,
        // no fast finalization
        for rep in 1..3 {
            vote(&mut close, rep, value(1), VoteKind::First, 0).unwrap();
        }
        vote(&mut close, 3, value(1), VoteKind::Notar, 0).unwrap();
        assert!(close.round(0).certificates.is_notarized(&value(1)));
        assert_eq!(close.round(0).state, ElectionState::Terminated);
        assert_eq!(
            close.votes_due(&[FOLLOWER]),
            vec![
                (0, value(1), VoteKind::First),
                (0, value(1), VoteKind::Final)
            ]
        );
        close.mark_voted(0, value(1), VoteKind::Final);

        close.tick(t(1));
        assert_eq!(close.info().round, 1);
        assert_eq!(
            close.take_events(),
            vec![CloseEvent::RoundEntered {
                round: 1,
                leader: Some(keys()[1])
            }]
        );
        // Round 1: the leader proposes the notarized value again
        vote(&mut close, 1, value(1), VoteKind::First, 1).unwrap();
        let due = close.votes_due(&[FOLLOWER]);
        assert!(due.contains(&(1, value(1), VoteKind::First)));

        // A replica which does not hold the value has no block in its tree
        // and stays in round 0
        let mut other = close_election();
        other.set_state(&state(2));
        other.tick(t(0));
        for rep in 0..4 {
            vote(&mut other, rep, value(1), VoteKind::First, 0).unwrap();
        }
        other.tick(t(1));
        assert_eq!(other.info().round, 0);
        assert_eq!(other.votes_due(&[FOLLOWER]), vec![]);
    }

    #[test]
    fn a_leader_whose_value_is_not_chained_does_not_propose() {
        let mut close = EpochClose::new(ConsensusEpoch::ZERO, keys(), TIMEOUT);
        let leader1 = keys()[1];
        close.set_state(&state(2));
        close.tick(t(0));
        for rep in 0..4 {
            vote(&mut close, rep, value(1), VoteKind::First, 0).unwrap();
        }
        // Not validated here, so round 0 is not left; timeout certificate ends it
        for rep in 0..4 {
            vote(&mut close, rep, value(1), VoteKind::Timeout, 0).unwrap();
        }
        close.tick(t(1));
        assert_eq!(close.info().round, 1);
        // Round 0 is notarized for value 1 and timed out: both chains are valid
        assert_eq!(
            close.votes_due(&[leader1]),
            vec![(1, value(2), VoteKind::First)]
        );

        // Without the timeout certificate only the notarized value is chained
        let mut close = EpochClose::new(ConsensusEpoch::ZERO, keys(), TIMEOUT);
        close.set_state(&state(1));
        close.tick(t(0));
        for rep in 0..4 {
            vote(&mut close, rep, value(1), VoteKind::First, 0).unwrap();
        }
        close.tick(t(1));
        assert_eq!(close.info().round, 1);
        // Line 11: the exit final vote of a replica which notarized nothing,
        // and the leader's proposal of the chained value
        assert_eq!(
            close.votes_due(&[leader1]),
            vec![
                (0, value(1), VoteKind::Final),
                (1, value(1), VoteKind::First)
            ]
        );
        close.mark_voted(0, value(1), VoteKind::Final);
        close.set_state(&state(2));
        assert!(
            !close
                .votes_due(&[leader1])
                .iter()
                .any(|(round, _, _)| *round == 1)
        );
        close.set_state(&state(1));
        assert!(
            close
                .votes_due(&[leader1])
                .contains(&(1, value(1), VoteKind::First))
        );
    }

    #[test]
    fn second_look_notarizes_the_own_value_and_times_out_another() {
        let mut close = close_election();
        close.set_state(&state(1));
        close.tick(t(0));
        close.tick(t(5));
        close.mark_voted(0, value(1), VoteKind::Abstain);
        // Many first votes for the own value, no proposal seen: a second look
        for rep in 1..4 {
            vote(&mut close, rep, value(1), VoteKind::First, 0).unwrap();
        }
        let due = close.votes_due(&[FOLLOWER]);
        assert!(due.contains(&(0, value(1), VoteKind::Notar)));
        assert!(!due.iter().any(|(_, _, kind)| *kind == VoteKind::Timeout));
        close.mark_voted(0, value(1), VoteKind::Notar);
        // Looked at once; the cast vote is only re-broadcast
        assert_eq!(
            close
                .votes_due(&[FOLLOWER])
                .iter()
                .filter(|(_, _, kind)| *kind == VoteKind::Notar)
                .count(),
            1
        );

        // Many first votes for another value: the timeout block is notarized
        let mut close = close_election();
        close.set_state(&state(2));
        close.tick(t(0));
        vote(&mut close, 0, value(2), VoteKind::First, 0).unwrap();
        close.mark_voted(0, value(2), VoteKind::First);
        for rep in 1..4 {
            vote(&mut close, rep, value(1), VoteKind::First, 0).unwrap();
        }
        let due = close.votes_due(&[FOLLOWER]);
        assert!(due.contains(&(0, value(2), VoteKind::Timeout)));
        assert!(!due.contains(&(0, value(1), VoteKind::Notar)));
        close.mark_voted(0, value(2), VoteKind::Timeout);
        // The timeout block is notarized once
        assert_eq!(
            close
                .votes_due(&[FOLLOWER])
                .iter()
                .filter(|(_, _, kind)| *kind == VoteKind::Timeout)
                .count(),
            1
        );
    }

    #[test]
    fn split_first_votes_trigger_the_timeout_vote() {
        let mut close = close_election();
        close.set_state(&state(1));
        close.tick(t(0));
        vote(&mut close, 0, value(1), VoteKind::First, 0).unwrap();
        close.mark_voted(0, value(1), VoteKind::First);
        for rep in 1..3 {
            vote(&mut close, rep, value(1), VoteKind::First, 0).unwrap();
        }
        for rep in 3..6 {
            vote(&mut close, rep, value(rep as u64), VoteKind::Abstain, 0).unwrap();
        }
        assert!(
            close
                .votes_due(&[FOLLOWER])
                .contains(&(0, value(1), VoteKind::Timeout))
        );
        close.mark_voted(0, value(1), VoteKind::Timeout);
        // The timeout certificate ends the round
        for rep in 0..3 {
            vote(&mut close, rep, value(1), VoteKind::Timeout, 0).unwrap();
        }
        assert_eq!(close.round(0).state, ElectionState::TimedOut);
        close.tick(t(1));
        assert_eq!(close.info().round, 1);
        // Round 0 was left without a final vote, round 1 starts afresh
        assert_eq!(
            close.votes_due(&[FOLLOWER]),
            vec![
                (0, value(1), VoteKind::First),
                (0, value(1), VoteKind::Timeout)
            ]
        );
    }

    #[test]
    fn votes_for_later_rounds_create_them_within_bounds() {
        let mut close = close_election();
        vote(&mut close, 0, value(1), VoteKind::First, 3).unwrap();
        assert_eq!(close.rounds.len(), 4);
        assert_eq!(
            vote(&mut close, 0, value(1), VoteKind::First, 100),
            Err(VoteError::Ignored)
        );
        assert_eq!(
            vote(&mut close, 0, value(1), VoteKind::First, 3),
            Err(VoteError::Replay)
        );
        // Unentered rounds are not voted in
        close.set_state(&state(1));
        assert!(!close.info().started);
        close.tick(t(0));
        assert!(close.info().started);
        assert_eq!(close.votes_due(&[FOLLOWER]), vec![]);
    }

    #[test]
    fn evidence_and_solicitation() {
        let mut close = close_election();
        assert!(close.certificate_evidence(0).is_none());
        assert_eq!(close.solicitations(t(0), Duration::from_secs(1)), vec![]);
        close.set_state(&state(2));
        close.tick(t(0));
        assert!(close.certificate_evidence(0).is_none());
        assert_eq!(
            close.solicitations(t(0), Duration::from_secs(1)),
            vec![(0, value(2))]
        );
        assert_eq!(close.solicitations(t(0), Duration::from_secs(1)), vec![]);
        assert_eq!(
            close.solicitations(t(1), Duration::from_secs(1)),
            vec![(0, value(2))]
        );

        vote(&mut close, 0, value(1), VoteKind::First, 0).unwrap();
        close.mark_voted(0, value(2), VoteKind::Abstain);
        let evidence = close.certificate_evidence(0).unwrap();
        assert_eq!(
            evidence.statements,
            vec![(VoteKind::Abstain, vec![value(2)])]
        );
        assert!(evidence.blocks.is_empty());
        assert!(close.certificate_evidence(1).is_none());
    }

    /// The certificate may arrive before this replica's instances settled:
    /// it agrees once its own value turns out to be the finalized one
    #[test]
    fn agrees_once_its_own_value_is_the_finalized_one() {
        let mut close = close_election();
        for rep in 0..5 {
            vote(&mut close, rep, value(1), VoteKind::First, 0).unwrap();
        }
        assert!(close.is_closed());
        assert!(!close.is_agreed());
        assert!(close.take_events().contains(&CloseEvent::Closed {
            round: 0,
            value: value(1),
            own: false
        }));
        // An instance without a certificate keeps the replica from taking part
        let mut unterminated = state(2);
        unterminated.add_election(
            &Account::from(3),
            1,
            ElectionState::Active,
            &Certificates::default(),
        );
        close.set_state(&unterminated);
        assert!(!close.is_agreed());
        close.set_state(&state(2));
        assert!(!close.is_agreed());
        // Still without a certificate here, but the state without that
        // instance is the finalized one: agreed, the instance is undecided
        let mut agreeing = state(1);
        agreeing.add_election(
            &Account::from(3),
            1,
            ElectionState::Active,
            &Certificates::default(),
        );
        close.set_state(&agreeing);
        assert!(close.is_agreed());
        assert_eq!(
            close.take_events(),
            vec![
                CloseEvent::Ready(value(2)),
                CloseEvent::Ready(value(1)),
                CloseEvent::Agreed(value(1))
            ]
        );
    }

    #[test]
    fn leaders_follow_the_representatives_seen() {
        let mut close = EpochClose::new(ConsensusEpoch::ZERO, vec![key(5)], TIMEOUT);
        assert_eq!(close.leader(0), Some(key(5)));
        assert_eq!(close.leader(1), Some(key(5)));
        close.set_leaders(keys());
        assert_eq!(close.leader(1), Some(key(1)));
        close.set_leaders(Vec::new());
        assert_eq!(close.leader(1), None);
    }

    #[test]
    fn root_and_id_are_per_epoch_and_round() {
        let close = EpochClose::new(ConsensusEpoch::new(3), keys(), TIMEOUT);
        assert_ne!(
            EpochClose::root_of(ConsensusEpoch::new(3)),
            EpochClose::root_of(ConsensusEpoch::new(4))
        );
        assert_eq!(
            close.id(2),
            ElectionId::new(
                EpochClose::root_of(ConsensusEpoch::new(3)),
                ConsensusEpoch::close_round(ConsensusEpoch::new(3), 2)
            )
        );
        // The leader rotates with the epoch as well
        assert_eq!(close.leader(0), Some(keys()[3]));
    }

    /*
     * Test helpers
     */

    const TIMEOUT: Duration = Duration::from_secs(5);
    const LEADER: PublicKey = key(0);
    const FOLLOWER: PublicKey = key(3);

    const fn key(index: usize) -> PublicKey {
        // Six equal representatives in public key order
        PublicKey::from_bytes([[1; 32], [2; 32], [3; 32], [4; 32], [5; 32], [6; 32]][index])
    }

    fn keys() -> Vec<PublicKey> {
        (0..6).map(key).collect()
    }

    /// The committee of six equal representatives: four for a certificate
    fn committees() -> Committees {
        let weights: FxHashMap<PublicKey, Amount> =
            keys().into_iter().map(|k| (k, Amount::raw(100))).collect();
        Committees::single(Arc::new(Committee::new(weights)))
    }

    fn t(secs: u64) -> Timestamp {
        Timestamp::new_test_instance() + Duration::from_secs(secs)
    }

    fn state(entry: u64) -> EpochState {
        let mut state = EpochState::default();
        state
            .hash
            .add(&Account::from(entry), 1, &BlockHash::from(entry));
        state
    }

    fn value(entry: u64) -> BlockHash {
        state(entry).close_value(ConsensusEpoch::ZERO)
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
