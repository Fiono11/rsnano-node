//! Mutable A/B services and their certificate evaluations (Algorithm 5).
//! Authentication, ancestry and complementing witnesses are checked before
//! these transitions. These services do not seal a phase or discard late input.
use std::collections::{BTreeMap, BTreeSet};

use rsnano_types::{BlockHash, PublicKey};
use serde::{Deserialize, Serialize};

use super::CheckpointError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BValue {
    pub flag: bool,
    pub value: BlockHash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BOutcome {
    Adopt(BlockHash),
    Commit(BlockHash),
}

impl BOutcome {
    pub fn value(self) -> BlockHash {
        match self {
            Self::Adopt(v) | Self::Commit(v) => v,
        }
    }
}

/// Algorithm 5's bounded A set: preserve two distinct values, replacing the
/// smaller only when an input exceeds the existing maximum. A second value
/// permanently prevents a later singleton response.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AState {
    values: BTreeSet<BlockHash>,
}
impl AState {
    pub fn deliver(&mut self, value: BlockHash) -> Vec<BlockHash> {
        if self.values.len() < 2 {
            self.values.insert(value);
        } else if self.values.last().is_some_and(|maximum| value > *maximum) {
            self.values.pop_first();
            self.values.insert(value);
        }
        self.values.iter().copied().collect()
    }
    pub fn values(&self) -> Vec<BlockHash> {
        self.values.iter().copied().collect()
    }
}

/// Retains up to two false values until a true value is learned; thereafter
/// retains that true value and the largest false value. False evidence cannot
/// erase the true value. Conflicting true values are a certificate error.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BState {
    true_value: Option<BlockHash>,
    false_values: BTreeSet<BlockHash>,
}
impl BState {
    pub fn deliver(&mut self, input: BValue) -> Result<Vec<BValue>, CheckpointError> {
        if input.flag {
            if self.true_value.is_some_and(|v| v != input.value) {
                return Err(CheckpointError::InvalidEvidence);
            }
            self.true_value = Some(input.value);
        } else {
            self.false_values.insert(input.value);
        }
        let capacity = if self.true_value.is_some() { 1 } else { 2 };
        while self.false_values.len() > capacity {
            self.false_values.pop_first();
        }
        Ok(self.values())
    }
    pub fn values(&self) -> Vec<BValue> {
        let mut result: Vec<_> = self
            .false_values
            .iter()
            .map(|value| BValue {
                flag: false,
                value: *value,
            })
            .collect();
        if let Some(value) = self.true_value {
            result.push(BValue { flag: true, value });
        }
        result
    }
}

/// Count exactly Q distinct responders whose signatures, originating requests
/// and complementing witnesses have already been independently verified.
fn check_responders<T>(q: u64, responses: &[(PublicKey, Vec<T>)]) -> Result<(), CheckpointError> {
    if q == 0 || responses.len() as u64 != q {
        return Err(CheckpointError::InvalidSize);
    }
    let mut identities = BTreeSet::new();
    for (id, values) in responses {
        if !identities.insert(*id) {
            return Err(CheckpointError::DuplicateSigner);
        }
        if values.is_empty() || values.len() > 2 {
            return Err(CheckpointError::InvalidEvidence);
        }
    }
    Ok(())
}

pub fn evaluate_a(
    q: u64,
    responses: &[(PublicKey, Vec<BlockHash>)],
) -> Result<BValue, CheckpointError> {
    check_responders(q, responses)?;
    let mut all = BTreeSet::new();
    for (_, values) in responses {
        if values.len() == 2 && values[0] >= values[1] {
            return Err(CheckpointError::InvalidEvidence);
        }
        all.extend(values.iter().copied());
    }
    Ok(BValue {
        flag: all.len() == 1,
        value: *all.last().unwrap(),
    })
}

pub fn evaluate_b(
    q: u64,
    responses: &[(PublicKey, Vec<BValue>)],
) -> Result<BOutcome, CheckpointError> {
    check_responders(q, responses)?;
    let mut true_value = None;
    let mut max_false = None;
    for (_, values) in responses {
        if values.len() == 2 && values[0] >= values[1] {
            return Err(CheckpointError::InvalidEvidence);
        }
        for entry in values {
            if entry.flag {
                if true_value.is_some_and(|v| v != entry.value) {
                    return Err(CheckpointError::InvalidEvidence);
                }
                true_value = Some(entry.value);
            } else {
                max_false = Some(max_false.map_or(entry.value, |v: BlockHash| v.max(entry.value)));
            }
        }
    }
    if let Some(value) = true_value {
        if responses
            .iter()
            .all(|(_, values)| values.as_slice() == [BValue { flag: true, value }])
        {
            Ok(BOutcome::Commit(value))
        } else {
            Ok(BOutcome::Adopt(value))
        }
    } else {
        Ok(BOutcome::Adopt(
            max_false.ok_or(CheckpointError::InvalidEvidence)?,
        ))
    }
}

/// Response evidence for originating broadcasts. Callers supply authenticated,
/// instance-bound responses; one identity counts once even when it sends many
/// different snapshots. W and Q queries intentionally remain separate.
#[derive(Default)]
pub struct Witnesses {
    responses: BTreeMap<BlockHash, BTreeSet<PublicKey>>,
}
impl Witnesses {
    pub fn record(&mut self, request: BlockHash, responder: PublicKey) {
        self.responses.entry(request).or_default().insert(responder);
    }
    pub fn count(&self, request: &BlockHash) -> u64 {
        self.responses
            .get(request)
            .map_or(0, |ids| ids.len() as u64)
    }
    pub fn admits(&self, request: BlockHash, origins: &[BlockHash], w: u64) -> bool {
        w > 0
            && (self.count(&request) >= w
                || (!origins.is_empty() && origins.iter().all(|id| self.count(id) >= w)))
    }
    pub fn eligible(&self, origins: &[BlockHash], q: u64) -> bool {
        q > 0 && !origins.is_empty() && origins.iter().all(|id| self.count(id) >= q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(i: u64) -> BlockHash {
        BlockHash::from(i)
    }
    fn b(flag: bool, i: u64) -> BValue {
        BValue { flag, value: v(i) }
    }
    fn responses<T: Clone>(values: &[Vec<T>]) -> Vec<(PublicKey, Vec<T>)> {
        values
            .iter()
            .enumerate()
            .map(|(i, values)| {
                (
                    rsnano_types::PrivateKey::from(i as u64 + 1).public_key(),
                    values.clone(),
                )
            })
            .collect()
    }
    #[test]
    fn a_keeps_conflict_and_maximum_after_late_requests() {
        let mut a = AState::default();
        assert_eq!(a.deliver(v(2)), vec![v(2)]);
        assert_eq!(a.deliver(v(1)), vec![v(1), v(2)]);
        assert_eq!(a.deliver(v(3)), vec![v(2), v(3)]);
        assert_eq!(a.deliver(v(1)), vec![v(2), v(3)]);
        assert_eq!(a.deliver(v(3)), vec![v(2), v(3)]);
        let mut a = AState::default();
        a.deliver(v(1));
        a.deliver(v(5));
        assert_eq!(a.deliver(v(3)), vec![v(1), v(5)]);
    }
    #[test]
    fn b_retains_true_and_maximum_false_after_late_requests() {
        let mut state = BState::default();
        assert_eq!(state.deliver(b(false, 1)).unwrap(), vec![b(false, 1)]);
        assert_eq!(
            state.deliver(b(false, 2)).unwrap(),
            vec![b(false, 1), b(false, 2)]
        );
        assert_eq!(
            state.deliver(b(true, 1)).unwrap(),
            vec![b(false, 2), b(true, 1)]
        );
        assert_eq!(
            state.deliver(b(false, 3)).unwrap(),
            vec![b(false, 3), b(true, 1)]
        );
        assert_eq!(
            state.deliver(b(false, 1)).unwrap(),
            vec![b(false, 3), b(true, 1)]
        );
        assert_eq!(
            state.deliver(b(true, 2)),
            Err(CheckpointError::InvalidEvidence)
        );
        assert_eq!(state.values(), vec![b(false, 3), b(true, 1)]);
    }
    #[test]
    fn a_evaluation_requires_singleton_responses_for_true() {
        assert_eq!(
            evaluate_a(4, &responses(&vec![vec![v(1)]; 4])),
            Ok(b(true, 1))
        );
        assert_eq!(
            evaluate_a(
                4,
                &responses(&[vec![v(1)], vec![v(1)], vec![v(1)], vec![v(1), v(2)]])
            ),
            Ok(b(false, 2))
        );
        let mut duplicate = responses(&vec![vec![v(1)]; 4]);
        duplicate[1] = duplicate[0].clone();
        assert_eq!(
            evaluate_a(4, &duplicate),
            Err(CheckpointError::DuplicateSigner)
        );
    }
    #[test]
    fn b_commit_adopt_and_false_maximum_are_distinct() {
        assert_eq!(
            evaluate_b(4, &responses(&vec![vec![b(true, 1)]; 4])),
            Ok(BOutcome::Commit(v(1)))
        );
        assert_eq!(
            evaluate_b(
                4,
                &responses(&[
                    vec![b(false, 3)],
                    vec![b(false, 2)],
                    vec![b(true, 1)],
                    vec![b(false, 2), b(true, 1)]
                ])
            ),
            Ok(BOutcome::Adopt(v(1)))
        );
        assert_eq!(
            evaluate_b(
                4,
                &responses(&[
                    vec![b(false, 1)],
                    vec![b(false, 2)],
                    vec![b(false, 3)],
                    vec![b(false, 2)]
                ])
            ),
            Ok(BOutcome::Adopt(v(3)))
        );
        assert_eq!(
            evaluate_b(
                4,
                &responses(&[
                    vec![b(true, 1)],
                    vec![b(true, 1)],
                    vec![b(true, 2)],
                    vec![b(true, 1)]
                ])
            ),
            Err(CheckpointError::InvalidEvidence)
        );
    }
    #[test]
    fn weak_admission_does_not_make_a_response_eligible() {
        let mut witnesses = Witnesses::default();
        let origin = v(1);
        for i in 1..=2 {
            witnesses.record(origin, rsnano_types::PrivateKey::from(i).public_key());
        }
        witnesses.record(origin, rsnano_types::PrivateKey::from(1).public_key());
        assert_eq!(witnesses.count(&origin), 2);
        assert!(witnesses.admits(v(2), &[origin], 2));
        assert!(!witnesses.eligible(&[origin], 4));
        for i in 3..=4 {
            witnesses.record(origin, rsnano_types::PrivateKey::from(i).public_key());
        }
        assert!(witnesses.eligible(&[origin], 4));
        assert!(!witnesses.eligible(&[origin, v(9)], 4));
        assert!(!witnesses.eligible(&[], 4));
    }
}
