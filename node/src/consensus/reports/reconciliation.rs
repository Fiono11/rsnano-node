use std::time::Duration;

use rsnano_messages::{Report, ReportAck, ReportPart, ReportPayload, ReportReq};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::BlockHash;

use crate::consensus::election::{ReportKey, ReportKind, VoteReport};

/// RAI, Section 6.2: where a reconciliation against one signed report root
/// stands. It compares the roots, asks for the bucket digests, and fetches
/// the entries of the buckets that differ. It ends when the reconstructed
/// map hashes to the signed root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReconcileStep {
    /// The local map already hashes to the signed root: nothing to recover
    Done,
    /// Waiting for the 256 bucket digests
    Digests,
    /// Waiting for the entries of the buckets that differ
    Buckets,
    /// Every part was taken but the map does not hash to the signed root:
    /// the reporter served something else than it signed
    Failed,
}

pub(crate) struct Reconciliation {
    report: Report,
    /// The map as reconciled so far, starting from this node's own votes
    map: VoteReport,
    step: ReconcileStep,
    /// The buckets still to fetch
    pending: Vec<u8>,
    /// The part asked for last and when, to repeat an unanswered request
    asked: Option<(ReportPart, Timestamp)>,
    /// What the reconciliation cost: the parts asked for and the entries
    /// taken. The dictionary is a fixed partition of the key space, so a
    /// difference spread over many slots touches many buckets; this is how
    /// that cost is measured against a real workload.
    requests: usize,
    entries_taken: usize,
}

#[allow(dead_code)] // root() and report() are read by the close, Section 9.1
impl Reconciliation {
    pub fn new(report: Report, base: VoteReport) -> Self {
        let step = if base.root() == report.root {
            ReconcileStep::Done
        } else {
            ReconcileStep::Digests
        };
        Self {
            report,
            map: base,
            step,
            pending: Vec::new(),
            asked: None,
            requests: 0,
            entries_taken: 0,
        }
    }

    /// How many parts were asked for and how many entries were taken
    pub fn cost(&self) -> (usize, usize) {
        (self.requests, self.entries_taken)
    }

    pub fn root(&self) -> BlockHash {
        self.report.root
    }

    pub fn report(&self) -> &Report {
        &self.report
    }

    /// The map as reconciled. Only meaningful once complete: before that it
    /// is this node's own votes plus what has been recovered.
    pub fn map(&self) -> &VoteReport {
        &self.map
    }

    pub fn is_complete(&self) -> bool {
        self.step == ReconcileStep::Done
    }

    pub fn has_failed(&self) -> bool {
        self.step == ReconcileStep::Failed
    }

    /// Whether this reconciliation has been started, i.e. a part was asked
    /// for. A stored report is reconciled on demand, not on arrival.
    pub fn is_started(&self) -> bool {
        self.requests > 0 || self.is_complete() || self.has_failed()
    }

    /// Whether the last request has gone unanswered for too long
    pub fn is_overdue(&self, now: Timestamp, timeout: Duration) -> bool {
        self.asked
            .is_some_and(|(_, asked)| asked.elapsed(now) >= timeout)
    }

    /// The next part to ask the reporter for, if the reconciliation is not
    /// done. Repeating it is the same request again.
    pub fn next_request(&mut self, now: Timestamp) -> Option<ReportReq> {
        let part = self.next_part()?;
        self.requests += 1;
        self.asked = Some((part, now));
        Some(ReportReq {
            epoch: self.report.epoch,
            reporter: self.report.reporter,
            part,
        })
    }

    fn next_part(&self) -> Option<ReportPart> {
        match self.step {
            ReconcileStep::Done | ReconcileStep::Failed => None,
            ReconcileStep::Digests => Some(ReportPart::Digests),
            ReconcileStep::Buckets => self.pending.last().map(|b| ReportPart::Bucket(*b)),
        }
    }

    /// Takes one answer. Every entry is checked against its key and the map
    /// is checked against the signed root at the end (Lemma 6.1).
    pub fn absorb(&mut self, ack: &ReportAck) {
        if ack.epoch != self.report.epoch || ack.reporter != self.report.reporter {
            return;
        }
        if self.asked.is_none_or(|(part, _)| part != ack.part) {
            // An answer to a part this reconciliation is not waiting for
            return;
        }
        self.asked = None;
        match (&ack.part, &ack.payload) {
            (ReportPart::Digests, ReportPayload::Digests(digests)) => {
                self.pending = self.map.differing_buckets(digests);
                self.step = ReconcileStep::Buckets;
            }
            (ReportPart::Bucket(bucket), ReportPayload::Entries(entries)) => {
                self.entries_taken += entries.len();
                let taken = self.map.absorb_bucket(
                    *bucket,
                    entries.iter().map(|entry| {
                        (
                            ReportKey::new(
                                entry.account,
                                entry.height,
                                if entry.is_final {
                                    ReportKind::Final
                                } else {
                                    ReportKind::First
                                },
                            ),
                            entry.hash,
                        )
                    }),
                );
                self.pending.retain(|pending| pending != bucket);
                if !taken {
                    self.step = ReconcileStep::Failed;
                    return;
                }
            }
            // An answer whose payload does not match the part asked for
            _ => {
                self.step = ReconcileStep::Failed;
                return;
            }
        }
        self.advance();
    }

    /// Where to go once a part has been taken: the buckets left, then the
    /// root check
    fn advance(&mut self) {
        if !self.pending.is_empty() {
            self.step = ReconcileStep::Buckets;
            return;
        }
        // Every differing bucket was taken: the map must now hash to the
        // root the reporter signed
        self.step = if self.map.root() == self.report.root {
            ReconcileStep::Done
        } else {
            ReconcileStep::Failed
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{Account, ConsensusEpoch, PublicKey, Signature};

    /// A report whose root this node's own map already hashes to needs no
    /// reconciliation at all
    #[test]
    fn an_identical_map_is_done_at_once() {
        let map = map_of(0..20);
        let mut reconciliation = Reconciliation::new(report(map.root()), map.clone());
        assert!(reconciliation.is_complete());
        assert_eq!(reconciliation.next_request(now()), None);
        assert_eq!(reconciliation.map().len(), map.len());
    }

    /// Only the buckets that differ are fetched, and each one whole
    #[test]
    fn only_the_differing_buckets_are_fetched() {
        let theirs = map_of(0..20);
        let ours = map_of(0..19);
        let mut reconciliation = Reconciliation::new(report(theirs.root()), ours);
        assert!(!reconciliation.is_complete());

        let request = reconciliation.next_request(now()).unwrap();
        assert_eq!(request.part, ReportPart::Digests);
        reconciliation.absorb(&digests(&theirs));

        // One entry differs, so one bucket does
        assert_eq!(reconciliation.pending.len(), 1);
        let bucket = reconciliation.pending[0];
        let request = reconciliation.next_request(now()).unwrap();
        assert_eq!(request.part, ReportPart::Bucket(bucket));
        reconciliation.absorb(&entries(bucket, &theirs));

        assert!(reconciliation.is_complete());
        assert_eq!(reconciliation.map().root(), theirs.root());
        assert_eq!(
            reconciliation.cost(),
            (2, theirs.bucket_entries(bucket).len())
        );
    }

    /// Lemma 6.1: a reporter that serves entries which do not rebuild the
    /// root it signed is refused
    #[test]
    fn a_map_that_does_not_rebuild_the_signed_root_fails() {
        let theirs = map_of(0..20);
        let ours = map_of(0..19);
        let mut reconciliation = Reconciliation::new(report(BlockHash::from(12345)), ours);
        reconciliation.absorb(&digests(&theirs));
        // Nothing was taken from an answer this reconciliation did not ask for
        assert!(!reconciliation.has_failed());

        let mut step = reconciliation.next_request(now());
        let mut rounds = 0;
        while let Some(request) = step {
            match request.part {
                ReportPart::Digests => reconciliation.absorb(&digests(&theirs)),
                ReportPart::Bucket(bucket) => reconciliation.absorb(&entries(bucket, &theirs)),
            }
            step = reconciliation.next_request(now());
            rounds += 1;
            assert!(rounds < 300);
        }
        // The entries were served honestly but the signed root was a lie
        assert!(reconciliation.has_failed());
        assert!(!reconciliation.is_complete());
    }

    /// An answer for a part the reconciliation is not waiting for changes
    /// nothing
    #[test]
    fn an_unexpected_answer_is_ignored() {
        let theirs = map_of(0..20);
        let mut reconciliation = Reconciliation::new(report(theirs.root()), map_of(0..19));
        reconciliation.next_request(now());
        reconciliation.absorb(&entries(0, &theirs));
        assert_eq!(reconciliation.step, ReconcileStep::Digests);
        assert!(!reconciliation.has_failed());
    }

    /// A request is overdue once its answer has not come within the timeout
    #[test]
    fn a_request_becomes_overdue() {
        let theirs = map_of(0..20);
        let mut reconciliation = Reconciliation::new(report(theirs.root()), map_of(0..19));
        let start = now();
        reconciliation.next_request(start);
        assert!(!reconciliation.is_overdue(start, Duration::from_secs(1)));
        assert!(reconciliation.is_overdue(start + Duration::from_secs(1), Duration::from_secs(1)));
        // An answer clears it
        reconciliation.absorb(&digests(&theirs));
        assert!(
            !reconciliation.is_overdue(start + Duration::from_secs(10), Duration::from_secs(1))
        );
    }

    /*
     * Test helpers
     */

    fn now() -> Timestamp {
        Timestamp::new_test_instance()
    }

    fn map_of(accounts: std::ops::Range<u64>) -> VoteReport {
        let mut map = VoteReport::new(ConsensusEpoch::ZERO);
        for i in accounts {
            map.add(
                ReportKey::new(Account::from(i), 1, ReportKind::First),
                BlockHash::from(i),
            );
        }
        map
    }

    fn report(root: BlockHash) -> Report {
        Report {
            epoch: ConsensusEpoch::ZERO,
            committee: BlockHash::from(7),
            root,
            reporter: PublicKey::from(1),
            signature: Signature::from_bytes([0; 64]),
        }
    }

    fn digests(map: &VoteReport) -> ReportAck {
        ReportAck {
            epoch: ConsensusEpoch::ZERO,
            reporter: PublicKey::from(1),
            part: ReportPart::Digests,
            payload: ReportPayload::Digests(map.bucket_digests()),
        }
    }

    fn entries(bucket: u8, map: &VoteReport) -> ReportAck {
        ReportAck {
            epoch: ConsensusEpoch::ZERO,
            reporter: PublicKey::from(1),
            part: ReportPart::Bucket(bucket),
            payload: ReportPayload::Entries(
                map.bucket_entries(bucket)
                    .into_iter()
                    .map(|(key, hash)| rsnano_messages::ReportEntry {
                        account: key.account,
                        height: key.height,
                        is_final: key.kind == ReportKind::Final,
                        hash,
                    })
                    .collect(),
            ),
        }
    }
}
