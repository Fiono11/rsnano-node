use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_messages::{
    ConfirmAck, LedgerSketchReply, LedgerSketchReq, Message, Publish, ReconReply, ReconReq, Report,
};
use rsnano_network::{Channel, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{ConsensusEpoch, PublicKey};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use super::{ReconRefusal, ReconcileResult, ReportExchange, ReportMessage};
use crate::{
    consensus::{AecService, EpochReport},
    transport::{MessageFlooder, MessageSender},
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
    sender: Mutex<MessageSender>,
    clock: Arc<SteadyClock>,
    stats: Arc<Stats>,
    /// When a request or a refusal was last logged per epoch: one line a
    /// second says what a reconciliation is stuck on without flooding the log
    logged: Mutex<HashMap<(ConsensusEpoch, bool), Timestamp>>,
    /// Reports taken at a boundary whose predecessor checkpoint was not
    /// decided here yet: signed once it is
    pending: Mutex<HashMap<ConsensusEpoch, Arc<EpochReport>>>,
    source_refreshed: Mutex<HashMap<ConsensusEpoch, Timestamp>>,
    votes_repeated: Mutex<Option<Timestamp>>,
    data: super::residual_data::ResidualData,
}

#[allow(dead_code)] // reconciled_count is read by the close, Section 9.1
impl ReportService {
    const LOG_INTERVAL: Duration = Duration::from_secs(1);

    pub(crate) fn new(
        active_elections: Arc<AecService>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        sender: MessageSender,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
        ledger: Arc<rsnano_ledger::Ledger>,
        forks: Arc<std::sync::RwLock<crate::consensus::ForkCache>>,
    ) -> Self {
        Self {
            exchange: Arc::new(Mutex::new(ReportExchange::new())),
            data: super::residual_data::ResidualData::new(ledger, forks, active_elections.clone()),
            active_elections,
            wallet_reps,
            flooder: Mutex::new(flooder),
            sender: Mutex::new(sender),
            clock,
            stats,
            logged: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            source_refreshed: Mutex::new(HashMap::new()),
            votes_repeated: Mutex::new(None),
        }
    }

    pub(crate) fn retain_received_block(&self, block: &rsnano_types::Block) {
        self.data.receive(block);
    }

    /// Explicit diagnostic RPC only; never used by consensus or normal polling.
    pub fn diagnostic_snapshot(&self) -> serde_json::Value {
        let exchange = self.exchange.lock().unwrap();
        let entries = |state: &crate::consensus::election::CertifiedState| {
            state
                .entries()
                .map(|(block, entry)| {
                    serde_json::json!({
                        "account": block.account, "height": block.height, "hash": block.hash,
                        "previous": entry.previous, "status": format!("{:?}", entry.status)
                    })
                })
                .collect::<Vec<_>>()
        };
        let residual = |votes: &crate::consensus::election::ResidualVotes| {
            serde_json::json!({
                "root": votes.root(), "first_evidence_complete": votes.first_evidence_complete(),
                "entries": votes.entries().map(|(b,k,p)| serde_json::json!({
                    "account": b.account, "height": b.height, "hash": b.hash,
                    "previous": p, "kind": format!("{:?}", k)
                })).collect::<Vec<_>>()
            })
        };
        serde_json::json!(exchange.epochs.iter().map(|(epoch, held)| {
            serde_json::json!({
                "epoch": epoch.as_u64(), "live_root": held.live.root(), "live": entries(&held.live),
                "signed": held.signed.iter().map(|r| serde_json::json!({
                    "reporter": r.reporter, "target": r.certified, "residual": r.residual,
                    "snapshot": held.state(r.certified).map(&entries),
                    "residual_evidence": held.residuals.get(&r.residual).map(&residual)
                })).collect::<Vec<_>>(),
                "received": held.theirs.values().map(|r| serde_json::json!({
                    "reporter": r.report.reporter, "target": r.report.certified,
                    "reconstructed": r.reconstructed.is_some(), "complete": r.is_complete(),
                    "expected_residual_root": r.report.residual,
                    "residual_evidence": r.residual.as_ref().or(r.working.as_ref()).map(&residual)
                })).collect::<Vec<_>>()
            })
        }).collect::<Vec<_>>())
    }

    pub fn new_null() -> Self {
        Self::new(
            Arc::new(AecService::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            MessageFlooder::new_null(),
            MessageSender::new_null(),
            Arc::new(SteadyClock::new_null()),
            Arc::new(Stats::default()),
            Arc::new(rsnano_ledger::Ledger::new_null()),
            Arc::new(std::sync::RwLock::new(crate::consensus::ForkCache::new())),
        )
    }

    /// Whether a diagnostic of this kind for the epoch is due: at most one
    /// per `LOG_INTERVAL`
    fn log_due(&self, epoch: ConsensusEpoch, refusal: bool) -> bool {
        let now = self.clock.now();
        let mut logged = self.logged.lock().unwrap();
        if logged
            .get(&(epoch, refusal))
            .is_some_and(|last| last.elapsed(now) < Self::LOG_INTERVAL)
        {
            return false;
        }
        logged.insert((epoch, refusal), now);
        true
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
        self.data
            .retain(epoch, report.residual.entries().map(|(b, _, _)| b.hash));
        let certified = report.certified.len();
        let residual = report.residual.hash_count();
        let [r, n, f] = report.certified.status_counts();
        let root = report.certified.root();
        let previous = self.active_elections.epoch_previous_state(epoch);
        let messages = {
            let mut exchange = self.exchange.lock().unwrap();
            if let Some(previous) = previous {
                exchange.set_predecessor(epoch, previous);
            }
            exchange.report_epoch(
                epoch,
                report.certified.clone(),
                report.residual.clone(),
                report.committee,
                predecessor,
                &keys,
            )
        };
        if messages.is_empty() {
            return;
        }
        crate::utils::diagnostic!(
            "EPOCH_REPORT epoch={} certified={} residual={} root={} reports={} R={} N={} F={}",
            epoch,
            certified,
            residual,
            root,
            messages.len(),
            r,
            n,
            f
        );
        self.send(messages, None);
    }

    /// A report of another replica: verified and stored. It is reconciled
    /// when a close has to be validated against it (Section 6.2), not on
    /// arrival: a reconciliation transfers most of the reporter's map, and
    /// nothing decides on a report yet.
    pub fn handle_report(&self, report: Report, _channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::Report, Direction::In);
        self.exchange.lock().unwrap().handle_report(report);
    }

    /// RAI: reconcile a stored report, which an epoch proposal has to be
    /// validated against. Both halves are made usable: the certified state
    /// by a reconstructive difference, the residual object by a fetch.
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
        let sketched = messages.iter().find_map(|message| match message {
            ReportMessage::Sketch(sketch) => Some(sketch.cells.len()),
            _ => None,
        });
        for message in &messages {
            if let ReportMessage::Request(request) = message
                && self.log_due(epoch, false)
            {
                crate::utils::diagnostic!(
                    "EPOCH_RECON_REQUEST epoch={} reporter={} target={} sources={:?} sketch_cells={}",
                    epoch,
                    reporter,
                    request.target,
                    request
                        .sources
                        .iter()
                        .map(|root| root.to_string()[..8].to_string())
                        .collect::<Vec<_>>(),
                    sketched.unwrap_or(0)
                );
            }
        }
        self.send(messages, None);
    }

    /// RAI: a request for a reconstructive difference. This node answers only
    /// if it knows both states; no answer is not a verdict.
    pub fn handle_request(&self, request: ReconReq, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ReconReq, Direction::In);
        let reply = self.answer_request(&request);
        match reply {
            Ok(replies) => self.send(
                replies.into_iter().map(ReportMessage::Reply).collect(),
                Some(channel),
            ),
            Err(refusal) => {
                if self.log_due(request.epoch, true) {
                    crate::utils::diagnostic!(
                        "EPOCH_RECON_REFUSED epoch={} target={} reason={:?} sources={:?}",
                        request.epoch,
                        request.target,
                        refusal,
                        request
                            .sources
                            .iter()
                            .map(|root| root.to_string()[..8].to_string())
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
    }

    /// A completed handoff must still serve lagging validators. Its periodic
    /// reconstruction loop has stopped, so refresh on an unknown source.
    /// Known sources take the existing fast path; unknown targets do no work.
    fn answer_request(&self, request: &ReconReq) -> Result<Vec<ReconReply>, ReconRefusal> {
        let reply = self.exchange.lock().unwrap().handle_request_pages(request);
        if !matches!(reply, Err(ReconRefusal::UnknownSource)) {
            return reply;
        }
        let now = self.clock.now();
        {
            let mut refreshed = self.source_refreshed.lock().unwrap();
            if refreshed
                .get(&request.epoch)
                .is_some_and(|last| last.elapsed(now) < Duration::from_millis(200))
            {
                return reply;
            }
            refreshed.retain(|_, last| last.elapsed(now) < Duration::from_secs(1));
            refreshed.insert(request.epoch, now);
        }
        let projection = self.active_elections.epoch_certified(request.epoch);
        let mut exchange = self.exchange.lock().unwrap();
        exchange.refresh_live(request.epoch, projection);
        exchange.handle_request_pages(request)
    }

    /// RAI: a difference towards a report this node is reconstructing. It is
    /// accepted exactly when the rebuilt state hashes to the signed root.
    pub fn handle_reply(&self, reply: ReconReply, _channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ReconReply, Direction::In);
        let result = self.exchange.lock().unwrap().handle_reply(&reply);
        log_reconciled(result);
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
        let unplaced = self.active_elections.unplaced_signed(epoch, &reporter);
        let unplaced_count = unplaced.len();
        let placements: Vec<_> = unplaced
            .into_iter()
            .filter_map(|(hash, kind)| {
                let (block, previous) = self.data.placement(&hash)?;
                Some((block, kind, previous))
            })
            .collect();
        let placed_count = placements.len();
        self.active_elections
            .place_signed(epoch, reporter, placements);
        let votes = self.active_elections.vote_records_of(epoch, &reporter);
        let held = votes.len();
        let result = self
            .exchange
            .lock()
            .unwrap()
            .derive_residual(epoch, reporter, votes, now);
        if let Some(result) = result {
            crate::utils::diagnostic!(
                "EPOCH_RESIDUAL epoch={} reporter={} votes={} derived={} complete={} unplaced={} placed={}",
                epoch,
                reporter,
                held,
                result.total,
                result.complete,
                unplaced_count,
                placed_count
            );
        }
        // Retry from validated reporter votes. A hash-only G commitment
        // cannot authenticate vote-kind metadata supplied by a sketch.
    }

    /// RAI, "Reconstructing a report": a sketch of a requester's tagged
    /// ledger set. This node answers if it holds the target, with the pages
    /// of the difference peeled out of the sketch.
    pub fn handle_ledger_sketch(&self, request: LedgerSketchReq, channel: &Arc<Channel>) {
        self.stats.inc_dir(
            StatType::Message,
            DetailType::LedgerSketchReq,
            Direction::In,
        );
        let replies = self.exchange.lock().unwrap().handle_ledger_sketch(&request);
        if let Some(replies) = replies {
            self.send(
                replies
                    .into_iter()
                    .map(ReportMessage::SketchReply)
                    .collect(),
                Some(channel),
            );
        }
    }

    /// RAI: a page of the difference a sketch peeled out, applied to the
    /// snapshot sketched and accepted exactly when it reaches the signed root
    pub fn handle_ledger_sketch_reply(&self, reply: LedgerSketchReply, _channel: &Arc<Channel>) {
        self.stats.inc_dir(
            StatType::Message,
            DetailType::LedgerSketchReply,
            Direction::In,
        );
        let incomplete = reply.incomplete;
        let result = self
            .exchange
            .lock()
            .unwrap()
            .handle_ledger_sketch_reply(&reply);
        if let Some(result) = result {
            crate::utils::diagnostic!(
                "EPOCH_LEDGER_SKETCH epoch={} reporter={} incomplete={} edits={} total={} complete={}",
                result.epoch,
                result.reporter,
                incomplete,
                result.entries,
                result.total,
                result.complete
            );
        }
    }

    /// Replay original signed messages for our frozen G through ordinary vote
    /// ingress. Keep serving retained reports after local closure for lagging
    /// peers. Never manufacture vote-kind evidence from a G hash/sketch.
    fn repeat_residual_votes(&self) {
        let now = self.clock.now();
        {
            let mut repeated = self.votes_repeated.lock().unwrap();
            if repeated.is_some_and(|last| last.elapsed(now) < ReportExchange::REPEAT_INTERVAL) {
                return;
            }
            *repeated = Some(now);
        }
        let requests: Vec<_> = {
            let exchange = self.exchange.lock().unwrap();
            exchange
                .epochs
                .iter()
                .flat_map(|(epoch, held)| {
                    held.signed.iter().filter_map(|report| {
                        let votes = held.residuals.get(&report.residual)?;
                        let hashes: std::collections::BTreeSet<_> =
                            votes.entries().map(|(b, _, _)| b.hash).collect();
                        Some((
                            *epoch,
                            report.reporter,
                            hashes.into_iter().collect::<Vec<_>>(),
                        ))
                    })
                })
                .collect()
        };
        for (epoch, reporter, hashes) in requests {
            self.data.retain(epoch, hashes.iter().copied());
            let block_count = self.data.retained(epoch, &hashes).len();
            let blocks = self.data.with_ancestry(epoch, &hashes);
            {
                let mut flooder = self.flooder.lock().unwrap();
                for block in blocks {
                    flooder.flood_prs_and_some_non_prs(
                        &Message::Publish(Publish::new_evidence(block)),
                        TrafficType::BlockBroadcastInitial,
                        1.0,
                    );
                }
            }
            let votes = self
                .active_elections
                .signed_votes_for(epoch, &reporter, &hashes);
            if !hashes.is_empty() {
                crate::utils::diagnostic!(
                    "EPOCH_RESIDUAL_DATA epoch={} reporter={} wanted={} blocks={} signed_batches={}",
                    epoch,
                    reporter,
                    hashes.len(),
                    block_count,
                    votes.len()
                );
            }
            if votes.is_empty() {
                continue;
            }
            let mut flooder = self.flooder.lock().unwrap();
            for vote in votes {
                flooder.flood_prs_and_some_non_prs(
                    &Message::ConfirmAck(ConfirmAck::new_with_certificate_evidence(
                        (*vote).clone(),
                    )),
                    TrafficType::Vote,
                    1.0,
                );
            }
        }
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
            // Signing had stopped, but T could not be frozen without its
            // predecessor. Freeze the complete projection now, once only.
            let report = self
                .active_elections
                .epoch_report_snapshot(epoch)
                .map(Arc::new)
                .unwrap_or(report);
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
        self.repeat_residual_votes();
        // Until the epoch is decided here, not until its election closes: a
        // replica that learned the certificate before it could derive the
        // value still needs the reports that value names
        let epochs: Vec<ConsensusEpoch> = self
            .active_elections
            .epoch_closes()
            .into_iter()
            .filter(|close| close.value.is_none())
            .map(|close| close.epoch)
            .collect();
        for epoch in epochs {
            if let Some(previous) = self.active_elections.epoch_previous_state(epoch) {
                self.exchange
                    .lock()
                    .unwrap()
                    .set_predecessor(epoch, previous);
            }
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

    fn send(&self, messages: Vec<ReportMessage>, channel: Option<&Arc<Channel>>) {
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
                    // A difference may come from any replica that knows both
                    // states, not only from the reporter, so the request is
                    // gossiped rather than addressed
                    self.stats
                        .inc_dir(StatType::Message, DetailType::ReconReq, Direction::Out);
                    self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                        &Message::ReconReq(request),
                        TrafficType::Generic,
                        1.0,
                    );
                }
                ReportMessage::Reply(reply) => {
                    let Some(target) = channel else {
                        continue;
                    };
                    self.stats
                        .inc_dir(StatType::Message, DetailType::ReconReply, Direction::Out);
                    self.sender.lock().unwrap().try_send(
                        target,
                        &Message::ReconReply(reply),
                        TrafficType::Generic,
                    );
                }
                ReportMessage::Sketch(request) => {
                    // Any replica holding the target can answer, so the
                    // sketch is gossiped like a request
                    self.stats.inc_dir(
                        StatType::Message,
                        DetailType::LedgerSketchReq,
                        Direction::Out,
                    );
                    self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                        &Message::LedgerSketchReq(request),
                        TrafficType::Generic,
                        1.0,
                    );
                }
                ReportMessage::SketchReply(reply) => {
                    let Some(target) = channel else {
                        continue;
                    };
                    self.stats.inc_dir(
                        StatType::Message,
                        DetailType::LedgerSketchReply,
                        Direction::Out,
                    );
                    self.sender.lock().unwrap().try_send(
                        target,
                        &Message::LedgerSketchReply(reply),
                        TrafficType::Generic,
                    );
                }
            }
        }
    }
}

/// RAI: what one reconciliation cost, for the record
fn log_reconciled(result: Option<ReconcileResult>) {
    let Some(result) = result else {
        return;
    };
    crate::utils::diagnostic!(
        "EPOCH_RECONCILED epoch={} reporter={} complete={} entries={} total={}",
        result.epoch,
        result.reporter,
        result.complete,
        result.entries,
        result.total
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::{
        CertifiedBlock, CertifiedState, CertifiedStatus, ResidualVotes,
    };
    use rsnano_types::{Account, BlockHash, PrivateKey};

    #[test]
    fn diagnostics_distinguish_matching_hashes_from_missing_first_evidence() {
        let service = ReportService::new_null();
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let t = CertifiedState::new();
        let block = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(2));
        let mut g = ResidualVotes::new();
        g.record(
            block,
            BlockHash::ZERO,
            crate::consensus::election::ResidualKind::First,
        );
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            epoch,
            t.clone(),
            g.clone(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[key.clone()],
        );
        {
            let mut exchange = service.exchange.lock().unwrap();
            exchange.refresh_live(epoch, t);
            exchange.handle_report(reporter.own_reports(epoch)[0].clone());
            exchange.reconcile(epoch, key.public_key(), service.clock.now());
            exchange.derive_residual(
                epoch,
                key.public_key(),
                [(
                    block,
                    crate::consensus::election::ResidualKind::Final,
                    BlockHash::ZERO,
                )],
                service.clock.now(),
            );
        }
        let snapshot = service.diagnostic_snapshot();
        let received = &snapshot[0]["received"][0];
        assert_eq!(
            received["expected_residual_root"],
            received["residual_evidence"]["root"]
        );
        assert_eq!(
            received["residual_evidence"]["first_evidence_complete"],
            false
        );
        assert_eq!(received["complete"], false);
    }

    #[test]
    fn serving_a_known_report_refreshes_an_unknown_source_without_periodic_tick() {
        let service = ReportService::new_null();
        let epoch = ConsensusEpoch::ZERO;
        let mut frozen = CertifiedState::new();
        frozen.certify(
            CertifiedBlock::new(Account::from(1), 1, BlockHash::from(2)),
            BlockHash::ZERO,
            CertifiedStatus::Notarized,
        );
        service.exchange.lock().unwrap().report_epoch(
            epoch,
            frozen.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(1)],
        );
        // The null AEC's complete projection is empty; the report exchange
        // still holds its earlier nonempty view, like a completed handoff.
        let request = ReconReq {
            epoch,
            target: frozen.root(),
            sources: vec![CertifiedState::new().root()],
        };
        assert_eq!(
            service.exchange.lock().unwrap().handle_request(&request),
            Err(ReconRefusal::UnknownSource)
        );
        let reply = service.answer_request(&request).unwrap().remove(0);
        assert_eq!(reply.target, frozen.root());
        assert_eq!(reply.added.len(), 1);
        assert_eq!(
            service.exchange.lock().unwrap().own_reports(epoch)[0].certified,
            frozen.root()
        );
    }

    #[test]
    fn unknown_target_does_not_trigger_projection_work() {
        let service = ReportService::new_null();
        let request = ReconReq {
            epoch: ConsensusEpoch::ZERO,
            target: BlockHash::from(42),
            sources: vec![BlockHash::from(43)],
        };
        assert_eq!(
            service.answer_request(&request),
            Err(ReconRefusal::UnknownTarget)
        );
        assert!(service.source_refreshed.lock().unwrap().is_empty());
    }
}
