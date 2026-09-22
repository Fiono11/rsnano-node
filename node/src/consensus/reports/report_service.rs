use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_messages::{Message, ReconReply, ReconReq, Report, ResidualReply, ResidualReq};
use rsnano_network::{Channel, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{ConsensusEpoch, PublicKey};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use super::{ReconcileResult, ReportExchange, ReportMessage};
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
}

#[allow(dead_code)] // reconciled_count is read by the close, Section 9.1
impl ReportService {
    const LOG_INTERVAL: Duration = Duration::from_secs(1);

    pub fn new(
        active_elections: Arc<AecService>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        sender: MessageSender,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            exchange: Arc::new(Mutex::new(ReportExchange::new())),
            active_elections,
            wallet_reps,
            flooder: Mutex::new(flooder),
            sender: Mutex::new(sender),
            clock,
            stats,
            logged: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub fn new_null() -> Self {
        Self::new(
            Arc::new(AecService::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            MessageFlooder::new_null(),
            MessageSender::new_null(),
            Arc::new(SteadyClock::new_null()),
            Arc::new(Stats::default()),
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
        let certified = report.certified.len();
        let residual = report.residual.len();
        let root = report.certified.root();
        let messages = {
            let mut exchange = self.exchange.lock().unwrap();
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
            "EPOCH_REPORT epoch={} certified={} residual={} root={} reports={}",
            epoch,
            certified,
            residual,
            root,
            messages.len()
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
        for message in &messages {
            if let ReportMessage::Request(request) = message
                && self.log_due(epoch, false)
            {
                crate::utils::diagnostic!(
                    "EPOCH_RECON_REQUEST epoch={} reporter={} target={} sources={:?}",
                    epoch,
                    reporter,
                    request.target,
                    request
                        .sources
                        .iter()
                        .map(|root| root.to_string()[..8].to_string())
                        .collect::<Vec<_>>()
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
        let reply = self.exchange.lock().unwrap().handle_request(&request);
        match reply {
            Ok(reply) => self.send(vec![ReportMessage::Reply(reply)], Some(channel)),
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

    /// RAI: a difference towards a report this node is reconstructing. It is
    /// accepted exactly when the rebuilt state hashes to the signed root.
    pub fn handle_reply(&self, reply: ReconReply, _channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ReconReply, Direction::In);
        let result = self.exchange.lock().unwrap().handle_reply(&reply);
        log_reconciled(result);
    }

    /// RAI: a fetch of a residual object. Any replica that holds the object
    /// answers, whether it is its own or one it reconstructed.
    pub fn handle_residual_request(&self, request: ResidualReq, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ResidualReq, Direction::In);
        let reply = self
            .exchange
            .lock()
            .unwrap()
            .handle_residual_request(&request);
        if let Some(reply) = reply {
            self.send(vec![ReportMessage::ResidualAnswer(reply)], Some(channel));
        }
    }

    /// RAI: part of a residual object, accepted exactly when what has been
    /// accumulated hashes to the root the reporter signed
    pub fn handle_residual_reply(&self, reply: ResidualReply, _channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ResidualReply, Direction::In);
        let result = self.exchange.lock().unwrap().handle_residual_reply(&reply);
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
        let epochs: Vec<ConsensusEpoch> = self
            .active_elections
            .epoch_closes()
            .into_iter()
            .filter(|close| close.value.is_none())
            .map(|close| close.epoch)
            .collect();
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
                ReportMessage::ResidualRequest(request) => {
                    // Anyone that reconstructed the object can serve it, so
                    // the fetch is gossiped rather than addressed
                    self.stats
                        .inc_dir(StatType::Message, DetailType::ResidualReq, Direction::Out);
                    self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                        &Message::ResidualReq(request),
                        TrafficType::Generic,
                        1.0,
                    );
                }
                ReportMessage::ResidualAnswer(reply) => {
                    let Some(target) = channel else {
                        continue;
                    };
                    self.stats.inc_dir(
                        StatType::Message,
                        DetailType::ResidualReply,
                        Direction::Out,
                    );
                    self.sender.lock().unwrap().try_send(
                        target,
                        &Message::ResidualReply(reply),
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
