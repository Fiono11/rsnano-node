use std::sync::Arc;

use rsnano_ledger::{AnySet, Ledger};
use std::collections::BTreeMap;

use rsnano_types::{Account, BlockHash, RaiCloseAttempt, RaiCloseRecord, RaiEpoch, RaiSlot};

use super::{
    CloseRecordEntries, RaiAdmissibilityError, RaiAdmissibilityValidator, RaiClosedSlotState,
};

pub struct LedgerRaiAdmissibilityValidator {
    ledger: Arc<Ledger>,
}

impl LedgerRaiAdmissibilityValidator {
    pub fn new(ledger: Arc<Ledger>) -> Self {
        Self { ledger }
    }
}

impl RaiAdmissibilityValidator for LedgerRaiAdmissibilityValidator {
    fn derive_close_frontiers(
        &self,
        epoch: RaiEpoch,
        previous_frontiers: &BTreeMap<Account, BlockHash>,
        entries: &CloseRecordEntries,
    ) -> Result<BTreeMap<Account, BlockHash>, RaiAdmissibilityError> {
        // The certified RAI ledger is derived only from the preceding certified
        // frontier version and the outcomes in the decided close cut. Importing
        // the ordinary node ledger here makes the close hash depend on which
        // unrelated confirmations happened to reach this replica before its
        // epoch timer fired.
        let mut frontiers = previous_frontiers.clone();
        for (slot, state) in entries {
            match state {
                RaiClosedSlotState::Finalized(hash) | RaiClosedSlotState::Carry(hash) => {
                    self.validate_slot_block(*slot, epoch, hash)?;
                    frontiers.insert(slot.account, *hash);
                }
                RaiClosedSlotState::Released => {}
            }
        }
        Ok(frontiers)
    }

    fn validate_slot_block(
        &self,
        slot: RaiSlot,
        _epoch: RaiEpoch,
        block_hash: &BlockHash,
    ) -> Result<(), RaiAdmissibilityError> {
        let Some(block) = self.ledger.any().get_block(block_hash) else {
            return Err(RaiAdmissibilityError::InadmissibleSlotBlock);
        };

        if block.account() != slot.account || block.height() != slot.account_height {
            return Err(RaiAdmissibilityError::InadmissibleSlotBlock);
        }

        Ok(())
    }

    fn validate_close_record(
        &self,
        record: &RaiCloseRecord,
        entries: &CloseRecordEntries,
        _attempt: RaiCloseAttempt,
        hash: &BlockHash,
    ) -> Result<(), RaiAdmissibilityError> {
        if record.hash() != *hash {
            return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
        }

        let any = self.ledger.any();
        for (account, frontier) in &record.frontiers {
            let Some(block) = any.get_block(frontier) else {
                return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
            };
            if block.account() != *account {
                return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
            }
        }

        for (slot, state) in entries {
            match state {
                RaiClosedSlotState::Finalized(hash) | RaiClosedSlotState::Carry(hash) => {
                    let Some(frontier) = record.frontiers.get(&slot.account) else {
                        return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
                    };
                    let Some(mut block) = any.get_block(frontier) else {
                        return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
                    };
                    while block.height() > slot.account_height {
                        let previous = block.previous();
                        if previous.is_zero() {
                            return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
                        }
                        block = any
                            .get_block(&previous)
                            .ok_or(RaiAdmissibilityError::MissingCloseRecordPackage)?;
                    }
                    if block.height() != slot.account_height || block.hash() != *hash {
                        return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
                    }
                }
                RaiClosedSlotState::Released => {
                    if let Some(frontier) = record.frontiers.get(&slot.account)
                        && let Some(mut block) = any.get_block(frontier)
                    {
                        while block.height() > slot.account_height {
                            let previous = block.previous();
                            if previous.is_zero() {
                                break;
                            }
                            block = any
                                .get_block(&previous)
                                .ok_or(RaiAdmissibilityError::MissingCloseRecordPackage)?;
                        }
                        if block.height() == slot.account_height {
                            return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{Account, RaiCloseRecord};
    use std::collections::BTreeMap;

    #[test]
    fn rejects_unknown_slot_block() {
        let validator = LedgerRaiAdmissibilityValidator::new(Arc::new(Ledger::new_null()));

        let result = validator.validate_slot_block(RaiSlot::default(), 0, &BlockHash::from(1));

        assert_eq!(result, Err(RaiAdmissibilityError::InadmissibleSlotBlock));
    }

    #[test]
    fn epoch_zero_frontiers_are_derived_only_from_the_close_cut() {
        let validator = LedgerRaiAdmissibilityValidator::new(Arc::new(Ledger::new_null()));

        let frontiers = validator
            .derive_close_frontiers(0, &BTreeMap::new(), &CloseRecordEntries::new())
            .unwrap();

        assert!(frontiers.is_empty());
    }

    #[test]
    fn rejects_close_record_with_unknown_frontier() {
        let validator = LedgerRaiAdmissibilityValidator::new(Arc::new(Ledger::new_null()));
        let record = RaiCloseRecord::new(
            0,
            BlockHash::ZERO,
            [(Account::from(1), BlockHash::from(2))]
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
        );

        assert_eq!(
            validator.validate_close_record(&record, &CloseRecordEntries::new(), 0, &record.hash()),
            Err(RaiAdmissibilityError::MissingCloseRecordPackage)
        );
    }
}
