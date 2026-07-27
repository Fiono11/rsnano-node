#[cfg(feature = "rai_protocol")]
use std::collections::HashMap;
use std::{
    net::SocketAddrV6,
    sync::{Arc, Mutex, RwLock},
};

use tracing::trace;

use rsnano_messages::{Message, NetworkFilter};
#[cfg(feature = "rai_protocol")]
use rsnano_network::TrafficType;
use rsnano_network::{Channel, Network};
use rsnano_types::VoteDelivery;
#[cfg(feature = "rai_protocol")]
use rsnano_types::{BlockHash, RaiElectionId, RaiVote};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};
use rsnano_work::WorkThresholds;

use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::{bootstrapper::Bootstrapper, responder::BootstrapResponder},
    consensus::{AggregatorRequest, RequestAggregator, VoteProcessorQueue},
    telemetry::Telemetry,
    wallets::WalletRepresentatives,
};
use rsnano_ledger::BlockSource;

#[cfg(feature = "rai_protocol")]
use crate::{
    consensus::{RaiPendingReportProcessor, RaiVoteProcessor},
    transport::MessageFlooder,
};

/// Process messages that were received from other nodes in the network
pub struct NetworkMessageProcessor {
    stats: Arc<Stats>,
    network_filter: Arc<NetworkFilter>,
    network: Arc<RwLock<Network>>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    request_aggregator: Arc<RequestAggregator>,
    vote_processor_queue: Arc<VoteProcessorQueue>,
    #[cfg(feature = "rai_protocol")]
    rai_vote_processor: Arc<RaiVoteProcessor>,
    #[cfg(feature = "rai_protocol")]
    rai_pending_report_processor: Arc<RaiPendingReportProcessor>,
    #[cfg(feature = "rai_protocol")]
    rai_message_flooder: Mutex<MessageFlooder>,
    #[cfg(feature = "rai_protocol")]
    pending_close_votes: Mutex<HashMap<BlockHash, Vec<RaiVote>>>,
    telemetry: Arc<Telemetry>,
    bootstrap_responder: Arc<BootstrapResponder>,
    bootstrapper: Arc<Bootstrapper>,
    work_thresholds: WorkThresholds,
}

impl NetworkMessageProcessor {
    pub(crate) fn new(
        stats: Arc<Stats>,
        network: Arc<RwLock<Network>>,
        network_filter: Arc<NetworkFilter>,
        block_processor_queue: Arc<BlockProcessorQueue>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        request_aggregator: Arc<RequestAggregator>,
        vote_processor_queue: Arc<VoteProcessorQueue>,
        #[cfg(feature = "rai_protocol")] rai_vote_processor: Arc<RaiVoteProcessor>,
        #[cfg(feature = "rai_protocol")] rai_pending_report_processor: Arc<
            RaiPendingReportProcessor,
        >,
        #[cfg(feature = "rai_protocol")] rai_message_flooder: MessageFlooder,
        telemetry: Arc<Telemetry>,
        bootstrap_responder: Arc<BootstrapResponder>,
        bootstrapper: Arc<Bootstrapper>,
        work_thresholds: WorkThresholds,
    ) -> Self {
        Self {
            stats,
            network,
            network_filter,
            block_processor_queue,
            wallet_reps,
            request_aggregator,
            vote_processor_queue,
            #[cfg(feature = "rai_protocol")]
            rai_vote_processor,
            #[cfg(feature = "rai_protocol")]
            rai_pending_report_processor,
            #[cfg(feature = "rai_protocol")]
            rai_message_flooder: Mutex::new(rai_message_flooder),
            #[cfg(feature = "rai_protocol")]
            pending_close_votes: Mutex::new(HashMap::new()),
            telemetry,
            bootstrap_responder,
            bootstrapper,
            work_thresholds,
        }
    }

    pub fn process(&self, message: Message, channel: &Arc<Channel>) {
        self.stats.inc_dir(
            StatType::Message,
            message.message_type().into(),
            Direction::In,
        );

        trace!(
            ?message,
            channel_id = ?channel.channel_id(),
            "network processed"
        );

        match message {
            Message::Keepalive(keepalive) => {
                // Check for special node port data
                let peer0 = keepalive.peers[0];
                // The first entry is used to inform us of the peering address of the sending node
                if peer0.ip().is_unspecified() && peer0.port() != 0 {
                    let peering_addr =
                        SocketAddrV6::new(*channel.peer_addr().ip(), peer0.port(), 0, 0);

                    // Remember this for future forwarding to other peers
                    self.network
                        .read()
                        .unwrap()
                        .set_peering_addr(channel.channel_id(), peering_addr);
                }
            }
            Message::Publish(publish) => {
                let mut ok = true;

                if !self.work_thresholds.validate_entry_block(&publish.block) {
                    self.stats
                        .inc(StatType::BlockProcessor, DetailType::InsufficientWork);
                    ok = false;
                }

                if ok {
                    // Put blocks that are being initially broadcasted in a separate queue, so that they won't have to compete with rebroadcasted blocks
                    // Both queues have the same priority and size, so the potential for exploiting this is limited
                    let source = if publish.is_originator {
                        BlockSource::LiveOriginator
                    } else {
                        BlockSource::Live
                    };

                    trace!(block_hash = ?publish.block.hash(), channel_id = ?channel.channel_id(), "Received publish");

                    if self.bootstrapper.is_bootstrapping() {
                        // We ignore live blocks during bootstrap, so that those live blocks won't
                        // fill up the bootstrap queue
                        ok = false;
                    } else {
                        ok = self.block_processor_queue.push(BlockContext::new(
                            publish.block,
                            source,
                            channel.channel_id(),
                        ));
                    }
                }

                if !ok {
                    // The message couldn't be handled. We have to remove it from the duplicate
                    // filter, so that it can be retransmitted and handled later
                    self.network_filter.clear(publish.digest);
                    self.stats
                        .inc_dir(StatType::Drop, DetailType::Publish, Direction::In);
                }
            }
            Message::ConfirmReq(req) => {
                // Don't load nodes with disabled voting
                // TODO: This check should be cached somewhere
                if self.wallet_reps.lock().unwrap().voting_enabled() {
                    let aggregator_req = AggregatorRequest {
                        channel: channel.clone(),
                        roots_hashes: req.roots_hashes,
                    };
                    self.request_aggregator.request(aggregator_req);
                }
            }
            Message::ConfirmAck(ack) => {
                // Ignore zero account votes
                if ack.vote().voter.is_zero() {
                    self.stats.inc_dir(
                        StatType::Drop,
                        DetailType::ConfirmAckZeroAccount,
                        Direction::In,
                    );
                }

                let source = match ack.is_rebroadcasted() {
                    true => VoteDelivery::Forwarded,
                    false => VoteDelivery::Direct,
                };

                let added = self.vote_processor_queue.enqueue(
                    Arc::new(ack.vote().clone()),
                    Some(channel.clone()),
                    source,
                    None,
                );

                if !added {
                    // The message couldn't be handled. We have to remove it from the duplicate
                    // filter, so that it can be retransmitted and handled later
                    self.network_filter.clear(ack.digest);
                    self.stats
                        .inc_dir(StatType::Drop, DetailType::ConfirmAck, Direction::In);
                }
            }
            Message::Handshake(_) => {
                self.stats.inc_dir(
                    StatType::Message,
                    DetailType::NodeIdHandshake,
                    Direction::In,
                );
            }
            Message::TelemetryReq => {
                // Ignore telemetry requests as telemetry is being periodically broadcasted since V25+
            }
            Message::TelemetryAck(ack) => self.telemetry.process(&ack, channel),
            Message::AscPullReq(req) => {
                self.bootstrap_responder.enqueue(req, channel.clone());
            }
            Message::AscPullAck(ack) => self.bootstrapper.process(ack),
            #[cfg(feature = "rai_protocol")]
            Message::RaiVote(vote) => {
                let requests = self
                    .rai_vote_processor
                    .reconciliation_requests_for_vote(&vote);
                if !requests.is_empty() {
                    if let Some(target) = close_vote_hash(&vote) {
                        let mut pending = self.pending_close_votes.lock().unwrap();
                        let votes = pending.entry(target).or_default();
                        if !votes.contains(&vote) {
                            votes.push(vote.clone());
                        }
                    }
                    let mut sender = self.rai_message_flooder.lock().unwrap();
                    for request in requests {
                        sender.try_send_channel_id(
                            channel.channel_id(),
                            &Message::RaiReconciliation(request),
                            TrafficType::Generic,
                        );
                    }
                }
                if self.rai_vote_processor.process(&vote).is_ok() {
                    self.rai_message_flooder.lock().unwrap().flood(
                        &Message::RaiVote(vote),
                        TrafficType::Generic,
                        1.0,
                    );
                }
            }
            #[cfg(feature = "rai_protocol")]
            Message::RaiPendingReport(report) => {
                if self.rai_pending_report_processor.process(&report).is_ok() {
                    self.rai_message_flooder.lock().unwrap().flood(
                        &Message::RaiPendingReport(report),
                        TrafficType::Generic,
                        1.0,
                    );
                }
            }
            #[cfg(feature = "rai_protocol")]
            Message::RaiReconciliation(message) => match &message {
                rsnano_messages::RaiReconciliation::Request(request) => {
                    let response = self.rai_vote_processor.reconciliation_response(request);
                    self.rai_message_flooder
                        .lock()
                        .unwrap()
                        .try_send_channel_id(
                            channel.channel_id(),
                            &Message::RaiReconciliation(response),
                            TrafficType::Generic,
                        );
                }
                rsnano_messages::RaiReconciliation::CutDelta { target_hash, .. }
                | rsnano_messages::RaiReconciliation::FrontierDelta { target_hash, .. } => {
                    let attempt = self
                        .pending_close_votes
                        .lock()
                        .unwrap()
                        .get(target_hash)
                        .and_then(|votes| votes.first())
                        .and_then(close_vote_attempt)
                        .unwrap_or(0);
                    if self
                        .rai_vote_processor
                        .apply_reconciliation(&message, attempt)
                    {
                        let votes = self
                            .pending_close_votes
                            .lock()
                            .unwrap()
                            .remove(target_hash)
                            .unwrap_or_default();
                        for vote in votes {
                            if self.rai_vote_processor.process(&vote).is_ok() {
                                self.rai_message_flooder.lock().unwrap().flood(
                                    &Message::RaiVote(vote),
                                    TrafficType::Generic,
                                    1.0,
                                );
                            }
                        }
                    }
                }
                rsnano_messages::RaiReconciliation::Miss(_) => {}
            },
            Message::FrontierReq(_)
            | Message::BulkPush
            | Message::BulkPull(_)
            | Message::BulkPullAccount(_) => {
                // obsolete messages
            }
        }
    }
}

#[cfg(feature = "rai_protocol")]
fn close_vote_hash(vote: &RaiVote) -> Option<BlockHash> {
    match vote.value {
        rsnano_types::RaiElectionValue::CloseCutHash(hash)
        | rsnano_types::RaiElectionValue::CloseRecordHash(hash) => Some(hash),
        _ => None,
    }
}

#[cfg(feature = "rai_protocol")]
fn close_vote_attempt(vote: &RaiVote) -> Option<u64> {
    match vote.election_id {
        RaiElectionId::CloseCut { attempt, .. } | RaiElectionId::CloseRecord { attempt, .. } => {
            Some(attempt)
        }
        _ => None,
    }
}
