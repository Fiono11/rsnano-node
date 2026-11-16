use std::{collections::HashMap, time::Duration};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::BlockHash;

use crate::consensus::Unattached;

/// RAI: what the ledger said about a block's attachment, kept between the
/// voter's ticks. The voter scans every election each tick, and an instance
/// that has not first voted asks the ledger again each time: a replica that
/// fell behind holds thousands of them - held at the epoch gate, waiting for
/// a parent - and the reads took its whole tick, so it voted later still.
///
/// An answer is reused for an interval that doubles while the answer stays
/// the same, from one tick up to `MAX_INTERVAL`, and all of them are dropped
/// when the checkpoint blocks attach to changes. Only the ledger's part is
/// kept: what the container adds to it (a complete parent of the current
/// epoch, a closed lock, the epoch gate) is decided anew each tick.
pub(crate) struct AttachmentMemo {
    entries: HashMap<BlockHash, Entry>,
    /// Checks answered by the ledger and from the memo since `take_counts`
    asked: u64,
    reused: u64,
}

struct Entry {
    result: Result<(), Unattached>,
    checked: Timestamp,
    interval: Duration,
}

impl AttachmentMemo {
    /// The voter's tick: an answer is asked for again on the next one first
    const FIRST_INTERVAL: Duration = Duration::from_millis(20);
    /// The longest an answer is reused, which is how much later than now a
    /// block is first voted once its parent became final
    const MAX_INTERVAL: Duration = Duration::from_millis(100);
    /// An entry not asked for this long belongs to an election that is gone
    const UNUSED_AFTER: Duration = Duration::from_secs(2);

    pub(crate) fn new() -> Self {
        Self {
            entries: HashMap::new(),
            asked: 0,
            reused: 0,
        }
    }

    /// The attachment of a block: the answer held for it while it is fresh,
    /// what `ask` says otherwise
    pub(crate) fn check(
        &mut self,
        hash: &BlockHash,
        now: Timestamp,
        ask: impl FnOnce() -> Result<(), Unattached>,
    ) -> Result<(), Unattached> {
        if let Some(entry) = self.entries.get(hash)
            && entry.checked.elapsed(now) < entry.interval
        {
            self.reused += 1;
            return entry.result;
        }
        self.asked += 1;
        let result = ask();
        let interval = match self.entries.get(hash) {
            Some(entry) if entry.result == result => (entry.interval * 2).min(Self::MAX_INTERVAL),
            _ => Self::FIRST_INTERVAL,
        };
        self.entries.insert(
            *hash,
            Entry {
                result,
                checked: now,
                interval,
            },
        );
        result
    }

    /// The checkpoint blocks attach to changed: every answer is asked for
    /// again
    pub(crate) fn reset(&mut self) {
        self.entries.clear();
    }

    /// Drops the answers no election asks for any more
    pub(crate) fn cleanup(&mut self, now: Timestamp) {
        self.entries
            .retain(|_, entry| entry.checked.elapsed(now) < Self::UNUSED_AFTER);
    }

    /// How many checks the ledger answered and how many the memo did since
    /// the last call
    pub(crate) fn take_counts(&mut self) -> (u64, u64) {
        (
            std::mem::take(&mut self.asked),
            std::mem::take(&mut self.reused),
        )
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn an_answer_is_reused_within_its_interval() {
        let mut memo = AttachmentMemo::new();
        let asked = Cell::new(0);
        let hash = BlockHash::from(1);
        let start = Timestamp::new_test_instance();

        assert_eq!(
            check(&mut memo, &hash, start, &asked, UNATTACHED),
            UNATTACHED
        );
        assert_eq!(
            check(&mut memo, &hash, start + ms(19), &asked, Ok(())),
            UNATTACHED
        );

        assert_eq!(asked.get(), 1);
        assert_eq!(memo.take_counts(), (1, 1));
    }

    #[test]
    fn an_unchanged_answer_is_asked_for_less_and_less_often() {
        let mut memo = AttachmentMemo::new();
        let asked = Cell::new(0);
        let hash = BlockHash::from(1);
        let start = Timestamp::new_test_instance();

        // One check per voter tick for a second
        let asked_at: Vec<u64> = (0..50)
            .filter(|tick| {
                let before = asked.get();
                check(&mut memo, &hash, start + ms(tick * 20), &asked, UNATTACHED).unwrap_err();
                asked.get() > before
            })
            .map(|tick| tick * 20)
            .collect();

        assert_eq!(asked_at[..5], [0, 20, 60, 140, 240]);
        assert_eq!(asked_at.len(), 12);
    }

    #[test]
    fn a_changed_answer_is_asked_for_again_on_the_next_tick() {
        let mut memo = AttachmentMemo::new();
        let asked = Cell::new(0);
        let hash = BlockHash::from(1);
        let start = Timestamp::new_test_instance();
        for tick in 0..8 {
            let _ = check(&mut memo, &hash, start + ms(tick * 20), &asked, UNATTACHED);
        }
        let attached_at = start + ms(240);

        assert_eq!(check(&mut memo, &hash, attached_at, &asked, Ok(())), Ok(()));
        let before = asked.get();
        assert_eq!(
            check(&mut memo, &hash, attached_at + ms(20), &asked, UNATTACHED),
            UNATTACHED
        );

        assert_eq!(asked.get(), before + 1);
    }

    #[test]
    fn a_reset_asks_for_every_answer_again() {
        let mut memo = AttachmentMemo::new();
        let asked = Cell::new(0);
        let hash = BlockHash::from(1);
        let start = Timestamp::new_test_instance();
        let _ = check(&mut memo, &hash, start, &asked, UNATTACHED);

        memo.reset();

        assert_eq!(
            check(&mut memo, &hash, start + ms(1), &asked, Ok(())),
            Ok(())
        );
        assert_eq!(asked.get(), 2);
    }

    #[test]
    fn answers_no_longer_asked_for_are_dropped() {
        let mut memo = AttachmentMemo::new();
        let asked = Cell::new(0);
        let start = Timestamp::new_test_instance();
        let _ = check(&mut memo, &BlockHash::from(1), start, &asked, UNATTACHED);
        let _ = check(
            &mut memo,
            &BlockHash::from(2),
            start + ms(1500),
            &asked,
            UNATTACHED,
        );

        memo.cleanup(start + ms(2100));

        assert_eq!(memo.len(), 1);
    }

    /* Test helpers */

    const UNATTACHED: Result<(), Unattached> = Err(Unattached::Previous);

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// A check whose ledger answer is `answer`, counted in `asked`
    fn check(
        memo: &mut AttachmentMemo,
        hash: &BlockHash,
        now: Timestamp,
        asked: &Cell<u32>,
        answer: Result<(), Unattached>,
    ) -> Result<(), Unattached> {
        memo.check(hash, now, || {
            asked.set(asked.get() + 1);
            answer
        })
    }
}
