use std::sync::Arc;

use rsnano_ledger::{AnySet, Ledger};
use rsnano_types::{BlockHash, RaiCloseAttempt, RaiEpoch, RaiSlot};

use super::{RaiAdmissibilityError, RaiAdmissibilityValidator};

pub struct LedgerRaiAdmissibilityValidator {
    ledger: Arc<Ledger>,
}

impl LedgerRaiAdmissibilityValidator {
    pub fn new(ledger: Arc<Ledger>) -> Self {
        Self { ledger }
    }
}

impl RaiAdmissibilityValidator for LedgerRaiAdmissibilityValidator {
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
        _epoch: RaiEpoch,
        _attempt: RaiCloseAttempt,
        _hash: &BlockHash,
    ) -> Result<(), RaiAdmissibilityError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_slot_block() {
        let validator = LedgerRaiAdmissibilityValidator::new(Arc::new(Ledger::new_null()));

        let result = validator.validate_slot_block(RaiSlot::default(), 0, &BlockHash::from(1));

        assert_eq!(result, Err(RaiAdmissibilityError::InadmissibleSlotBlock));
    }
}
