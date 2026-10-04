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
        let mut pool = FirstVotePool::new(self.context());
        for &(signer, value) in values {
            pool.receive(self.vote(signer, rank, value), &self.evidence)
                .unwrap();
        }
        pool
    }
}

#[test]
fn signing_is_immutable_per_instance_and_survives_uncertain_persistence() {
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
    let next = w.context().first_vote(
        1,
        BlockHash::from(2),
        World::intro(1, 2),
        &key,
        &w.evidence,
        &mut journal,
    );
    assert_eq!(next, Err(CheckpointError::WrongRank));
    assert_eq!(journal.signatures_created, 1);
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
        Err(CheckpointError::WrongRank)
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
            Err(if rank == 0 {
                CheckpointError::WrongAncestry
            } else {
                CheckpointError::WrongRank
            })
        );
    }
    assert_eq!(
        context.first_vote(
            0,
            BlockHash::from(2),
            World::intro(0, 1),
            &PrivateKey::from(1),
            &w.evidence,
            &mut journal
        ),
        Err(CheckpointError::InvalidEvidence)
    );
    assert_eq!(journal.signatures_created, 0);
}

#[test]
fn unknown_values_and_ancestry_are_parked_and_retried() {
    let mut w = World::new(1, 1);
    let vote = w.vote(1, 0, 1);
    let mut pool = FirstVotePool::new(w.context());
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
    mixed.snapshot[0] = w.vote(1, 0, 1);
    mixed.snapshot[0].rank = 1;
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
fn fast_certificates_cannot_be_replayed_at_slow_ranks() {
    let w = World::new(1, 1);
    let pool = w.pool(0, &[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
    let mut fc = pool.fast_certificate(BlockHash::from(1)).unwrap();
    assert_eq!(fc.verify(&w.context(), 0, &w.evidence), Ok(fc.value));
    for rank in [1, 2] {
        fc.rank = rank;
        assert_eq!(
            fc.verify(&w.context(), rank, &w.evidence),
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
    let next = w.context().first_vote(
        1,
        BlockHash::from(2),
        World::intro(1, 2),
        &key,
        &w.evidence,
        &mut journal,
    );
    assert_eq!(next, Err(CheckpointError::WrongRank));
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

#[test]
fn slow_membership_and_instance_are_deterministic_and_separate() {
    let w = World::new(1, 1);
    let fast = w.context();
    let slow = SlowContext::new(&fast);
    let expected: BTreeSet<_> = w
        .committee
        .weights()
        .keys()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(4)
        .collect();
    assert_eq!(slow.members(), &expected);
    assert_eq!((slow.quorum(), slow.witness_threshold()), (3, 2));
    assert_ne!(slow.instance(), fast.instance().digest());
    let reordered = Committee::equal_weight(
        (1..=6).rev().map(|i| PrivateKey::from(i).public_key()),
        1,
        1,
    )
    .unwrap();
    let reordered = CheckpointContext::new(fast.instance(), reordered).unwrap();
    assert_eq!(slow.instance(), SlowContext::new(&reordered).instance());
    let mut other = fast.instance();
    other.epoch = ConsensusEpoch::new(4);
    let other = CheckpointContext::new(other, w.committee.clone()).unwrap();
    assert_ne!(slow.instance(), SlowContext::new(&other).instance());
}

#[test]
fn certified_fallback_starts_before_fast_completion_and_keeps_fast_listener() {
    let w = World::new(1, 1);
    let fast = w.context();
    let slow = SlowContext::new(&fast);
    let key = (1..=6)
        .map(PrivateKey::from)
        .find(|k| slow.members().contains(&k.public_key()))
        .unwrap();
    let mut pool = w.pool(0, &[(1, 1), (2, 1), (3, 1), (4, 1)]);
    assert!(pool.fast_certificate(BlockHash::from(1)).is_none());
    let RecoveryProgress::Resolved(rc) = pool.resolve(None, &w.evidence).unwrap() else {
        panic!("expected RC0")
    };
    let proposal = slow.proposal(&fast, rc.clone(), &key, &w.evidence).unwrap();
    assert_eq!(proposal.verify(&slow, &fast, &w.evidence), Ok(rc.value));
    let decoded: SlowProposal =
        serde_json::from_slice(&serde_json::to_vec(&proposal).unwrap()).unwrap();
    assert_eq!(decoded, proposal);
    pool.receive(w.vote(5, 0, 1), &w.evidence).unwrap();
    assert_eq!(
        pool.fast_certificate(rc.value)
            .unwrap()
            .verify(&fast, 0, &w.evidence),
        Ok(rc.value)
    );

    let outsider = (1..=6)
        .map(PrivateKey::from)
        .find(|k| !slow.members().contains(&k.public_key()))
        .unwrap();
    assert_eq!(
        slow.proposal(&fast, rc.clone(), &outsider, &w.evidence),
        Err(CheckpointError::NonMember)
    );
    let mut wrong = rc.clone();
    wrong.rank = 1;
    assert_eq!(
        slow.proposal(&fast, wrong, &key, &w.evidence),
        Err(CheckpointError::WrongRank)
    );
    let mut wrong = rc;
    wrong.value = BlockHash::from(2);
    assert_eq!(
        slow.proposal(&fast, wrong, &key, &w.evidence),
        Err(CheckpointError::InvalidRecovery)
    );
    let mut forged = proposal.clone();
    forged.recovery.snapshot[0].signature = Signature::default();
    assert_eq!(
        forged.verify(&slow, &fast, &w.evidence),
        Err(CheckpointError::BadSignature)
    );
    let mut forged = proposal;
    forged.instance = fast.instance().digest();
    assert_eq!(
        forged.verify(&slow, &fast, &w.evidence),
        Err(CheckpointError::WrongInstance)
    );
}

mod slow_certificates {
    use super::*;

    /// Exact fixture for already verified request ancestry. This deliberately
    /// rejects unknown requests; it is not the production recursive verifier.
    #[derive(Default)]
    struct Requests {
        requests: BTreeMap<BlockHash, (SlowPhase, u64)>,
        origins: BTreeMap<BlockHash, SlowValue>,
    }
    impl SlowRequestEvidence for Requests {
        fn validate_request(
            &self,
            _: &SlowContext,
            phase: SlowPhase,
            rank: u64,
            request: BlockHash,
        ) -> Result<(), CheckpointError> {
            match self.requests.get(&request) {
                Some(expected) if *expected == (phase, rank) => Ok(()),
                Some(_) => Err(CheckpointError::InvalidEvidence),
                None => Err(CheckpointError::MissingEvidence(request)),
            }
        }
        fn validate_origin(
            &self,
            context: &SlowContext,
            phase: SlowPhase,
            rank: u64,
            request: BlockHash,
            value: SlowValue,
        ) -> Result<(), CheckpointError> {
            self.validate_request(context, phase, rank, request)?;
            if self.origins.get(&request) != Some(&value) {
                return Err(CheckpointError::InvalidEvidence);
            }
            Ok(())
        }
    }

    struct Fixture {
        slow: SlowContext,
        keys: Vec<PrivateKey>,
        cert: SlowCertificate,
        evidence: Requests,
    }
    impl Fixture {
        fn new(phase: SlowPhase, rank: u64, values: Vec<Vec<SlowValue>>) -> Self {
            let w = World::new(1, 1);
            let slow = SlowContext::new(&w.context());
            let keys: Vec<_> = (1..=6)
                .map(PrivateKey::from)
                .filter(|k| slow.members().contains(&k.public_key()))
                .take(3)
                .collect();
            let request = BlockHash::from(50);
            let mut evidence = Requests::default();
            evidence.requests.insert(request, (phase, rank));
            let mut origins = BTreeMap::new();
            for value in values.iter().flatten() {
                let id = BlockHash::from(100 + origins.len() as u64);
                origins.entry(*value).or_insert(id);
            }
            let entries = |values: &[SlowValue]| -> Vec<SlowEntry> {
                values
                    .iter()
                    .map(|value| SlowEntry {
                        value: *value,
                        origin: origins[value],
                    })
                    .collect()
            };
            let responses = values
                .iter()
                .zip(&keys)
                .map(|(values, key)| {
                    SlowResponse::sign(&slow, rank, phase, request, entries(values), key).unwrap()
                })
                .collect();
            let witnesses = origins
                .iter()
                .map(|(value, origin)| {
                    let origin_rank = match value {
                        SlowValue::R { rank, .. } => *rank,
                        _ => rank,
                    };
                    evidence.requests.insert(*origin, (phase, origin_rank));
                    evidence.origins.insert(*origin, *value);
                    OriginWitnesses {
                        origin: *origin,
                        responses: keys
                            .iter()
                            .map(|key| {
                                SlowResponse::sign(
                                    &slow,
                                    origin_rank,
                                    phase,
                                    *origin,
                                    entries(&[*value]),
                                    key,
                                )
                                .unwrap()
                            })
                            .collect(),
                    }
                })
                .collect();
            let cert = SlowCertificate {
                instance: slow.instance(),
                rank,
                phase,
                request,
                responses,
                witnesses,
                retained_r: None,
            };
            Self {
                slow,
                keys,
                cert,
                evidence,
            }
        }
        fn verify(&self) -> Result<VerifiedSlowCertificate, CheckpointError> {
            self.cert.verify(&self.slow, &self.evidence)
        }
        fn resign(&mut self, index: usize) {
            let r = &self.cert.responses[index];
            self.cert.responses[index] = SlowResponse::sign(
                &self.slow,
                r.rank,
                r.phase,
                r.request,
                r.entries.clone(),
                &self.keys[index],
            )
            .unwrap();
        }
    }
    fn a(v: u64) -> SlowValue {
        SlowValue::A(BlockHash::from(v))
    }
    fn b(flag: bool, v: u64) -> SlowValue {
        SlowValue::B(BValue {
            flag,
            value: BlockHash::from(v),
        })
    }
    fn uniform(phase: SlowPhase, value: SlowValue) -> Fixture {
        Fixture::new(phase, 0, vec![vec![value]; 3])
    }

    #[test]
    fn r_selects_rank_before_value_and_a_follows_that_selection() {
        let f = Fixture::new(
            SlowPhase::R,
            0,
            vec![
                vec![SlowValue::R {
                    rank: 0,
                    value: BlockHash::from(9),
                }],
                vec![SlowValue::R {
                    rank: 2,
                    value: BlockHash::from(1),
                }],
                vec![SlowValue::R {
                    rank: 2,
                    value: BlockHash::from(3),
                }],
            ],
        );
        let verified = f.verify().unwrap();
        assert_eq!(
            verified.result(),
            SlowResult::R {
                rank: 2,
                value: BlockHash::from(3)
            }
        );
        assert_eq!(
            verified.next_action(),
            Ok(SlowAction::A {
                rank: 2,
                value: BlockHash::from(3)
            })
        );
        assert_eq!(verified.rank(), 0); // triggering R rank differs from selection
        let decoded: SlowCertificate =
            serde_json::from_slice(&serde_json::to_vec(&f.cert).unwrap()).unwrap();
        assert_eq!(decoded.verify(&f.slow, &f.evidence), Ok(verified));
    }

    #[test]
    fn a_and_b_transitions_distinguish_adoption_from_commit() {
        let f = Fixture::new(
            SlowPhase::A,
            4,
            vec![vec![a(1)], vec![a(1), a(2)], vec![a(2)]],
        );
        assert_eq!(
            f.verify().unwrap().next_action(),
            Ok(SlowAction::B {
                rank: 4,
                value: BValue {
                    flag: false,
                    value: BlockHash::from(2)
                }
            })
        );
        let f = Fixture::new(
            SlowPhase::B,
            4,
            vec![
                vec![b(false, 9)],
                vec![b(true, 1)],
                vec![b(false, 9), b(true, 1)],
            ],
        );
        assert_eq!(
            f.verify().unwrap().next_action(),
            Ok(SlowAction::R {
                rank: 5,
                value: BlockHash::from(1)
            })
        );
        let f = uniform(SlowPhase::B, b(true, 1));
        assert_eq!(
            f.verify().unwrap().next_action(),
            Ok(SlowAction::Decide(BlockHash::from(1)))
        );
        let f = Fixture::new(SlowPhase::B, u64::MAX, vec![vec![b(false, 1)]; 3]);
        assert_eq!(
            f.verify().unwrap().next_action(),
            Err(CheckpointError::WrongRank)
        );
    }

    #[test]
    fn weak_witnesses_and_duplicate_witnesses_never_make_a_quorum() {
        let mut f = uniform(SlowPhase::A, a(1));
        let removed = f.cert.witnesses[0].responses.pop().unwrap();
        assert_eq!(
            f.cert.witnesses[0].responses.len() as u64,
            f.slow.witness_threshold()
        );
        assert_eq!(f.verify(), Err(CheckpointError::InvalidSize));
        f.cert.witnesses[0].responses.push(removed);
        f.cert.witnesses[0].responses[1] = f.cert.witnesses[0].responses[0].clone();
        assert_eq!(f.verify(), Err(CheckpointError::DuplicateSigner));
        let mut f = uniform(SlowPhase::A, a(1));
        f.cert.responses[1] = f.cert.responses[0].clone();
        assert_eq!(f.verify(), Err(CheckpointError::DuplicateSigner));
    }

    #[test]
    fn every_carried_value_needs_matching_origin_and_strong_witnesses() {
        let mut f = Fixture::new(SlowPhase::A, 0, vec![vec![a(1), a(2)]; 3]);
        let removed = f.cert.witnesses.pop().unwrap();
        assert_eq!(
            f.verify(),
            Err(CheckpointError::MissingEvidence(removed.origin))
        );
        f.cert.witnesses.push(removed);
        f.cert.responses[0].entries[1].origin = f.cert.responses[0].entries[0].origin;
        f.resign(0);
        assert_eq!(f.verify(), Err(CheckpointError::InvalidEvidence));
        let mut f = uniform(SlowPhase::A, a(1));
        f.evidence.requests.remove(&f.cert.request);
        assert_eq!(
            f.verify(),
            Err(CheckpointError::MissingEvidence(f.cert.request))
        );
    }

    #[test]
    fn signed_fields_and_request_scopes_cannot_be_substituted() {
        let mut f = uniform(SlowPhase::A, a(1));
        let original = f.cert.responses[0].clone();
        for field in 0..5 {
            f.cert.responses[0] = original.clone();
            let response = &mut f.cert.responses[0];
            match field {
                0 => response.request = BlockHash::from(999),
                1 => response.rank += 1,
                2 => response.entries[0].origin = BlockHash::from(999),
                3 => response.entries[0].value = a(2),
                _ => {
                    response.phase = SlowPhase::B;
                    response.entries[0].value = b(false, 1);
                }
            }
            assert_eq!(f.verify(), Err(CheckpointError::BadSignature));
        }
        f.cert.responses[0] = original;
        f.cert.responses[0].request = BlockHash::from(999);
        f.resign(0); // authentic response to a different broadcast
        assert_eq!(f.verify(), Err(CheckpointError::InvalidEvidence));
        let mut f = uniform(SlowPhase::A, a(1));
        f.cert.witnesses[0].responses[0].request = f.cert.request;
        let r = &f.cert.witnesses[0].responses[0];
        f.cert.witnesses[0].responses[0] = SlowResponse::sign(
            &f.slow,
            r.rank,
            r.phase,
            r.request,
            r.entries.clone(),
            &f.keys[0],
        )
        .unwrap();
        assert_eq!(f.verify(), Err(CheckpointError::InvalidEvidence));
    }

    #[test]
    fn outsiders_cross_instance_replays_and_malformed_snapshots_are_rejected() {
        let mut f = uniform(SlowPhase::A, a(1));
        let outsider = (1..=6)
            .map(PrivateKey::from)
            .find(|k| !f.slow.members().contains(&k.public_key()))
            .unwrap();
        let r = &f.cert.responses[0];
        assert_eq!(
            SlowResponse::sign(
                &f.slow,
                0,
                SlowPhase::A,
                r.request,
                r.entries.clone(),
                &outsider
            ),
            Err(CheckpointError::NonMember)
        );
        f.cert.responses[0].signer = outsider.public_key();
        assert_eq!(f.verify(), Err(CheckpointError::NonMember));
        let mut f = uniform(SlowPhase::A, a(1));
        f.cert.instance = BlockHash::ZERO;
        assert_eq!(f.verify(), Err(CheckpointError::WrongInstance));
        let f = uniform(SlowPhase::A, a(1));
        let entry = f.cert.responses[0].entries[0].clone();
        assert_eq!(
            SlowResponse::sign(
                &f.slow,
                0,
                SlowPhase::A,
                f.cert.request,
                vec![entry.clone(), entry],
                &f.keys[0]
            ),
            Err(CheckpointError::InvalidEvidence)
        );
        assert_eq!(
            SlowResponse::sign(
                &f.slow,
                1,
                SlowPhase::R,
                f.cert.request,
                vec![SlowEntry {
                    value: SlowValue::R {
                        rank: 0,
                        value: BlockHash::from(1)
                    },
                    origin: BlockHash::from(100)
                }],
                &f.keys[0]
            ),
            Err(CheckpointError::InvalidEvidence)
        );
    }
}

mod recursive_slow {
    use super::*;
    fn limits() -> VerificationLimits {
        VerificationLimits::default()
    }
    struct Run {
        w: World,
        slow: SlowContext,
        keys: Vec<PrivateKey>,
        store: SlowProofStore,
        proposer: SlowProposer,
    }
    impl Run {
        fn new() -> Self {
            let w = World::new(1, 1);
            let slow = SlowContext::new(&w.context());
            let keys: Vec<_> = (1..=6)
                .map(PrivateKey::from)
                .filter(|k| slow.members().contains(&k.public_key()))
                .collect();
            let RecoveryProgress::Resolved(rc) = w
                .pool(0, &[(1, 1), (2, 1), (3, 1), (4, 1)])
                .resolve(None, &w.evidence)
                .unwrap()
            else {
                panic!()
            };
            let proposal = slow
                .proposal(&w.context(), rc, &keys[0], &w.evidence)
                .unwrap();
            let mut store = SlowProofStore::default();
            let proposer = SlowProposer::start(
                &slow,
                &w.context(),
                &w.evidence,
                proposal,
                &keys[0],
                &mut store,
            )
            .unwrap();
            Self {
                w,
                slow,
                keys,
                store,
                proposer,
            }
        }
        fn answers(
            &self,
            request: &SlowRequest,
            value: SlowValue,
            origin: BlockHash,
        ) -> Vec<SlowResponse> {
            self.keys
                .iter()
                .take(3)
                .map(|key| {
                    SlowResponse::sign(
                        &self.slow,
                        request.rank,
                        request.phase(),
                        request.digest(),
                        vec![SlowEntry { value, origin }],
                        key,
                    )
                    .unwrap()
                })
                .collect()
        }
        fn feed_current(&mut self) {
            let r = self.proposer.current_request();
            let answers = self.answers(r, r.value, r.digest());
            for answer in answers {
                self.proposer.receive(&self.slow, answer).unwrap();
            }
        }
        fn poll(&mut self) -> Result<ProposerProgress, CheckpointError> {
            self.proposer.poll(
                &self.slow,
                &self.w.context(),
                &self.w.evidence,
                &self.keys[0],
                &mut self.store,
                limits(),
            )
        }
        fn verify_request(&self, id: BlockHash) -> Result<(), CheckpointError> {
            self.store
                .verifier(&self.slow, &self.w.context(), &self.w.evidence, limits())
                .verify_request(id)
        }
        fn verify_certificate(
            &self,
            id: BlockHash,
        ) -> Result<VerifiedSlowCertificate, CheckpointError> {
            self.store
                .verifier(&self.slow, &self.w.context(), &self.w.evidence, limits())
                .verify_certificate(id)
        }
    }

    #[test]
    fn real_ancestry_drives_r_a_b_to_an_independently_verified_decision() {
        let mut run = Run::new();
        assert_eq!(run.poll(), Ok(ProposerProgress::Waiting));
        for phase in [SlowPhase::A, SlowPhase::B] {
            run.feed_current();
            let ProposerProgress::Broadcast(request) = run.poll().unwrap() else {
                panic!()
            };
            assert_eq!(request.phase(), phase);
            assert_eq!(run.verify_request(request.digest()), Ok(()));
            assert_eq!(run.poll(), Ok(ProposerProgress::Waiting)); // old responses cannot advance a new phase
        }
        run.feed_current();
        let decision = run.poll().unwrap();
        let ProposerProgress::Decided { value, certificate } = decision else {
            panic!()
        };
        assert_eq!(value, BlockHash::from(1));
        assert_eq!(
            run.verify_certificate(certificate).unwrap().result(),
            SlowResult::B(BOutcome::Commit(value))
        );
        assert_eq!(run.poll(), Ok(decision));
        let encoded = serde_json::to_vec(run.proposer.current_request()).unwrap();
        let decoded: SlowRequest = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.digest(), run.proposer.current_request().digest());
        // Proof verification is read-only and also available to nonmembers.
        assert_eq!(
            run.verify_certificate(certificate).unwrap().instance(),
            run.slow.instance()
        );
    }

    #[test]
    fn missing_dependencies_retry_and_wrong_value_rank_phase_are_rejected() {
        let mut run = Run::new();
        run.feed_current();
        let ProposerProgress::Broadcast(a) = run.poll().unwrap() else {
            panic!()
        };
        let SlowJustification::Previous(parent) = a.justification else {
            panic!()
        };
        let mut isolated = SlowProofStore::default();
        let id = isolated.insert_request(a.clone());
        assert_eq!(
            isolated
                .verifier(&run.slow, &run.w.context(), &run.w.evidence, limits())
                .verify_request(id),
            Err(CheckpointError::MissingEvidence(parent))
        );
        isolated.insert_certificate(run.store.certificate(parent).unwrap().clone());
        let root = run.store.certificate(parent).unwrap().request;
        assert_eq!(
            isolated
                .verifier(&run.slow, &run.w.context(), &run.w.evidence, limits())
                .verify_request(id),
            Err(CheckpointError::MissingEvidence(root))
        );
        isolated.insert_request(run.store.request(root).unwrap().clone());
        assert_eq!(
            isolated
                .verifier(&run.slow, &run.w.context(), &run.w.evidence, limits())
                .verify_request(id),
            Ok(())
        );
        for (rank, value) in [
            (0, SlowValue::A(BlockHash::from(2))),
            (1, a.value),
            (
                0,
                SlowValue::B(BValue {
                    flag: true,
                    value: BlockHash::from(1),
                }),
            ),
        ] {
            let bad = SlowRequest::sign(
                &run.slow,
                rank,
                value,
                SlowJustification::Previous(parent),
                &run.keys[0],
            )
            .unwrap();
            let id = run.store.insert_request(bad);
            assert_eq!(run.verify_request(id), Err(CheckpointError::WrongAncestry));
        }
        let mut tampered = a;
        tampered.justification = SlowJustification::Previous(BlockHash::from(77));
        let id = run.store.insert_request(tampered);
        assert_eq!(run.verify_request(id), Err(CheckpointError::BadSignature));
    }

    #[test]
    fn request_witness_validation_never_upgrades_a_weak_certificate_to_strong() {
        let mut run = Run::new();
        run.feed_current();
        let ProposerProgress::Broadcast(a) = run.poll().unwrap() else {
            panic!()
        };
        let SlowJustification::Previous(parent) = a.justification else {
            panic!()
        };
        let mut weak = run.store.certificate(parent).unwrap().clone();
        for witness in &mut weak.witnesses {
            witness.responses.truncate(2);
        }
        let weak_id = run.store.insert_certificate(weak.clone());
        let request = SlowRequest::sign(
            &run.slow,
            a.rank,
            a.value,
            SlowJustification::Previous(weak_id),
            &run.keys[0],
        )
        .unwrap();
        let id = run.store.insert_request(request);
        let fast = run.w.context();
        let verifier = run
            .store
            .verifier(&run.slow, &fast, &run.w.evidence, limits());
        assert_eq!(verifier.verify_request(id), Ok(()));
        assert_eq!(
            verifier.verify_certificate(weak_id),
            Err(CheckpointError::InvalidSize)
        );
        weak.witnesses[0].responses.truncate(1);
        let insufficient = run.store.insert_certificate(weak);
        let request = SlowRequest::sign(
            &run.slow,
            a.rank,
            a.value,
            SlowJustification::Previous(insufficient),
            &run.keys[0],
        )
        .unwrap();
        let id = run.store.insert_request(request);
        assert_eq!(run.verify_request(id), Err(CheckpointError::InvalidSize));
    }

    #[test]
    fn recursive_validation_is_bounded_and_does_not_cache_failed_checks() {
        let mut run = Run::new();
        run.feed_current();
        let ProposerProgress::Broadcast(a) = run.poll().unwrap() else {
            panic!()
        };
        for limits in [
            VerificationLimits {
                max_depth: 1,
                max_objects: 100,
            },
            VerificationLimits {
                max_depth: 100,
                max_objects: 1,
            },
        ] {
            assert_eq!(
                run.store
                    .verifier(&run.slow, &run.w.context(), &run.w.evidence, limits)
                    .verify_request(a.digest()),
                Err(CheckpointError::VerificationLimit)
            );
        }
        assert_eq!(run.verify_request(a.digest()), Ok(()));
        run.w.evidence.values.clear();
        assert!(matches!(
            run.verify_request(a.digest()),
            Err(CheckpointError::MissingEvidence(_))
        ));
    }

    #[test]
    fn proposer_retains_local_r_maximum_outside_the_first_response_quorum() {
        let mut run = Run::new();
        // Another legitimate RC0 permits a different initial value; no FC exists.
        let RecoveryProgress::Resolved(rc) = run
            .w
            .pool(0, &[(1, 2), (5, 2), (6, 2), (2, 1)])
            .resolve(None, &run.w.evidence)
            .unwrap()
        else {
            panic!()
        };
        let proposal = run
            .slow
            .proposal(&run.w.context(), rc, &run.keys[1], &run.w.evidence)
            .unwrap();
        let other = SlowRequest::sign(
            &run.slow,
            0,
            SlowValue::R {
                rank: 0,
                value: BlockHash::from(2),
            },
            SlowJustification::Initial(proposal),
            &run.keys[1],
        )
        .unwrap();
        let id = run.store.insert_request(other.clone());
        run.proposer
            .observe_request(
                &run.slow,
                &run.w.context(),
                &run.w.evidence,
                &run.store,
                id,
                limits(),
            )
            .unwrap();
        run.feed_current();
        assert_eq!(run.poll(), Ok(ProposerProgress::Waiting)); // retained maximum needs its witnesses
        for answer in run.answers(&other, other.value, id) {
            run.proposer.receive(&run.slow, answer).unwrap();
        }
        let ProposerProgress::Broadcast(a) = run.poll().unwrap() else {
            panic!()
        };
        assert_eq!(a.value, SlowValue::A(BlockHash::from(2)));
        assert_eq!(run.verify_request(a.digest()), Ok(()));
        let SlowJustification::Previous(parent) = a.justification else {
            panic!()
        };
        assert!(run.store.certificate(parent).unwrap().retained_r.is_some());
    }

    #[test]
    fn proposer_keeps_eligible_history_when_a_later_snapshot_lacks_evidence() {
        let mut run = Run::new();
        run.feed_current();
        let request = run.proposer.current_request().clone();
        let incomplete = SlowResponse::sign(
            &run.slow,
            0,
            SlowPhase::R,
            request.digest(),
            vec![SlowEntry {
                value: SlowValue::R {
                    rank: 1,
                    value: BlockHash::from(9),
                },
                origin: BlockHash::from(999),
            }],
            &run.keys[0],
        )
        .unwrap();
        run.proposer.receive(&run.slow, incomplete).unwrap();
        assert!(matches!(run.poll(), Ok(ProposerProgress::Broadcast(_))));
    }
    #[test]
    fn proposer_adopts_then_runs_the_next_rank_with_real_b_ancestry() {
        let mut run = Run::new();
        run.feed_current();
        let ProposerProgress::Broadcast(a1) = run.poll().unwrap() else {
            panic!()
        };
        let RecoveryProgress::Resolved(rc) = run
            .w
            .pool(0, &[(1, 2), (5, 2), (6, 2), (2, 1)])
            .resolve(None, &run.w.evidence)
            .unwrap()
        else {
            panic!()
        };
        let proposal = run
            .slow
            .proposal(&run.w.context(), rc, &run.keys[1], &run.w.evidence)
            .unwrap();
        let r2 = SlowRequest::sign(
            &run.slow,
            0,
            SlowValue::R {
                rank: 0,
                value: BlockHash::from(2),
            },
            SlowJustification::Initial(proposal),
            &run.keys[1],
        )
        .unwrap();
        let r2_id = run.store.insert_request(r2.clone());
        let answers = run.answers(&r2, r2.value, r2_id);
        let cert = SlowCertificate {
            instance: run.slow.instance(),
            rank: 0,
            phase: SlowPhase::R,
            request: r2_id,
            responses: answers.clone(),
            retained_r: None,
            witnesses: vec![OriginWitnesses {
                origin: r2_id,
                responses: answers,
            }],
        };
        let cert_id = run.store.insert_certificate(cert);
        let a2 = SlowRequest::sign(
            &run.slow,
            0,
            SlowValue::A(BlockHash::from(2)),
            SlowJustification::Previous(cert_id),
            &run.keys[1],
        )
        .unwrap();
        let a2_id = run.store.insert_request(a2.clone());
        assert_eq!(run.verify_request(a2_id), Ok(()));
        for answer in run.answers(&a2, a2.value, a2_id) {
            run.proposer.receive(&run.slow, answer).unwrap();
        }
        for key in run.keys.iter().take(3) {
            let answer = SlowResponse::sign(
                &run.slow,
                0,
                SlowPhase::A,
                a1.digest(),
                vec![
                    SlowEntry {
                        value: a1.value,
                        origin: a1.digest(),
                    },
                    SlowEntry {
                        value: a2.value,
                        origin: a2_id,
                    },
                ],
                key,
            )
            .unwrap();
            run.proposer.receive(&run.slow, answer).unwrap();
        }
        let ProposerProgress::Broadcast(b) = run.poll().unwrap() else {
            panic!()
        };
        assert_eq!(
            b.value,
            SlowValue::B(BValue {
                flag: false,
                value: BlockHash::from(2)
            })
        );
        run.feed_current();
        let ProposerProgress::Broadcast(next) = run.poll().unwrap() else {
            panic!()
        };
        assert_eq!(next.rank, 1);
        assert_eq!(
            next.value,
            SlowValue::R {
                rank: 1,
                value: BlockHash::from(2)
            }
        );
        assert_eq!(run.verify_request(next.digest()), Ok(()));
        for _ in 0..2 {
            run.feed_current();
            assert!(matches!(run.poll(), Ok(ProposerProgress::Broadcast(_))));
        }
        run.feed_current();
        assert!(
            matches!(run.poll(),Ok(ProposerProgress::Decided {value,..}) if value == BlockHash::from(2))
        );
    }
}

mod slow_network {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn observer_fetches_complete_ancestry_without_casting_slow_votes() {
        let mut net = Network::new(false, false);
        net.start();
        net.drain();
        net.assert_agreement();
        let (value, certificate) = net.replicas[0].decision().unwrap();
        let mut invalid = net.replicas[0]
            .proof_store()
            .certificate(certificate)
            .unwrap()
            .clone();
        invalid.responses.clear();
        let bad_id = invalid.digest();
        let mut rejected =
            CheckpointObserver::new(net.w.context(), VerificationLimits::default(), 1 << 20)
                .unwrap();
        rejected
            .receive(SlowMessage::Certificate(invalid), &net.w.evidence)
            .unwrap();
        assert!(
            rejected
                .receive(SlowMessage::Decision(bad_id), &net.w.evidence)
                .unwrap()
                .iter()
                .any(|effect| matches!(effect, SlowEffect::Rejected { .. }))
        );
        assert_eq!(rejected.decision(), None);

        let mut observer =
            CheckpointObserver::new(net.w.context(), VerificationLimits::default(), 1 << 20)
                .unwrap();
        let first = observer
            .receive(SlowMessage::Decision(certificate), &net.w.evidence)
            .unwrap();
        assert_eq!(observer.decision(), None);
        assert_eq!(
            first,
            vec![SlowEffect::Broadcast(SlowMessage::Fetch(certificate))]
        );
        assert!(
            observer
                .receive(SlowMessage::Decision(certificate), &net.w.evidence)
                .unwrap()
                .is_empty()
        );
        // Drop the first fetch; explicit retry reopens it.
        let mut queue: VecDeque<_> = observer.retry(&net.w.evidence).into();
        let mut decisions = 0;
        let mut fetches = 0;
        while let Some(effect) = queue.pop_front() {
            match effect {
                SlowEffect::Broadcast(SlowMessage::Fetch(id)) => {
                    fetches += 1;
                    assert!(fetches < 1000);
                    for reply in net.replicas[0]
                        .receive(SlowMessage::Fetch(id), &net.w.evidence)
                        .unwrap()
                    {
                        if let SlowEffect::Reply(message) = reply {
                            queue.extend(observer.receive(message, &net.w.evidence).unwrap());
                        }
                    }
                }
                SlowEffect::Decided {
                    value: v,
                    certificate: c,
                } => {
                    assert_eq!((v, c), (value, certificate));
                    decisions += 1;
                }
                SlowEffect::Broadcast(SlowMessage::Decision(id)) => assert_eq!(id, certificate),
                other => panic!("observer must not vote: {other:?}"),
            }
        }
        assert!(fetches > 1);
        assert_eq!(decisions, 1);
        assert_eq!(observer.decision(), Some((value, certificate)));
        assert!(
            observer
                .receive(SlowMessage::Decision(certificate), &net.w.evidence)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            &observer
                .receive(SlowMessage::Fetch(certificate), &net.w.evidence)
                .unwrap()[0],
            SlowEffect::Reply(SlowMessage::Certificate(_))
        ));
    }

    struct Network {
        w: World,
        replicas: Vec<SlowReplica>,
        transports: Vec<CheckpointTransport<usize>>,
        queue: VecDeque<(usize, usize, SlowMessage)>,
        active: Vec<bool>,
        decisions: Vec<usize>,
        requests: usize,
        responses: usize,
        fetches: usize,
        deliveries: usize,
        drop_certificate_broadcasts: bool,
        drop_response_broadcasts: bool,
    }
    impl Network {
        fn new(split: bool, suspended: bool) -> Self {
            let w = World::new(1, 1);
            let slow = SlowContext::new(&w.context());
            let keys: Vec<_> = (1..=6)
                .map(PrivateKey::from)
                .filter(|k| slow.members().contains(&k.public_key()))
                .collect();
            let RecoveryProgress::Resolved(v) = w
                .pool(0, &[(1, 1), (2, 1), (3, 1), (4, 1)])
                .resolve(None, &w.evidence)
                .unwrap()
            else {
                panic!()
            };
            let RecoveryProgress::Resolved(u) = w
                .pool(0, &[(1, 2), (5, 2), (6, 2), (2, 1)])
                .resolve(None, &w.evidence)
                .unwrap()
            else {
                panic!()
            };
            let replicas = keys
                .into_iter()
                .enumerate()
                .map(|(i, key)| {
                    let proposal = slow
                        .proposal(
                            &w.context(),
                            if split && i % 2 == 1 {
                                u.clone()
                            } else {
                                v.clone()
                            },
                            &key,
                            &w.evidence,
                        )
                        .unwrap();
                    SlowReplica::new(
                        w.context(),
                        key,
                        proposal,
                        &w.evidence,
                        VerificationLimits::default(),
                    )
                    .unwrap()
                })
                .collect();
            let transports = (0..4)
                .map(|_| {
                    CheckpointTransport::new(
                        &w.context(),
                        SlowIoLimits::default().max_message_bytes,
                    )
                    .unwrap()
                })
                .collect();
            Self {
                w,
                replicas,
                transports,
                queue: VecDeque::new(),
                active: vec![true, true, true, !suspended],
                decisions: vec![0; 4],
                requests: 0,
                responses: 0,
                fetches: 0,
                deliveries: 0,
                drop_certificate_broadcasts: false,
                drop_response_broadcasts: false,
            }
        }
        fn effects(&mut self, from: usize, peer: usize, effects: Vec<SlowEffect>) {
            for effect in effects {
                match effect {
                    SlowEffect::Broadcast(message) => {
                        match &message {
                            SlowMessage::Fetch(_) => self.fetches += 1,
                            SlowMessage::Request(_) => self.requests += 1,
                            SlowMessage::Response(_) => self.responses += 1,
                            _ => {}
                        }
                        if self.drop_response_broadcasts
                            && matches!(message, SlowMessage::Response(_))
                        {
                            continue;
                        }
                        if self.drop_certificate_broadcasts
                            && matches!(message, SlowMessage::Certificate(_))
                        {
                            continue;
                        }
                        for to in 0..4 {
                            if to != from && self.active[to] {
                                self.queue.push_back((from, to, message.clone()));
                            }
                        }
                    }
                    SlowEffect::Reply(message) => {
                        if self.active[peer] {
                            self.queue.push_back((from, peer, message));
                        }
                    }
                    SlowEffect::Decided { value, .. } => {
                        assert!(value == BlockHash::from(1) || value == BlockHash::from(2));
                        self.decisions[from] += 1;
                    }
                    SlowEffect::Rejected { object, reason } => {
                        panic!("honest transcript rejected {object:?}: {reason:?}")
                    }
                }
            }
        }
        fn start(&mut self) {
            for i in 0..4 {
                if self.active[i] {
                    let effects = self.replicas[i].retry(&self.w.evidence).unwrap();
                    self.effects(i, i, effects);
                }
            }
        }
        fn drain(&mut self) {
            while let Some((from, to, message)) = self.queue.pop_front() {
                self.deliveries += 1;
                assert!(
                    self.deliveries < 30_000,
                    "message exchange failed to quiesce; decisions {:?}",
                    self.decisions
                );
                let frames = self.transports[from]
                    .encode(&CheckpointMessage::Slow(message.clone()))
                    .unwrap();
                let mut assembled = None;
                for frame in frames.into_iter().rev() {
                    let network = rsnano_messages::Message::CheckpointFrame(frame);
                    let mut serializer = rsnano_messages::MessageSerializer::default();
                    let mut decoder = rsnano_messages::MessageDeserializer::new(Default::default());
                    decoder.push(serializer.serialize(&network));
                    let rsnano_messages::Message::CheckpointFrame(received) =
                        decoder.try_deserialize().unwrap().unwrap().message
                    else {
                        panic!()
                    };
                    if let Some(message) = self.transports[to].receive(from, &received).unwrap() {
                        assembled = Some(message);
                    }
                }
                let Some(CheckpointMessage::Slow(decoded)) = assembled else {
                    panic!()
                };
                assert_eq!(decoded, message);
                let effects = self.replicas[to]
                    .receive(decoded, &self.w.evidence)
                    .unwrap();
                self.effects(to, from, effects);
            }
        }
        fn assert_agreement(&self) {
            let values: BTreeSet<_> = self
                .replicas
                .iter()
                .enumerate()
                .filter(|(i, _)| self.active[*i])
                .map(|(_, r)| r.decision().expect("replica must decide").0)
                .collect();
            assert_eq!(values.len(), 1);
            for i in 0..4 {
                if self.active[i] {
                    assert_eq!(self.decisions[i], 1);
                }
            }
        }
    }

    #[test]
    fn independent_replicas_fetch_missing_proofs_and_decide() {
        let mut net = Network::new(false, false);
        net.drop_certificate_broadcasts = true; // requests arrive without preceding proof objects
        net.start();
        net.drain();
        net.assert_agreement();
        assert!(net.fetches > 0);
        assert!(net.requests >= 12);
        assert!(net.responses >= 36);
        for replica in &net.replicas {
            let (_, certificate) = replica.decision().unwrap();
            let fast = net.w.context();
            let slow = SlowContext::new(&fast);
            assert!(matches!(
                replica
                    .proof_store()
                    .verifier(&slow, &fast, &net.w.evidence, VerificationLimits::default())
                    .verify_certificate(certificate)
                    .unwrap()
                    .result(),
                SlowResult::B(BOutcome::Commit(_))
            ));
        }
    }

    #[test]
    fn split_inputs_exchange_real_mutable_responses_and_converge() {
        let mut net = Network::new(true, false);
        net.start();
        net.drain();
        net.assert_agreement();
    }

    #[test]
    fn decided_replicas_serve_a_returning_replica_and_relay_its_decision_proof() {
        let mut net = Network::new(false, true);
        net.start();
        net.drain();
        net.assert_agreement();
        assert!(net.replicas[3].decision().is_none());
        let certificate = net.replicas[0].decision().unwrap().1;
        let before = net.responses;
        net.active[3] = true;
        let effects = net.replicas[3].retry(&net.w.evidence).unwrap();
        net.effects(3, 3, effects);
        net.queue
            .push_front((0, 3, SlowMessage::Decision(certificate)));
        net.drain();
        net.assert_agreement();
        assert!(net.responses > before);
    }

    #[test]
    fn invalid_request_cannot_raise_responder_r_state() {
        let net = Network::new(false, false);
        let fast = net.w.context();
        let slow = SlowContext::new(&fast);
        let key = (1..=6)
            .map(PrivateKey::from)
            .find(|k| slow.members().contains(&k.public_key()))
            .unwrap();
        let root = net.replicas[0].current_request().clone();
        let bad = SlowRequest::sign(
            &slow,
            0,
            SlowValue::R {
                rank: 0,
                value: BlockHash::from(9),
            },
            root.justification.clone(),
            &key,
        )
        .unwrap();
        let mut store = SlowProofStore::default();
        let bad_id = store.insert_request(bad);
        let root_id = store.insert_request(root.clone());
        let mut responder = SlowResponder::new(&slow, key.public_key()).unwrap();
        assert_eq!(
            responder.deliver(
                &slow,
                &fast,
                &net.w.evidence,
                &store,
                bad_id,
                &key,
                VerificationLimits::default()
            ),
            Err(CheckpointError::WrongAncestry)
        );
        let response = responder
            .deliver(
                &slow,
                &fast,
                &net.w.evidence,
                &store,
                root_id,
                &key,
                VerificationLimits::default(),
            )
            .unwrap();
        assert_eq!(
            response.entries,
            vec![SlowEntry {
                value: root.value,
                origin: root_id
            }]
        );
    }

    #[test]
    fn missing_response_origins_are_fetched_again_on_retry() {
        let mut net = Network::new(false, false);
        let fast = net.w.context();
        let slow = SlowContext::new(&fast);
        let key = (1..=6)
            .map(PrivateKey::from)
            .find(|k| slow.members().contains(&k.public_key()))
            .unwrap();
        let missing = BlockHash::from(909);
        let response = SlowResponse::sign(
            &slow,
            0,
            SlowPhase::R,
            missing,
            vec![SlowEntry {
                value: SlowValue::R {
                    rank: 0,
                    value: BlockHash::from(1),
                },
                origin: missing,
            }],
            &key,
        )
        .unwrap();
        let first = net.replicas[0]
            .receive(SlowMessage::Response(response), &net.w.evidence)
            .unwrap();
        let unrelated = net.replicas[0]
            .receive(SlowMessage::Fetch(BlockHash::from(910)), &net.w.evidence)
            .unwrap();
        assert!(!unrelated.contains(&SlowEffect::Broadcast(SlowMessage::Fetch(missing))));
        let retry = net.replicas[0].retry(&net.w.evidence).unwrap();
        for effects in [first, retry] {
            assert!(effects.contains(&SlowEffect::Broadcast(SlowMessage::Fetch(missing))));
        }
        assert!(net.replicas[0].decision().is_none());
    }
    #[test]
    fn verification_budget_does_not_lose_already_committed_outgoing_effects() {
        let net = Network::new(false, false);
        let fast = net.w.context();
        let slow = SlowContext::new(&fast);
        let mut keys = (1..=6)
            .map(PrivateKey::from)
            .filter(|k| slow.members().contains(&k.public_key()));
        let key = keys.next().unwrap();
        let SlowJustification::Initial(proposal) =
            net.replicas[0].current_request().justification.clone()
        else {
            panic!()
        };
        let mut replica = SlowReplica::new(
            fast,
            key,
            proposal,
            &net.w.evidence,
            VerificationLimits {
                max_depth: 1,
                max_objects: 4096,
            },
        )
        .unwrap();
        replica.retry(&net.w.evidence).unwrap();
        let root = replica.current_request().clone();
        let mut effects = Vec::new();
        for key in keys.take(2) {
            let response = SlowResponse::sign(
                &slow,
                0,
                SlowPhase::R,
                root.digest(),
                vec![SlowEntry {
                    value: root.value,
                    origin: root.digest(),
                }],
                &key,
            )
            .unwrap();
            effects.extend(
                replica
                    .receive(SlowMessage::Response(response), &net.w.evidence)
                    .unwrap(),
            );
        }
        assert_eq!(replica.current_request().phase(), SlowPhase::A);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, SlowEffect::Broadcast(SlowMessage::Certificate(_))))
        );
        assert!(effects.iter().any(|e| matches!(
            e,
            SlowEffect::Rejected {
                reason: CheckpointError::VerificationLimit,
                ..
            }
        )));
        replica
            .set_verification_limits(VerificationLimits::default())
            .unwrap();
        let effects = replica.retry(&net.w.evidence).unwrap();
        assert!(effects.iter().any(|e|matches!(e,SlowEffect::Broadcast(SlowMessage::Request(r)) if r.phase()==SlowPhase::A)));
    }
    #[test]
    fn explicit_retry_recovers_when_all_initial_outgoing_messages_are_lost() {
        let mut net = Network::new(false, false);
        let initial: Vec<_> = net
            .replicas
            .iter()
            .map(|r| r.current_request().clone())
            .collect();
        for replica in &mut net.replicas {
            let _lost = replica.retry(&net.w.evidence).unwrap();
        }
        net.start();
        net.drain();
        net.assert_agreement();
        for (replica, root) in net.replicas.iter().zip(initial) {
            assert_eq!(replica.proof_store().request(root.digest()).unwrap(), &root);
        }
    }
    #[test]
    fn one_response_retry_batches_recover_a_lost_response_round() {
        let mut net = Network::new(false, false);
        net.drop_response_broadcasts = true;
        net.start();
        net.drain();
        assert!(net.replicas.iter().all(|r| r.decision().is_none()));
        for replica in &mut net.replicas {
            replica
                .set_io_limits(SlowIoLimits {
                    max_retry_responses: 1,
                    ..Default::default()
                })
                .unwrap();
        }
        net.drop_response_broadcasts = false;
        for _ in 0..16 {
            net.start();
            net.drain();
            if net.replicas.iter().all(|r| r.decision().is_some()) {
                break;
            }
        }
        net.assert_agreement();
    }

    #[test]
    fn oversized_messages_are_rejected_before_entering_the_proof_store() {
        let mut net = Network::new(false, false);
        let root = net.replicas[0].current_request().clone();
        let bound = SlowMessage::Request(root.clone())
            .encoded_len(usize::MAX)
            .unwrap();
        net.replicas[0]
            .set_io_limits(SlowIoLimits {
                max_message_bytes: bound,
                max_retry_response_bytes: bound,
                max_retry_responses: 1,
            })
            .unwrap();
        let slow = SlowContext::new(&net.w.context());
        let key = (1..=6)
            .map(PrivateKey::from)
            .find(|k| slow.members().contains(&k.public_key()))
            .unwrap();
        let response = SlowResponse::sign(
            &slow,
            0,
            SlowPhase::R,
            root.digest(),
            vec![SlowEntry {
                value: root.value,
                origin: root.digest(),
            }],
            &key,
        )
        .unwrap();
        let oversized = SlowCertificate {
            instance: slow.instance(),
            rank: 0,
            phase: SlowPhase::R,
            request: root.digest(),
            responses: vec![response; 100],
            retained_r: None,
            witnesses: vec![],
        };
        let id = oversized.digest();
        assert_eq!(
            net.replicas[0].receive(SlowMessage::Certificate(oversized), &net.w.evidence),
            Err(CheckpointError::InvalidSize)
        );
        assert_eq!(
            net.replicas[0].proof_store().certificate(id),
            Err(CheckpointError::MissingEvidence(id))
        );
        assert_eq!(net.replicas[0].current_request(), &root);
        assert!(net.replicas[0].decision().is_none());
    }
}

mod slow_io_tests {
    use super::super::slow_io::ResponseReplay;
    use super::*;
    fn response(i: u64) -> SlowResponse {
        let w = World::new(1, 1);
        let slow = SlowContext::new(&w.context());
        let key = (1..=6)
            .map(PrivateKey::from)
            .find(|k| slow.members().contains(&k.public_key()))
            .unwrap();
        SlowResponse::sign(
            &slow,
            0,
            SlowPhase::R,
            BlockHash::from(i),
            vec![SlowEntry {
                value: SlowValue::R {
                    rank: 0,
                    value: BlockHash::from(1),
                },
                origin: BlockHash::from(i),
            }],
            &key,
        )
        .unwrap()
    }
    fn ids(batch: Vec<SlowMessage>) -> Vec<BlockHash> {
        batch
            .into_iter()
            .map(|m| match m {
                SlowMessage::Response(r) => r.digest(),
                _ => panic!(),
            })
            .collect()
    }
    #[test]
    fn encoded_size_matches_json_and_enforces_the_exact_boundary() {
        let message = SlowMessage::Response(response(1));
        let encoded = serde_json::to_vec(&message).unwrap();
        assert_eq!(message.encoded_len(encoded.len()), Ok(encoded.len()));
        assert_eq!(
            message.encoded_len(encoded.len() - 1),
            Err(CheckpointError::InvalidSize)
        );
        assert_eq!(message.encoded_len(0), Err(CheckpointError::InvalidSize));
        let decoded: SlowMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, message);
    }
    #[test]
    fn bounded_batches_rotate_without_new_arrivals_overtaking_old_evidence() {
        let mut replay = ResponseReplay::default();
        let mut history = BTreeMap::new();
        let mut expected = Vec::new();
        for i in 1..=3 {
            let r = response(i);
            let id = r.digest();
            expected.push(id);
            history.insert(id, r);
            replay.remember(id);
        }
        let budget = SlowIoLimits {
            max_retry_responses: 1,
            ..Default::default()
        };
        assert_eq!(
            ids(replay.batch(&history, budget).unwrap()),
            vec![expected[0]]
        );
        let r = response(4);
        let new = r.digest();
        history.insert(new, r);
        replay.remember(new);
        assert_eq!(
            ids(replay.batch(&history, budget).unwrap()),
            vec![expected[1]]
        );
        assert_eq!(
            ids(replay.batch(&history, budget).unwrap()),
            vec![expected[2]]
        );
        assert_eq!(
            ids(replay.batch(&history, budget).unwrap()),
            vec![expected[0]]
        );
        assert_eq!(ids(replay.batch(&history, budget).unwrap()), vec![new]);
        let all = ids(replay.batch(&history, SlowIoLimits::default()).unwrap());
        assert_eq!(all.len(), 4);
        assert_eq!(all.into_iter().collect::<BTreeSet<_>>().len(), 4);
    }
    #[test]
    fn byte_budget_is_enforced_and_failed_batches_do_not_advance_the_cursor() {
        let mut replay = ResponseReplay::default();
        let mut history = BTreeMap::new();
        let mut expected = Vec::new();
        for i in 1..=3 {
            let r = response(i);
            let id = r.digest();
            expected.push(id);
            history.insert(id, r);
            replay.remember(id);
        }
        let size = SlowMessage::Response(history[&expected[0]].clone())
            .encoded_len(usize::MAX)
            .unwrap();
        let budget = SlowIoLimits {
            max_message_bytes: size,
            max_retry_responses: 8,
            max_retry_response_bytes: 2 * size - 1,
        };
        let bad = SlowIoLimits {
            max_message_bytes: size - 1,
            ..budget
        };
        assert_eq!(
            replay.batch(&history, bad),
            Err(CheckpointError::InvalidSize)
        );
        assert_eq!(
            ids(replay.batch(&history, budget).unwrap()),
            vec![expected[0]]
        );
        let batch = replay
            .batch(
                &history,
                SlowIoLimits {
                    max_retry_response_bytes: 2 * size,
                    ..budget
                },
            )
            .unwrap();
        let total: usize = batch
            .iter()
            .map(|m| m.encoded_len(usize::MAX).unwrap())
            .sum();
        assert_eq!(total, 2 * size);
        assert_eq!(ids(batch), vec![expected[1], expected[2]]);
        assert_eq!(history.len(), 3);
    }
}

#[test]
fn fast_wire_network_roundtrips_and_verifies_all_kinds() {
    let w = World::new(1, 1);
    let pool = w.pool(0, &[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
    let RecoveryProgress::Resolved(rc) = pool.resolve(None, &w.evidence).unwrap() else {
        panic!()
    };
    let codec = FastWireCodec::new(w.context().instance(), 1024 * 1024).unwrap();
    for message in [
        FastMessage::First(w.vote(1, 0, 1)),
        FastMessage::Certificate(pool.fast_certificate(BlockHash::from(1)).unwrap()),
        FastMessage::Recovery(rc),
    ] {
        let bytes = codec.encode(&message).unwrap();
        let frames = frame_slow_payload(&bytes, 1024 * 1024 + 44).unwrap();
        let mut assembly = SlowFrameAssembler::new(1024 * 1024 + 44).unwrap();
        let mut result = None;
        for frame in frames {
            let network = rsnano_messages::Message::CheckpointFrame(
                rsnano_messages::CheckpointFrame::new(frame).unwrap(),
            );
            let mut serializer = rsnano_messages::MessageSerializer::default();
            let mut decoder = rsnano_messages::MessageDeserializer::new(Default::default());
            decoder.push(serializer.serialize(&network));
            let rsnano_messages::Message::CheckpointFrame(frame) =
                decoder.try_deserialize().unwrap().unwrap().message
            else {
                panic!()
            };
            if let Some(bytes) = assembly.receive(1, frame.as_bytes()).unwrap() {
                result = Some(codec.decode(&bytes).unwrap());
            }
        }
        let decoded = result.unwrap();
        assert_eq!(decoded, message);
        match decoded {
            FastMessage::First(v) => {
                w.context().verify_first_vote(0, &v, &w.evidence).unwrap();
            }
            FastMessage::Certificate(c) => {
                c.verify(&w.context(), 0, &w.evidence).unwrap();
            }
            FastMessage::Recovery(c) => {
                c.verify(&w.context(), 0, &w.evidence).unwrap();
            }
        }
    }
}

#[test]
fn fast_wire_rejects_replay_bad_lengths_and_cross_namespace() {
    let w = World::new(1, 1);
    let codec = FastWireCodec::new(w.context().instance(), 4096).unwrap();
    let vote = w.vote(1, 0, 1);
    let message = FastMessage::First(vote.clone());
    let bytes = codec.encode(&message).unwrap();
    for end in 0..bytes.len() {
        assert!(codec.decode(&bytes[..end]).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(codec.decode(&extra).is_err());
    let mut other = w.context().instance();
    other.session = BlockHash::from(900);
    assert_eq!(
        FastWireCodec::new(other, 4096).unwrap().decode(&bytes),
        Err(CheckpointError::WrongInstance)
    );
    assert!(
        FastWireCodec::new(w.context().instance(), bytes.len() - 45)
            .unwrap()
            .encode(&message)
            .is_err()
    );
    let slow = SlowWireCodec::new(w.context().instance().digest(), 4096).unwrap();
    assert!(slow.decode(&bytes).is_err());
    assert!(
        codec
            .decode(
                &slow
                    .encode(&SlowMessage::Fetch(BlockHash::from(1)))
                    .unwrap()
            )
            .is_err()
    );
    let mut rank = vote.clone();
    rank.rank = 1;
    assert_eq!(
        codec.encode(&FastMessage::First(rank)),
        Err(CheckpointError::WrongRank)
    );
    // Bypass encode to exercise hostile nested body validation on decode.
    let invalid = FastMessage::Certificate(FastCertificate {
        instance: w.context().instance(),
        rank: 0,
        value: vote.value,
        votes: vec![FirstVote {
            instance: other,
            ..vote.clone()
        }],
    });
    let body = serde_json::to_vec(&invalid).unwrap();
    let mut forged = bytes[..44].to_vec();
    forged[40..44].copy_from_slice(&(body.len() as u32).to_le_bytes());
    forged.extend(body);
    assert_eq!(codec.decode(&forged), Err(CheckpointError::WrongInstance));
    // Decoding alone never authenticates a signature.
    let mut forged_vote = vote;
    forged_vote.signature = Signature::default();
    let FastMessage::First(decoded) = codec
        .decode(&codec.encode(&FastMessage::First(forged_vote)).unwrap())
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        w.context().verify_first_vote(0, &decoded, &w.evidence),
        Err(CheckpointError::BadSignature)
    );
}

#[test]
fn checkpoint_transport_routes_and_rejects_foreign_or_unknown_payloads() {
    let w = World::new(1, 1);
    let mut transport = CheckpointTransport::new(&w.context(), 1 << 20).unwrap();
    let messages = [
        CheckpointMessage::Fast(FastMessage::First(w.vote(1, 0, 1))),
        CheckpointMessage::Slow(SlowMessage::Fetch(BlockHash::from(123))),
    ];
    for message in messages {
        for _ in 0..2 {
            let frames = transport.encode(&message).unwrap();
            assert_eq!(frames.len(), 1);
            assert_eq!(
                transport.receive(1, &frames[0]).unwrap(),
                Some(message.clone())
            );
        }
    }
    let mut foreign = w.context().instance();
    foreign.session = BlockHash::from(987);
    let mut vote = w.vote(1, 0, 1);
    vote.instance = foreign;
    let bytes = FastWireCodec::new(foreign, 4096)
        .unwrap()
        .encode(&FastMessage::First(vote))
        .unwrap();
    let frame =
        rsnano_messages::CheckpointFrame::new(frame_slow_payload(&bytes, 4096).unwrap().remove(0))
            .unwrap();
    assert_eq!(
        transport.receive(1, &frame),
        Err(CheckpointError::WrongInstance)
    );
    let frame = rsnano_messages::CheckpointFrame::new(
        frame_slow_payload(b"unknown namespace", 4096)
            .unwrap()
            .remove(0),
    )
    .unwrap();
    assert_eq!(
        transport.receive(1, &frame),
        Err(CheckpointError::InvalidEvidence)
    );
    assert!(CheckpointTransport::<usize>::new(&w.context(), usize::MAX).is_err());
}

#[test]
fn checkpoint_transport_interleaves_large_proofs_and_clears_connections() {
    let w = World::new(1, 1);
    let mut transport = CheckpointTransport::new(&w.context(), 1 << 20).unwrap();
    // Oversized quorum is structurally encodable, but must fail consensus
    // verification. Transport cannot be mistaken for a certificate verifier.
    let certificate = FastCertificate {
        instance: w.context().instance(),
        rank: 0,
        value: BlockHash::from(1),
        votes: vec![w.vote(1, 0, 1); 200],
    };
    assert_eq!(
        certificate.verify(&w.context(), 0, &w.evidence),
        Err(CheckpointError::InvalidSize)
    );
    let message = CheckpointMessage::Fast(FastMessage::Certificate(certificate));
    let frames = transport.encode(&message).unwrap();
    assert!(frames.len() > 1);
    assert_eq!(transport.receive(1, &frames[0]).unwrap(), None);
    transport.forget_connection(&1);
    for frame in frames.iter().skip(1).rev() {
        assert_eq!(transport.receive(1, frame).unwrap(), None);
    }
    let small = CheckpointMessage::Slow(SlowMessage::Decision(BlockHash::from(7)));
    let small_frame = transport.encode(&small).unwrap();
    assert_eq!(transport.receive(1, &small_frame[0]).unwrap(), Some(small));
    // Another connection cannot complete the first connection's assembly.
    assert_eq!(transport.receive(2, &frames[0]).unwrap(), None);
    assert_eq!(transport.receive(1, &frames[0]).unwrap(), Some(message));
    transport.forget_connection(&2);
}

#[test]
fn participant_starts_fallback_then_accepts_late_fast_and_keeps_serving() {
    let w = World::new(1, 1);
    let key = (1..=6)
        .map(PrivateKey::from)
        .find(|k| {
            SlowContext::new(&w.context())
                .members()
                .contains(&k.public_key())
        })
        .unwrap();
    let mut participant = CheckpointParticipant::new(w.context(), key).unwrap();
    for signer in 1..=4 {
        participant
            .receive_fast(FastMessage::First(w.vote(signer, 0, 1)), &w.evidence)
            .unwrap();
    }
    assert!(participant.fallback_started());
    assert_eq!(participant.decision(), None);
    let effects = participant
        .receive_fast(FastMessage::First(w.vote(5, 0, 1)), &w.evidence)
        .unwrap();
    assert_eq!(participant.decision(), Some(BlockHash::from(1)));
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, ParticipantEffect::Decided(_)))
            .count(),
        1
    );
    assert!(
        !participant
            .receive_fast(FastMessage::First(w.vote(5, 0, 1)), &w.evidence)
            .unwrap()
            .iter()
            .any(|e| matches!(e, ParticipantEffect::Decided(_)))
    );
    let retry = participant.retry(&w.evidence).unwrap();
    assert!(retry.iter().any(|e| matches!(
        e,
        ParticipantEffect::Slow(SlowEffect::Broadcast(SlowMessage::Request(_)))
    )));
    assert!(
        retry
            .iter()
            .any(|e| matches!(e, ParticipantEffect::Fast(FastMessage::Certificate(_))))
    );
}

#[test]
fn participant_slow_decision_and_post_decision_proof_service() {
    let w = World::new(1, 1);
    let keys: Vec<_> = (1..=6)
        .map(PrivateKey::from)
        .filter(|k| {
            SlowContext::new(&w.context())
                .members()
                .contains(&k.public_key())
        })
        .collect();
    let mut participants: Vec<_> = keys
        .into_iter()
        .map(|key| CheckpointParticipant::new(w.context(), key).unwrap())
        .collect();
    let RecoveryProgress::Resolved(rc) = w
        .pool(0, &[(1, 1), (2, 1), (3, 1), (4, 1)])
        .resolve(None, &w.evidence)
        .unwrap()
    else {
        panic!()
    };
    let mut queue = std::collections::VecDeque::new();
    for (i, participant) in participants.iter_mut().enumerate() {
        for effect in participant.admit_recovery(rc.clone(), &w.evidence).unwrap() {
            queue.push_back((i, i, effect));
        }
    }
    let mut decisions = [0; 4];
    let mut deliveries = 0;
    while let Some((from, peer, effect)) = queue.pop_front() {
        deliveries += 1;
        assert!(deliveries < 30000);
        match effect {
            ParticipantEffect::Decided(value) => {
                assert_eq!(value, rc.value);
                decisions[from] += 1;
            }
            ParticipantEffect::Fast(_) => {}
            ParticipantEffect::Slow(SlowEffect::Broadcast(message)) => {
                for (to, participant) in participants.iter_mut().enumerate() {
                    if to == from {
                        continue;
                    }
                    for effect in participant
                        .receive_slow(message.clone(), &w.evidence)
                        .unwrap()
                    {
                        queue.push_back((to, from, effect));
                    }
                }
            }
            ParticipantEffect::Slow(SlowEffect::Reply(message)) => {
                for effect in participants[peer]
                    .receive_slow(message, &w.evidence)
                    .unwrap()
                {
                    queue.push_back((peer, from, effect));
                }
            }
            ParticipantEffect::Slow(SlowEffect::Rejected { reason, .. }) => panic!("{reason:?}"),
            ParticipantEffect::Slow(SlowEffect::Decided { .. }) => {}
        }
    }
    assert_eq!(decisions, [1; 4]);
    for participant in &mut participants {
        assert_eq!(participant.decision(), Some(rc.value));
        assert!(
            participant
                .retry(&w.evidence)
                .unwrap()
                .iter()
                .any(|e| matches!(
                    e,
                    ParticipantEffect::Slow(SlowEffect::Broadcast(SlowMessage::Decision(_)))
                ))
        );
    }
}

#[test]
fn drivers_select_roles_buffer_early_fetch_and_reach_slow_agreement() {
    let w = World::new(1, 1);
    let mut drivers: Vec<CheckpointDriver<usize>> = (1..=6)
        .map(|i| CheckpointDriver::new(w.context(), PrivateKey::from(i)).unwrap())
        .collect();
    assert_eq!(
        drivers.iter().filter(|d| d.is_slow_participant()).count(),
        4
    );
    let selected = drivers
        .iter()
        .position(|d| d.is_slow_participant())
        .unwrap();
    let RecoveryProgress::Resolved(rc) = w
        .pool(0, &[(1, 1), (2, 1), (3, 1), (4, 1)])
        .resolve(None, &w.evidence)
        .unwrap()
    else {
        panic!()
    };
    let key = PrivateKey::from((selected + 1) as u64);
    let slow = SlowContext::new(&w.context());
    let proposal = slow
        .proposal(&w.context(), rc.clone(), &key, &w.evidence)
        .unwrap();
    let mut store = SlowProofStore::default();
    let proposer =
        SlowProposer::start(&slow, &w.context(), &w.evidence, proposal, &key, &mut store).unwrap();
    let request = proposer.current_request().digest();
    let early = CheckpointMessage::Slow(SlowMessage::Fetch(request));
    assert!(
        drivers[selected]
            .receive(99, early.clone(), &w.evidence)
            .unwrap()
            .is_empty()
    );
    drivers[selected].receive(99, early, &w.evidence).unwrap();
    assert_eq!(drivers[selected].pending_early(), 1);
    let mut queue = std::collections::VecDeque::new();
    let mut addressed_reply = false;
    for (i, driver) in drivers.iter_mut().enumerate() {
        for effect in driver
            .receive(
                0,
                CheckpointMessage::Fast(FastMessage::Recovery(rc.clone())),
                &w.evidence,
            )
            .unwrap()
        {
            if let DriverEffect::Reply(99, CheckpointMessage::Slow(SlowMessage::Request(r))) =
                &effect
            {
                assert_eq!(r.digest(), request);
                addressed_reply = true;
            } else {
                queue.push_back((i, effect));
            }
        }
    }
    assert!(addressed_reply);
    assert_eq!(drivers[selected].pending_early(), 0);
    let mut decisions = [0; 6];
    let mut count = 0;
    while let Some((from, effect)) = queue.pop_front() {
        count += 1;
        assert!(count < 50000);
        match effect {
            DriverEffect::Broadcast(message) => {
                for (to, driver) in drivers.iter_mut().enumerate() {
                    if to != from {
                        for effect in driver.receive(from, message.clone(), &w.evidence).unwrap() {
                            queue.push_back((to, effect));
                        }
                    }
                }
            }
            DriverEffect::Reply(to, message) => {
                for effect in drivers[to].receive(from, message, &w.evidence).unwrap() {
                    queue.push_back((to, effect));
                }
            }
            DriverEffect::Decided(value) => {
                assert_eq!(value, rc.value);
                decisions[from] += 1;
            }
            DriverEffect::Rejected(error) => panic!("{error:?}"),
        }
    }
    assert_eq!(decisions, [1; 6]);
    assert!(drivers.iter().all(|d| d.decision() == Some(rc.value)));
    // A late matching fast proof cannot notify any role twice.
    let fc = w
        .pool(0, &[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)])
        .fast_certificate(rc.value)
        .unwrap();
    for driver in &mut drivers {
        assert!(
            !driver
                .receive(
                    0,
                    CheckpointMessage::Fast(FastMessage::Certificate(fc.clone())),
                    &w.evidence
                )
                .unwrap()
                .iter()
                .any(|e| matches!(e, DriverEffect::Decided(_)))
        );
    }
}

#[test]
fn driver_local_first_release_is_journaled_and_retransmitted() {
    let w = World::new(1, 1);
    let mut journal = VolatileFirstVoteJournal::default();
    for signer in 1..=6 {
        let mut driver: CheckpointDriver<usize> =
            CheckpointDriver::new(w.context(), PrivateKey::from(signer)).unwrap();
        let expected = w.vote(signer, 0, 1);
        for value in [1, 2] {
            let effects = driver
                .propose(
                    BlockHash::from(value),
                    World::intro(0, value),
                    &mut journal,
                    &w.evidence,
                )
                .unwrap();
            assert!(
                effects.contains(&DriverEffect::Broadcast(CheckpointMessage::Fast(
                    FastMessage::First(expected.clone())
                )))
            );
        }
        assert!(
            driver
                .retry(&w.evidence)
                .unwrap()
                .contains(&DriverEffect::Broadcast(CheckpointMessage::Fast(
                    FastMessage::First(expected)
                )))
        );
    }
}

#[test]
fn application_candidates_require_reconstructed_build_state_and_authenticated_introduction() {
    use crate::consensus::election::{
        CertifiedState, EpochLedger, EpochValue, ReportIndex, ReportRef, ReportSource,
        ResidualVotes, SelectedReport,
    };
    struct Source {
        keys: Vec<PublicKey>,
        certified: CertifiedState,
        residual: ResidualVotes,
    }
    impl ReportSource for Source {
        fn report(&self, reference: &ReportRef) -> Option<SelectedReport<'_>> {
            self.keys
                .contains(&reference.reporter)
                .then_some(SelectedReport {
                    reporter: reference.reporter,
                    weight: Amount::raw(1),
                    certified: &self.certified,
                    residual: &self.residual,
                })
        }
    }
    let source = Source {
        keys: (1..=6).map(|i| PrivateKey::from(i).public_key()).collect(),
        certified: CertifiedState::new(),
        residual: ResidualVotes::new(),
    };
    let previous = EpochLedger::new();
    let committee = Committee::equal_weight(source.keys.clone(), 1, 1).unwrap();
    let instance = CheckpointInstance {
        session: BlockHash::from(10),
        epoch: ConsensusEpoch::ZERO,
        predecessor: previous.state_hash(),
        committee: committee.digest(),
    };
    let context = CheckpointContext::new(instance, committee).unwrap();
    let mut refs: Vec<_> = source
        .keys
        .iter()
        .take(5)
        .map(|reporter| ReportRef {
            reporter: *reporter,
            certified: source.certified.root(),
            residual: source.residual.root(),
        })
        .collect();
    refs.sort();
    let states: Vec<_> = refs.iter().map(|r| source.report(r).unwrap()).collect();
    let index = ReportIndex::new(&previous, &states);
    let selection: Vec<_> = refs.iter().copied().zip(states.iter().copied()).collect();
    let (value, _) = EpochValue::propose(
        instance.epoch,
        &previous,
        &selection,
        &index,
        Amount::raw(3),
    );
    let signed = CheckpointCandidate::sign(&context, value.clone(), &PrivateKey::from(1)).unwrap();
    let decoded: CheckpointCandidate =
        serde_json::from_slice(&serde_json::to_vec(&signed).unwrap()).unwrap();
    assert_eq!(decoded, signed);
    let validated = signed
        .clone()
        .validate(&context, &previous, &source, &index)
        .unwrap();
    assert_eq!(validated.state().state_hash(), value.state);
    let mut evidence = CandidateEvidence::new(instance);
    assert!(evidence.validate_value(&instance, value.hash()).is_err());
    let mut journal = CandidateEndorsementJournal::default();
    let endorsements: Vec<_> = (1..=context.thresholds().q)
        .map(|i| {
            journal
                .endorse(&validated, &context, &PrivateKey::from(i))
                .unwrap()
        })
        .collect();
    let admission = CandidateAdmission { endorsements };
    admission.verify(&signed, &context).unwrap();
    let mut duplicate = admission.clone();
    duplicate.endorsements[1] = duplicate.endorsements[0].clone();
    assert_eq!(
        duplicate.verify(&signed, &context),
        Err(CheckpointError::DuplicateSigner)
    );
    // A second valid report selection from the same proposer cannot replace any
    // previously endorsed initial candidate.
    let mut alternate_refs: Vec<_> = source
        .keys
        .iter()
        .skip(1)
        .map(|reporter| ReportRef {
            reporter: *reporter,
            certified: source.certified.root(),
            residual: source.residual.root(),
        })
        .collect();
    alternate_refs.sort();
    let alternate = CheckpointCandidate::sign(
        &context,
        EpochValue::from_parts(instance.epoch, alternate_refs, value.state),
        &PrivateKey::from(1),
    )
    .unwrap();
    let alternate_valid = alternate
        .clone()
        .validate(&context, &previous, &source, &index)
        .unwrap();
    assert_eq!(
        journal
            .endorse(&alternate_valid, &context, &PrivateKey::from(1))
            .unwrap(),
        admission.endorsements[0]
    );
    assert_eq!(
        admission.verify(&alternate, &context),
        Err(CheckpointError::InvalidEvidence)
    );
    let id = evidence.insert(validated, &admission, &context).unwrap();
    evidence
        .validate_introduction(&instance, 0, value.hash(), &Introduction::Proposal(id))
        .unwrap();
    assert!(
        evidence
            .validate_introduction(
                &instance,
                0,
                BlockHash::from(999),
                &Introduction::Proposal(id)
            )
            .is_err()
    );
    assert!(evidence.r_maximum(&instance, 0, id).is_err());
    let request = InitialRRequest::sign(&context, id, &PrivateKey::from(1), &evidence).unwrap();
    let responses: Vec<_> = (1..=context.thresholds().q)
        .map(|i| {
            InitialRResponder::new(instance)
                .receive(&request, &context, &evidence, &PrivateKey::from(i))
                .unwrap()
        })
        .collect();
    let certificate = InitialRCertificate { request, responses };
    let mut duplicate = certificate.clone();
    duplicate.responses[1] = duplicate.responses[0].clone();
    assert_eq!(
        duplicate.verify(&context, &evidence),
        Err(CheckpointError::DuplicateSigner)
    );
    let mut forged = certificate.clone();
    forged.responses[0].candidate = BlockHash::from(1234);
    assert_eq!(
        forged.verify(&context, &evidence),
        Err(CheckpointError::BadSignature)
    );
    let rid = evidence.insert_initial_r(certificate, &context).unwrap();
    assert_eq!(evidence.r_maximum(&instance, 0, rid).unwrap(), value.hash());
    assert!(evidence.r_maximum(&instance, 1, rid).is_err());
    let other =
        CheckpointCandidate::sign(&context, alternate.value.clone(), &PrivateKey::from(2)).unwrap();
    let validated_other = other
        .validate(&context, &previous, &source, &index)
        .unwrap();
    let other_admission = CandidateAdmission {
        endorsements: (1..=context.thresholds().q)
            .map(|i| {
                journal
                    .endorse(&validated_other, &context, &PrivateKey::from(i))
                    .unwrap()
            })
            .collect(),
    };
    let other_id = evidence
        .insert(validated_other, &other_admission, &context)
        .unwrap();
    let (low, high) =
        if evidence.admitted_value(id).unwrap() < evidence.admitted_value(other_id).unwrap() {
            (id, other_id)
        } else {
            (other_id, id)
        };
    let low_request =
        InitialRRequest::sign(&context, low, &PrivateKey::from(1), &evidence).unwrap();
    let high_request =
        InitialRRequest::sign(&context, high, &PrivateKey::from(2), &evidence).unwrap();
    let responses = (1..=context.thresholds().q)
        .map(|i| {
            let key = PrivateKey::from(i);
            let mut responder = InitialRResponder::new(instance);
            responder
                .receive(&high_request, &context, &evidence, &key)
                .unwrap();
            let response = responder
                .receive(&low_request, &context, &evidence, &key)
                .unwrap();
            assert_eq!(response.candidate, high);
            response
        })
        .collect();
    let max_proof = InitialRCertificate {
        request: low_request,
        responses,
    };
    assert_eq!(
        max_proof.verify(&context, &evidence).unwrap(),
        evidence.admitted_value(high).unwrap()
    );

    // Independent stores learn candidates, admissions and initial-R evidence
    // exclusively through application messages over the Nano frame transport.
    let mut exchanges: Vec<_> = (1..=6)
        .map(|i| {
            ApplicationExchange::new(
                CheckpointContext::new(instance, context.committee.clone()).unwrap(),
                PrivateKey::from(i),
                1 << 20,
            )
            .unwrap()
        })
        .collect();
    let mut journals: Vec<_> = (0..6)
        .map(|_| CandidateEndorsementJournal::default())
        .collect();
    let mut transports: Vec<CheckpointTransport<usize>> = (0..6)
        .map(|_| CheckpointTransport::new(&context, 1 << 20).unwrap())
        .collect();
    let mut validate =
        |candidate: CheckpointCandidate| candidate.validate(&context, &previous, &source, &index);
    let mut queue = std::collections::VecDeque::new();
    // An endorsement arrives ahead of its candidate and generates a fetch.
    for effect in exchanges[5]
        .receive(
            ApplicationMessage::Endorsement(admission.endorsements[0].clone()),
            &mut journals[5],
            &mut validate,
        )
        .unwrap()
    {
        queue.push_back((5, 0, effect));
    }
    for effect in exchanges[0]
        .receive(
            ApplicationMessage::Candidate(signed.clone()),
            &mut journals[0],
            &mut validate,
        )
        .unwrap()
    {
        queue.push_back((0, 0, effect));
    }
    queue.push_back((
        0,
        0,
        ApplicationEffect::Broadcast(ApplicationMessage::Candidate(signed.clone())),
    ));
    let mut admissions = [0; 6];
    let mut ready = [0; 6];
    let mut fetched = 0;
    for stage in 0..2 {
        if stage == 1 {
            let request =
                InitialRRequest::sign(&context, id, &PrivateKey::from(1), exchanges[0].evidence())
                    .unwrap();
            // Lose the initial transmission and recover it through explicit retry.
            exchanges[0]
                .receive(
                    ApplicationMessage::InitialRequest(request),
                    &mut journals[0],
                    &mut validate,
                )
                .unwrap();
            for effect in exchanges[0].retry(&mut journals[0], &mut validate) {
                queue.push_back((0, 0, effect));
            }
        }
        let mut count = 0;
        while let Some((from, peer, effect)) = queue.pop_front() {
            count += 1;
            assert!(count < 50000);
            let destinations = match effect {
                ApplicationEffect::Broadcast(message) => {
                    if matches!(message, ApplicationMessage::Fetch(_)) {
                        fetched += 1;
                    }
                    (0..6)
                        .filter(|to| *to != from)
                        .map(|to| (to, message.clone()))
                        .collect::<Vec<_>>()
                }
                ApplicationEffect::Reply(message) => vec![(peer, message)],
                ApplicationEffect::Admitted(value) => {
                    assert_eq!(value, id);
                    admissions[from] += 1;
                    Vec::new()
                }
                ApplicationEffect::InitialReady(_) => {
                    ready[from] += 1;
                    Vec::new()
                }
                ApplicationEffect::MissingEvidence(_) => Vec::new(),
                ApplicationEffect::Rejected(error) => panic!("{error:?}"),
            };
            for (to, message) in destinations {
                let frames = transports[from]
                    .encode(&CheckpointMessage::Application(message))
                    .unwrap();
                for frame in frames {
                    let mut serializer = rsnano_messages::MessageSerializer::default();
                    let mut decoder = rsnano_messages::MessageDeserializer::new(Default::default());
                    decoder.push(
                        serializer.serialize(&rsnano_messages::Message::CheckpointFrame(frame)),
                    );
                    let rsnano_messages::Message::CheckpointFrame(frame) =
                        decoder.try_deserialize().unwrap().unwrap().message
                    else {
                        panic!()
                    };
                    if let Some(CheckpointMessage::Application(message)) =
                        transports[to].receive(from, &frame).unwrap()
                    {
                        for effect in exchanges[to]
                            .receive(message, &mut journals[to], &mut validate)
                            .unwrap()
                        {
                            queue.push_back((to, from, effect));
                        }
                    }
                }
            }
        }
    }
    assert!(fetched > 0);
    assert_eq!(admissions, [1; 6]);
    assert!(ready.iter().all(|n| *n > 0));
    assert!(
        exchanges
            .iter()
            .all(|e| e.evidence().admitted_value(id).unwrap() == value.hash())
    );

    // A decision certificate received before its application proof must park,
    // request that proof, and decide automatically after the reply arrives.
    let observer_key = (1..=6)
        .map(PrivateKey::from)
        .find(|k| {
            !SlowContext::new(&context)
                .members()
                .contains(&k.public_key())
        })
        .unwrap();
    let mut observer_session: CheckpointSession<usize> = CheckpointSession::new(
        CheckpointContext::new(instance, context.committee.clone()).unwrap(),
        observer_key,
    )
    .unwrap();
    let mut observer_journals = CheckpointSessionJournals::default();
    let mut first_journal = VolatileFirstVoteJournal::default();
    let votes = (1..=context.thresholds().f_fast)
        .map(|i| {
            context
                .first_vote(
                    0,
                    value.hash(),
                    Introduction::Proposal(id),
                    &PrivateKey::from(i),
                    &evidence,
                    &mut first_journal,
                )
                .unwrap()
        })
        .collect();
    let fc = FastCertificate {
        instance,
        rank: 0,
        value: value.hash(),
        votes,
    };
    let effects = observer_session
        .receive(
            0,
            CheckpointMessage::Fast(FastMessage::Certificate(fc)),
            &mut observer_journals,
            &mut validate,
        )
        .unwrap();
    assert_eq!(observer_session.decision(), None);
    let mut completed = 0;
    for effect in effects {
        if let DriverEffect::Broadcast(CheckpointMessage::Application(ApplicationMessage::Fetch(
            missing,
        ))) = effect
        {
            for reply in exchanges[0]
                .receive(
                    ApplicationMessage::Fetch(missing),
                    &mut journals[0],
                    &mut validate,
                )
                .unwrap()
            {
                if let ApplicationEffect::Reply(message) = reply {
                    for output in observer_session
                        .receive(
                            0,
                            CheckpointMessage::Application(message),
                            &mut observer_journals,
                            &mut |candidate: CheckpointCandidate| {
                                Err(CheckpointError::MissingEvidence(candidate.digest()))
                            },
                        )
                        .unwrap()
                    {
                        if let DriverEffect::Decided(v) = output {
                            assert_eq!(v, value.hash());
                            completed += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(completed, 0);
    assert_eq!(observer_session.decision(), None);
    // Reconstructing reports alone must unblock the parked admission and FC;
    // no new network message or retransmission timer is necessary.
    for output in observer_session.evidence_updated(&mut observer_journals, &mut validate) {
        if let DriverEffect::Decided(v) = output {
            assert_eq!(v, value.hash());
            completed += 1;
        }
    }
    assert_eq!(completed, 1);
    assert!(
        observer_session
            .evidence_updated(&mut observer_journals, &mut validate)
            .is_empty()
    );
    assert_eq!(observer_session.decision(), Some(value.hash()));

    // Exercise the concrete node interface through independent adapters and
    // framed messages, including repeated candidate ticks and decision polling.
    {
        use crate::consensus::reports::CheckpointElection;
        let mut adapters: Vec<_> = (1..=6)
            .map(|i| {
                CheckpointElectionAdapter::<usize, _>::new(
                    CheckpointContext::new(instance, context.committee.clone()).unwrap(),
                    PrivateKey::from(i),
                    CheckpointSessionJournals::default(),
                    |candidate: CheckpointCandidate| {
                        candidate.validate(&context, &previous, &source, &index)
                    },
                    16 * 1024 * 1024,
                )
                .unwrap()
            })
            .collect();
        let mut work = std::collections::VecDeque::new();
        for (i, adapter) in adapters.iter_mut().enumerate() {
            adapter.candidate(value.clone());
            work.extend(adapter.drain_effects().map(|e| (i, e)));
            adapter.candidate(value.clone());
            assert_eq!(adapter.drain_effects().count(), 0);
        }
        let mut steps = 0;
        while let Some((from, effect)) = work.pop_front() {
            steps += 1;
            assert!(steps < 100_000);
            let destinations = match effect {
                DriverEffect::Broadcast(message) => (0..6)
                    .filter(|i| *i != from)
                    .map(|i| (i, message.clone()))
                    .collect::<Vec<_>>(),
                DriverEffect::Reply(to, message) => vec![(to, message)],
                DriverEffect::Decided(v) => {
                    assert_eq!(v, value.hash());
                    Vec::new()
                }
                DriverEffect::Rejected(error) => panic!("adapter rejected: {error:?}"),
            };
            for (to, message) in destinations {
                for frame in adapters[from].encode(&message).unwrap() {
                    adapters[to].receive(from, &frame).unwrap();
                    work.extend(adapters[to].drain_effects().map(|e| (to, e)));
                }
            }
        }
        for adapter in &mut adapters {
            let decision = adapter.decided().expect("adapter decision");
            assert!(adapter.verify(&decision));
            assert_eq!(decision.value.hash(), value.hash());
            assert!(adapter.decided().is_none());
            adapter.evidence_updated();
            assert_eq!(adapter.drain_effects().count(), 0);
        }
        // Recreating an adapter retains process-lifetime proposal locks.
        let journals = adapters.pop().unwrap().into_journals();
        let mut recreated = CheckpointElectionAdapter::<usize, _>::new(
            CheckpointContext::new(instance, context.committee.clone()).unwrap(),
            PrivateKey::from(6),
            journals,
            |candidate: CheckpointCandidate| {
                candidate.validate(&context, &previous, &source, &index)
            },
            16 * 1024 * 1024,
        )
        .unwrap();
        recreated.candidate(alternate.value.clone());
        assert!(recreated.drain_effects().any(|effect| matches!(effect,
            DriverEffect::Broadcast(CheckpointMessage::Application(ApplicationMessage::Candidate(c)))
                if c.value.hash() == value.hash())));
    }

    // Complete candidate admission -> journaled FIRST -> fast/slow decision,
    // with independent session stores and actual wire routing at every hop.
    for split in [false, true] {
        let mut sessions: Vec<CheckpointSession<usize>> = (1..=6)
            .map(|i| {
                CheckpointSession::new(
                    CheckpointContext::new(instance, context.committee.clone()).unwrap(),
                    PrivateKey::from(i),
                )
                .unwrap()
            })
            .collect();
        let mut session_journals: Vec<_> = (0..6)
            .map(|_| CheckpointSessionJournals::default())
            .collect();
        let mut session_transports: Vec<CheckpointTransport<usize>> = (0..6)
            .map(|_| CheckpointTransport::new(&context, 1 << 20).unwrap())
            .collect();
        let mut queue = std::collections::VecDeque::new();
        let mut decisions = [0; 6];
        let mut fast_proofs = 0;
        for i in 0..6 {
            let input = if split && i >= 3 {
                alternate.value.clone()
            } else {
                value.clone()
            };
            let lost = sessions[i]
                .propose(input, &mut session_journals[i], &mut validate)
                .unwrap();
            assert!(!lost.iter().any(|e| matches!(
                e,
                DriverEffect::Broadcast(CheckpointMessage::Fast(FastMessage::First(_)))
            )));
            // Lose all initial output, including the candidate broadcast.
        }
        for i in 0..6 {
            for effect in sessions[i].retry(&mut session_journals[i], &mut validate) {
                queue.push_back((i, effect));
            }
        }
        let mut deliveries = 0;
        while let Some((from, effect)) = queue.pop_front() {
            deliveries += 1;
            assert!(deliveries < 200000, "session effects did not quiesce");
            let destinations = match effect {
                DriverEffect::Broadcast(message) => {
                    if matches!(
                        message,
                        CheckpointMessage::Fast(FastMessage::Certificate(_))
                    ) {
                        fast_proofs += 1;
                    }
                    (0..6)
                        .filter(|to| *to != from)
                        .map(|to| (to, message.clone()))
                        .collect::<Vec<_>>()
                }
                DriverEffect::Reply(peer, message) => vec![(peer, message)],
                DriverEffect::Decided(v) => {
                    assert!(v == value.hash() || v == alternate.value.hash());
                    decisions[from] += 1;
                    Vec::new()
                }
                DriverEffect::Rejected(error) => panic!("session rejected {error:?}"),
            };
            for (to, message) in destinations {
                for frame in session_transports[from].encode(&message).unwrap() {
                    let mut serializer = rsnano_messages::MessageSerializer::default();
                    let mut decoder = rsnano_messages::MessageDeserializer::new(Default::default());
                    decoder.push(
                        serializer.serialize(&rsnano_messages::Message::CheckpointFrame(frame)),
                    );
                    let rsnano_messages::Message::CheckpointFrame(frame) =
                        decoder.try_deserialize().unwrap().unwrap().message
                    else {
                        panic!()
                    };
                    if let Some(message) = session_transports[to].receive(from, &frame).unwrap() {
                        for effect in sessions[to]
                            .receive(from, message, &mut session_journals[to], &mut validate)
                            .unwrap()
                        {
                            queue.push_back((to, effect));
                        }
                    }
                }
            }
        }
        assert_eq!(decisions, [1; 6]);
        assert!(
            sessions
                .iter()
                .all(|s| s.decision() == sessions[0].decision())
        );
        if !split {
            assert!(fast_proofs > 0);
        } else {
            assert_eq!(fast_proofs, 0);
        }
        let limits = VerificationLimits::default();
        let decision = sessions[0]
            .checkpoint_decision(limits, 16 << 20)
            .unwrap()
            .unwrap();
        assert_eq!(decision.value.hash(), sessions[0].decision().unwrap());
        for session in &sessions {
            session
                .verify_decision(&decision, limits, 16 << 20)
                .unwrap();
        }
        let proof = CheckpointDecisionProof::decode(&decision.proof, 16 << 20).unwrap();
        assert_eq!(proof.encode(decision.proof.len()).unwrap(), decision.proof);
        assert!(proof.encode(decision.proof.len() - 1).is_err());
        let mut altered_decision = crate::consensus::reports::CheckpointDecision {
            value: decision.value.clone(),
            proof: decision.proof.clone(),
        };
        altered_decision.value.state = BlockHash::from(999);
        assert!(
            sessions[0]
                .verify_decision(&altered_decision, limits, 16 << 20)
                .is_err()
        );
        altered_decision.value = decision.value.clone();
        altered_decision.proof[7] ^= 1;
        assert!(
            sessions[0]
                .verify_decision(&altered_decision, limits, 16 << 20)
                .is_err()
        );
        if let CheckpointDecisionProof::Slow(mut bundle) = proof {
            assert!(
                bundle
                    .verify(
                        &context,
                        sessions[0].evidence(),
                        VerificationLimits {
                            max_depth: 1,
                            max_objects: 1
                        }
                    )
                    .is_err()
            );
            let mut store = SlowProofStore::default();
            for request in &bundle.requests {
                store.insert_request(request.clone());
            }
            for certificate in &bundle.certificates {
                store.insert_certificate(certificate.clone());
            }
            let request = store
                .request(store.certificate(bundle.root).unwrap().request)
                .unwrap();
            let SlowJustification::Previous(previous) = request.justification else {
                panic!()
            };
            let noncommit = SlowDecisionProof::collect(&store, previous, 4096).unwrap();
            assert_eq!(
                noncommit.verify(&context, sessions[0].evidence(), limits),
                Err(CheckpointError::InvalidEvidence)
            );
            let duplicate = bundle.requests[0].clone();
            bundle.requests.push(duplicate);
            assert_eq!(
                bundle.verify(&context, sessions[0].evidence(), limits),
                Err(CheckpointError::InvalidEvidence)
            );
            bundle.requests.pop();
            bundle.requests.pop();
            assert!(
                bundle
                    .verify(&context, sessions[0].evidence(), limits)
                    .is_err()
            );
        } else {
            assert!(!split);
        }

        // Recreating a session with process-lifetime journals preserves the
        // first candidate even when the caller's new preference changes.
        let mut recreated: CheckpointSession<usize> = CheckpointSession::new(
            CheckpointContext::new(instance, context.committee.clone()).unwrap(),
            PrivateKey::from(1),
        )
        .unwrap();
        let effects = recreated
            .propose(
                alternate.value.clone(),
                &mut session_journals[0],
                &mut validate,
            )
            .unwrap();
        assert!(effects.iter().any(|e|matches!(e,DriverEffect::Broadcast(CheckpointMessage::Application(ApplicationMessage::Candidate(c))) if c.value==value)));
    }

    let mut altered = signed.clone();
    altered.value.state = BlockHash::from(99);
    assert_eq!(
        altered.authenticate(&context),
        Err(CheckpointError::BadSignature)
    );
    let bad = CheckpointCandidate::sign(&context, altered.value, &PrivateKey::from(1)).unwrap();
    assert!(matches!(
        bad.validate(&context, &previous, &source, &index),
        Err(CheckpointError::InvalidValue)
    ));
    let missing = Source {
        keys: Vec::new(),
        certified: CertifiedState::new(),
        residual: ResidualVotes::new(),
    };
    assert!(matches!(
        signed
            .clone()
            .validate(&context, &previous, &missing, &index),
        Err(CheckpointError::MissingEvidence(_))
    ));
    let mut wrong = signed;
    wrong.instance.session = BlockHash::from(11);
    assert_eq!(
        wrong.authenticate(&context),
        Err(CheckpointError::WrongInstance)
    );
}

#[test]
fn application_wire_bounds_namespace_and_nested_instance_checks() {
    let w = World::new(1, 1);
    let instance = w.context().instance();
    let codec = ApplicationWireCodec::new(instance, 4096).unwrap();
    let message = ApplicationMessage::Fetch(BlockHash::from(11));
    let bytes = codec.encode(&message).unwrap();
    assert_eq!(codec.decode(&bytes).unwrap(), message);
    for len in 0..bytes.len() {
        assert!(codec.decode(&bytes[..len]).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(codec.decode(&extra).is_err());
    assert!(
        FastWireCodec::new(instance, 4096)
            .unwrap()
            .decode(&bytes)
            .is_err()
    );
    let mut other = instance;
    other.session = BlockHash::from(99);
    assert_eq!(
        ApplicationWireCodec::new(other, 4096)
            .unwrap()
            .decode(&bytes),
        Err(CheckpointError::WrongInstance)
    );
    assert!(
        ApplicationWireCodec::new(instance, 1)
            .unwrap()
            .encode(&message)
            .is_err()
    );
    let nested = ApplicationMessage::InitialCertificate(InitialRCertificate {
        request: InitialRRequest {
            instance,
            requester: PrivateKey::from(1).public_key(),
            candidate: BlockHash::from(1),
            signature: Signature::default(),
        },
        responses: vec![InitialRResponse {
            instance: other,
            request: BlockHash::from(1),
            responder: PrivateKey::from(2).public_key(),
            candidate: BlockHash::from(1),
            signature: Signature::default(),
        }],
    });
    assert_eq!(codec.encode(&nested), Err(CheckpointError::WrongInstance));
    let body = serde_json::to_vec(&nested).unwrap();
    let mut forged = bytes[..44].to_vec();
    forged[40..44].copy_from_slice(&(body.len() as u32).to_le_bytes());
    forged.extend(body);
    assert_eq!(codec.decode(&forged), Err(CheckpointError::WrongInstance));
}
