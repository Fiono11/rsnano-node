use std::collections::{BTreeMap, BTreeSet};

use rsnano_types::{Amount, BlockHash, ConsensusEpoch, PrivateKey, PublicKey, Signature};

use super::*;
use crate::consensus::election::Committee;

/// Explicit already-verified evidence at the commit-8 trust boundary. This
/// fixture is not an implementation of recursive R/A/B certificate validation.
struct Evidence {
    instance: CheckpointInstance,
    values: BTreeSet<BlockHash>,
    introductions: BTreeMap<(u64, BlockHash), Introduction>,
    r: BTreeMap<(u64, BlockHash), BlockHash>,
}

impl CheckpointEvidence for Evidence {
    fn validate_value(
        &self,
        instance: &CheckpointInstance,
        value: BlockHash,
    ) -> Result<(), CheckpointError> {
        if *instance != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        if !self.values.contains(&value) {
            return Err(CheckpointError::MissingEvidence(value));
        }
        Ok(())
    }
    fn validate_introduction(
        &self,
        instance: &CheckpointInstance,
        rank: u64,
        value: BlockHash,
        introduction: &Introduction,
    ) -> Result<(), CheckpointError> {
        self.validate_value(instance, value)?;
        match self.introductions.get(&(rank, value)) {
            Some(expected) if expected == introduction => Ok(()),
            Some(_) => Err(CheckpointError::InvalidEvidence),
            None => Err(CheckpointError::MissingEvidence(introduction.digest())),
        }
    }
    fn r_maximum(
        &self,
        instance: &CheckpointInstance,
        rank: u64,
        evidence: BlockHash,
    ) -> Result<BlockHash, CheckpointError> {
        if *instance != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        self.r
            .get(&(rank, evidence))
            .copied()
            .ok_or(CheckpointError::MissingEvidence(evidence))
    }
}

#[derive(Default)]
struct Journal {
    records: BTreeMap<(CheckpointInstance, u64, PublicKey), FirstVote>,
    fail_before: bool,
    fail_after: bool,
    signatures_created: usize,
}
impl FirstVoteJournal for Journal {
    fn get_or_insert(
        &mut self,
        instance: CheckpointInstance,
        rank: u64,
        signer: PublicKey,
        create: impl FnOnce() -> Result<FirstVote, CheckpointError>,
    ) -> Result<FirstVote, CheckpointError> {
        if self.fail_before {
            return Err(CheckpointError::JournalFailure);
        }
        let key = (instance, rank, signer);
        if let Some(vote) = self.records.get(&key) {
            return Ok(vote.clone());
        }
        let vote = create()?;
        self.signatures_created += 1;
        self.records.insert(key, vote.clone());
        if self.fail_after {
            return Err(CheckpointError::JournalFailure);
        }
        Ok(vote)
    }
}

struct World {
    committee: Committee,
    evidence: Evidence,
}
impl World {
    fn new(f: u32, p: u32) -> Self {
        let n = 3 * f + 2 * p + 1;
        let committee = Committee::equal_weight(
            (1..=u64::from(n)).map(|i| PrivateKey::from(i).public_key()),
            f,
            p,
        )
        .unwrap();
        let instance = CheckpointInstance {
            session: BlockHash::from(11),
            epoch: ConsensusEpoch::new(3),
            predecessor: BlockHash::from(12),
            committee: committee.digest(),
        };
        let mut evidence = Evidence {
            instance,
            values: (1..=10).map(BlockHash::from).collect(),
            introductions: BTreeMap::new(),
            r: BTreeMap::new(),
        };
        for rank in 0..=2 {
            for value in 1..=10 {
                let intro = Self::intro(rank, value);
                evidence
                    .introductions
                    .insert((rank, BlockHash::from(value)), intro);
            }
        }
        evidence
            .r
            .insert((0, BlockHash::from(900)), BlockHash::from(9));
        Self {
            committee,
            evidence,
        }
    }
    fn intro(rank: u64, value: u64) -> Introduction {
        match rank {
            0 => Introduction::Proposal(BlockHash::from(100 + value)),
            1 => Introduction::PreviousB(BlockHash::from(200 + value)),
            _ => Introduction::PreviousFast(BlockHash::from(300 + value)),
        }
    }
    fn context(&self) -> CheckpointContext {
        CheckpointContext::new(self.evidence.instance, self.committee.clone()).unwrap()
    }
    fn vote(&self, signer: u64, rank: u64, value: u64) -> FirstVote {
        self.context()
            .first_vote(
                rank,
                BlockHash::from(value),
                Self::intro(rank, value),
                &PrivateKey::from(signer),
                &self.evidence,
                &mut Journal::default(),
            )
            .unwrap()
    }
    fn pool(&self, rank: u64, values: &[(u64, u64)]) -> FirstVotePool {
        let mut pool = FirstVotePool::new(self.context(), rank);
        for &(signer, value) in values {
            pool.receive(self.vote(signer, rank, value), &self.evidence)
                .unwrap();
        }
        pool
    }
}

#[test]
fn signing_is_immutable_per_rank_and_survives_uncertain_persistence() {
    let w = World::new(1, 1);
    let key = PrivateKey::from(1);
    let mut journal = Journal::default();
    let first = w
        .context()
        .first_vote(
            0,
            BlockHash::from(1),
            World::intro(0, 1),
            &key,
            &w.evidence,
            &mut journal,
        )
        .unwrap();
    // R's current maximum and triggering request change, but no second signature.
    let again = w
        .context()
        .first_vote(
            0,
            BlockHash::from(2),
            World::intro(0, 2),
            &key,
            &w.evidence,
            &mut journal,
        )
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(journal.signatures_created, 1);
    let next = w
        .context()
        .first_vote(
            1,
            BlockHash::from(2),
            World::intro(1, 2),
            &key,
            &w.evidence,
            &mut journal,
        )
        .unwrap();
    assert_eq!(next.value, BlockHash::from(2));
    assert_eq!(journal.signatures_created, 2);
    let mut journal = Journal {
        fail_before: true,
        ..Default::default()
    };
    assert_eq!(
        w.context().first_vote(
            0,
            BlockHash::from(1),
            World::intro(0, 1),
            &key,
            &w.evidence,
            &mut journal
        ),
        Err(CheckpointError::JournalFailure)
    );
    assert_eq!(journal.signatures_created, 0);
    journal.fail_before = false;
    journal.fail_after = true;
    assert_eq!(
        w.context().first_vote(
            0,
            BlockHash::from(1),
            World::intro(0, 1),
            &key,
            &w.evidence,
            &mut journal
        ),
        Err(CheckpointError::JournalFailure)
    );
    journal.fail_after = false;
    let recovered = w
        .context()
        .first_vote(
            0,
            BlockHash::from(2),
            World::intro(0, 2),
            &key,
            &w.evidence,
            &mut journal,
        )
        .unwrap();
    assert_eq!(recovered, first);
    assert_eq!(journal.signatures_created, 1);
}

#[test]
fn all_signed_fields_and_context_are_bound() {
    let w = World::new(1, 1);
    let vote = w.vote(1, 0, 1);
    assert_eq!(w.context().verify_first_vote(0, &vote, &w.evidence), Ok(()));
    let mut changed = vote.clone();
    changed.value = BlockHash::from(2);
    assert_eq!(
        w.context().verify_first_vote(0, &changed, &w.evidence),
        Err(CheckpointError::BadSignature)
    );
    changed = vote.clone();
    changed.introduction = World::intro(0, 2);
    assert_eq!(
        w.context().verify_first_vote(0, &changed, &w.evidence),
        Err(CheckpointError::BadSignature)
    );
    changed = vote.clone();
    changed.signer = PrivateKey::from(2).public_key();
    assert_eq!(
        w.context().verify_first_vote(0, &changed, &w.evidence),
        Err(CheckpointError::BadSignature)
    );
    changed = vote.clone();
    changed.rank = 1;
    assert_eq!(
        w.context().verify_first_vote(1, &changed, &w.evidence),
        Err(CheckpointError::BadSignature)
    );
    assert_eq!(
        w.context().verify_first_vote(1, &vote, &w.evidence),
        Err(CheckpointError::WrongRank)
    );
    changed = vote.clone();
    changed.signature = Signature::default();
    assert_eq!(
        w.context().verify_first_vote(0, &changed, &w.evidence),
        Err(CheckpointError::BadSignature)
    );
    for field in 0..4 {
        let mut other = vote.instance;
        match field {
            0 => other.session = BlockHash::from(99),
            1 => other.epoch = ConsensusEpoch::new(99),
            2 => other.predecessor = BlockHash::from(99),
            _ => other.committee = BlockHash::from(99),
        }
        changed = vote.clone();
        changed.instance = other;
        assert_eq!(
            w.context().verify_first_vote(0, &changed, &w.evidence),
            Err(CheckpointError::WrongInstance)
        );
        if field != 3 {
            let other_context = CheckpointContext::new(other, w.committee.clone()).unwrap();
            assert_eq!(
                other_context.verify_first_vote(0, &changed, &w.evidence),
                Err(CheckpointError::BadSignature)
            );
        }
    }
    let mut wrong = vote.instance;
    wrong.committee = BlockHash::ZERO;
    assert!(matches!(
        CheckpointContext::new(wrong, w.committee.clone()),
        Err(CheckpointError::WrongInstance)
    ));
    let weighted = Committee::with_online(w.committee.weights().clone(), Amount::raw(6));
    assert!(matches!(
        CheckpointContext::new(vote.instance, weighted),
        Err(CheckpointError::InvalidCommittee)
    ));
}

#[test]
fn membership_and_legal_ancestry_are_required_before_signing() {
    let w = World::new(1, 1);
    let context = w.context();
    let mut journal = Journal::default();
    assert_eq!(
        context.first_vote(
            0,
            BlockHash::from(1),
            World::intro(0, 1),
            &PrivateKey::from(7),
            &w.evidence,
            &mut journal
        ),
        Err(CheckpointError::NonMember)
    );
    for (rank, introduction) in [
        (0, World::intro(1, 1)),
        (1, World::intro(0, 1)),
        (2, World::intro(0, 1)),
    ] {
        assert_eq!(
            context.first_vote(
                rank,
                BlockHash::from(1),
                introduction,
                &PrivateKey::from(1),
                &w.evidence,
                &mut journal
            ),
            Err(CheckpointError::WrongAncestry)
        );
    }
    assert_eq!(
        context.first_vote(
            1,
            BlockHash::from(2),
            World::intro(1, 1),
            &PrivateKey::from(1),
            &w.evidence,
            &mut journal
        ),
        Err(CheckpointError::InvalidEvidence)
    );
    assert_eq!(journal.signatures_created, 0);
    for rank in [1, 2] {
        assert!(
            context
                .verify_first_vote(rank, &w.vote(1, rank, 1), &w.evidence)
                .is_ok()
        );
    }
}

#[test]
fn unknown_values_and_ancestry_are_parked_and_retried() {
    let mut w = World::new(1, 1);
    let vote = w.vote(1, 0, 1);
    let mut pool = FirstVotePool::new(w.context(), 0);
    w.evidence.values.remove(&vote.value);
    assert_eq!(
        pool.receive(vote.clone(), &w.evidence),
        Ok(VoteAdmission::Pending)
    );
    assert_eq!(
        pool.receive(vote.clone(), &w.evidence),
        Ok(VoteAdmission::Pending)
    );
    assert_eq!(pool.pending_len(), 1);
    assert!(pool.snapshot().is_empty());
    assert!(pool.fast_certificate(vote.value).is_none());
    w.evidence.values.insert(vote.value);
    w.evidence.introductions.remove(&(0, vote.value));
    assert_eq!(
        pool.retry_pending(&w.evidence),
        vec![Ok(VoteAdmission::Pending)]
    );
    w.evidence
        .introductions
        .insert((0, vote.value), vote.introduction);
    assert_eq!(
        pool.retry_pending(&w.evidence),
        vec![Ok(VoteAdmission::Counted)]
    );
    assert_eq!(pool.pending_len(), 0);
    assert_eq!(pool.snapshot(), vec![vote.clone()]);
    assert_eq!(
        pool.receive(vote, &w.evidence),
        Ok(VoteAdmission::Duplicate)
    );
    let mut bad = w.vote(2, 0, 1);
    bad.signature = Signature::default();
    assert_eq!(
        pool.receive(bad, &w.evidence),
        Err(CheckpointError::BadSignature)
    );
    assert_eq!(pool.pending_len(), 0);
    // A signed record whose dependency later proves invalid is removed,
    // without contributing to either recovery or fast counts.
    let invalid = w.vote(2, 0, 2);
    w.evidence.introductions.remove(&(0, invalid.value));
    assert_eq!(
        pool.receive(invalid.clone(), &w.evidence),
        Ok(VoteAdmission::Pending)
    );
    w.evidence
        .introductions
        .insert((0, invalid.value), World::intro(0, 3));
    assert_eq!(
        pool.retry_pending(&w.evidence),
        vec![Err(CheckpointError::InvalidEvidence)]
    );
    assert_eq!(pool.pending_len(), 0);
    assert_eq!(pool.snapshot().len(), 1);
}

#[test]
fn equivocation_does_not_hide_a_fast_certificate_or_double_count_recovery() {
    let w = World::new(1, 1);
    let mut pool = w.pool(0, &[(4, 2), (1, 1), (2, 1), (5, 1), (6, 1)]);
    assert!(pool.fast_certificate(BlockHash::from(1)).is_none());
    pool.receive(w.vote(4, 0, 1), &w.evidence).unwrap();
    assert_eq!(pool.snapshot().len(), 5);
    assert_eq!(
        pool.snapshot()
            .iter()
            .find(|v| v.signer == PrivateKey::from(4).public_key())
            .unwrap()
            .value,
        BlockHash::from(2)
    );
    let fc = pool.fast_certificate(BlockHash::from(1)).unwrap();
    assert_eq!(
        fc.verify(&w.context(), 0, &w.evidence),
        Ok(BlockHash::from(1))
    );
    let encoded = serde_json::to_vec(&fc).unwrap();
    let decoded: FastCertificate = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, fc);
    assert_eq!(decoded.verify(&w.context(), 0, &w.evidence), Ok(fc.value));
    let mut bad = fc.clone();
    bad.votes[1] = bad.votes[0].clone();
    assert_eq!(
        bad.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::DuplicateSigner)
    );
    bad = fc.clone();
    bad.votes.pop();
    assert_eq!(
        bad.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::InvalidSize)
    );
    bad = fc.clone();
    bad.value = BlockHash::from(2);
    assert_eq!(
        bad.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::InvalidEvidence)
    );
    bad = fc;
    bad.rank = 1;
    assert_eq!(
        bad.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::WrongRank)
    );
}

#[test]
fn worked_hidden_fast_recovery_waits_then_selects_v() {
    let w = World::new(1, 1);
    let mut slow = w.pool(0, &[(1, 1), (2, 1), (3, 2), (4, 2)]);
    assert!(
        matches!(slow.resolve(None, &w.evidence).unwrap(), RecoveryProgress::Ambiguous(values) if values.len() == 2)
    );
    let fast = w.pool(0, &[(1, 1), (2, 1), (4, 1), (5, 1), (6, 1)]);
    let fc = fast.fast_certificate(BlockHash::from(1)).unwrap();
    assert!(fc.verify(&w.context(), 0, &w.evidence).is_ok());
    let peer = w.pool(0, &[(1, 1), (2, 1), (3, 2), (4, 2), (6, 1)]);
    let RecoveryProgress::Resolved(rc) = peer.resolve(None, &w.evidence).unwrap() else {
        panic!("resolved at P");
    };
    // Independent import works while the receiver's own snapshot is ambiguous.
    assert_eq!(
        rc.verify(slow.context(), slow.rank(), &w.evidence),
        Ok(BlockHash::from(1))
    );
    slow.receive(w.vote(6, 0, 1), &w.evidence).unwrap();
    assert_eq!(
        slow.resolve(None, &w.evidence),
        Ok(RecoveryProgress::Resolved(rc.clone()))
    );
    let decoded: RecoveryCertificate =
        serde_json::from_slice(&serde_json::to_vec(&rc).unwrap()).unwrap();
    assert_eq!(decoded.verify(&w.context(), 0, &w.evidence), Ok(fc.value));
}

#[test]
fn recovery_can_resolve_at_q_but_never_picks_an_ambiguous_maximum() {
    let w = World::new(1, 1);
    let below = w.pool(0, &[(1, 1), (2, 1), (3, 1)]);
    assert_eq!(
        below.resolve(None, &w.evidence),
        Ok(RecoveryProgress::NeedVotes)
    );
    let pool = w.pool(0, &[(1, 1), (2, 1), (3, 1), (4, 2)]);
    let RecoveryProgress::Resolved(rc) = pool.resolve(None, &w.evidence).unwrap() else {
        panic!("singleton at Q");
    };
    assert_eq!(rc.value, BlockHash::from(1));
    assert_eq!(rc.snapshot.len(), 4);
    let ambiguous = w.pool(0, &[(1, 1), (2, 1), (3, 2), (4, 2)]);
    let forged = RecoveryCertificate {
        snapshot: ambiguous.snapshot(),
        value: BlockHash::from(2),
        ..rc.clone()
    };
    assert_eq!(
        forged.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::InvalidRecovery)
    );
    let mut duplicated = rc.clone();
    duplicated.snapshot[1] = duplicated.snapshot[0].clone();
    assert_eq!(
        duplicated.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::DuplicateSigner)
    );
    let mut mixed = rc;
    mixed.snapshot[0] = w.vote(1, 1, 1);
    assert_eq!(
        mixed.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::WrongRank)
    );
}

#[test]
fn empty_recovery_requires_verified_r_maximum_not_snapshot_maximum() {
    let w = World::new(1, 1);
    let pool = w.pool(0, &[(1, 1), (2, 2), (3, 3), (4, 4)]);
    assert_eq!(
        pool.resolve(None, &w.evidence),
        Ok(RecoveryProgress::NeedREvidence)
    );
    assert_eq!(
        pool.resolve(Some(BlockHash::from(901)), &w.evidence),
        Err(CheckpointError::MissingEvidence(BlockHash::from(901)))
    );
    let RecoveryProgress::Resolved(rc) = pool
        .resolve(Some(BlockHash::from(900)), &w.evidence)
        .unwrap()
    else {
        panic!("empty recovery");
    };
    assert_eq!(rc.value, BlockHash::from(9));
    assert!(matches!(rc.resolution, RecoveryResolution::Empty { .. }));
    let mut bad = rc.clone();
    bad.value = BlockHash::from(4);
    assert_eq!(
        bad.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::InvalidRecovery)
    );
    bad = rc.clone();
    bad.resolution = RecoveryResolution::Singleton;
    assert_eq!(
        bad.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::InvalidRecovery)
    );
    let decoded: RecoveryCertificate =
        serde_json::from_slice(&serde_json::to_vec(&rc).unwrap()).unwrap();
    assert_eq!(decoded.verify(&w.context(), 0, &w.evidence), Ok(rc.value));
}

#[test]
fn fast_listener_runs_at_higher_ranks() {
    let w = World::new(1, 1);
    for rank in [1, 2] {
        let pool = w.pool(rank, &[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
        let fc = pool.fast_certificate(BlockHash::from(1)).unwrap();
        assert_eq!(
            fc.verify(&w.context(), rank, &w.evidence),
            Ok(BlockHash::from(1))
        );
        assert_eq!(
            fc.verify(&w.context(), rank - 1, &w.evidence),
            Err(CheckpointError::WrongRank)
        );
    }
}

#[test]
fn exhaustive_hidden_fast_snapshots_with_byzantine_equivocation() {
    // Enumerate every FC signer set, snapshot identity subset, and allowed
    // conflicting vote subset. All non-fast values may be collapsed into w:
    // hidden-fast membership depends only on v's count and snapshot size.
    for (f, p) in [(1, 1), (2, 1)] {
        let w = World::new(f, p);
        let context = w.context();
        let t = context.thresholds();
        let end = 1u32 << t.n;
        let byzantine = (1u32 << f) - 1;
        let mut checked = 0;
        for fc in 0..end {
            if u64::from(fc.count_ones()) != t.f_fast {
                continue;
            }
            for snapshot in 0..end {
                if u64::from(snapshot.count_ones()) < t.q {
                    continue;
                }
                // Correct FC signers must report v. Other identities may
                // report w, including a Byzantine signer of the hidden FC.
                let may_conflict = snapshot & (byzantine | !fc);
                let mut conflicting = may_conflict;
                loop {
                    let values: Vec<_> = (0..t.n)
                        .filter(|i| snapshot & (1 << i) != 0)
                        .map(|i| BlockHash::from(if conflicting & (1 << i) != 0 { 2 } else { 1 }))
                        .collect();
                    let possible = super::recovery::candidate_values(&context, &values).unwrap();
                    assert!(
                        possible.contains(&BlockHash::from(1)),
                        "f={f},p={p},fc={fc},snapshot={snapshot},conflicting={conflicting}"
                    );
                    checked += 1;
                    if conflicting == 0 {
                        break;
                    }
                    conflicting = (conflicting - 1) & may_conflict;
                }
            }
        }
        assert!(checked > 100);
    }
}

#[test]
fn exhaustive_partitions_resolve_at_p_and_p_minus_one_can_be_ambiguous() {
    // Integer partitions enumerate all support distributions up to value
    // renaming, including more than two conflicting values.
    fn partitions(left: u64, max: u64, counts: &mut Vec<u64>, visit: &mut impl FnMut(&[u64])) {
        if left == 0 {
            visit(counts);
            return;
        }
        for count in 1..=left.min(max) {
            counts.push(count);
            partitions(left - count, count, counts, visit);
            counts.pop();
        }
    }
    for (f, p) in [(1, 1), (2, 1)] {
        let w = World::new(f, p);
        let context = w.context();
        let t = context.thresholds();
        for m in t.p_recovery..=t.n {
            partitions(m, m, &mut Vec::new(), &mut |counts| {
                let values: Vec<_> = counts
                    .iter()
                    .enumerate()
                    .flat_map(|(i, count)| {
                        std::iter::repeat_n(BlockHash::from(i as u64 + 1), *count as usize)
                    })
                    .collect();
                assert!(
                    super::recovery::candidate_values(&context, &values)
                        .unwrap()
                        .len()
                        <= 1
                );
            });
        }
        let ambiguous: Vec<_> = [1u64, 2]
            .into_iter()
            .flat_map(|v| std::iter::repeat_n(BlockHash::from(v), (f + p) as usize))
            .collect();
        assert_eq!(ambiguous.len() as u64, t.p_recovery - 1);
        assert_eq!(
            super::recovery::candidate_values(&context, &ambiguous)
                .unwrap()
                .len(),
            2
        );
    }
}
