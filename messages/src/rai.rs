use bitvec::prelude::BitArray;
use rsnano_types::{RaiPendingReport, RaiVote};

use crate::MessageVariant;

impl MessageVariant for RaiVote {}

impl MessageVariant for RaiPendingReport {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        BitArray::new(self.slots.len() as u16)
    }
}

pub(crate) fn rai_pending_report_count(extensions: BitArray<u16>) -> usize {
    extensions.data as usize
}

#[cfg(test)]
mod tests {
    use crate::{Message, assert_deserializable};
    use rsnano_types::{
        Account, BlockHash, PrivateKey, RaiElectionId, RaiElectionValue, RaiPendingReport, RaiSlot,
        RaiVote,
    };

    #[test]
    fn serialize_rai_vote() {
        let key = PrivateKey::from(1);
        let vote = RaiVote::new_first(
            &key,
            RaiElectionId::Slot {
                slot: RaiSlot::new(Account::from(2), 3),
                epoch: 4,
            },
            RaiElectionValue::Block(BlockHash::from(5)),
        );

        assert_deserializable(&Message::RaiVote(vote));
    }

    #[test]
    fn serialize_rai_pending_report() {
        let key = PrivateKey::from(1);
        let report = RaiPendingReport::new(
            &key,
            2,
            vec![
                RaiSlot::new(Account::from(3), 4),
                RaiSlot::new(Account::from(5), 6),
            ],
        );

        assert_deserializable(&Message::RaiPendingReport(report));
    }

    #[test]
    fn serialize_large_rai_pending_report() {
        let key = PrivateKey::from(1);
        let slots = (0..2850)
            .map(|height| RaiSlot::new(Account::from(height + 1), height as u64))
            .collect();
        let report = RaiPendingReport::new(&key, 2, slots);

        assert_deserializable(&Message::RaiPendingReport(report));
    }
}
