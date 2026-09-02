use rsnano_ledger::LedgerEvent;
use std::{collections::HashMap, sync::Mutex};

/// Tracks confirmed-block notifications between ledger commit and AEC epoch accounting.
#[derive(Default)]
pub struct EpochCementationTracker {
    pending: Mutex<HashMap<u64, usize>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{BlockHash, SavedBlock};

    #[test]
    fn remains_pending_until_every_confirmation_is_applied() {
        let tracker = EpochCementationTracker::default();
        let blocks = vec![
            (
                SavedBlock::new_test_instance_with_key(1),
                BlockHash::from(11),
                1,
            ),
            (
                SavedBlock::new_test_instance_with_key(2),
                BlockHash::from(12),
                1,
            ),
            (
                SavedBlock::new_test_instance_with_key(3),
                BlockHash::from(13),
                2,
            ),
        ];
        tracker.event_enqueued(&LedgerEvent::BlocksConfirmed(blocks.clone()));

        assert_eq!(tracker.pending(1), 2);
        assert_eq!(tracker.pending(2), 1);
        tracker.confirmations_applied(&blocks[..1]);
        assert_eq!(tracker.pending(1), 1);
        tracker.confirmations_applied(&blocks[1..]);
        assert_eq!(tracker.pending(1), 0);
        assert_eq!(tracker.pending(2), 0);
    }
}

impl EpochCementationTracker {
    pub fn event_enqueued(&self, event: &LedgerEvent) {
        let LedgerEvent::BlocksConfirmed(blocks) = event else {
            return;
        };
        let mut pending = self.pending.lock().unwrap();
        for (_, _, epoch) in blocks {
            if *epoch > 0 {
                *pending.entry(*epoch).or_default() += 1;
            }
        }
    }

    pub fn confirmations_applied(
        &self,
        blocks: &[(rsnano_types::SavedBlock, rsnano_types::BlockHash, u64)],
    ) {
        let mut pending = self.pending.lock().unwrap();
        for (_, _, epoch) in blocks {
            if *epoch == 0 {
                continue;
            }
            let count = pending
                .get_mut(epoch)
                .expect("applied epoch confirmation was not tracked");
            *count = count
                .checked_sub(1)
                .expect("applied more epoch confirmations than were enqueued");
            if *count == 0 {
                pending.remove(epoch);
            }
        }
    }

    pub fn pending(&self, epoch: u64) -> usize {
        self.pending
            .lock()
            .unwrap()
            .get(&epoch)
            .copied()
            .unwrap_or_default()
    }
}
