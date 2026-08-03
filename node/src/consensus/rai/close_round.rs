use std::collections::{BTreeMap, BTreeSet};

use rsnano_types::{BlockHash, RaiEpoch};

use super::{RaiElectionVoteState, RaiLocalResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RaiCloseKind {
    Cut,
    Record,
}

/// Logical identity of one close election.  Committee-local locks live in the
/// vote state stored under this identity, so locks from different rounds (or
/// the cut and record elections) can never alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RaiCloseElectionId {
    pub kind: RaiCloseKind,
    pub epoch: RaiEpoch,
    pub round: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiCloseRoundResult {
    Pending,
    LiveCarry(BlockHash),
    Dead,
    Decided(BlockHash),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiCloseRoundAction {
    Wait,
    StartFresh { round: u32, hash: BlockHash },
    StartCarry { round: u32, hash: BlockHash },
    Inert,
}

#[derive(Clone, Debug)]
pub struct RaiCloseRoundState {
    pub id: RaiCloseElectionId,
    pub candidates: BTreeSet<BlockHash>,
    pub validated_preimages: BTreeSet<BlockHash>,
    pub evidence: RaiElectionVoteState,
    pub carried: Option<BlockHash>,
    pub finished: RaiCloseRoundResult,
}

impl RaiCloseRoundState {
    fn new(id: RaiCloseElectionId, hash: BlockHash, carried: Option<BlockHash>) -> Self {
        Self {
            id,
            candidates: BTreeSet::from([hash]),
            validated_preimages: BTreeSet::from([hash]),
            evidence: RaiElectionVoteState::default(),
            carried,
            finished: RaiCloseRoundResult::Pending,
        }
    }

    /// Recomputes the result solely from validated, stored certificate votes.
    /// A timeout certificate in any required committee, or two incompatible
    /// committee-local terminal certificates, excludes every possible present
    /// or delayed global fast/final certificate.
    pub fn derive(&self) -> RaiCloseRoundResult {
        if let RaiCloseRoundResult::Decided(hash) = self.finished {
            return RaiCloseRoundResult::Decided(hash);
        }
        let mut supported = BTreeSet::new();
        let mut terminal_count = 0;
        for committee in 0..self.evidence.committees.len() {
            match self.evidence.local_result(committee) {
                Some(RaiLocalResult::Timeout) => return RaiCloseRoundResult::Dead,
                Some(
                    RaiLocalResult::Notarized(hash)
                    | RaiLocalResult::Fast(hash)
                    | RaiLocalResult::Final(hash),
                ) => {
                    supported.insert(hash);
                    terminal_count += 1;
                }
                None => {}
            }
        }
        if supported.len() > 1 {
            RaiCloseRoundResult::Dead
        } else if terminal_count == self.evidence.committees.len() && terminal_count != 0 {
            RaiCloseRoundResult::LiveCarry(*supported.first().unwrap())
        } else {
            RaiCloseRoundResult::Pending
        }
    }
}

#[derive(Clone, Debug)]
pub struct RaiCloseRoundTracker {
    kind: RaiCloseKind,
    epoch: RaiEpoch,
    current_round: u32,
    rounds: BTreeMap<u32, RaiCloseRoundState>,
    decision: Option<(u32, BlockHash)>,
}

impl RaiCloseRoundTracker {
    pub fn new(kind: RaiCloseKind, epoch: RaiEpoch) -> Self {
        Self {
            kind,
            epoch,
            current_round: 0,
            rounds: BTreeMap::new(),
            decision: None,
        }
    }

    pub fn current_round(&self) -> u32 {
        self.current_round
    }
    pub fn round(&self, round: u32) -> Option<&RaiCloseRoundState> {
        self.rounds.get(&round)
    }
    pub fn decision(&self) -> Option<(u32, BlockHash)> {
        self.decision
    }

    pub fn snapshot(&self) -> Self {
        self.clone()
    }

    pub fn from_snapshot(snapshot: Self) -> Option<Self> {
        let current = snapshot.rounds.get(&snapshot.current_round)?;
        if current.id.kind != snapshot.kind
            || current.id.epoch != snapshot.epoch
            || current.id.round != snapshot.current_round
            || current.validated_preimages.is_empty()
        {
            return None;
        }
        Some(snapshot)
    }

    pub fn start_round_zero(&mut self, hash: BlockHash) -> RaiCloseRoundAction {
        if self.decision.is_some() {
            return RaiCloseRoundAction::Inert;
        }
        self.rounds.entry(0).or_insert_with(|| {
            RaiCloseRoundState::new(
                RaiCloseElectionId {
                    kind: self.kind,
                    epoch: self.epoch,
                    round: 0,
                },
                hash,
                None,
            )
        });
        RaiCloseRoundAction::StartFresh { round: 0, hash }
    }

    pub fn store_evidence(&mut self, round: u32, evidence: RaiElectionVoteState) -> bool {
        let Some(state) = self.rounds.get_mut(&round) else {
            return false;
        };
        if self.decision.is_some() {
            return false;
        }
        state.evidence = evidence;
        state.finished = state.derive();
        true
    }

    pub fn add_validated_preimage(&mut self, round: u32, hash: BlockHash) -> bool {
        let Some(state) = self.rounds.get_mut(&round) else {
            return false;
        };
        state.candidates.insert(hash);
        state.validated_preimages.insert(hash)
    }

    pub fn next(&mut self, fresh: BlockHash) -> RaiCloseRoundAction {
        if self.decision.is_some() {
            return RaiCloseRoundAction::Inert;
        }
        let Some(source) = self.rounds.get(&self.current_round) else {
            return self.start_round_zero(fresh);
        };
        let derived = source.derive();
        let next_round = match self.current_round.checked_add(1) {
            Some(round) => round,
            None => return RaiCloseRoundAction::Wait,
        };
        let (hash, carried) = match derived {
            RaiCloseRoundResult::Dead => (fresh, None),
            RaiCloseRoundResult::LiveCarry(hash) if source.validated_preimages.contains(&hash) => {
                (hash, Some(hash))
            }
            RaiCloseRoundResult::LiveCarry(_) | RaiCloseRoundResult::Pending => {
                return RaiCloseRoundAction::Wait;
            }
            RaiCloseRoundResult::Decided(_) => return RaiCloseRoundAction::Inert,
        };
        if let Some(existing) = self.rounds.get(&next_round) {
            // A restored/replayed transition must reproduce the exact value
            // already installed for the successor, never the caller's newly
            // computed fresh preference.
            let existing_hash = *existing
                .validated_preimages
                .first()
                .expect("a close round always retains its opening");
            return if existing.carried.is_some() {
                RaiCloseRoundAction::StartCarry {
                    round: next_round,
                    hash: existing_hash,
                }
            } else {
                RaiCloseRoundAction::StartFresh {
                    round: next_round,
                    hash: existing_hash,
                }
            };
        }
        self.rounds.insert(
            next_round,
            RaiCloseRoundState::new(
                RaiCloseElectionId {
                    kind: self.kind,
                    epoch: self.epoch,
                    round: next_round,
                },
                hash,
                carried,
            ),
        );
        self.current_round = next_round;
        if carried.is_some() {
            RaiCloseRoundAction::StartCarry {
                round: next_round,
                hash,
            }
        } else {
            RaiCloseRoundAction::StartFresh {
                round: next_round,
                hash,
            }
        }
    }

    pub fn decide(&mut self, round: u32, hash: BlockHash) -> bool {
        if let Some(existing) = self.decision {
            return existing == (round, hash);
        }
        let Some(state) = self.rounds.get_mut(&round) else {
            return false;
        };
        if !state.validated_preimages.contains(&hash) {
            return false;
        }
        state.finished = RaiCloseRoundResult::Decided(hash);
        self.decision = Some((round, hash));
        true
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc};

    use rsnano_ledger::RepWeights;
    use rsnano_types::{Amount, PublicKey, RaiCommitteeScope};

    use super::*;
    use crate::consensus::rai::BlockHashOrTimeout;

    fn hash(value: u64) -> BlockHash {
        BlockHash::from(value)
    }

    fn evidence(results: &[(u64, BlockHashOrTimeout)]) -> RaiElectionVoteState {
        let mut committees = Vec::new();
        for (signer, result) in results {
            let key = PublicKey::from(*signer);
            let weights = Arc::new(RepWeights::from([(key, Amount::raw(1))]));
            let mut state = super::super::RaiCommitteeInstance {
                weights,
                thresholds: super::super::RaiThresholds {
                    faulty: Amount::ZERO,
                    slack: Amount::ZERO,
                    progression: Amount::raw(1),
                    notarization: Amount::raw(1),
                    fast: Amount::raw(1),
                    finalization: Amount::raw(1),
                },
                votes: super::super::RaiCommitteeVoteState::default(),
            };
            state.votes.notar.insert(key, HashSet::from([*result]));
            committees.push(state);
        }
        RaiElectionVoteState {
            committees,
            outcome: Default::default(),
        }
    }

    #[test]
    fn timer_expiry_without_certificate_does_not_prove_death() {
        let mut rounds = RaiCloseRoundTracker::new(RaiCloseKind::Cut, 0.into());
        rounds.start_round_zero(hash(1));
        assert_eq!(rounds.next(hash(2)), RaiCloseRoundAction::Wait);
        assert_eq!(rounds.current_round(), 0);
    }

    #[test]
    fn timeout_certificate_advances_exactly_once() {
        let mut rounds = RaiCloseRoundTracker::new(RaiCloseKind::Cut, 0.into());
        rounds.start_round_zero(hash(1));
        rounds.store_evidence(0, evidence(&[(1, BlockHashOrTimeout::Timeout)]));
        assert_eq!(
            rounds.next(hash(2)),
            RaiCloseRoundAction::StartFresh {
                round: 1,
                hash: hash(2)
            }
        );
        assert_eq!(rounds.next(hash(3)), RaiCloseRoundAction::Wait);
        assert_eq!(rounds.current_round(), 1);
    }

    #[test]
    fn replayed_retry_keeps_the_original_fresh_hash() {
        let mut rounds = RaiCloseRoundTracker::new(RaiCloseKind::Cut, 0.into());
        rounds.start_round_zero(hash(1));
        rounds.store_evidence(0, evidence(&[(1, BlockHashOrTimeout::Timeout)]));
        assert_eq!(
            rounds.next(hash(2)),
            RaiCloseRoundAction::StartFresh {
                round: 1,
                hash: hash(2)
            }
        );

        // Replaying the source-round event after local visibility changed must
        // not start the same election id with a different candidate.
        rounds.current_round = 0;
        assert_eq!(
            rounds.next(hash(99)),
            RaiCloseRoundAction::StartFresh {
                round: 1,
                hash: hash(2)
            }
        );
    }

    #[test]
    fn unique_live_value_is_carried_and_overrides_fresh() {
        let mut rounds = RaiCloseRoundTracker::new(RaiCloseKind::Record, 4.into());
        rounds.start_round_zero(hash(7));
        rounds.store_evidence(0, evidence(&[(1, BlockHashOrTimeout::Block(hash(7)))]));
        assert_eq!(
            rounds.next(hash(99)),
            RaiCloseRoundAction::StartCarry {
                round: 1,
                hash: hash(7)
            }
        );
        assert_eq!(rounds.round(1).unwrap().carried, Some(hash(7)));
    }

    #[test]
    fn delayed_conflict_changes_only_pending_derivation() {
        let mut rounds = RaiCloseRoundTracker::new(RaiCloseKind::Cut, 2.into());
        rounds.start_round_zero(hash(1));
        rounds.store_evidence(0, evidence(&[(1, BlockHashOrTimeout::Block(hash(1)))]));
        assert_eq!(
            rounds.round(0).unwrap().derive(),
            RaiCloseRoundResult::LiveCarry(hash(1))
        );
        rounds.store_evidence(
            0,
            evidence(&[
                (1, BlockHashOrTimeout::Block(hash(1))),
                (2, BlockHashOrTimeout::Block(hash(2))),
            ]),
        );
        assert_eq!(rounds.round(0).unwrap().derive(), RaiCloseRoundResult::Dead);
        assert_eq!(
            rounds.next(hash(3)),
            RaiCloseRoundAction::StartFresh {
                round: 1,
                hash: hash(3)
            }
        );
    }

    #[test]
    fn restart_at_transition_is_idempotent_and_kinds_are_isolated() {
        let mut cut = RaiCloseRoundTracker::new(RaiCloseKind::Cut, 0.into());
        cut.start_round_zero(hash(1));
        cut.store_evidence(0, evidence(&[(1, BlockHashOrTimeout::Timeout)]));
        let mut restarted = RaiCloseRoundTracker::from_snapshot(cut.snapshot()).unwrap();
        assert_eq!(cut.next(hash(2)), restarted.next(hash(2)));
        assert_eq!(cut.current_round(), 1);

        let mut record = RaiCloseRoundTracker::new(RaiCloseKind::Record, 0.into());
        record.start_round_zero(hash(9));
        assert_eq!(record.current_round(), 0);
        assert_eq!(record.next(hash(10)), RaiCloseRoundAction::Wait);
    }

    #[test]
    fn carried_hash_without_preimage_waits() {
        let mut rounds = RaiCloseRoundTracker::new(RaiCloseKind::Cut, 0.into());
        rounds.start_round_zero(hash(1));
        // Evidence for an otherwise valid value whose opening was never accepted.
        rounds.store_evidence(0, evidence(&[(1, BlockHashOrTimeout::Block(hash(2)))]));
        assert_eq!(rounds.next(hash(3)), RaiCloseRoundAction::Wait);
    }

    #[test]
    fn locks_are_per_round_and_committee_instance() {
        let mut first = RaiElectionVoteState::new(vec![Arc::new(RepWeights::from([(
            PublicKey::from(1),
            Amount::raw(1),
        )]))]);
        first
            .record_first_vote(
                PublicKey::from(1),
                BlockHashOrTimeout::Block(hash(1)),
                RaiCommitteeScope::All,
            )
            .unwrap();
        let mut second = first.clone();
        second.committees[0].votes = Default::default();
        second
            .record_first_vote(
                PublicKey::from(1),
                BlockHashOrTimeout::Block(hash(2)),
                RaiCommitteeScope::All,
            )
            .unwrap();
    }
}
