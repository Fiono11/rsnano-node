use rsnano_types::{
    BlockHash, RaiCloseAttempt, RaiElectionId, RaiElectionValue, RaiEpoch, RaiSlot,
};

use super::RaiCloseState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiAdmissibilityError {
    IncompatibleElectionValue,
    InadmissibleSlotBlock,
    UnknownCloseCut,
    IncompleteCloseCut,
    MissingCloseRecordPackage,
}

pub trait RaiAdmissibilityValidator: Send + Sync {
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
        _epoch: RaiEpoch,
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
                let Some(close_value) = self.close_state.close_value(*epoch, hash) else {
                    return Err(RaiAdmissibilityError::UnknownCloseCut);
                };

                if self
                    .close_state
                    .visible_slots(*epoch)
                    .is_some_and(|visible| visible.iter().all(|slot| close_value.contains(slot)))
                {
                    return Ok(());
                }

                Err(RaiAdmissibilityError::IncompleteCloseCut)
            }
            (RaiElectionId::CloseCut { .. }, RaiElectionValue::Timeout) => Ok(()),
            (
                RaiElectionId::CloseRecord { epoch, attempt },
                RaiElectionValue::CloseRecordHash(hash),
            ) => {
                if !self.close_state.has_close_record_value(*epoch, hash) {
                    return Err(RaiAdmissibilityError::MissingCloseRecordPackage);
                }

                self.validator.validate_close_record(*epoch, *attempt, hash)
            }
            (RaiElectionId::CloseRecord { .. }, RaiElectionValue::Timeout) => Ok(()),
            _ => Err(RaiAdmissibilityError::IncompatibleElectionValue),
        }
    }
}
