use std::collections::{BTreeMap, BTreeSet};

use crate::committee::Committee;
use crate::error::{RaiError, Result};
use crate::types::{CommitteeId, ElectionId, Hash32, VoteValue};
use crate::vote::{SignedVote, VoteKind, VotePool};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum CertificateKind {
    Notarization,
    Fast,
    Final,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalCertificate {
    pub kind: CertificateKind,
    pub election: ElectionId,
    pub committee: CommitteeId,
    pub value: VoteValue,
    pub votes: Vec<SignedVote>,
}

impl LocalCertificate {
    pub fn derive(
        pool: &VotePool,
        committee: &Committee,
        election: &ElectionId,
        value: VoteValue,
        kind: CertificateKind,
    ) -> Option<Self> {
        let (vote_kinds, threshold) = match kind {
            CertificateKind::Notarization => (
                vec![VoteKind::First, VoteKind::Notarization],
                committee.notar_threshold(),
            ),
            CertificateKind::Fast => {
                if value == VoteValue::Timeout {
                    return None;
                }
                (vec![VoteKind::First], committee.fast_threshold())
            }
            CertificateKind::Final => {
                if value == VoteValue::Timeout {
                    return None;
                }
                (vec![VoteKind::Final], committee.final_threshold())
            }
        };
        let votes = pool.votes_for(committee.id, election, value, &vote_kinds);
        let weight = committee.weight_of_signers(votes.iter().map(|vote| vote.signer));
        if weight < threshold {
            return None;
        }
        Some(Self {
            kind,
            election: election.clone(),
            committee: committee.id,
            value,
            votes,
        })
    }

    pub fn signer_set(&self) -> BTreeSet<u64> {
        self.votes.iter().map(|vote| vote.signer).collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimeoutReason {
    Certificate,
    Conflict { left: Hash32, right: Hash32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalResult {
    Timeout(TimeoutReason),
    Notarized(Hash32),
    Fast(Hash32),
    Final(Hash32),
}

impl LocalResult {
    pub fn value(&self) -> Option<Hash32> {
        match self {
            Self::Timeout(_) => None,
            Self::Notarized(value) | Self::Fast(value) | Self::Final(value) => Some(*value),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobalResult {
    Timeout,
    Notarized(Hash32),
    Converged(Hash32),
    Fast(Hash32),
    Final(Hash32),
}

impl GlobalResult {
    pub fn value(&self) -> Option<Hash32> {
        match self {
            Self::Timeout => None,
            Self::Notarized(value)
            | Self::Converged(value)
            | Self::Fast(value)
            | Self::Final(value) => Some(*value),
        }
    }
}

pub fn derive_local_result(
    pool: &VotePool,
    committee: &Committee,
    election: &ElectionId,
) -> Result<Option<LocalResult>> {
    let values = pool.candidate_values(committee.id, election);
    let mut notarized = Vec::new();
    let mut fast = Vec::new();
    let mut final_values = Vec::new();

    for value in values {
        if LocalCertificate::derive(
            pool,
            committee,
            election,
            value,
            CertificateKind::Notarization,
        )
        .is_some()
        {
            notarized.push(value);
        }
        if LocalCertificate::derive(pool, committee, election, value, CertificateKind::Fast)
            .is_some()
        {
            fast.push(value);
        }
        if LocalCertificate::derive(pool, committee, election, value, CertificateKind::Final)
            .is_some()
        {
            final_values.push(value);
        }
    }

    if final_values.len() > 1 || fast.len() > 1 {
        return Err(RaiError::SafetyFault(format!(
            "committee {} produced conflicting fast/final certificates for {}",
            committee.id, election
        )));
    }

    let strong = final_values
        .first()
        .copied()
        .or_else(|| fast.first().copied());
    if let Some(strong_value) = strong {
        if notarized.iter().any(|value| *value != strong_value) {
            return Err(RaiError::SafetyFault(format!(
                "fast/final certificate conflicts with notarization in committee {} for {}",
                committee.id, election
            )));
        }
    }

    if let Some(VoteValue::Candidate(value)) = final_values.first().copied() {
        return Ok(Some(LocalResult::Final(value)));
    }
    if let Some(VoteValue::Candidate(value)) = fast.first().copied() {
        return Ok(Some(LocalResult::Fast(value)));
    }

    if notarized.contains(&VoteValue::Timeout) {
        return Ok(Some(LocalResult::Timeout(TimeoutReason::Certificate)));
    }

    let candidates: Vec<Hash32> = notarized
        .into_iter()
        .filter_map(VoteValue::candidate)
        .collect();
    match candidates.as_slice() {
        [] => Ok(None),
        [value] => Ok(Some(LocalResult::Notarized(*value))),
        [left, right, ..] => Ok(Some(LocalResult::Timeout(TimeoutReason::Conflict {
            left: *left,
            right: *right,
        }))),
    }
}

pub fn derive_global_result(
    pool: &VotePool,
    committees: &[Committee],
    election: &ElectionId,
) -> Result<Option<GlobalResult>> {
    if committees.is_empty() {
        return Err(RaiError::UnknownElection(election.to_string()));
    }

    let mut local = BTreeMap::new();
    for committee in committees {
        let Some(result) = derive_local_result(pool, committee, election)? else {
            return Ok(None);
        };
        local.insert(committee.id, result);
    }
    let merged = merge_local_results(local.values())?;
    Ok(match (election.is_close(), merged) {
        (true, Some(GlobalResult::Notarized(value))) => Some(GlobalResult::Converged(value)),
        (_, other) => other,
    })
}

pub fn merge_local_results<'a>(
    results: impl IntoIterator<Item = &'a LocalResult>,
) -> Result<Option<GlobalResult>> {
    let results: Vec<&LocalResult> = results.into_iter().collect();
    if results.is_empty() {
        return Ok(None);
    }
    if results
        .iter()
        .any(|result| matches!(result, LocalResult::Timeout(_)))
    {
        return Ok(Some(GlobalResult::Timeout));
    }

    let values: BTreeSet<Hash32> = results.iter().filter_map(|result| result.value()).collect();
    if values.len() > 1 {
        let all_final = results
            .iter()
            .all(|result| matches!(result, LocalResult::Final(_)));
        if all_final {
            return Err(RaiError::SafetyFault(
                "conflicting local final results across committees".into(),
            ));
        }
        return Ok(Some(GlobalResult::Timeout));
    }
    let value = *values
        .iter()
        .next()
        .expect("non-timeout local result has a value");

    if results
        .iter()
        .any(|result| matches!(result, LocalResult::Notarized(_)))
    {
        return Ok(Some(GlobalResult::Notarized(value)));
    }
    if results
        .iter()
        .any(|result| matches!(result, LocalResult::Final(_)))
    {
        return Ok(Some(GlobalResult::Final(value)));
    }
    Ok(Some(GlobalResult::Fast(value)))
}

pub fn conflicting_notarizations(
    pool: &VotePool,
    committee: &Committee,
    election: &ElectionId,
) -> Option<(Hash32, Hash32)> {
    let candidates: Vec<Hash32> = pool
        .candidate_values(committee.id, election)
        .into_iter()
        .filter_map(VoteValue::candidate)
        .filter(|value| {
            LocalCertificate::derive(
                pool,
                committee,
                election,
                VoteValue::Candidate(*value),
                CertificateKind::Notarization,
            )
            .is_some()
        })
        .collect();
    if candidates.len() >= 2 {
        Some((candidates[0], candidates[1]))
    } else {
        None
    }
}
