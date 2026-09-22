mod reconciliation;
mod report_plugin;
mod report_service;

pub(crate) use reconciliation::Reconciliation;
pub(crate) use report_plugin::{ReportPlugin, ReportTicker};
pub use report_service::ReportService;

use std::collections::{BTreeMap, HashMap};

use rsnano_messages::{Report, ReportAck, ReportEntry, ReportPart, ReportPayload, ReportReq};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey, PublicKey};

#[cfg(test)]
use crate::consensus::election::ReportKey;
use crate::consensus::election::{ReportKind, SignedReport, VoteReport};

/// RAI, Section 6: the reports of one run. This node's own report per epoch,
/// signed by each of its representatives, and the reports of the other
/// replicas as they are reconciled.
///
/// The reports are collected and reconciled but nothing decides on them yet:
/// the close of an epoch still agrees on one state hash. What a close
/// proposal needs from here - the signed reports, the reconciled maps - is
/// already exposed (`own_reports`, `reconciled`), and is read once the close
/// selects N−f reports and keeps every hash that passes `possible_Q`
/// (Section 7).
///
/// Pure state: what to send is returned to the caller, which owns the
/// network. Nothing here reads a clock or a socket.
pub(crate) struct ReportExchange {
    /// The epochs this node has reported on, newest last
    own: BTreeMap<ConsensusEpoch, OwnReport>,
    /// The reports of the other replicas, by epoch and reporter
    theirs: HashMap<(ConsensusEpoch, PublicKey), Reconciliation>,
    /// Epochs kept; older ones are dropped with their reconciliations
    max_epochs: usize,
}

#[allow(dead_code)] // read by the close, Section 9.1
pub(crate) struct OwnReport {
    /// The map of the first and final votes this node issued in the epoch
    pub map: VoteReport,
    /// One signed report per representative this node votes with. A close
    /// proposal carries them (Section 9.1), and a replica which missed the
    /// broadcast is answered with them.
    pub signed: Vec<Report>,
}

/// What one reconciliation ended up costing, for the record
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReconcileResult {
    pub epoch: ConsensusEpoch,
    pub reporter: PublicKey,
    /// The reconstructed map hashes to the signed root
    pub complete: bool,
    /// Parts asked for
    pub requests: usize,
    /// Entries taken from the reporter
    pub entries: usize,
    /// Entries in the reconciled map
    pub total: usize,
}

/// What the exchange asks the caller to send
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReportMessage {
    /// Broadcast this node's own report
    Broadcast(Report),
    /// Ask one reporter for a part of its map
    Request(ReportReq),
    /// Answer a reporter's request
    Reply(ReportAck),
}

#[allow(dead_code)] // the readers of a reconciled report are the close's
impl ReportExchange {
    /// Epochs whose reports are kept: the closing one and a little history,
    /// so a replica that lags can still reconcile
    pub const MAX_EPOCHS: usize = 4;

    pub fn new() -> Self {
        Self {
            own: BTreeMap::new(),
            theirs: HashMap::new(),
            max_epochs: Self::MAX_EPOCHS,
        }
    }

    /// Section 6.1: this node stops issuing ordinary votes for the epoch and
    /// signs one report per representative it votes with. The map is what it
    /// voted in the epoch; `committee` is H(O_e).
    pub fn report_epoch(
        &mut self,
        epoch: ConsensusEpoch,
        map: VoteReport,
        committee: BlockHash,
        keys: &[PrivateKey],
    ) -> Vec<ReportMessage> {
        if self.own.contains_key(&epoch) {
            return Vec::new();
        }
        let root = map.root();
        let signed: Vec<Report> = keys
            .iter()
            .map(|key| {
                let payload = SignedReport {
                    epoch,
                    committee,
                    root,
                    reporter: key.public_key(),
                }
                .payload();
                Report::new(key, epoch, committee, root, payload)
            })
            .collect();
        let messages = signed
            .iter()
            .cloned()
            .map(ReportMessage::Broadcast)
            .collect();
        self.own.insert(epoch, OwnReport { map, signed });
        self.trim();
        messages
    }

    /// Whether this node has reported on the epoch
    pub fn has_reported(&self, epoch: ConsensusEpoch) -> bool {
        self.own.contains_key(&epoch)
    }

    pub fn own_root(&self, epoch: ConsensusEpoch) -> Option<BlockHash> {
        self.own.get(&epoch).map(|own| own.map.root())
    }

    /// The signed reports of this node for the epoch, one per representative
    pub fn own_reports(&self, epoch: ConsensusEpoch) -> &[Report] {
        self.own
            .get(&epoch)
            .map(|own| own.signed.as_slice())
            .unwrap_or_default()
    }

    /// The reports of the epoch this node has verified, reconciled or not
    pub fn reports(&self, epoch: ConsensusEpoch) -> Vec<&Reconciliation> {
        self.theirs
            .iter()
            .filter(|((e, _), _)| *e == epoch)
            .map(|(_, reconciliation)| reconciliation)
            .collect()
    }

    /// The reports of the epoch this node has reconciled in full: what a
    /// close proposal selects from (Section 7)
    pub fn reconciled(&self, epoch: ConsensusEpoch) -> Vec<&Reconciliation> {
        self.reports(epoch)
            .into_iter()
            .filter(|reconciliation| reconciliation.is_complete())
            .collect()
    }

    /// Lemma 6.1: a report is taken only with a valid signature over its
    /// epoch, committee, root and reporter. It is stored, not reconciled:
    /// Section 6.2 reconciles a report when a close proposal has to be
    /// validated against it, and until then the root alone is what this node
    /// needs to keep.
    pub fn handle_report(&mut self, report: Report) -> bool {
        let payload = SignedReport {
            epoch: report.epoch,
            committee: report.committee,
            root: report.root,
            reporter: report.reporter,
        }
        .payload();
        if !report.verify(payload) {
            return false;
        }
        let key = (report.epoch, report.reporter);
        // A reporter signs one report per epoch; a second one from the same
        // reporter changes nothing, whatever root it carries
        if self.theirs.contains_key(&key) {
            return false;
        }
        let base = self
            .own
            .get(&report.epoch)
            .map(|own| own.map.clone())
            .unwrap_or_else(|| VoteReport::new(report.epoch));
        self.theirs.insert(key, Reconciliation::new(report, base));
        true
    }

    /// Section 6.2: start reconciling a report this node holds, against its
    /// own map of the epoch. Returns nothing if the report is unknown or its
    /// reconciliation is already under way or done.
    pub fn reconcile(
        &mut self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
        now: Timestamp,
    ) -> Option<ReportMessage> {
        let reconciliation = self.theirs.get_mut(&(epoch, reporter))?;
        if reconciliation.is_started() {
            return None;
        }
        reconciliation.next_request(now).map(ReportMessage::Request)
    }

    /// Section 6.2: answer a reconciliation request from this node's own map
    pub fn handle_request(
        &self,
        request: &ReportReq,
        local_reps: &[PublicKey],
    ) -> Option<ReportAck> {
        if !local_reps.contains(&request.reporter) {
            return None;
        }
        let own = self.own.get(&request.epoch)?;
        let payload = match request.part {
            ReportPart::Digests => ReportPayload::Digests(own.map.bucket_digests()),
            ReportPart::Bucket(bucket) => ReportPayload::Entries(
                own.map
                    .bucket_entries(bucket)
                    .into_iter()
                    .take(ReportAck::MAX_ENTRIES)
                    .map(|(key, hash)| ReportEntry {
                        account: key.account,
                        height: key.height,
                        is_final: key.kind == ReportKind::Final,
                        hash,
                    })
                    .collect(),
            ),
        };
        Some(ReportAck {
            epoch: request.epoch,
            reporter: request.reporter,
            part: request.part,
            payload,
        })
    }

    /// Section 6.2: take one answer into the reconciliation it belongs to and
    /// ask for the next part. Returns the messages to send and, once the
    /// reconciliation ends, what it cost: the parts asked for and the
    /// entries taken.
    pub fn handle_ack(
        &mut self,
        ack: ReportAck,
        now: Timestamp,
    ) -> (Vec<ReportMessage>, Option<ReconcileResult>) {
        let key = (ack.epoch, ack.reporter);
        let Some(reconciliation) = self.theirs.get_mut(&key) else {
            return (Vec::new(), None);
        };
        let was_running = !reconciliation.is_complete() && !reconciliation.has_failed();
        reconciliation.absorb(&ack);
        let messages: Vec<ReportMessage> = reconciliation
            .next_request(now)
            .into_iter()
            .map(ReportMessage::Request)
            .collect();
        let ended = was_running && (reconciliation.is_complete() || reconciliation.has_failed());
        let result = ended.then(|| {
            let (requests, entries) = reconciliation.cost();
            ReconcileResult {
                epoch: key.0,
                reporter: key.1,
                complete: reconciliation.is_complete(),
                requests,
                entries,
                total: reconciliation.map().len(),
            }
        });
        (messages, result)
    }

    /// The requests to repeat: a reconciliation whose answer did not come
    pub fn due_requests(
        &mut self,
        now: Timestamp,
        timeout: std::time::Duration,
    ) -> Vec<ReportMessage> {
        let mut messages = Vec::new();
        for reconciliation in self.theirs.values_mut() {
            if reconciliation.is_complete() || !reconciliation.is_overdue(now, timeout) {
                continue;
            }
            if let Some(request) = reconciliation.next_request(now) {
                messages.push(ReportMessage::Request(request));
            }
        }
        messages
    }

    fn trim(&mut self) {
        while self.own.len() > self.max_epochs {
            let Some(oldest) = self.own.keys().next().copied() else {
                break;
            };
            self.own.remove(&oldest);
            self.theirs.retain(|(epoch, _), _| *epoch != oldest);
        }
    }
}

/// RAI: a report map built from a list of slot votes, as the active
/// elections hand them over at the epoch boundary
#[cfg(test)]
pub(crate) fn report_entries(
    votes: impl IntoIterator<
        Item = (
            rsnano_types::Account,
            u64,
            Option<BlockHash>,
            Option<BlockHash>,
        ),
    >,
    epoch: ConsensusEpoch,
) -> VoteReport {
    let mut map = VoteReport::new(epoch);
    for (account, height, first, final_) in votes {
        if let Some(hash) = first {
            map.add(ReportKey::new(account, height, ReportKind::First), hash);
        }
        if let Some(hash) = final_ {
            map.add(ReportKey::new(account, height, ReportKind::Final), hash);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::Account;
    use std::time::Duration;

    #[test]
    fn reporting_an_epoch_signs_one_report_per_representative() {
        let mut exchange = ReportExchange::new();
        let keys = [PrivateKey::from(1), PrivateKey::from(2)];
        let map = map_of(&[(1, 1)]);
        let root = map.root();

        let messages = exchange.report_epoch(ConsensusEpoch::ZERO, map, BlockHash::from(7), &keys);

        assert_eq!(messages.len(), 2);
        assert!(exchange.has_reported(ConsensusEpoch::ZERO));
        assert_eq!(exchange.own_root(ConsensusEpoch::ZERO), Some(root));
        for (message, key) in messages.iter().zip(keys.iter()) {
            let ReportMessage::Broadcast(report) = message else {
                panic!("expected a broadcast");
            };
            assert_eq!(report.reporter, key.public_key());
            assert_eq!(report.root, root);
            assert_eq!(report.committee, BlockHash::from(7));
        }
        // An epoch is reported once
        assert!(
            exchange
                .report_epoch(
                    ConsensusEpoch::ZERO,
                    map_of(&[(2, 1)]),
                    BlockHash::from(7),
                    &keys
                )
                .is_empty()
        );
        assert_eq!(exchange.own_root(ConsensusEpoch::ZERO), Some(root));
    }

    /// Lemma 6.1: the reporter's signature binds the root; an unsigned or
    /// forged report is not taken
    #[test]
    fn a_report_with_a_bad_signature_is_ignored() {
        let mut exchange = ReportExchange::new();
        let mut report = signed_report(
            &PrivateKey::from(1),
            ConsensusEpoch::ZERO,
            map_of(&[(1, 1)]),
        );
        report.root = BlockHash::from(999);
        assert!(!exchange.handle_report(report));
        assert!(exchange.reports(ConsensusEpoch::ZERO).is_empty());
    }

    /// Section 6.2: the reconciliation of a report against this node's own
    /// map recovers the entries it lacks and ends at the signed root
    #[test]
    fn a_report_is_reconciled_against_the_own_map() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        // The reporter voted in 40 slots, this node saw 38 of them
        let theirs = map_of(&(0..40).map(|i| (i, 1)).collect::<Vec<_>>());
        let ours = map_of(&(0..38).map(|i| (i, 1)).collect::<Vec<_>>());
        let report = signed_report(&key, epoch, theirs.clone());

        let mut reporter = ReportExchange::new();
        reporter.report_epoch(epoch, theirs.clone(), BlockHash::from(7), &[key.clone()]);

        let mut ours_exchange = ReportExchange::new();
        ours_exchange.report_epoch(epoch, ours, BlockHash::from(7), &[PrivateKey::from(2)]);

        assert!(ours_exchange.handle_report(report));
        let mut messages: Vec<ReportMessage> = ours_exchange
            .reconcile(epoch, key.public_key(), now())
            .into_iter()
            .collect();
        let mut rounds = 0;
        while let Some(message) = messages.pop() {
            let ReportMessage::Request(request) = message else {
                panic!("expected a request");
            };
            let ack = reporter
                .handle_request(&request, &[key.public_key()])
                .expect("the reporter answers");
            messages.extend(ours_exchange.handle_ack(ack, now()).0);
            rounds += 1;
            assert!(rounds < 100, "reconciliation does not terminate");
        }

        let reconciled = ours_exchange.reconciled(epoch);
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].map().root(), theirs.root());
        assert_eq!(reconciled[0].map().len(), theirs.len());
    }

    /// The full-report fallback (Section 6.2): a replica with no common base
    /// reconciles from an empty map
    #[test]
    fn a_report_is_reconciled_from_an_empty_map() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let theirs = map_of(&(0..30).map(|i| (i, 1)).collect::<Vec<_>>());
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(epoch, theirs.clone(), BlockHash::from(7), &[key.clone()]);

        let mut ours = ReportExchange::new();
        assert!(ours.handle_report(signed_report(&key, epoch, theirs.clone())));
        let mut messages: Vec<ReportMessage> = ours
            .reconcile(epoch, key.public_key(), now())
            .into_iter()
            .collect();
        let mut rounds = 0;
        while let Some(message) = messages.pop() {
            let ReportMessage::Request(request) = message else {
                panic!("expected a request");
            };
            let ack = reporter
                .handle_request(&request, &[key.public_key()])
                .unwrap();
            messages.extend(ours.handle_ack(ack, now()).0);
            rounds += 1;
            assert!(rounds < 200, "reconciliation does not terminate");
        }
        let reconciled = ours.reconciled(epoch);
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].map().root(), theirs.root());
        // A first and a final vote for each of the thirty slots
        assert_eq!(reconciled[0].map().len(), 60);
    }

    #[test]
    fn a_request_for_another_representative_is_not_answered() {
        let mut exchange = ReportExchange::new();
        let key = PrivateKey::from(1);
        exchange.report_epoch(
            ConsensusEpoch::ZERO,
            map_of(&[(1, 1)]),
            BlockHash::from(7),
            &[key.clone()],
        );
        let request = ReportReq {
            epoch: ConsensusEpoch::ZERO,
            reporter: PublicKey::from(99),
            part: ReportPart::Digests,
        };
        assert!(
            exchange
                .handle_request(&request, &[key.public_key()])
                .is_none()
        );
        let mine = ReportReq {
            reporter: key.public_key(),
            ..request
        };
        assert!(
            exchange
                .handle_request(&mine, &[key.public_key()])
                .is_some()
        );
    }

    /// A request whose answer never comes is repeated
    #[test]
    fn an_unanswered_request_is_repeated() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let mut exchange = ReportExchange::new();
        let start = now();
        assert!(exchange.handle_report(signed_report(&key, epoch, map_of(&[(1, 1)]))));
        assert!(exchange.reconcile(epoch, key.public_key(), start).is_some());
        assert!(
            exchange
                .due_requests(start, Duration::from_secs(1))
                .is_empty()
        );
        let later = start + Duration::from_secs(2);
        assert_eq!(
            exchange.due_requests(later, Duration::from_secs(1)).len(),
            1
        );
    }

    /// Only the recent epochs are kept
    #[test]
    fn old_epochs_are_dropped() {
        let mut exchange = ReportExchange::new();
        let key = PrivateKey::from(1);
        for i in 0..(ReportExchange::MAX_EPOCHS as u64 + 2) {
            let epoch = ConsensusEpoch::new(i);
            exchange.report_epoch(epoch, map_of(&[(1, 1)]), BlockHash::from(7), &[key.clone()]);
        }
        assert!(exchange.own_root(ConsensusEpoch::ZERO).is_none());
        assert!(exchange.own_root(ConsensusEpoch::new(1)).is_none());
        assert!(exchange.own_root(ConsensusEpoch::new(2)).is_some());
    }

    /*
     * Test helpers
     */

    fn now() -> Timestamp {
        Timestamp::new_test_instance()
    }

    fn map_of(slots: &[(u64, u64)]) -> VoteReport {
        report_entries(
            slots.iter().map(|(account, height)| {
                (
                    Account::from(*account),
                    *height,
                    Some(BlockHash::from(*account)),
                    Some(BlockHash::from(*account)),
                )
            }),
            ConsensusEpoch::ZERO,
        )
    }

    fn signed_report(key: &PrivateKey, epoch: ConsensusEpoch, map: VoteReport) -> Report {
        let root = map.root();
        let committee = BlockHash::from(7);
        let payload = SignedReport {
            epoch,
            committee,
            root,
            reporter: key.public_key(),
        }
        .payload();
        Report::new(key, epoch, committee, root, payload)
    }
}
