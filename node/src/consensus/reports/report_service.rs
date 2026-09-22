use std::sync::{Arc, Mutex};

use rsnano_messages::{Message, ReconReply, ReconReq, Report};
use rsnano_network::{Channel, TrafficType};
use rsnano_types::{ConsensusEpoch, PublicKey};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use super::{ReconcileResult, ReportExchange, ReportMessage};
use crate::{
    consensus::AecService,
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
    exchange: Mutex<ReportExchange>,
    active_elections: Arc<AecService>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    flooder: Mutex<MessageFlooder>,
    sender: Mutex<MessageSender>,
    stats: Arc<Stats>,
}

#[allow(dead_code)] // reconciled_count is read by the close, Section 9.1
impl ReportService {
    pub fn new(
        active_elections: Arc<AecService>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        sender: MessageSender,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            exchange: Mutex::new(ReportExchange::new()),
            active_elections,
            wallet_reps,
            flooder: Mutex::new(flooder),
            sender: Mutex::new(sender),
            stats,
        }
    }

    pub fn new_null() -> Self {
        Self::new(
            Arc::new(AecService::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            MessageFlooder::new_null(),
            MessageSender::new_null(),
            Arc::new(Stats::default()),
        )
    }

    /// Section 6.1: the node has left the epoch and issues no further
    /// ordinary votes for it. Its report is signed and broadcast.
    pub fn epoch_left(&self, epoch: ConsensusEpoch) {
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        if keys.is_empty() {
            return;
        }
        let Some(report) = self.active_elections.epoch_report(epoch) else {
            return;
        };
        let certified = report.certified.len();
        let residual = report.residual.len();
        let root = report.certified.root();
        let messages = {
            let mut exchange = self.exchange.lock().unwrap();
            exchange.report_epoch(
                epoch,
                report.certified,
                report.residual,
                report.committee,
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
    /// validated against
    pub fn reconcile(&self, epoch: ConsensusEpoch, reporter: PublicKey) {
        let Some((message, result)) = self.exchange.lock().unwrap().reconcile(epoch, reporter)
        else {
            return;
        };
        log_reconciled(result);
        self.send(message.into_iter().collect(), None);
    }

    /// RAI: a request for a reconstructive difference. This node answers only
    /// if it knows both states; no answer is not a verdict.
    pub fn handle_request(&self, request: ReconReq, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ReconReq, Direction::In);
        let reply = self.exchange.lock().unwrap().handle_request(&request);
        if let Some(reply) = reply {
            self.send(vec![ReportMessage::Reply(reply)], Some(channel));
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

    /// Drives the reconciliations of the epochs still closing. The live
    /// certified state is refreshed from the active elections first: gossip
    /// keeps delivering the votes of a closed epoch, and it is that growth
    /// which eventually gives this node a state it shares with a reporter.
    pub fn tick(&self) {
        let epochs: Vec<ConsensusEpoch> = self
            .active_elections
            .epoch_closes()
            .into_iter()
            .filter(|close| close.closed.is_none())
            .map(|close| close.epoch)
            .collect();
        for epoch in epochs {
            let reporters: Vec<PublicKey> = {
                let exchange = self.exchange.lock().unwrap();
                let usable: Vec<PublicKey> = exchange
                    .usable(epoch)
                    .iter()
                    .map(|(report, _)| report.reporter)
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
