use std::collections::BTreeMap;

use rsnano_types::{
    Account, BlockHash, RaiCloseAttempt, RaiCloseRecord, RaiElectionId, RaiElectionValue, RaiEpoch,
    RaiSlot,
};

use super::{CloseRecordEntries, RaiCloseState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiAdmissibilityError {
    IncompatibleElectionValue,
    InadmissibleSlotBlock,
    UnknownCloseCut,
    IncompleteCloseCut,
    MissingCloseRecordPackage,
}

pub trait RaiAdmissibilityValidator: Send + Sync {
    fn derive_close_frontiers(
        &self,
        _epoch: RaiEpoch,
        previous_frontiers: &BTreeMap<Account, BlockHash>,
        entries: &CloseRecordEntries,
    ) -> Result<BTreeMap<Account, BlockHash>, RaiAdmissibilityError> {
        let mut frontiers = previous_frontiers.clone();
        for (slot, state) in entries {
            match state {
                super::RaiClosedSlotState::Finalized(hash)
                | super::RaiClosedSlotState::Carry(hash) => {
                    frontiers.insert(slot.account, *hash);
                }
                super::RaiClosedSlotState::Released => {}
            }
        }
        Ok(frontiers)
    }

    fn validate_slot_block(
        &self,
        _slot: RaiSlot,
        _epoch: RaiEpoch,
        _block_hash: &BlockHash,
    ) -> Result<(), RaiAdmissibilityError> {
        Ok(())
    }

    fn validate_close_record(
        &self,
        _record: &RaiCloseRecord,
        _entries: &CloseRecordEntries,
        _attempt: RaiCloseAttempt,
        _hash: &BlockHash,
    ) -> Result<(), RaiAdmissibilityError> {
        Ok(())
    }
}

#[derive(Default)]
pub struct RaiDefaultAdmissibilityValidator;

impl RaiAdmissibilityValidator for RaiDefaultAdmissibilityValidator {}

pub struct RaiAdmissibility<'a> {
    close_state: &'a RaiCloseState,
    validator: &'a dyn RaiAdmissibilityValidator,
}

impl<'a> RaiAdmissibility<'a> {
    pub fn new(
        close_state: &'a RaiCloseState,
        validator: &'a dyn RaiAdmissibilityValidator,
    ) -> Self {
        Self {
            close_state,
            validator,
        }
    }

    pub fn validate(
        &self,
        election_id: &RaiElectionId,
        value: &RaiElectionValue,
    ) -> Result<(), RaiAdmissibilityError> {
        match (election_id, value) {
            (RaiElectionId::Slot { slot, epoch }, RaiElectionValue::Block(hash)) => {
                self.validator.validate_slot_block(*slot, *epoch, hash)
            }
            (RaiElectionId::Slot { .. }, RaiElectionValue::Timeout) => Ok(()),
            (RaiElectionId::CloseCut { epoch, .. }, RaiElectionValue::CloseCutHash(hash)) => {
                if self.close_state.close_value(*epoch, hash).is_none() {
                    return Err(RaiAdmissibilityError::UnknownCloseCut);
                }

                Ok(())
            }
            (RaiElectionId::CloseCut { .. }, RaiElectionValue::Timeout) => Ok(()),
            (
                RaiElectionId::CloseRecord { epoch, attempt },
                RaiElectionValue::CloseRecordHash(hash),
            ) => {
                let Some(package) = self.close_state.close_record_value(*epoch, hash) else {
                    return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
                };
                if package.record.epoch != *epoch || package.record.hash() != *hash {
                    return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
                }

                self.validator.validate_close_record(
                    &package.record,
                    &package.entries,
                    *attempt,
                    hash,
                )
            }
            (RaiElectionId::CloseRecord { .. }, RaiElectionValue::Timeout) => Ok(()),
            _ => Err(RaiAdmissibilityError::IncompatibleElectionValue),
        }
    }
}
