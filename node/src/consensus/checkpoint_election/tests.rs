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

#[test]
fn volatile_journal_keeps_locks_when_the_current_r_value_changes() {
    let w = World::new(1, 1);
    let mut journal = VolatileFirstVoteJournal::default();
    let key = PrivateKey::from(1);
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
    let repeated = w
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
    assert_eq!(first, repeated);
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
}

/// A conformance counterexample to the unrestricted persistence assertion in
/// v1.2 Lemma 6.5, not a test claiming two conflicting decisions. Correct
/// mutable responders retain true/max state; an earlier signed certificate
/// nevertheless remains valid under the stated predecessor verification rule.
#[test]
fn earlier_b_adoption_certificate_survives_a_later_true_certificate() {
    use serde::Serialize;
    #[derive(Clone, Serialize)]
    struct SignedSnapshot<T> {
        instance: CheckpointInstance,
        rank: u64,
        request: BlockHash,
        signer: PublicKey,
        values: Vec<T>,
        signature: Signature,
    }
    impl<T: Serialize> SignedSnapshot<T> {
        fn digest(&self) -> BlockHash {
            rsnano_types::Blake2HashBuilder::new()
                .update(b"RAI conformance snapshot")
                .update(
                    serde_json::to_vec(&(
                        self.instance,
                        self.rank,
                        self.request,
                        self.signer,
                        &self.values,
                    ))
                    .unwrap(),
                )
                .build()
        }
        fn new(instance: CheckpointInstance, request: u64, signer: u64, values: Vec<T>) -> Self {
            let key = PrivateKey::from(signer);
            let mut result = Self {
                instance,
                rank: 0,
                request: BlockHash::from(request),
                signer: key.public_key(),
                values,
                signature: Signature::default(),
            };
            result.signature = key.sign(result.digest().as_bytes());
            result
        }
        fn verify(&self, w: &World) {
            assert_eq!(self.instance, w.evidence.instance);
            assert_eq!(w.committee.weight(&self.signer), Amount::raw(1));
            self.signer
                .verify(self.digest().as_bytes(), &self.signature)
                .unwrap();
        }
    }
    let w = World::new(1, 1);
    let v = BlockHash::from(1);
    let other = BlockHash::from(2);
    // Correct first votes: 1:v, 2:v, 3:other, 5:other, 6:third.
    // Both singleton recoveries are legal without first-vote equivocation.
    let RecoveryProgress::Resolved(rc_v) = w
        .pool(0, &[(1, 1), (2, 1), (3, 2), (6, 3)])
        .resolve(None, &w.evidence)
        .unwrap()
    else {
        panic!()
    };
    let RecoveryProgress::Resolved(rc_other) = w
        .pool(0, &[(1, 1), (3, 2), (5, 2), (6, 3)])
        .resolve(None, &w.evidence)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(rc_v.verify(&w.context(), 0, &w.evidence), Ok(v));
    assert_eq!(rc_other.verify(&w.context(), 0, &w.evidence), Ok(other));
    // IDs 101/102 name A(v)/A(other); 201/202 name B(false,other)/B(true,v).
    // Each set contains four correct responders; identity 4 may schedule the
    // conflicting requests and withhold certificates as the Byzantine replica.
    let ids = [1u64, 2, 3, 5];
    let mut a_states = [
        AState::default(),
        AState::default(),
        AState::default(),
        AState::default(),
    ];
    let mut b_states = [
        BState::default(),
        BState::default(),
        BState::default(),
        BState::default(),
    ];
    let mut witnesses = Witnesses::default();
    let a_true: Vec<_> = ids
        .iter()
        .zip(a_states.iter_mut())
        .map(|(&id, state)| SignedSnapshot::new(w.evidence.instance, 101, id, state.deliver(v)))
        .collect();
    for snapshot in &a_true {
        snapshot.verify(&w);
        witnesses.record(snapshot.request, snapshot.signer);
    }
    let true_result = evaluate_a(
        4,
        &a_true
            .iter()
            .map(|s| (s.signer, s.values.clone()))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(
        true_result,
        BValue {
            flag: true,
            value: v
        }
    );
    let a_false: Vec<_> = ids
        .iter()
        .zip(a_states.iter_mut())
        .map(|(&id, state)| SignedSnapshot::new(w.evidence.instance, 102, id, state.deliver(other)))
        .collect();
    for snapshot in &a_false {
        snapshot.verify(&w);
        witnesses.record(snapshot.request, snapshot.signer);
    }
    assert!(witnesses.eligible(&[BlockHash::from(101), BlockHash::from(102)], 4));
    let false_result = evaluate_a(
        4,
        &a_false
            .iter()
            .map(|s| (s.signer, s.values.clone()))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(
        false_result,
        BValue {
            flag: false,
            value: other
        }
    );
    assert!(witnesses.admits(
        BlockHash::from(201),
        &[BlockHash::from(101), BlockHash::from(102)],
        2
    ));
    let old_b: Vec<_> = ids
        .iter()
        .zip(b_states.iter_mut())
        .map(|(&id, state)| {
            SignedSnapshot::new(
                w.evidence.instance,
                201,
                id,
                state.deliver(false_result).unwrap(),
            )
        })
        .collect();
    for snapshot in &old_b {
        snapshot.verify(&w);
        witnesses.record(snapshot.request, snapshot.signer);
    }
    assert!(witnesses.eligible(&[BlockHash::from(201)], 4));
    let old_result = evaluate_b(
        4,
        &old_b
            .iter()
            .map(|s| (s.signer, s.values.clone()))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(old_result, BOutcome::Adopt(other));
    // Deliver the delayed, valid B(true,v). No correct responder erases v.
    assert!(witnesses.admits(BlockHash::from(202), &[BlockHash::from(101)], 2));
    let new_b: Vec<_> = ids
        .iter()
        .zip(b_states.iter_mut())
        .map(|(&id, state)| {
            SignedSnapshot::new(
                w.evidence.instance,
                202,
                id,
                state.deliver(true_result).unwrap(),
            )
        })
        .collect();
    for snapshot in &new_b {
        snapshot.verify(&w);
        witnesses.record(snapshot.request, snapshot.signer);
    }
    assert!(witnesses.eligible(&[BlockHash::from(201), BlockHash::from(202)], 4));
    assert_eq!(
        evaluate_b(
            4,
            &new_b
                .iter()
                .map(|s| (s.signer, s.values.clone()))
                .collect::<Vec<_>>()
        ),
        Ok(BOutcome::Adopt(v))
    );
    // Complete ancestry and all W/Q witnesses for the old certificate still
    // exist. A stateless verifier recomputes Adopt(other), not Adopt(v).
    for snapshot in &old_b {
        snapshot.verify(&w);
    }
    assert_eq!(
        evaluate_b(
            4,
            &old_b
                .iter()
                .map(|s| (s.signer, s.values.clone()))
                .collect::<Vec<_>>()
        ),
        Ok(BOutcome::Adopt(other))
    );
    assert!(
        b_states
            .iter()
            .all(|state| state.values().contains(&true_result))
    );
    eprintln!(
        "RAI_PERSISTENCE_TRACE {}",
        serde_json::json!({
            "instance": w.evidence.instance, "recoveries": [rc_v, rc_other],
            "a_true": a_true, "a_false": a_false, "old_b": old_b, "new_b": new_b,
            "old_adopt": other, "later_adopt": v,
            "scope": "certificate persistence lemma; no conflicting decisions asserted"
        })
    );
}
