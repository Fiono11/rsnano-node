use std::collections::{BTreeMap, BTreeSet};

use crate::committee::Committee;
use crate::crypto::{CryptoProvider, Signature};
use crate::error::{RaiError, Result};
use crate::types::{put_u64, CommitteeId, ElectionId, ReplicaId, VoteValue, Weight};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum VoteKind {
    First,
    Notarization,
    Final,
}

impl VoteKind {
    fn tag(self) -> u8 {
        match self {
            Self::First => 0,
            Self::Notarization => 1,
            Self::Final => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedVote {
    pub signer: ReplicaId,
    pub election: ElectionId,
    pub committee: CommitteeId,
    pub value: VoteValue,
    pub kind: VoteKind,
    pub signature: Signature,
}

impl SignedVote {
    pub fn new(
        crypto: &impl CryptoProvider,
        signer: ReplicaId,
        election: ElectionId,
        committee: CommitteeId,
        value: VoteValue,
        kind: VoteKind,
    ) -> Result<Self> {
        let bytes = Self::signing_bytes_for(signer, &election, committee, value, kind);
        let signature = crypto
            .sign(signer, &bytes)
            .ok_or(RaiError::InvalidSignature)?;
        Ok(Self {
            signer,
            election,
            committee,
            value,
            kind,
            signature,
        })
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        Self::signing_bytes_for(
            self.signer,
            &self.election,
            self.committee,
            self.value,
            self.kind,
        )
    }

    fn signing_bytes_for(
        signer: ReplicaId,
        election: &ElectionId,
        committee: CommitteeId,
        value: VoteValue,
        kind: VoteKind,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(96);
        out.extend_from_slice(b"rai-vote-v2-ed25519");
        put_u64(&mut out, signer);
        election.encode(&mut out);
        put_u64(&mut out, committee);
        value.encode(&mut out);
        out.push(kind.tag());
        out
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct VoteKey {
    committee: CommitteeId,
    election: ElectionId,
    value: VoteValue,
    kind: VoteKind,
}

#[derive(Clone, Debug, Default)]
pub struct VotePool {
    votes: BTreeMap<VoteKey, BTreeMap<ReplicaId, SignedVote>>,
    first_by_signer: BTreeMap<(CommitteeId, ElectionId, ReplicaId), VoteValue>,
    final_by_signer: BTreeMap<(CommitteeId, ElectionId, ReplicaId), VoteValue>,
}

impl VotePool {
    pub fn insert(
        &mut self,
        vote: SignedVote,
        committee: &Committee,
        crypto: &impl CryptoProvider,
    ) -> Result<bool> {
        if vote.committee != committee.id {
            return Err(RaiError::InvalidVote("vote committee mismatch".into()));
        }
        if !committee.contains(vote.signer) {
            return Err(RaiError::InvalidVote(format!(
                "replica {} is not in committee {}",
                vote.signer, committee.id
            )));
        }
        if !crypto.verify(vote.signer, &vote.signing_bytes(), &vote.signature) {
            return Err(RaiError::InvalidSignature);
        }

        let signer_key = (vote.committee, vote.election.clone(), vote.signer);
        match vote.kind {
            VoteKind::First => {
                if let Some(existing) = self.first_by_signer.get(&signer_key) {
                    if *existing != vote.value {
                        return Err(RaiError::DuplicateFirstVote);
                    }
                }
                if let Some(final_value) = self.final_by_signer.get(&signer_key) {
                    if *final_value != vote.value {
                        return Err(RaiError::InvalidVote(
                            "late first vote conflicts with the signer's final vote".into(),
                        ));
                    }
                }
                // A first vote is unique, but it is not required to match the
                // signer's notarization support. A signer may notarize a value
                // different from its first vote, and may notarize multiple
                // values in the conflicting-notarization timeout path. Only a
                // known final vote constrains a late-arriving first vote.
            }
            VoteKind::Notarization => {
                // Correct replicas cast notarization only after first-voting,
                // but the signed messages may be delivered in either order.
                // Once a final vote is known, only same-value late support is
                // compatible with the final-vote subset guard.
                if let Some(final_value) = self.final_by_signer.get(&signer_key) {
                    if *final_value != vote.value {
                        return Err(RaiError::InvalidVote(
                            "late notarization vote conflicts with the signer's final vote".into(),
                        ));
                    }
                }
            }
            VoteKind::Final => {
                if let Some(existing) = self.final_by_signer.get(&signer_key) {
                    if *existing != vote.value {
                        return Err(RaiError::DuplicateFinalVote);
                    }
                }
                let support = self.support_values(vote.signer, vote.committee, &vote.election);
                // Empty support is valid: the empty set is a subset of the
                // final value. Any support already present must match it.
                if support.iter().any(|value| *value != vote.value) {
                    return Err(RaiError::InvalidVote(format!(
                        "final vote support is not a subset of {{{}}}",
                        vote.value
                    )));
                }
                if vote.value == VoteValue::Timeout {
                    return Err(RaiError::InvalidVote(
                        "timeout values cannot receive final votes".into(),
                    ));
                }
            }
        }

        let key = VoteKey {
            committee: vote.committee,
            election: vote.election.clone(),
            value: vote.value,
            kind: vote.kind,
        };
        let inserted = self
            .votes
            .entry(key)
            .or_default()
            .insert(vote.signer, vote.clone())
            .is_none();

        if inserted {
            match vote.kind {
                VoteKind::First => {
                    self.first_by_signer.insert(signer_key, vote.value);
                }
                VoteKind::Final => {
                    self.final_by_signer.insert(signer_key, vote.value);
                }
                VoteKind::Notarization => {}
            }
        }
        Ok(inserted)
    }

    /// Returns the signer's first-vote value in one committee instance.
    /// First-vote state is deliberately committee-local, as required by RAI.
    pub fn first_choice_in_committee(
        &self,
        signer: ReplicaId,
        committee: CommitteeId,
        election: &ElectionId,
    ) -> Option<VoteValue> {
        self.first_by_signer
            .get(&(committee, election.clone(), signer))
            .copied()
    }

    /// Compatibility helper. Returns a value only when every committee-local
    /// first vote currently known for this signer/election agrees. It is not
    /// used as a protocol lock.
    pub fn first_choice(&self, signer: ReplicaId, election: &ElectionId) -> Option<VoteValue> {
        let values = self
            .first_by_signer
            .iter()
            .filter_map(|((_, id, voter), value)| {
                (id == election && *voter == signer).then_some(*value)
            })
            .collect::<BTreeSet<_>>();
        (values.len() == 1).then(|| *values.iter().next().expect("one value"))
    }

    pub fn first_value(
        &self,
        signer: ReplicaId,
        committee: CommitteeId,
        election: &ElectionId,
    ) -> Option<VoteValue> {
        self.first_by_signer
            .get(&(committee, election.clone(), signer))
            .copied()
    }

    pub fn final_value(
        &self,
        signer: ReplicaId,
        committee: CommitteeId,
        election: &ElectionId,
    ) -> Option<VoteValue> {
        self.final_by_signer
            .get(&(committee, election.clone(), signer))
            .copied()
    }

    pub fn has_notarization_vote(
        &self,
        signer: ReplicaId,
        committee: CommitteeId,
        election: &ElectionId,
        value: VoteValue,
    ) -> bool {
        let key = VoteKey {
            committee,
            election: election.clone(),
            value,
            kind: VoteKind::Notarization,
        };
        self.votes
            .get(&key)
            .map(|votes| votes.contains_key(&signer))
            .unwrap_or(false)
    }

    pub fn support_values(
        &self,
        signer: ReplicaId,
        committee: CommitteeId,
        election: &ElectionId,
    ) -> BTreeSet<VoteValue> {
        let mut support = BTreeSet::new();
        if let Some(value) = self.first_value(signer, committee, election) {
            support.insert(value);
        }
        for (key, votes) in &self.votes {
            if key.committee == committee
                && &key.election == election
                && key.kind == VoteKind::Notarization
                && votes.contains_key(&signer)
            {
                support.insert(key.value);
            }
        }
        support
    }

    pub fn count_first(
        &self,
        committee: CommitteeId,
        election: &ElectionId,
        value: VoteValue,
    ) -> usize {
        self.count(committee, election, value, VoteKind::First)
    }

    pub fn weight_first(
        &self,
        committee: &Committee,
        election: &ElectionId,
        value: VoteValue,
    ) -> Weight {
        committee.weight_of_signers(
            self.votes_for(committee.id, election, value, &[VoteKind::First])
                .into_iter()
                .map(|vote| vote.signer),
        )
    }

    pub fn count_final(
        &self,
        committee: CommitteeId,
        election: &ElectionId,
        value: VoteValue,
    ) -> usize {
        self.count(committee, election, value, VoteKind::Final)
    }

    pub fn weight_final(
        &self,
        committee: &Committee,
        election: &ElectionId,
        value: VoteValue,
    ) -> Weight {
        committee.weight_of_signers(
            self.votes_for(committee.id, election, value, &[VoteKind::Final])
                .into_iter()
                .map(|vote| vote.signer),
        )
    }

    pub fn count_notar_support(
        &self,
        committee: CommitteeId,
        election: &ElectionId,
        value: VoteValue,
    ) -> usize {
        let mut signers = BTreeSet::new();
        for kind in [VoteKind::First, VoteKind::Notarization] {
            let key = VoteKey {
                committee,
                election: election.clone(),
                value,
                kind,
            };
            if let Some(votes) = self.votes.get(&key) {
                signers.extend(votes.keys().copied());
            }
        }
        signers.len()
    }

    pub fn weight_notar_support(
        &self,
        committee: &Committee,
        election: &ElectionId,
        value: VoteValue,
    ) -> Weight {
        committee.weight_of_signers(
            self.votes_for(
                committee.id,
                election,
                value,
                &[VoteKind::First, VoteKind::Notarization],
            )
            .into_iter()
            .map(|vote| vote.signer),
        )
    }

    pub fn votes_for(
        &self,
        committee: CommitteeId,
        election: &ElectionId,
        value: VoteValue,
        kinds: &[VoteKind],
    ) -> Vec<SignedVote> {
        let mut by_signer = BTreeMap::new();
        for kind in kinds {
            let key = VoteKey {
                committee,
                election: election.clone(),
                value,
                kind: *kind,
            };
            if let Some(votes) = self.votes.get(&key) {
                for (signer, vote) in votes {
                    by_signer.entry(*signer).or_insert_with(|| vote.clone());
                }
            }
        }
        by_signer.into_values().collect()
    }

    pub fn all_votes(&self) -> Vec<SignedVote> {
        self.votes
            .values()
            .flat_map(|by_signer| by_signer.values().cloned())
            .collect()
    }

    pub fn all_votes_for_election(&self, election: &ElectionId) -> Vec<SignedVote> {
        let mut votes = Vec::new();
        for (key, by_signer) in &self.votes {
            if &key.election == election {
                votes.extend(by_signer.values().cloned());
            }
        }
        votes
    }

    pub fn candidate_values(
        &self,
        committee: CommitteeId,
        election: &ElectionId,
    ) -> BTreeSet<VoteValue> {
        self.votes
            .keys()
            .filter(|key| key.committee == committee && &key.election == election)
            .map(|key| key.value)
            .collect()
    }

    pub fn all_first_count(&self, committee: CommitteeId, election: &ElectionId) -> usize {
        self.first_by_signer
            .keys()
            .filter(|(q, id, _)| *q == committee && id == election)
            .count()
    }

    pub fn all_first_weight(&self, committee: &Committee, election: &ElectionId) -> Weight {
        committee.weight_of_signers(self.first_by_signer.keys().filter_map(|(q, id, signer)| {
            (*q == committee.id && id == election).then_some(*signer)
        }))
    }

    pub fn max_candidate_first_count(
        &self,
        committee: CommitteeId,
        election: &ElectionId,
    ) -> usize {
        self.candidate_values(committee, election)
            .into_iter()
            .filter(|value| *value != VoteValue::Timeout)
            .map(|value| self.count_first(committee, election, value))
            .max()
            .unwrap_or(0)
    }

    pub fn max_candidate_first_weight(
        &self,
        committee: &Committee,
        election: &ElectionId,
    ) -> Weight {
        self.candidate_values(committee.id, election)
            .into_iter()
            .filter(|value| *value != VoteValue::Timeout)
            .map(|value| self.weight_first(committee, election, value))
            .max()
            .unwrap_or(0)
    }

    pub fn many_values(&self, committee: &Committee, election: &ElectionId) -> BTreeSet<VoteValue> {
        self.candidate_values(committee.id, election)
            .into_iter()
            .filter(|value| *value != VoteValue::Timeout)
            .filter(|value| {
                self.weight_first(committee, election, *value) >= committee.second_look_threshold()
            })
            .collect()
    }

    pub fn timeout_ready(&self, committee: &Committee, election: &ElectionId) -> bool {
        self.all_first_weight(committee, election)
            .saturating_sub(self.max_candidate_first_weight(committee, election))
            >= committee.second_look_threshold()
    }

    pub fn fast_dead(
        &self,
        committee: &Committee,
        election: &ElectionId,
        candidate: VoteValue,
    ) -> bool {
        let signers = self
            .first_by_signer
            .iter()
            .filter_map(|((q, id, signer), value)| {
                (*q == committee.id && id == election && *value != candidate).then_some(*signer)
            });
        committee.weight_of_signers(signers) > committee.f + committee.p
    }

    pub fn final_dead(
        &self,
        committee: &Committee,
        election: &ElectionId,
        candidate: VoteValue,
    ) -> bool {
        let torn = committee.members.iter().copied().filter(|signer| {
            self.support_values(*signer, committee.id, election)
                .iter()
                .any(|value| *value != candidate)
                && self.final_value(*signer, committee.id, election) != Some(candidate)
        });
        committee.weight_of_signers(torn) > 2 * committee.f + committee.p
    }

    pub fn all_values_dead_with_values(
        &self,
        committee: &Committee,
        election: &ElectionId,
        known_values: impl IntoIterator<Item = VoteValue>,
    ) -> bool {
        let mut values = self.candidate_values(committee.id, election);
        values.extend(known_values);
        values.remove(&VoteValue::Timeout);
        values.into_iter().all(|value| {
            self.fast_dead(committee, election, value)
                && self.final_dead(committee, election, value)
        })
    }

    pub fn all_values_dead(&self, committee: &Committee, election: &ElectionId) -> bool {
        self.all_values_dead_with_values(committee, election, std::iter::empty())
    }

    pub fn timeout_allowed_with_values(
        &self,
        committee: &Committee,
        election: &ElectionId,
        known_values: impl IntoIterator<Item = VoteValue>,
    ) -> bool {
        self.timeout_ready(committee, election)
            || self.all_values_dead_with_values(committee, election, known_values)
    }

    pub fn timeout_allowed(&self, committee: &Committee, election: &ElectionId) -> bool {
        self.timeout_allowed_with_values(committee, election, std::iter::empty())
    }

    fn count(
        &self,
        committee: CommitteeId,
        election: &ElectionId,
        value: VoteValue,
        kind: VoteKind,
    ) -> usize {
        let key = VoteKey {
            committee,
            election: election.clone(),
            value,
            kind,
        };
        self.votes.get(&key).map_or(0, BTreeMap::len)
    }
}
