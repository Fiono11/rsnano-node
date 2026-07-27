use std::collections::BTreeSet;

use rsnano_types::{Account, BlockHash, RaiCloseRecord, RaiEpoch, RaiSlot};

use super::{CloseRecordEntries, RaiCloseState, VisibleSlots};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiCloseVersionKind {
    Cut,
    Frontiers,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseReconRequest {
    pub kind: RaiCloseVersionKind,
    pub epoch: RaiEpoch,
    pub base_hash: BlockHash,
    pub target_hash: BlockHash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiFrontierReplacement {
    pub account: Account,
    pub old: Option<BlockHash>,
    pub new: Option<BlockHash>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiCloseReconDelta {
    Cut {
        epoch: RaiEpoch,
        base_hash: BlockHash,
        target_hash: BlockHash,
        added: BTreeSet<RaiSlot>,
        removed: BTreeSet<RaiSlot>,
    },
    Frontiers {
        epoch: RaiEpoch,
        base_hash: BlockHash,
        target_hash: BlockHash,
        replacements: Vec<RaiFrontierReplacement>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseReconMiss(pub RaiCloseReconRequest);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiCloseReconError {
    WrongDeltaKind,
    MissingBase,
    MissingTarget,
    InvalidDelta,
    HashMismatch,
    Inadmissible,
}

/// Implements the unauthenticated close-version transport described by
/// `Hash reconciliation of close versions` in the RAI specification.
/// Callers remain responsible for transporting requests and deltas.  In
/// particular, a delta is never consensus evidence: the target is cached only
/// after its canonical hash and the caller-provided admissibility predicate
/// have both succeeded.
pub struct RaiCloseReconciler;

impl RaiCloseReconciler {
    pub fn make_delta(
        state: &RaiCloseState,
        request: &RaiCloseReconRequest,
    ) -> Result<RaiCloseReconDelta, RaiCloseReconMiss> {
        match request.kind {
            RaiCloseVersionKind::Cut => {
                let Some(base) = state.close_value(request.epoch, &request.base_hash) else {
                    return Err(RaiCloseReconMiss(request.clone()));
                };
                let Some(target) = state.close_value(request.epoch, &request.target_hash) else {
                    return Err(RaiCloseReconMiss(request.clone()));
                };
                Ok(RaiCloseReconDelta::Cut {
                    epoch: request.epoch,
                    base_hash: request.base_hash,
                    target_hash: request.target_hash,
                    added: target.difference(base).copied().collect(),
                    removed: base.difference(target).copied().collect(),
                })
            }
            RaiCloseVersionKind::Frontiers => {
                let Some(base) = state.close_record_value(request.epoch, &request.base_hash) else {
                    return Err(RaiCloseReconMiss(request.clone()));
                };
                let Some(target) = state.close_record_value(request.epoch, &request.target_hash)
                else {
                    return Err(RaiCloseReconMiss(request.clone()));
                };
                let accounts: BTreeSet<_> = base
                    .record
                    .frontiers
                    .keys()
                    .chain(target.record.frontiers.keys())
                    .copied()
                    .collect();
                let replacements = accounts
                    .into_iter()
                    .filter_map(|account| {
                        let old = base.record.frontiers.get(&account).copied();
                        let new = target.record.frontiers.get(&account).copied();
                        (old != new).then_some(RaiFrontierReplacement { account, old, new })
                    })
                    .collect();
                Ok(RaiCloseReconDelta::Frontiers {
                    epoch: request.epoch,
                    base_hash: request.base_hash,
                    target_hash: request.target_hash,
                    replacements,
                })
            }
        }
    }

    pub fn apply_cut<F>(
        state: &mut RaiCloseState,
        delta: &RaiCloseReconDelta,
        admissible: F,
    ) -> Result<VisibleSlots, RaiCloseReconError>
    where
        F: FnOnce(&VisibleSlots) -> bool,
    {
        let RaiCloseReconDelta::Cut {
            epoch,
            base_hash,
            target_hash,
            added,
            removed,
        } = delta
        else {
            return Err(RaiCloseReconError::WrongDeltaKind);
        };
        let mut reconstructed = state
            .close_value(*epoch, base_hash)
            .cloned()
            .ok_or(RaiCloseReconError::MissingBase)?;
        if !removed.is_subset(&reconstructed) || !added.is_disjoint(&reconstructed) {
            return Err(RaiCloseReconError::InvalidDelta);
        }
        for slot in removed {
            reconstructed.remove(slot);
        }
        reconstructed.extend(added.iter().copied());
        if RaiCloseState::hash_close_cut(&reconstructed) != *target_hash {
            return Err(RaiCloseReconError::HashMismatch);
        }
        if !admissible(&reconstructed) {
            return Err(RaiCloseReconError::Inadmissible);
        }
        state.cache_close_value(*epoch, *target_hash, reconstructed.clone());
        Ok(reconstructed)
    }

    pub fn apply_frontiers<F>(
        state: &mut RaiCloseState,
        delta: &RaiCloseReconDelta,
        entries: CloseRecordEntries,
        admissible: F,
    ) -> Result<RaiCloseRecord, RaiCloseReconError>
    where
        F: FnOnce(&RaiCloseRecord, &CloseRecordEntries) -> bool,
    {
        let RaiCloseReconDelta::Frontiers {
            epoch,
            base_hash,
            target_hash,
            replacements,
        } = delta
        else {
            return Err(RaiCloseReconError::WrongDeltaKind);
        };
        let base = state
            .close_record_value(*epoch, base_hash)
            .ok_or(RaiCloseReconError::MissingBase)?;
        let mut frontiers = base.record.frontiers.clone();
        for replacement in replacements {
            if frontiers.get(&replacement.account).copied() != replacement.old {
                return Err(RaiCloseReconError::InvalidDelta);
            }
            match replacement.new {
                Some(hash) if !hash.is_zero() => {
                    frontiers.insert(replacement.account, hash);
                }
                Some(_) => return Err(RaiCloseReconError::InvalidDelta),
                None => {
                    frontiers.remove(&replacement.account);
                }
            }
        }
        let record = RaiCloseRecord::new(*epoch, base.record.previous_close_hash, frontiers);
        if record.hash() != *target_hash {
            return Err(RaiCloseReconError::HashMismatch);
        }
        if !admissible(&record, &entries) {
            return Err(RaiCloseReconError::Inadmissible);
        }
        state.cache_close_record_value(*epoch, *target_hash, record.clone(), entries);
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::rai::RaiClosedSlotState;

    fn slot(account: u64, height: u64) -> RaiSlot {
        RaiSlot::new(Account::from(account), height)
    }

    #[test]
    fn cut_delta_round_trip_is_hash_checked_and_cached() {
        let mut responder = RaiCloseState::new();
        responder.mark_visible(0, slot(1, 1));
        let base_hash = responder.record_current_close_value(0);
        responder.mark_visible(0, slot(2, 1));
        let target_hash = responder.current_close_hash(0);

        let request = RaiCloseReconRequest {
            kind: RaiCloseVersionKind::Cut,
            epoch: 0,
            base_hash,
            target_hash,
        };
        let delta = RaiCloseReconciler::make_delta(&responder, &request).unwrap();

        let mut requester = RaiCloseState::new();
        assert!(requester.cache_close_value(0, base_hash, [slot(1, 1)].into_iter().collect()));
        let reconstructed =
            RaiCloseReconciler::apply_cut(&mut requester, &delta, |_| true).unwrap();
        assert_eq!(
            reconstructed,
            [slot(1, 1), slot(2, 1)].into_iter().collect()
        );
        assert_eq!(requester.close_value(0, &target_hash), Some(&reconstructed));
    }

    #[test]
    fn cut_delta_rejects_wrong_target_without_caching() {
        let base: VisibleSlots = [slot(1, 1)].into_iter().collect();
        let base_hash = RaiCloseState::hash_close_cut(&base);
        let mut requester = RaiCloseState::new();
        requester.cache_close_value(0, base_hash, base);
        let bogus_hash = BlockHash::from(99);
        let delta = RaiCloseReconDelta::Cut {
            epoch: 0,
            base_hash,
            target_hash: bogus_hash,
            added: [slot(2, 1)].into_iter().collect(),
            removed: BTreeSet::new(),
        };

        assert_eq!(
            RaiCloseReconciler::apply_cut(&mut requester, &delta, |_| true),
            Err(RaiCloseReconError::HashMismatch)
        );
        assert!(requester.close_value(0, &bogus_hash).is_none());
    }

    #[test]
    fn frontier_delta_checks_expected_old_values_and_hash() {
        let mut responder = RaiCloseState::new();
        responder.start_closing(0).unwrap();
        responder
            .install_cut(0, [slot(1, 1)].into_iter().collect())
            .unwrap();
        responder
            .record_cut_drain(
                0,
                [(
                    slot(1, 1),
                    RaiClosedSlotState::Finalized(BlockHash::from(7)),
                )],
            )
            .unwrap();
        let base_hash = responder
            .record_current_close_record_value_with_frontiers(
                0,
                [(Account::from(1), BlockHash::from(6))]
                    .into_iter()
                    .collect(),
            )
            .unwrap();
        let target_hash = responder.record_current_close_record_value(0).unwrap();
        let request = RaiCloseReconRequest {
            kind: RaiCloseVersionKind::Frontiers,
            epoch: 0,
            base_hash,
            target_hash,
        };
        let delta = RaiCloseReconciler::make_delta(&responder, &request).unwrap();
        let entries = responder
            .close_record_value(0, &target_hash)
            .unwrap()
            .entries
            .clone();

        let record =
            RaiCloseReconciler::apply_frontiers(&mut responder, &delta, entries, |_, _| true)
                .unwrap();
        assert_eq!(record.hash(), target_hash);
        assert_eq!(record.frontiers[&Account::from(1)], BlockHash::from(7));
    }

    #[test]
    fn responder_returns_miss_unless_both_preimages_are_retained() {
        let state = RaiCloseState::new();
        let request = RaiCloseReconRequest {
            kind: RaiCloseVersionKind::Cut,
            epoch: 0,
            base_hash: BlockHash::from(1),
            target_hash: BlockHash::from(2),
        };
        assert_eq!(
            RaiCloseReconciler::make_delta(&state, &request),
            Err(RaiCloseReconMiss(request))
        );
    }
}
