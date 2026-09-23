use std::collections::BTreeSet;

use rsnano_messages::{CloseProofReply, EpochProp};
use rsnano_types::{Amount, BlockHash, ConsensusEpoch, VoteKind};

use crate::consensus::election::{Committees, EpochValue, ReportRef};

/// Only this verifier constructs the commitment consumed by checkpoint transfer.
/// The caller supplies committees derived from its already installed predecessor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifiedCheckpoint {
    pub epoch: ConsensusEpoch,
    pub state: BlockHash,
}

pub(crate) fn proposal_value(prop: &EpochProp) -> EpochValue {
    EpochValue::from_parts(
        prop.epoch,
        prop.slot,
        prop.parent,
        prop.reports
            .iter()
            .map(|r| ReportRef {
                reporter: r.reporter,
                certified: r.certified,
                residual: r.residual,
            })
            .collect(),
        prop.state,
    )
}

pub(crate) fn verify_close_proof(
    proof: &CloseProofReply,
    next: ConsensusEpoch,
    committees: &Committees,
) -> Option<VerifiedCheckpoint> {
    let prop = &proof.proposal;
    if prop.epoch != next
        || prop.epoch.as_u64() >= (1 << 47)
        || prop.slot >= (1 << 16)
        || prop.reports.is_empty()
        || prop.reports.len() > EpochProp::MAX_REPORTS
        || prop
            .reports
            .windows(2)
            .any(|p| p[0].reporter >= p[1].reporter)
        || proof.votes.len() > CloseProofReply::MAX_VOTES
    {
        return None;
    }
    let leaders: Vec<_> = committees
        .iter()
        .flat_map(|c| c.weights().keys().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if leaders.is_empty()
        || leaders[((next.as_u64() % leaders.len() as u64
            + prop.slot as u64 % leaders.len() as u64)
            % leaders.len() as u64) as usize]
            != prop.leader
    {
        return None;
    }
    let hash = proposal_value(prop).hash();
    if !prop.verify(hash) {
        return None;
    }
    let domain = ConsensusEpoch::close_round(next, prop.slot);
    let mut first = BTreeSet::new();
    let mut finals = BTreeSet::new();
    for vote in &proof.votes {
        if vote.epoch != domain || !vote.hashes.contains(&hash) || vote.validate().is_err() {
            return None;
        }
        let voters = match vote.kind() {
            VoteKind::First => &mut first,
            VoteKind::Final => &mut finals,
            _ => return None,
        };
        if !voters.insert(vote.voter) {
            return None;
        }
    }
    let quorum = |fast: bool| {
        committees.iter().all(|c| {
            if c.online().is_zero() {
                return false;
            }
            let voters = if fast { &first } else { &finals };
            let weight = voters
                .iter()
                .fold(0u128, |sum, rep| sum.saturating_add(c.weight(rep).number()));
            Amount::raw(weight)
                >= if fast {
                    c.thresholds().fast
                } else {
                    c.thresholds().certificate
                }
        })
    };
    (quorum(false) || quorum(true)).then_some(VerifiedCheckpoint {
        epoch: next,
        state: prop.state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::Committee;
    use rsnano_messages::ReportSelection;
    use rsnano_types::{PrivateKey, UnixMillisTimestamp, Vote};
    use std::sync::Arc;

    #[test]
    fn both_committees_must_certify_the_placement() {
        let (mut proof, committees) = fixture(VoteKind::Final);
        assert!(verify(&proof, &committees).is_some());
        proof
            .votes
            .retain(|v| (1..=6).any(|i| PrivateKey::from(i).public_key() == v.voter));
        assert!(verify(&proof, &committees).is_none());
    }
    #[test]
    fn fast_pair_works_but_mixed_pair_does_not() {
        let (proof, committees) = fixture(VoteKind::First);
        assert!(verify(&proof, &committees).is_some());
        let (mut mixed, _) = fixture(VoteKind::Final);
        mixed.votes.truncate(6);
        mixed.votes.extend(proof.votes.into_iter().skip(6));
        assert!(verify(&mixed, &committees).is_none());
    }
    #[test]
    fn duplicate_signers_and_forged_votes_fail() {
        let (mut proof, committees) = fixture(VoteKind::Final);
        proof.votes.push(proof.votes[0].clone());
        assert!(verify(&proof, &committees).is_none());
        proof.votes.pop();
        proof.votes[0].voter = PrivateKey::from(99).public_key();
        assert!(verify(&proof, &committees).is_none());
    }
    #[test]
    fn unsigned_state_wrong_slot_and_out_of_order_epoch_fail() {
        let (mut proof, committees) = fixture(VoteKind::Final);
        assert!(verify_close_proof(&proof, ConsensusEpoch::new(1), &committees).is_none());
        proof.proposal.slot += 1;
        assert!(verify(&proof, &committees).is_none());
        proof.proposal.slot -= 1;
        proof.proposal.state = BlockHash::from(999);
        assert!(verify(&proof, &committees).is_none());
    }
    #[test]
    fn proposal_signature_alone_is_not_a_certificate() {
        let (mut proof, committees) = fixture(VoteKind::Final);
        proof.votes.clear();
        assert!(verify(&proof, &committees).is_none());
    }
    /* Test helpers */
    fn verify(proof: &CloseProofReply, committees: &Committees) -> Option<VerifiedCheckpoint> {
        verify_close_proof(proof, ConsensusEpoch::ZERO, committees)
    }
    fn fixture(kind: VoteKind) -> (CloseProofReply, Committees) {
        let committee = |start| {
            Arc::new(Committee::new(
                (start..start + 6)
                    .map(|i| (PrivateKey::from(i).public_key(), Amount::raw(100)))
                    .collect(),
            ))
        };
        let committees = Committees::joint(committee(1), committee(7));
        let leader = (1..=12)
            .map(PrivateKey::from)
            .min_by_key(|k| k.public_key())
            .unwrap();
        let mut prop = EpochProp::new(
            &leader,
            ConsensusEpoch::ZERO,
            0,
            BlockHash::ZERO,
            BlockHash::from(50),
            vec![ReportSelection {
                reporter: leader.public_key(),
                certified: BlockHash::from(1),
                residual: BlockHash::from(2),
            }],
            BlockHash::ZERO,
        );
        let hash = proposal_value(&prop).hash();
        prop.signature = leader.sign(hash.as_bytes());
        let votes = (1..=12)
            .map(|i| {
                Vote::new_in_epoch_at(
                    &PrivateKey::from(i),
                    kind,
                    ConsensusEpoch::close_round(ConsensusEpoch::ZERO, 0),
                    UnixMillisTimestamp::ZERO,
                    vec![hash],
                )
            })
            .collect();
        (
            CloseProofReply {
                proposal: prop,
                votes,
            },
            committees,
        )
    }
}
