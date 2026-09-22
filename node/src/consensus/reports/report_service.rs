use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_messages::{Message, Report, ReportAck, ReportReq};
use rsnano_network::{Channel, TrafficType};
use rsnano_nullable_clock::SteadyClock;
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
    /// Where each reporter's report came from: its requests go back there
    channels: Mutex<HashMap<(ConsensusEpoch, PublicKey), Arc<Channel>>>,
    active_elections: Arc<AecService>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    flooder: Mutex<MessageFlooder>,
    sender: Mutex<MessageSender>,
    clock: Arc<SteadyClock>,
    stats: Arc<Stats>,
}

#[allow(dead_code)] // reconciled_count is read by the close, Section 9.1
impl ReportService {
    /// How long an unanswered reconciliation request waits before it is
    /// repeated
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

    pub fn new(
        active_elections: Arc<AecService>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        sender: MessageSender,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            exchange: Mutex::new(ReportExchange::new()),
            channels: Mutex::new(HashMap::new()),
            active_elections,
            wallet_reps,
            flooder: Mutex::new(flooder),
            sender: Mutex::new(sender),
            clock,
            stats,
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

    /// Section 6.1: the node has left the epoch and issues no further
    /// ordinary votes for it. Its report is signed and broadcast.
    pub fn epoch_left(&self, epoch: ConsensusEpoch) {
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        if keys.is_empty() {
            return;
        }
        let Some((map, committee)) = self.active_elections.epoch_report(epoch) else {
            return;
        };
        let entries = map.len();
        let root = map.root();
        let messages = {
            let mut exchange = self.exchange.lock().unwrap();
            exchange.report_epoch(epoch, map, committee, &keys)
        };
        if messages.is_empty() {
            return;
        }
        crate::utils::diagnostic!(
            "EPOCH_REPORT epoch={} entries={} root={} committee={} reports={}",
            epoch,
            entries,
            root,
            committee,
            messages.len()
        );
        self.send(messages, None);
    }

    /// A report of another replica: verified and stored. It is reconciled
    /// when a close has to be validated against it (Section 6.2), not on
    /// arrival: a reconciliation transfers most of the reporter's map, and
    /// nothing decides on a report yet.
    pub fn handle_report(&self, report: Report, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::Report, Direction::In);
        let key = (report.epoch, report.reporter);
        let taken = self.exchange.lock().unwrap().handle_report(report);
        if taken {
            self.channels.lock().unwrap().insert(key, channel.clone());
        }
    }

    /// Section 6.2: reconcile a stored report, which a close proposal has to
    /// be validated against
    pub fn reconcile(&self, epoch: ConsensusEpoch, reporter: PublicKey) {
        let now = self.clock.now();
        let message = self
            .exchange
            .lock()
            .unwrap()
            .reconcile(epoch, reporter, now);
        self.send(message.into_iter().collect(), None);
    }

    /// Section 6.2: a reconciliation request for one of this node's reports
    pub fn handle_request(&self, request: ReportReq, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ReportReq, Direction::In);
        let local_reps: Vec<PublicKey> = self.wallet_reps.lock().unwrap().rep_pub_keys().collect();
        let ack = self
            .exchange
            .lock()
            .unwrap()
            .handle_request(&request, &local_reps);
        if let Some(ack) = ack {
            self.send(vec![ReportMessage::Reply(ack)], Some(channel));
        }
    }

    /// Section 6.2: one part of a report this node is reconciling
    pub fn handle_ack(&self, ack: ReportAck, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ReportAck, Direction::In);
        let now = self.clock.now();
        let (messages, result) = self.exchange.lock().unwrap().handle_ack(ack, now);
        log_reconciled(result);
        self.send(messages, Some(channel));
    }

    /// Repeats the reconciliation requests whose answers did not come
    pub fn tick(&self) {
        let now = self.clock.now();
        let messages = self
            .exchange
            .lock()
            .unwrap()
            .due_requests(now, Self::REQUEST_TIMEOUT);
        self.send(messages, None);
    }

    /// How many reports of the epoch are reconciled here: what a close
    /// proposal could select from (Section 7 needs N−f of them)
    pub fn reconciled_count(&self, epoch: ConsensusEpoch) -> usize {
        self.exchange.lock().unwrap().reconciled(epoch).len()
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
                    let target = channel.cloned().or_else(|| {
                        self.channels
                            .lock()
                            .unwrap()
                            .get(&(request.epoch, request.reporter))
                            .cloned()
                    });
                    let Some(target) = target else {
                        continue;
                    };
                    self.stats
                        .inc_dir(StatType::Message, DetailType::ReportReq, Direction::Out);
                    self.sender.lock().unwrap().try_send(
                        &target,
                        &Message::ReportReq(request),
                        TrafficType::Generic,
                    );
                }
                ReportMessage::Reply(ack) => {
                    let Some(target) = channel else {
                        continue;
                    };
                    self.stats
                        .inc_dir(StatType::Message, DetailType::ReportAck, Direction::Out);
                    self.sender.lock().unwrap().try_send(
                        target,
                        &Message::ReportAck(ack),
                        TrafficType::Generic,
                    );
                }
            }
        }
    }
}

/// RAI, Section 6.2: what one reconciliation cost, for the record. The
/// authenticated dictionary is a fixed partition of the key space, so a
/// difference spread over many slots touches many buckets; this is how that
/// cost is measured against a real workload.
fn log_reconciled(result: Option<ReconcileResult>) {
    let Some(result) = result else {
        return;
    };
    crate::utils::diagnostic!(
        "EPOCH_RECONCILED epoch={} reporter={} complete={} requests={} entries={} total={}",
        result.epoch,
        result.reporter,
        result.complete,
        result.requests,
        result.entries,
        result.total
    );
}
