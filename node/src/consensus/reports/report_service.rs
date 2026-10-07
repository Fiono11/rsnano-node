use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rsnano_messages::{
    ConfirmAck, EpochInstalled, EvidenceReq, Message, Report, ReportSymbolsReply, ReportSymbolsReq,
};
use rsnano_network::{Channel, TrafficType};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{ConsensusEpoch, PublicKey};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use super::{
    ReconcileResult, ReportExchange, ReportMessage, Verification, unjustified, well_formed,
};
use crate::{
    consensus::{AecService, EpochReport},
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};

/// RAI, Section 6: the report phase on the wire. At the end of an epoch this
/// node signs one report per representative it votes with and broadcasts it;
/// the reports of the other replicas are reconciled against this node's own
/// map of the same epoch, which already holds most of the same votes.
///
/// The exchange itself is pure state (`ReportExchange`); this is the
/// infrastructure around it: the keys, the clock, the network.
pub struct ReportService {
    exchange: Arc<Mutex<ReportExchange>>,
    active_elections: Arc<AecService>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    flooder: Mutex<MessageFlooder>,
    clock: Arc<SteadyClock>,
    stats: Arc<Stats>,
    /// Reports taken at a boundary whose predecessor checkpoint was not
    /// decided here yet: signed once it is
    pending: Mutex<HashMap<ConsensusEpoch, Arc<EpochReport>>>,
}

impl ReportService {
    pub fn new(
        active_elections: Arc<AecService>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            exchange: Arc::new(Mutex::new(ReportExchange::new())),
            active_elections,
            wallet_reps,
            flooder: Mutex::new(flooder),
            clock,
            stats,
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn new_null() -> Self {
        Self::new(
            Arc::new(AecService::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            MessageFlooder::new_null(),
            Arc::new(SteadyClock::new_null()),
            Arc::new(Stats::default()),
        )
    }

    /// Algorithm 1 line 8: the node stopped signing in the epoch at its
    /// boundary and took its report there, under the same lock. The report
    /// is signed "against closed S_{e-1}" and broadcast here; a boundary
    /// reached before that checkpoint was decided here signs as soon as it
    /// is (see `tick`).
    pub fn epoch_left(&self, epoch: ConsensusEpoch, report: Arc<EpochReport>) {
        let Some(predecessor) = self.predecessor_of(epoch) else {
            crate::utils::diagnostic!("EPOCH_REPORT_DEFERRED epoch={}", epoch);
            self.pending.lock().unwrap().insert(epoch, report);
            return;
        };
        self.sign_report(epoch, report, predecessor);
    }

    /// d_{e-1}: the hash of the decided predecessor checkpoint, if this node
    /// holds it
    fn predecessor_of(&self, epoch: ConsensusEpoch) -> Option<rsnano_types::BlockHash> {
        self.active_elections
            .epoch_previous_state(epoch)
            .map(|state| state.state_hash())
    }

    fn sign_report(
        &self,
        epoch: ConsensusEpoch,
        report: Arc<EpochReport>,
        predecessor: rsnano_types::BlockHash,
    ) {
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        if keys.is_empty() {
            return;
        }
        // The inherited base is known only once the predecessor is decided
        let (certified_state, residual_votes) =
            self.active_elections
                .complete_report(epoch, &report.certified, &report.residual);
        let certified = certified_state.len();
        let residual = residual_votes.len();
        let root = certified_state.root();
        let messages = {
            let mut exchange = self.exchange.lock().unwrap();
            exchange.report_epoch(
                epoch,
                certified_state,
                residual_votes,
                report.committee,
                predecessor,
                &keys,
            )
        };
        if messages.is_empty() {
            return;
        }
        crate::utils::diagnostic!(
            "EPOCH_REPORT epoch={} certified={} residual={} root={} reports={}",
            epoch,
            certified,
            residual,
            root,
            messages.len()
        );
        self.send(messages, None);
    }

    /// RAI, "Retained evidence": this node installed the decided checkpoint
    /// of an epoch; it says so, signed, and counts its own acknowledgement
    pub fn epoch_installed(&self, epoch: ConsensusEpoch) {
        let Some(state) = self.active_elections.epoch_previous_state(epoch.next()) else {
            return;
        };
        let state = state.state_hash();
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        for key in keys {
            let message = EpochInstalled::new(&key, epoch, state);
            self.acknowledge(&message);
            self.stats.inc_dir(
                StatType::Message,
                DetailType::EpochInstalled,
                Direction::Out,
            );
            self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                &Message::EpochInstalled(message),
                TrafficType::Generic,
                1.0,
            );
        }
    }

    /// RAI: a member acknowledges a checkpoint it installed
    pub fn handle_epoch_installed(&self, message: EpochInstalled, _channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::EpochInstalled, Direction::In);
        if !message.verify() {
            return;
        }
        self.acknowledge(&message);
    }

    /// Counts an acknowledgement in the successor committee; once `N - f` of
    /// its weight installed the checkpoint, the epoch's reports are released
    fn acknowledge(&self, message: &EpochInstalled) {
        if !self
            .active_elections
            .acknowledge_install(message.epoch, message.state, message.member)
        {
            return;
        }
        if self.exchange.lock().unwrap().release_epoch(message.epoch) {
            self.stats
                .inc(StatType::Message, DetailType::EpochEvidenceReleased);
            crate::utils::diagnostic!("EPOCH_EVIDENCE_RELEASED epoch={}", message.epoch);
        }
    }

    /// Verify and store a signed report for reconstruction on the next tick.
    pub fn handle_report(&self, report: Report, _channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::Report, Direction::In);
        self.exchange.lock().unwrap().handle_report(report);
    }

    /// RAI: a node rebuilding an inventory this node holds asks for its
    /// coded symbols; they go back on the channel the request came in on
    pub fn handle_symbols_request(&self, request: ReportSymbolsReq, channel: &Arc<Channel>) {
        self.stats.inc_dir(
            StatType::Message,
            DetailType::ReportSymbolsReq,
            Direction::In,
        );
        let Some(reply) = self.exchange.lock().unwrap().symbols_for(&request) else {
            return;
        };
        if self.flooder.lock().unwrap().try_send(
            channel,
            &Message::ReportSymbolsReply(reply),
            TrafficType::Generic,
        ) {
            self.stats.inc_dir(
                StatType::Message,
                DetailType::ReportSymbolsReply,
                Direction::Out,
            );
        }
    }

    /// RAI: coded symbols of an inventory this node is rebuilding
    pub fn handle_symbols_reply(&self, reply: ReportSymbolsReply, _channel: &Arc<Channel>) {
        self.stats.inc_dir(
            StatType::Message,
            DetailType::ReportSymbolsReply,
            Direction::In,
        );
        let now = self.clock.now();
        let (messages, results) = self.exchange.lock().unwrap().handle_symbols(&reply, now);
        for result in results {
            log_reconciled(Some(result));
        }
        self.send(messages, None);
    }

    /// Accept a certified inventory at once when a locally held state has
    /// exactly the signed root; otherwise start rebuilding it from symbols
    pub fn reconcile(&self, epoch: ConsensusEpoch, reporter: PublicKey) {
        let now = self.clock.now();
        let Some((messages, result)) = self
            .exchange
            .lock()
            .unwrap()
            .reconcile(epoch, reporter, now)
        else {
            return;
        };
        log_reconciled(result);

        self.send(messages, None);
    }

    /// RAI: derive a report's residual object from the reporter's votes
    /// this node received, once its certified state is reconstructed
    fn derive_residual(&self, epoch: ConsensusEpoch, reporter: PublicKey) {
        let now = self.clock.now();
        if !self
            .exchange
            .lock()
            .unwrap()
            .needs_residual(epoch, &reporter, now)
        {
            return;
        }
        let votes = self.active_elections.vote_records_of(epoch, &reporter);
        let result = self
            .exchange
            .lock()
            .unwrap()
            .derive_residual(epoch, reporter, votes, now);
        log_reconciled(result);
    }

    /// Drives the reconciliations of the epochs still closing. The live
    /// certified state is refreshed from the active elections first: gossip
    /// keeps delivering the votes of a closed epoch, and it is that growth
    /// which eventually gives this node a state it shares with a reporter.
    pub fn tick(&self) {
        // A report deferred for want of its predecessor checkpoint is signed
        // once that checkpoint is decided here
        let deferred: Vec<ConsensusEpoch> = self.pending.lock().unwrap().keys().copied().collect();
        for epoch in deferred {
            let Some(predecessor) = self.predecessor_of(epoch) else {
                continue;
            };
            let Some(report) = self.pending.lock().unwrap().remove(&epoch) else {
                continue;
            };
            self.sign_report(epoch, report, predecessor);
        }
        // This node's own reports go out again for as long as it holds them:
        // a replica that missed the broadcast at the boundary can not select
        // them, and one still deriving an old epoch's state needs them
        let repeated = self
            .exchange
            .lock()
            .unwrap()
            .repeat_reports(self.clock.now());
        self.send(repeated, None);
        // Until the epoch is decided here, not until its election closes: a
        // replica that learned the certificate before it could derive the
        // value still needs the reports that value names
        let epochs = self.exchange.lock().unwrap().pending_epochs();
        for epoch in epochs {
            let reporters: Vec<PublicKey> = {
                let exchange = self.exchange.lock().unwrap();
                let usable: Vec<PublicKey> = exchange
                    .usable(epoch)
                    .iter()
                    .map(|(report, _, _)| report.reporter)
                    .collect();
                exchange
                    .reports(epoch)
                    .iter()
                    .map(|report| report.reporter)
                    .filter(|reporter| !usable.contains(reporter))
                    .collect()
            };
            if reporters.is_empty() {
                continue;
            }
            let live = self.active_elections.epoch_certified(epoch);
            self.exchange.lock().unwrap().refresh_live(epoch, live);
            for reporter in reporters {
                self.reconcile(epoch, reporter);
                self.derive_residual(epoch, reporter);
            }
        }
        let requests = self
            .exchange
            .lock()
            .unwrap()
            .stream_requests(self.clock.now());
        self.send(requests, None);
        self.verify_reports();
    }

    /// RAI: checks every reconstructed report against the signed votes
    /// held here before it becomes usable, and asks for what is missing
    fn verify_reports(&self) {
        let now = self.clock.now();
        let epochs = self.exchange.lock().unwrap().pending_epochs();
        for epoch in epochs {
            let previous = self.active_elections.epoch_previous_state(epoch);
            let aec = &self.active_elections;
            let requests = self.exchange.lock().unwrap().verify(
                epoch,
                now,
                |reporter, certified, residual, only| {
                    if !well_formed(certified, residual, previous.as_deref()) {
                        crate::utils::diagnostic!(
                            "EPOCH_REPORT_MALFORMED epoch={} reporter={}",
                            epoch,
                            reporter
                        );
                        return Some(Verification::Malformed);
                    }
                    unjustified(
                        epoch,
                        certified,
                        residual,
                        only,
                        &|epoch, hashes| aec.certificate_kinds(epoch, hashes),
                        &|block, entry| {
                            previous
                                .as_ref()
                                .is_some_and(|previous| inherited_from(previous, block, entry))
                        },
                        &|votes| aec.has_votes(epoch, reporter, votes),
                    )
                    .map(Verification::Missing)
                },
            );
            for (reporter, missing) in requests {
                self.stats.add(
                    StatType::Message,
                    DetailType::ReportUnverified,
                    missing.len() as u64,
                );
                let sample: Vec<String> = missing
                    .iter()
                    .take(4)
                    .map(|hash| {
                        let (first, final_) = self.active_elections.support_counts(epoch, hash);
                        format!(
                            "{}:first={}:final={}",
                            &hash.to_string()[..16],
                            first,
                            final_
                        )
                    })
                    .collect();
                crate::utils::diagnostic!(
                    "EPOCH_EVIDENCE_MISSING epoch={} reporter={} hashes={} sample={:?}",
                    epoch,
                    reporter,
                    missing.len(),
                    sample
                );
                for chunk in missing.chunks(EvidenceReq::MAX_HASHES) {
                    self.stats
                        .inc_dir(StatType::Message, DetailType::EvidenceReq, Direction::Out);
                    self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                        &Message::EvidenceReq(EvidenceReq {
                            epoch,
                            hashes: chunk.to_vec(),
                        }),
                        TrafficType::Generic,
                        1.0,
                    );
                }
            }
        }
    }

    /// RAI: a node lacking the evidence of a report's certificates asks for
    /// it; the signed votes held here go back as certificate evidence
    pub fn handle_evidence_request(&self, request: EvidenceReq, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::EvidenceReq, Direction::In);
        let hashes: Vec<_> = request
            .hashes
            .into_iter()
            .take(EvidenceReq::MAX_HASHES)
            .collect();
        let votes = self.active_elections.evidence_votes(request.epoch, &hashes);
        let mut flooder = self.flooder.lock().unwrap();
        for vote in votes {
            flooder.try_send(
                channel,
                &Message::ConfirmAck(ConfirmAck::new_with_certificate_evidence((*vote).clone())),
                TrafficType::VoteReply,
            );
        }
    }

    /// How many reports of the epoch are usable here: what an epoch proposal
    /// selects from, and it needs N−f of them
    pub fn usable_count(&self, epoch: ConsensusEpoch) -> usize {
        self.exchange.lock().unwrap().usable(epoch).len()
    }

    /// RAI: the exchange itself, which the epoch decision derives its values
    /// from: the reports it selects are the ones reconstructed here
    pub(crate) fn exchange(&self) -> Arc<Mutex<ReportExchange>> {
        self.exchange.clone()
    }

    fn send(&self, messages: Vec<ReportMessage>, _channel: Option<&Arc<Channel>>) {
        for message in messages {
            match message {
                ReportMessage::Broadcast(report) => {
                    let message = Message::Report(report);
                    self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                        &message,
                        TrafficType::Generic,
                        1.0,
                    );
                    self.stats
                        .inc_dir(StatType::Message, DetailType::Report, Direction::Out);
                }
                ReportMessage::Request(request) => {
                    self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                        &Message::ReportSymbolsReq(request),
                        TrafficType::Generic,
                        1.0,
                    );
                    self.stats.inc_dir(
                        StatType::Message,
                        DetailType::ReportSymbolsReq,
                        Direction::Out,
                    );
                }
            }
        }
    }
}

/// RAI: whether a report entry is the predecessor checkpoint's own: a
/// finalized block it finalized, a notarization lock it retains, or the
/// recovery protection it carries. Such an entry needs no fresh evidence.
pub(super) fn inherited_from(
    previous: &crate::consensus::election::EpochLedger,
    block: &crate::consensus::election::CertifiedBlock,
    entry: crate::consensus::election::Certification,
) -> bool {
    use crate::consensus::election::{AccountSlot, CertifiedStatus, RetainedKind};
    let slot = AccountSlot::new(block.account, block.height);
    match entry.status {
        CertifiedStatus::Finalized => previous.is_finalized(&slot, &block.hash),
        CertifiedStatus::Notarized => {
            previous.is_locked(&slot, &block.hash)
                && previous.retained_kind(&block.hash) == RetainedKind::Notarized
        }
        CertifiedStatus::Recovery => {
            previous.valid_recovery_entry(&slot, block.hash, entry.previous)
        }
    }
}

/// RAI: what one reconciliation cost, for the record
fn log_reconciled(result: Option<ReconcileResult>) {
    let Some(result) = result else {
        return;
    };
    crate::utils::diagnostic!(
        "EPOCH_RECONCILED epoch={} reporter={} complete={} entries={} total={} symbols={} dropped={}",
        result.epoch,
        result.reporter,
        result.complete,
        result.entries,
        result.total,
        result.symbols,
        result.dropped
    );
}
