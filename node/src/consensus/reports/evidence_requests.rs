use std::{collections::HashMap, time::Duration};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, ConsensusEpoch};

/// RAI: which missing certificate evidence a node asks for now, and whom.
///
/// A report is checked again every `ReportExchange::RETRY_INTERVAL`, and each
/// check used to ask every principal representative for every hash still
/// missing. Across wide-area links the replies - every signed vote a peer
/// holds for each hash - take longer than that, so the same evidence was
/// asked for again and again while it was on its way, the replies overfilled
/// the vote queues and the evidence was dropped (a six-region run's close
/// waited 117 s for one node's reports to verify). Now a hash is asked first
/// of the reporter alone, which certified the block and holds its votes, and
/// asked again of every representative - the reporter may be faulty - only
/// after a delay that doubles each time.
pub(crate) struct EvidenceRequests {
    first_retry: Duration,
    longest_retry: Duration,
    asked: HashMap<(ConsensusEpoch, BlockHash), Asked>,
}

struct Asked {
    retries: u32,
    next_at: Timestamp,
}

/// The hashes to ask for now: of the reporter, and of every representative
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct DueRequests {
    pub reporter: Vec<BlockHash>,
    pub everyone: Vec<BlockHash>,
}

impl EvidenceRequests {
    /// The longest wait between two requests for the same evidence
    const LONGEST_RETRY: Duration = Duration::from_secs(5);

    pub(crate) fn new(first_retry: Duration) -> Self {
        Self {
            first_retry,
            longest_retry: Self::LONGEST_RETRY.max(first_retry),
            asked: HashMap::new(),
        }
    }

    /// Of the hashes a report still lacks evidence for, those due a request now
    pub(crate) fn due(
        &mut self,
        epoch: ConsensusEpoch,
        missing: &[BlockHash],
        now: Timestamp,
    ) -> DueRequests {
        let mut due = DueRequests::default();
        for hash in missing {
            match self.asked.get_mut(&(epoch, *hash)) {
                None => {
                    self.asked.insert(
                        (epoch, *hash),
                        Asked {
                            retries: 0,
                            next_at: now + self.first_retry,
                        },
                    );
                    due.reporter.push(*hash);
                }
                Some(asked) if now >= asked.next_at => {
                    asked.retries += 1;
                    let wait = self
                        .first_retry
                        .saturating_mul(1 << asked.retries.min(16))
                        .min(self.longest_retry);
                    asked.next_at = now + wait;
                    due.everyone.push(*hash);
                }
                Some(_) => {}
            }
        }
        due
    }

    /// Forgets the requests of the epochs before `epoch`
    pub(crate) fn forget_before(&mut self, epoch: ConsensusEpoch) {
        self.asked.retain(|(asked_in, _), _| *asked_in >= epoch);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.asked.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_evidence_is_asked_of_the_reporter_first() {
        let mut requests = EvidenceRequests::new(Duration::from_millis(300));
        let now = Timestamp::new_test_instance();
        let due = requests.due(epoch(), &[hash(1), hash(2)], now);
        assert_eq!(due.reporter, vec![hash(1), hash(2)]);
        assert!(due.everyone.is_empty());
    }

    #[test]
    fn evidence_on_its_way_is_not_asked_for_again() {
        let mut requests = EvidenceRequests::new(Duration::from_millis(300));
        let now = Timestamp::new_test_instance();
        requests.due(epoch(), &[hash(1)], now);
        let due = requests.due(epoch(), &[hash(1)], now + Duration::from_millis(299));
        assert_eq!(due, DueRequests::default());
    }

    /// A reporter that does not answer may be faulty: the retry goes to
    /// every representative, each one after twice the wait of the last
    #[test]
    fn retries_go_to_everyone_with_a_doubling_wait() {
        let mut requests = EvidenceRequests::new(Duration::from_millis(300));
        let start = Timestamp::new_test_instance();
        requests.due(epoch(), &[hash(1)], start);

        let first_retry = start + Duration::from_millis(300);
        let due = requests.due(epoch(), &[hash(1)], first_retry);
        assert_eq!(due.everyone, vec![hash(1)]);

        let early = first_retry + Duration::from_millis(599);
        assert_eq!(
            requests.due(epoch(), &[hash(1)], early),
            DueRequests::default()
        );
        let second_retry = first_retry + Duration::from_millis(600);
        assert_eq!(
            requests.due(epoch(), &[hash(1)], second_retry).everyone,
            vec![hash(1)]
        );
    }

    #[test]
    fn the_wait_stops_growing_at_five_seconds() {
        let mut requests = EvidenceRequests::new(Duration::from_millis(300));
        let mut now = Timestamp::new_test_instance();
        requests.due(epoch(), &[hash(1)], now);
        for _ in 0..10 {
            now = now + Duration::from_secs(5);
            assert_eq!(
                requests.due(epoch(), &[hash(1)], now).everyone,
                vec![hash(1)]
            );
        }
    }

    #[test]
    fn the_requests_of_older_epochs_are_forgotten() {
        let mut requests = EvidenceRequests::new(Duration::from_millis(300));
        let now = Timestamp::new_test_instance();
        requests.due(ConsensusEpoch::new(1), &[hash(1)], now);
        requests.due(ConsensusEpoch::new(2), &[hash(1)], now);
        requests.forget_before(ConsensusEpoch::new(2));
        assert_eq!(requests.len(), 1);
        let due = requests.due(ConsensusEpoch::new(1), &[hash(1)], now);
        assert_eq!(due.reporter, vec![hash(1)]);
    }

    /*
     * Test helpers
     */

    fn epoch() -> ConsensusEpoch {
        ConsensusEpoch::new(3)
    }

    fn hash(n: u64) -> BlockHash {
        BlockHash::from(n)
    }
}
