use std::{
    net::SocketAddrV6,
    sync::{Arc, Mutex, RwLock},
};

use tracing::trace;

use rsnano_messages::{Message, NetworkFilter};
use rsnano_network::{Channel, Network};
use rsnano_types::VoteDelivery;
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};
use rsnano_work::WorkThresholds;

#[cfg(feature = "ledger_snapshots")]
use crate::ledger_snapshots::LedgerSnapshots;
use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::{bootstrapper::Bootstrapper, responder::BootstrapResponder},
    consensus::{AggregatorRequest, RequestAggregator, VoteProcessorQueue},
    telemetry::Telemetry,
    wallets::WalletRepresentatives,
};
use rsnano_ledger::BlockSource;

/// Process messages that were received from other nodes in the network
pub struct NetworkMessageProcessor {
    stats: Arc<Stats>,
    network_filter: Arc<NetworkFilter>,
    network: Arc<RwLock<Network>>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    request_aggregator: Arc<RequestAggregator>,
    vote_processor_queue: Arc<VoteProcessorQueue>,
    telemetry: Arc<Telemetry>,
    bootstrap_responder: Arc<BootstrapResponder>,
    bootstrapper: Arc<Bootstrapper>,
    work_thresholds: WorkThresholds,
    #[cfg(feature = "ledger_snapshots")]
    ledger_snapshots: Arc<LedgerSnapshots>,
    #[cfg(feature = "rai_protocol")]
    active_elections: Arc<crate::consensus::AecService>,
    #[cfg(feature = "rai_protocol")]
    message_sender: Mutex<crate::transport::MessageSender>,
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
        telemetry: Arc<Telemetry>,
        bootstrap_responder: Arc<BootstrapResponder>,
        bootstrapper: Arc<Bootstrapper>,
        work_thresholds: WorkThresholds,
        #[cfg(feature = "ledger_snapshots")] ledger_snapshots: Arc<LedgerSnapshots>,
        #[cfg(feature = "rai_protocol")] active_elections: Arc<crate::consensus::AecService>,
        #[cfg(feature = "rai_protocol")] message_sender: crate::transport::MessageSender,
    ) -> Self {
        Self {
            stats,
            network,
            network_filter,
            block_processor_queue,
            wallet_reps,
            request_aggregator,
            vote_processor_queue,
            telemetry,
            bootstrap_responder,
            bootstrapper,
            work_thresholds,
            #[cfg(feature = "ledger_snapshots")]
            ledger_snapshots,
            #[cfg(feature = "rai_protocol")]
            active_elections,
            #[cfg(feature = "rai_protocol")]
            message_sender: Mutex::new(message_sender),
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
                #[cfg(feature = "rai_protocol")]
                if std::env::var_os("RSNANO_RAI_TRACE_PR").is_some() {
                    eprintln!(
                        "RAI_SOLICIT_TRACE recv_confirm_req channel={:?} requests={:?}",
                        channel.channel_id(),
                        req.roots_hashes
                    );
                }
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
                #[cfg(feature = "rai_protocol")]
                if matches!(
                    ack.vote().metadata.election_id,
                    rsnano_types::RaiElectionId::CloseCut { .. }
                        | rsnano_types::RaiElectionId::CloseRecord { .. }
                ) {
                    tracing::warn!(
                        vote = ?ack.vote(),
                        rebroadcasted = ack.is_rebroadcasted(),
                        "RAI_CLOSE_TRACE close election vote receive"
                    );
                }
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

                #[cfg(feature = "rai_protocol")]
                if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                    let vote = ack.vote();
                    eprintln!(
                        "RAI_MSG pr={pr} event=recv_vote enqueued={added} channel={} id={:?} phase={:?} voter={} vote_hash={} hashes={:?}",
                        channel.channel_id(),
                        vote.metadata.election_id,
                        vote.metadata.phase,
                        vote.voter,
                        vote.hash(),
                        vote.hashes
                    );
                }

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
            Message::FrontierReq(_)
            | Message::BulkPush
            | Message::BulkPull(_)
            | Message::BulkPullAccount(_) => {
                // obsolete messages
            }
            #[cfg(feature = "ledger_snapshots")]
            Message::SnapshotPreproposal(preproposal) => {
                self.ledger_snapshots.handle_preproposal(preproposal);
            }
            #[cfg(feature = "ledger_snapshots")]
            Message::SnapshotProposal(proposal) => {
                self.ledger_snapshots.handle_proposal(proposal);
            }
            #[cfg(feature = "ledger_snapshots")]
            Message::SnapshotProposalVote(proposal_vote) => {
                self.ledger_snapshots.handle_vote(proposal_vote);
            }
            #[cfg(feature = "rai_protocol")]
            Message::RaiReport(report) => {
                self.active_elections.rai_report_received(report.into());
            }
            #[cfg(feature = "rai_protocol")]
            Message::RaiReportRequest(request) => {
                for report in self.active_elections.rai_reports_for_epoch(request.epoch) {
                    self.message_sender.lock().unwrap().try_send(
                        channel,
                        &Message::RaiReport(report.into()),
                        rsnano_network::TrafficType::Generic,
                    );
                }
            }
            #[cfg(feature = "rai_protocol")]
            Message::RaiVoteRequest(request) => {
                let requested_epoch = rsnano_types::RaiEpoch::new(request.epoch);
                if let Some(version) = request.close_version {
                    match version {
                        rsnano_messages::RaiCloseVersionWire::Cut(cut) => {
                            if cut.epoch == request.epoch
                                && cut
                                    .obligations
                                    .iter()
                                    .all(|slot| slot.epoch == requested_epoch)
                            {
                                let reconciled = self.active_elections.reconcile_rai_close_cut(
                                    crate::consensus::rai::RaiCloseCut::new(
                                        requested_epoch,
                                        cut.obligations,
                                    ),
                                    request.root,
                                );
                                if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                                    eprintln!(
                                        "RAI_MSG pr={pr} event=reconcile_cut epoch={requested_epoch:?} reconciled={reconciled}"
                                    );
                                }
                            }
                        }
                        rsnano_messages::RaiCloseVersionWire::Record(record) => {
                            if record.epoch == request.epoch {
                                let reconciled = self.active_elections.reconcile_rai_close_record(
                                    crate::consensus::rai::RaiCloseRecord::new(
                                        requested_epoch,
                                        record.previous,
                                        record.frontiers.into_iter().map(
                                            |(account, height, frontier)| {
                                                (
                                                    account,
                                                    rsnano_types::ConfirmationHeightInfo::new(
                                                        height, frontier,
                                                    ),
                                                )
                                            },
                                        ),
                                    ),
                                    request.root,
                                );
                                if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                                    eprintln!(
                                        "RAI_MSG pr={pr} event=reconcile_record epoch={requested_epoch:?} reconciled={reconciled}"
                                    );
                                }
                            }
                        }
                    }
                    return;
                }
                // Close votes authenticate only a hash. Send every locally
                // retained canonical preimage as a one-way repair reply so a
                // lagging peer can validate and apply the signed vote leaves.
                // A lagging peer may already be in a successor round which
                // this responder never entered. Recognize the synthetic root
                // from the requested epoch/round domain rather than only from
                // locally retained election ids.
                let is_close_request = (0..=1024).any(|round| {
                    crate::consensus::rai::rai_close_cut_root(requested_epoch, round).root
                        == request.root
                        || crate::consensus::rai::rai_close_record_root(requested_epoch, round).root
                            == request.root
                });
                if !is_close_request {
                    for block in self.active_elections.rai_blocks_for_request(
                        request.hash,
                        request.root,
                        requested_epoch,
                    ) {
                        self.message_sender.lock().unwrap().try_send(
                            channel,
                            &Message::Publish(rsnano_messages::Publish::new_forward(block)),
                            rsnano_network::TrafficType::BlockBroadcast,
                        );
                    }
                }
                let cut_versions = is_close_request
                    .then(|| {
                        self.active_elections
                            .rai_close_cut_versions(requested_epoch)
                    })
                    .unwrap_or_default();
                let record_versions = is_close_request
                    .then(|| {
                        self.active_elections
                            .rai_close_record_versions(requested_epoch)
                    })
                    .unwrap_or_default();
                if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                    eprintln!(
                        "RAI_MSG pr={pr} event=repair_request epoch={requested_epoch:?} root={} cuts={} records={}",
                        request.root,
                        cut_versions.len(),
                        record_versions.len()
                    );
                }
                for cut in cut_versions {
                    self.message_sender.lock().unwrap().try_send(
                        channel,
                        &Message::RaiVoteRequest(rsnano_messages::RaiVoteRequest {
                            sequence: request.sequence,
                            epoch: request.epoch,
                            hash: cut.hash(),
                            root: request.root,
                            close_version: Some(rsnano_messages::RaiCloseVersionWire::Cut(
                                rsnano_messages::RaiCloseCutWire {
                                    epoch: request.epoch,
                                    obligations: cut.obligations.into_iter().collect(),
                                },
                            )),
                        }),
                        rsnano_network::TrafficType::VoteReply,
                    );
                }
                for record in record_versions {
                    self.message_sender.lock().unwrap().try_send(
                        channel,
                        &Message::RaiVoteRequest(rsnano_messages::RaiVoteRequest {
                            sequence: request.sequence,
                            epoch: request.epoch,
                            hash: record.hash(),
                            root: request.root,
                            close_version: Some(rsnano_messages::RaiCloseVersionWire::Record(
                                rsnano_messages::RaiCloseRecordWire {
                                    epoch: request.epoch,
                                    previous: record.previous,
                                    frontiers: record
                                        .frontiers
                                        .into_iter()
                                        .map(|(account, info)| {
                                            (account, info.height, info.frontier)
                                        })
                                        .collect(),
                                },
                            )),
                        }),
                        rsnano_network::TrafficType::VoteReply,
                    );
                }
                // Return all signed evidence retained for this election, not
                // merely this node's own cached vote. A single replica which
                // already has a certificate can therefore repair a lagging
                // peer even when earlier requests reached an incomplete set of
                // representatives.
                let votes = if is_close_request {
                    self.active_elections
                        .rai_close_votes_for_epoch(requested_epoch)
                } else {
                    self.active_elections
                        .rai_votes_for_root(&request.root, requested_epoch)
                };
                for vote in votes {
                    self.message_sender.lock().unwrap().try_send(
                        channel,
                        &Message::ConfirmAck(rsnano_messages::ConfirmAck::new_with_own_vote(vote)),
                        rsnano_network::TrafficType::VoteReply,
                    );
                }
                if self.wallet_reps.lock().unwrap().voting_enabled() {
                    // A compact terminal marker is authoritative for this
                    // slot even if drain repair has recreated a pending local
                    // election for the same root.  Prefer regenerating the
                    // notar vote from that marker; otherwise the pending
                    // election masks the ended outcome and peers can remain
                    // split between Draining and ElectingRecord forever.
                    let generated_notar = self.request_aggregator.generate_rai_notar_vote(
                        &request.hash,
                        &request.root,
                        requested_epoch,
                        channel,
                    );
                    if generated_notar == 0
                        && self.active_elections.rai_has_active_request_target(
                            &request.hash,
                            &request.root,
                            requested_epoch,
                        )
                    {
                        // Preserve the epoch carried by RaiVoteRequest. Slot
                        // roots can recur in several active epochs; routing
                        // this through the legacy aggregator (root/hash only)
                        // could sign for an arbitrary newer election and
                        // leave the requested drain permanently unrepaired.
                        self.request_aggregator.generate_rai_active_slot_vote(
                            &request.root,
                            requested_epoch,
                            channel,
                        );
                    } else if generated_notar == 0 {
                        self.request_aggregator.generate_rai_final_vote(
                            &request.hash,
                            &request.root,
                            requested_epoch,
                            channel,
                        );
                    }
                }
            }
        }
    }
}

#[cfg(feature = "ledger_snapshots")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preproposal_is_received() {
        use rsnano_messages::Preproposal;

        let ledger_snapshots = LedgerSnapshots::new_null();
        let receive_tracker = ledger_snapshots.track_received_preproposals();
        let network_message_processor = create_network_message_processor(ledger_snapshots);
        let preproposal = Preproposal::new_test_instance();

        network_message_processor.process(
            Message::SnapshotPreproposal(preproposal.clone()),
            &Channel::new_test_instance().into(),
        );

        assert_eq!(receive_tracker.output(), vec![preproposal]);
    }

    #[test]
    fn proposal_is_received() {
        use rsnano_messages::Proposal;

        let ledger_snapshots: LedgerSnapshots = LedgerSnapshots::new_null();
        let receive_tracker = ledger_snapshots.track_received_proposals();
        let network_message_processor = create_network_message_processor(ledger_snapshots);
        let proposal = Proposal::new_test_instance();

        network_message_processor.process(
            Message::SnapshotProposal(proposal.clone()),
            &Channel::new_test_instance().into(),
        );

        assert_eq!(receive_tracker.output(), vec![proposal]);
    }

    #[test]
    fn proposal_vote_is_received() {
        use rsnano_messages::ProposalVote;

        let ledger_snapshots: LedgerSnapshots = LedgerSnapshots::new_null();
        let receive_tracker = ledger_snapshots.track_received_votes();
        let network_message_processor = create_network_message_processor(ledger_snapshots);
        let proposal_vote = ProposalVote::new_test_instance();

        network_message_processor.process(
            Message::SnapshotProposalVote(proposal_vote.clone()),
            &Channel::new_test_instance().into(),
        );

        assert_eq!(receive_tracker.output(), vec![proposal_vote]);
    }

    fn create_network_message_processor(
        ledger_snapshots: LedgerSnapshots,
    ) -> NetworkMessageProcessor {
        NetworkMessageProcessor::new(
            Stats::default().into(),
            RwLock::new(Network::new_null()).into(),
            NetworkFilter::default().into(),
            BlockProcessorQueue::new_null().into(),
            Mutex::new(WalletRepresentatives::new_null()).into(),
            RequestAggregator::new_null().into(),
            VoteProcessorQueue::new_null().into(),
            Telemetry::new_null().into(),
            BootstrapResponder::new_null().into(),
            Bootstrapper::new_null().into(),
            WorkThresholds::new_stub(),
            ledger_snapshots.into(),
            #[cfg(feature = "rai_protocol")]
            crate::consensus::AecService::new_null().into(),
            #[cfg(feature = "rai_protocol")]
            crate::transport::MessageSender::new_null(),
        )
    }
}
