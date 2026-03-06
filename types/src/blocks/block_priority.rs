use crate::{Amount, BlockPriority, BlockSideband, SavedBlock};
use std::cmp::max;

pub fn block_priority(block: &SavedBlock, previous_block: Option<&SavedBlock>) -> BlockPriority {
    block_priority_sideband(&block.sideband(), previous_block)
}

pub fn block_priority_sideband(
    sideband: &BlockSideband,
    previous_block: Option<&SavedBlock>,
) -> BlockPriority {
    let previous_balance = previous_block
        .as_ref()
        .map(|b| b.balance())
        .unwrap_or_default();

    // Handle full send case nicely where the balance would otherwise be 0
    let priority_balance = max(
        sideband.balance,
        if sideband.details.is_send {
            previous_balance
        } else {
            Amount::ZERO
        },
    );

    // Use previous block timestamp as priority timestamp for least recently used
    // prioritization within the same bucket
    // Account info timestamp is not used here because it will get out of sync when
    // rollbacks happen
    let priority_timestamp = previous_block
        .map(|b| b.timestamp())
        .unwrap_or(sideband.timestamp);

    BlockPriority::new(priority_balance, priority_timestamp.into())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{BlockDetails, BlockSideband, Epoch, StateBlockArgs, UnixMillisTimestamp};

    #[test]
    fn open_block() {
        let open = SavedBlock::new_test_open_block();

        let prio = block_priority(&open, None);

        assert_eq!(prio.balance, open.balance());
        assert_eq!(prio.time, open.timestamp().into());
    }

    #[test]
    fn receive_block() {
        let prev_timestamp = UnixMillisTimestamp::new_test_instance();
        let receive_balance = Amount::nano(2000);

        let prio = test_block_priority(
            receive_balance,
            prev_timestamp + Duration::from_secs(1),
            Some((Amount::nano(1000), prev_timestamp)),
        );

        assert_eq!(prio.balance, receive_balance);
        assert_eq!(prio.time, prev_timestamp.into());
    }

    #[test]
    fn send_block() {
        let prev_timestamp = UnixMillisTimestamp::new_test_instance();
        let prev_balance = Amount::nano(100);

        let prio = test_block_priority(
            Amount::nano(50),
            prev_timestamp + Duration::from_secs(1),
            Some((prev_balance, prev_timestamp)),
        );

        assert_eq!(prio.balance, prev_balance);
        assert_eq!(prio.time, prev_timestamp.into());
    }

    #[test]
    fn full_send() {
        let prev_timestamp = UnixMillisTimestamp::new_test_instance();
        let prev_balance = Amount::nano(100);

        let prio = test_block_priority(
            Amount::ZERO,
            prev_timestamp + Duration::from_secs(1),
            Some((prev_balance, prev_timestamp)),
        );

        assert_eq!(prio.balance, prev_balance);
        assert_eq!(prio.time, prev_timestamp.into());
    }

    #[test]
    fn change_block() {
        let prev_timestamp = UnixMillisTimestamp::new_test_instance();
        let prev_balance = Amount::nano(100);

        let prio = test_block_priority(
            prev_balance,
            prev_timestamp + Duration::from_secs(1),
            Some((prev_balance, prev_timestamp)),
        );

        assert_eq!(prio.balance, prev_balance);
        assert_eq!(prio.time, prev_timestamp.into());
    }

    fn test_block_priority(
        balance: Amount,
        timestamp: UnixMillisTimestamp,
        previous: Option<(Amount, UnixMillisTimestamp)>,
    ) -> BlockPriority {
        let previous = previous.map(|(prev_balance, prev_timestamp)| {
            create_block(prev_balance, prev_timestamp, false)
        });

        let is_send = if let Some(p) = &previous
            && p.balance() > balance
        {
            true
        } else {
            false
        };
        let block = create_block(balance, timestamp, is_send);
        block_priority(&block, previous.as_ref())
    }

    fn create_block(balance: Amount, timestamp: UnixMillisTimestamp, is_send: bool) -> SavedBlock {
        let sideband = BlockSideband {
            timestamp,
            balance,
            details: BlockDetails::new(Epoch::Epoch2, is_send, !is_send, false),
            ..BlockSideband::new_test_instance()
        };

        let block = StateBlockArgs {
            balance,
            ..StateBlockArgs::new_test_instance()
        }
        .into();

        SavedBlock::new(block, sideband)
    }
}
