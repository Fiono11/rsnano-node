#[cfg(feature = "rai_protocol")]
use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};
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
    #[cfg(feature = "rai_protocol")]
    rai_close_repair_assembler: Mutex<RaiCloseRepairAssembler>,
}

#[cfg(feature = "rai_protocol")]
// A close round may have one replica-relative candidate per committee member.
// Repair rotates bounded response windows across those candidates.  Keep a
// partial canonical preimage long enough for every chunk to occupy the head of
// a peer's bounded write queue even under that worst-case rotation.
const RAI_CLOSE_REPAIR_ASSEMBLY_TTL: Duration = Duration::from_secs(120);
#[cfg(feature = "rai_protocol")]
const MAX_PENDING_RAI_CLOSE_REPAIR_ASSEMBLIES: usize = 16;
#[cfg(feature = "rai_protocol")]
const MAX_RAI_CLOSE_REPAIR_CHUNKS_PER_RESPONSE: usize = 16;
#[cfg(feature = "rai_protocol")]
const MAX_RAI_REPORT_CHUNKS_PER_RESPONSE: usize = 4;

/// A channel only has a bounded write queue. Sending every chunk of a very
/// large preimage on every retry would repeatedly fill it with the first
/// chunks and starve the tail. Rotate a bounded window by request sequence so
/// retries eventually cover the complete candidate without monopolizing the
/// vote-reply queue.
#[cfg(feature = "rai_protocol")]
fn rai_close_repair_response_window<T>(mut chunks: Vec<T>, sequence: u64) -> Vec<T> {
    if chunks.len() <= MAX_RAI_CLOSE_REPAIR_CHUNKS_PER_RESPONSE {
        return chunks;
    }
    // Advance one position rather than one whole window. If queue or
    // bandwidth pressure admits only a prefix of each response window, every
    // chunk still eventually occupies the first (most likely admitted) slot.
    let start = sequence % chunks.len() as u64;
    chunks.rotate_left(start as usize);
    chunks.truncate(MAX_RAI_CLOSE_REPAIR_CHUNKS_PER_RESPONSE);
    chunks
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RaiVoteRequestKind {
    Close,
    MarkedSlot,
    Legacy,
}

#[cfg(feature = "rai_protocol")]
impl RaiVoteRequestKind {
    fn permits_cached_vote_replay(self) -> bool {
        self == Self::Legacy
    }

    fn permits_vote_generation(self) -> bool {
        self == Self::Legacy
    }
}

#[cfg(feature = "rai_protocol")]
fn rai_close_kind_for_root(
    epoch: rsnano_types::RaiEpoch,
    root: &rsnano_types::Root,
) -> Option<crate::consensus::rai::RaiCloseKind> {
    // Retain the bounded root-only compatibility check for old envelopes.
    // Current exact requests carry the marker and a digest, but the synthetic
    // root still selects whether that digest denotes a cut or a record.
    (0..=1024).find_map(|round| {
        if crate::consensus::rai::rai_close_cut_root(epoch, round).root == *root {
            Some(crate::consensus::rai::RaiCloseKind::Cut)
        } else if crate::consensus::rai::rai_close_record_root(epoch, round).root == *root {
            Some(crate::consensus::rai::RaiCloseKind::Record)
        } else {
            None
        }
    })
}

#[cfg(feature = "rai_protocol")]
fn rai_close_preimage_response(
    sequence: u64,
    epoch: u64,
    hash: rsnano_types::BlockHash,
    root: rsnano_types::Root,
    close_version: Option<rsnano_messages::RaiCloseVersionWire>,
) -> Vec<rsnano_messages::RaiVoteRequest> {
    let Some(close_version) = close_version else {
        return Vec::new();
    };
    rai_close_repair_response_window(
        rsnano_messages::RaiVoteRequest {
            sequence,
            epoch,
            hash,
            root,
            close_version: Some(close_version),
        }
        .into_chunks(),
        sequence,
    )
}

/// New peers reserve sequence high bits as O(1) repair discriminators. The
/// bounded synthetic-root fallback preserves compatibility with peers which
/// predate those markers without penalizing ordinary traffic.
#[cfg(feature = "rai_protocol")]
fn classify_rai_vote_request(
    request: &rsnano_messages::RaiVoteRequest,
    requested_epoch: rsnano_types::RaiEpoch,
) -> Option<RaiVoteRequestKind> {
    let repair_kind = request.sequence
        & (rsnano_messages::RAI_CLOSE_REPAIR_SEQUENCE_FLAG
            | rsnano_messages::RAI_SLOT_REPAIR_SEQUENCE_FLAG);
    if repair_kind == rsnano_messages::RAI_SLOT_REPAIR_SEQUENCE_FLAG {
        // A marked slot envelope is payload repair only. Cached and fresh
        // vote evidence belongs on the ordinary batched ConfirmReq path. ZERO
        // requests the certificate-selected slot payload; a nonzero value is
        // an exact transitive dependency request discovered while installing
        // that certified frontier.
        return Some(RaiVoteRequestKind::MarkedSlot);
    }
    if repair_kind == rsnano_messages::RAI_CLOSE_REPAIR_SEQUENCE_FLAG {
        // Current close requests name the exact nonzero signed candidate whose
        // preimage is missing. Replies use the same marker and add the
        // canonical close_version payload.
        return Some(RaiVoteRequestKind::Close);
    }
    if repair_kind != 0 {
        // Both reserved kind bits set is not a valid request classification.
        return None;
    }
    if !request.hash.is_zero() {
        return Some(RaiVoteRequestKind::Legacy);
    }
    // Compatibility path only. Current senders always use the sequence flag.
    let is_close = rai_close_kind_for_root(requested_epoch, &request.root).is_some();
    Some(if is_close {
        RaiVoteRequestKind::Close
    } else {
        RaiVoteRequestKind::Legacy
    })
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RaiCloseRepairKey {
    channel_id: rsnano_network::ChannelId,
    epoch: u64,
    hash: rsnano_types::BlockHash,
    root: rsnano_types::Root,
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RaiCloseRecordRepairKey {
    common: RaiCloseRepairKey,
    previous: rsnano_types::BlockHash,
}

#[cfg(feature = "rai_protocol")]
struct RaiCloseChunkAssembly<T> {
    chunk_count: u32,
    chunks: BTreeMap<u32, Vec<T>>,
    updated: Instant,
}

#[cfg(feature = "rai_protocol")]
impl<T> RaiCloseChunkAssembly<T> {
    fn new(chunk_count: u32, now: Instant) -> Self {
        Self {
            chunk_count,
            chunks: BTreeMap::new(),
            updated: now,
        }
    }

    fn insert(&mut self, chunk_index: u32, entries: Vec<T>, now: Instant) {
        self.chunks.insert(chunk_index, entries);
        self.updated = now;
    }

    fn is_complete(&self) -> bool {
        self.chunks.len() == self.chunk_count as usize
    }

    fn into_entries(self) -> Option<Vec<T>> {
        if !self.is_complete() || !self.chunks.keys().copied().eq(0..self.chunk_count) {
            return None;
        }
        Some(self.chunks.into_values().flatten().collect())
    }
}

#[cfg(feature = "rai_protocol")]
#[derive(Default)]
struct RaiCloseRepairAssembler {
    cuts: HashMap<RaiCloseRepairKey, RaiCloseChunkAssembly<rsnano_types::RaiSlotId>>,
    records: HashMap<
        RaiCloseRecordRepairKey,
        RaiCloseChunkAssembly<(rsnano_types::Account, u64, rsnano_types::BlockHash)>,
    >,
}

#[cfg(feature = "rai_protocol")]
impl RaiCloseRepairAssembler {
    fn push_chunks<K, T>(
        assemblies: &mut HashMap<K, RaiCloseChunkAssembly<T>>,
        key: K,
        chunk_index: u32,
        chunk_count: u32,
        entries: Vec<T>,
    ) -> Option<Vec<T>>
    where
        K: Clone + Eq + std::hash::Hash,
    {
        let now = Instant::now();
        assemblies.retain(|_, assembly| {
            now.saturating_duration_since(assembly.updated) < RAI_CLOSE_REPAIR_ASSEMBLY_TTL
        });
        if !assemblies.contains_key(&key)
            && assemblies.len() >= MAX_PENDING_RAI_CLOSE_REPAIR_ASSEMBLIES
            && let Some(oldest) = assemblies
                .iter()
                .min_by_key(|(_, assembly)| assembly.updated)
                .map(|(key, _)| key.clone())
        {
            assemblies.remove(&oldest);
        }

        let assembly = assemblies
            .entry(key.clone())
            .or_insert_with(|| RaiCloseChunkAssembly::new(chunk_count, now));
        if assembly.chunk_count != chunk_count {
            *assembly = RaiCloseChunkAssembly::new(chunk_count, now);
        }
        assembly.insert(chunk_index, entries, now);

        if assembly.is_complete() {
            assemblies.remove(&key)?.into_entries()
        } else {
            None
        }
    }

    fn push_cut(
        &mut self,
        channel_id: rsnano_network::ChannelId,
        hash: rsnano_types::BlockHash,
        root: rsnano_types::Root,
        chunk: rsnano_messages::RaiCloseCutChunkWire,
    ) -> Option<rsnano_messages::RaiCloseCutWire> {
        if !chunk.has_valid_layout() {
            return None;
        }
        let key = RaiCloseRepairKey {
            channel_id,
            epoch: chunk.epoch,
            hash,
            root,
        };
        Self::push_chunks(
            &mut self.cuts,
            key,
            chunk.chunk_index,
            chunk.chunk_count,
            chunk.obligations,
        )
        .map(|obligations| rsnano_messages::RaiCloseCutWire {
            epoch: chunk.epoch,
            obligations,
        })
    }

    fn push_record(
        &mut self,
        channel_id: rsnano_network::ChannelId,
        hash: rsnano_types::BlockHash,
        root: rsnano_types::Root,
        chunk: rsnano_messages::RaiCloseRecordChunkWire,
    ) -> Option<rsnano_messages::RaiCloseRecordWire> {
        if !chunk.has_valid_layout() {
            return None;
        }
        let key = RaiCloseRecordRepairKey {
            common: RaiCloseRepairKey {
                channel_id,
                epoch: chunk.epoch,
                hash,
                root,
            },
            previous: chunk.previous,
        };
        Self::push_chunks(
            &mut self.records,
            key,
            chunk.chunk_index,
            chunk.chunk_count,
            chunk.frontiers,
        )
        .map(|frontiers| rsnano_messages::RaiCloseRecordWire {
            epoch: chunk.epoch,
            previous: chunk.previous,
            frontiers,
        })
    }
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
            #[cfg(feature = "rai_protocol")]
            rai_close_repair_assembler: Mutex::new(RaiCloseRepairAssembler::default()),
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
                    #[cfg(feature = "rai_protocol")]
                    let roots_hashes: Vec<_> = req
                        .roots_hashes
                        .into_iter()
                        .filter(|(hash, root)| {
                            !hash.is_zero()
                                || !self
                                    .request_aggregator
                                    .handle_rai_close_control_request(root, channel)
                        })
                        .collect();
                    #[cfg(not(feature = "rai_protocol"))]
                    let roots_hashes = req.roots_hashes;
                    if roots_hashes.is_empty() {
                        return;
                    }
                    let aggregator_req = AggregatorRequest {
                        channel: channel.clone(),
                        roots_hashes,
                    };
                    self.request_aggregator.request(aggregator_req);
                }
            }
            Message::ConfirmAck(ack) => {
                #[cfg(feature = "rai_protocol")]
                if ack.vote().rai_metadata_iter().any(|metadata| {
                    matches!(
                        &metadata.election_id,
                        rsnano_types::RaiElectionId::CloseCut { .. }
                            | rsnano_types::RaiElectionId::CloseRecord { .. }
                    )
                }) {
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
                        "RAI_MSG pr={pr} event=recv_vote enqueued={added} channel={} metadata={:?} voter={} vote_hash={} hashes={:?}",
                        channel.channel_id(),
                        vote.rai_metadata_iter().collect::<Vec<_>>(),
                        vote.voter,
                        vote.hash(),
                        vote.hashes().collect::<Vec<_>>()
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
                // Reports can be close to the 64 KiB wire-frame limit. Borrow
                // the epoch-sized store and clone only this rotating response
                // window so retries remain bounded without monopolizing the
                // AEC lock or repeatedly copying every retained payload.
                let reports = self.active_elections.rai_report_response_window(
                    request.epoch,
                    request.sequence,
                    MAX_RAI_REPORT_CHUNKS_PER_RESPONSE,
                );
                let mut sender = self.message_sender.lock().unwrap();
                for report in reports {
                    if !sender.try_send(
                        channel,
                        &Message::RaiReport(report.into()),
                        rsnano_network::TrafficType::VoteReply,
                    ) {
                        break;
                    }
                }
            }
            #[cfg(feature = "rai_protocol")]
            Message::RaiVoteRequest(request) => {
                let requested_epoch = rsnano_types::RaiEpoch::new(request.epoch);
                let Some(request_kind) = classify_rai_vote_request(&request, requested_epoch)
                else {
                    return;
                };
                if let Some(version) = request.close_version {
                    let complete_version = match version {
                        rsnano_messages::RaiCloseVersionWire::Cut(cut) => (cut.epoch
                            == request.epoch
                            && cut
                                .obligations
                                .iter()
                                .all(|slot| slot.epoch == requested_epoch))
                        .then_some(rsnano_messages::RaiCloseVersionWire::Cut(cut)),
                        rsnano_messages::RaiCloseVersionWire::Record(record) => (record.epoch
                            == request.epoch)
                            .then_some(rsnano_messages::RaiCloseVersionWire::Record(record)),
                        rsnano_messages::RaiCloseVersionWire::CutChunk(chunk) => {
                            if chunk.epoch == request.epoch
                                && chunk
                                    .obligations
                                    .iter()
                                    .all(|slot| slot.epoch == requested_epoch)
                            {
                                self.rai_close_repair_assembler
                                    .lock()
                                    .unwrap()
                                    .push_cut(
                                        channel.channel_id(),
                                        request.hash,
                                        request.root,
                                        chunk,
                                    )
                                    .map(rsnano_messages::RaiCloseVersionWire::Cut)
                            } else {
                                None
                            }
                        }
                        rsnano_messages::RaiCloseVersionWire::RecordChunk(chunk) => {
                            if chunk.epoch == request.epoch {
                                self.rai_close_repair_assembler
                                    .lock()
                                    .unwrap()
                                    .push_record(
                                        channel.channel_id(),
                                        request.hash,
                                        request.root,
                                        chunk,
                                    )
                                    .map(rsnano_messages::RaiCloseVersionWire::Record)
                            } else {
                                None
                            }
                        }
                    };
                    match complete_version {
                        Some(rsnano_messages::RaiCloseVersionWire::Cut(cut)) => {
                            let cut = crate::consensus::rai::RaiCloseCut::new(
                                requested_epoch,
                                cut.obligations,
                            );
                            // The envelope names the candidate whose signed
                            // evidence triggered repair. Do not retain an
                            // unrelated preimage supplied under that request.
                            if cut.hash() != request.hash {
                                return;
                            }
                            let reconciled = self
                                .active_elections
                                .reconcile_rai_close_cut(cut, request.root);
                            if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                                eprintln!(
                                    "RAI_MSG pr={pr} event=reconcile_cut epoch={requested_epoch:?} reconciled={reconciled}"
                                );
                            }
                        }
                        Some(rsnano_messages::RaiCloseVersionWire::Record(record)) => {
                            let record = crate::consensus::rai::RaiCloseRecord::new(
                                requested_epoch,
                                record.previous,
                                record
                                    .frontiers
                                    .into_iter()
                                    .map(|(account, height, frontier)| {
                                        (
                                            account,
                                            rsnano_types::ConfirmationHeightInfo::new(
                                                height, frontier,
                                            ),
                                        )
                                    }),
                            );
                            if record.hash() != request.hash {
                                return;
                            }
                            let reconciled = self
                                .active_elections
                                .reconcile_rai_close_record(record, request.root);
                            if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                                eprintln!(
                                    "RAI_MSG pr={pr} event=reconcile_record epoch={requested_epoch:?} reconciled={reconciled}"
                                );
                            }
                        }
                        Some(
                            rsnano_messages::RaiCloseVersionWire::CutChunk(_)
                            | rsnano_messages::RaiCloseVersionWire::RecordChunk(_),
                        )
                        | None => {}
                    }
                    return;
                }
                if request_kind == RaiVoteRequestKind::Close {
                    // ZERO was the old epoch-wide query. It deliberately has
                    // no reply now: a custom close request must be justified
                    // by one retained signed leaf naming an exact digest.
                    if request.hash.is_zero() {
                        return;
                    }
                    let close_version =
                        match rai_close_kind_for_root(requested_epoch, &request.root) {
                            Some(crate::consensus::rai::RaiCloseKind::Cut) => self
                                .active_elections
                                .rai_close_cut_version(requested_epoch, &request.hash)
                                .map(|cut| {
                                    rsnano_messages::RaiCloseVersionWire::Cut(
                                        rsnano_messages::RaiCloseCutWire {
                                            epoch: request.epoch,
                                            obligations: cut.obligations.into_iter().collect(),
                                        },
                                    )
                                }),
                            Some(crate::consensus::rai::RaiCloseKind::Record) => self
                                .active_elections
                                .rai_close_record_version(requested_epoch, &request.hash)
                                .map(|record| {
                                    rsnano_messages::RaiCloseVersionWire::Record(
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
                                    )
                                }),
                            None => None,
                        };
                    let replies = rai_close_preimage_response(
                        request.sequence,
                        request.epoch,
                        request.hash,
                        request.root,
                        close_version,
                    );
                    if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                        eprintln!(
                            "RAI_MSG pr={pr} event=repair_request epoch={requested_epoch:?} root={} hash={} chunks={}",
                            request.root,
                            request.hash,
                            replies.len()
                        );
                    }
                    let mut sender = self.message_sender.lock().unwrap();
                    for reply in replies {
                        if !sender.try_send(
                            channel,
                            &Message::RaiVoteRequest(reply),
                            rsnano_network::TrafficType::RaiCloseControl,
                        ) {
                            break;
                        }
                    }
                    // Close votes remain exclusively on the ordinary batched
                    // ConfirmReq/ConfirmAck path. Never replay or generate a
                    // ConfirmAck from this custom preimage exchange.
                    return;
                }

                for block in self.active_elections.rai_blocks_for_request(
                    request.hash,
                    request.root,
                    requested_epoch,
                ) {
                    // Keep a requested repair payload on the same bounded
                    // ordinary request lane as its solicitation. The bulk
                    // BlockBroadcast lane can be saturated by the workload
                    // precisely when a draining replica needs this reply.
                    self.message_sender.lock().unwrap().try_send(
                        channel,
                        &Message::Publish(rsnano_messages::Publish::new_forward(block)),
                        rsnano_network::TrafficType::ConfirmationRequests,
                    );
                }
                // Marked slot repair is Publish-only. Compatibility requests
                // may still use the old replay behavior; current slot votes
                // travel through ordinary batched ConfirmReq.
                let votes = if request_kind.permits_cached_vote_replay() {
                    self.active_elections
                        .rai_votes_for_root(&request.root, requested_epoch)
                } else {
                    Vec::new()
                };
                for vote in votes {
                    self.message_sender.lock().unwrap().try_send(
                        channel,
                        &Message::ConfirmAck(rsnano_messages::ConfirmAck::new_with_own_vote(vote)),
                        rsnano_network::TrafficType::VoteReply,
                    );
                }
                if request_kind.permits_vote_generation()
                    && self.wallet_reps.lock().unwrap().voting_enabled()
                {
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

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_close_repair_tests {
    use super::*;
    use crate::{
        consensus::{
            ActiveElectionsConfig, AecInsertRequest, ApplyVoteArgs, FilteredVote, ReceivedVote,
        },
        representatives::QuorumSnapshot,
    };
    use rsnano_ledger::{Ledger, RepWeights};
    use rsnano_messages::{RaiCloseVersionWire, RaiVoteRequest};
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{
        Account, Amount, BlockHash, BlockPriority, PrivateKey, QualifiedRoot, RaiCommitteeScope,
        RaiElectionId, RaiEpoch, RaiSlotId, RaiVoteMetadata, RaiVotePhase, Root, SavedBlock,
        UnixMillisTimestamp, Vote, VoteDelivery,
    };
    use std::{collections::HashSet, sync::Arc, time::Duration};

    fn request_with_cut(obligations: Vec<RaiSlotId>) -> RaiVoteRequest {
        RaiVoteRequest {
            sequence: 1,
            epoch: 7,
            hash: BlockHash::from(2),
            root: Root::from(3),
            close_version: Some(RaiCloseVersionWire::Cut(rsnano_messages::RaiCloseCutWire {
                epoch: 7,
                obligations,
            })),
        }
    }

    fn processor_with_rai_state(
        active_elections: Arc<crate::consensus::AecService>,
        message_sender: crate::transport::MessageSender,
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
            #[cfg(feature = "ledger_snapshots")]
            LedgerSnapshots::new_null().into(),
            active_elections,
            message_sender,
        )
    }

    #[test]
    fn marked_slot_request_publishes_payload_without_replaying_cached_votes() {
        let key = PrivateKey::from(1);
        let committee = Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let active_elections = Arc::new(crate::consensus::AecService::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_millis(25),
            committee.clone(),
            BlockHash::from(7),
            Arc::new(Ledger::new_null()),
        ));
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let qualified_root = block.qualified_root();
        let now = Timestamp::new_test_instance();
        active_elections.published_block_available(block.clone().into());
        active_elections
            .insert(
                AecInsertRequest::new_priority(block, BlockPriority::default()),
                now,
            )
            .unwrap();

        // Retain real signed evidence for this slot. Before marked requests
        // became payload-only, the handler replayed this ConfirmAck alongside
        // the Publish response and amplified one payload request into a vote
        // storm.
        let metadata = RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::ZERO,
                root: qualified_root.clone(),
            }),
            phase: RaiVotePhase::First,
            epoch: RaiEpoch::ZERO,
            scope: RaiCommitteeScope::All,
        };
        let vote = Arc::new(Vote::new_rai(
            &key,
            UnixMillisTimestamp::new(1),
            0,
            hash,
            metadata,
        ));
        let vote = FilteredVote::new(
            ReceivedVote::new(vote, VoteDelivery::Direct, None),
            BlockHash::ZERO,
        );
        let quorum = QuorumSnapshot::new_test_instance();
        assert_eq!(
            active_elections.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: committee.as_ref(),
                quorum_snapshot: &quorum,
                now,
            })[&hash],
            Ok(())
        );
        assert_eq!(
            active_elections
                .rai_votes_for_root(&qualified_root.root, RaiEpoch::ZERO)
                .len(),
            1,
            "the response path must have cached evidence available"
        );

        let message_sender = crate::transport::MessageSender::new_null();
        let sent = message_sender.track();
        let processor = processor_with_rai_state(active_elections, message_sender);
        processor.process(
            Message::RaiVoteRequest(RaiVoteRequest {
                sequence: rsnano_messages::RAI_SLOT_REPAIR_SEQUENCE_FLAG | 1,
                epoch: RaiEpoch::ZERO.number(),
                hash: BlockHash::ZERO,
                root: qualified_root.root,
                close_version: None,
            }),
            &Channel::new_test_instance().into(),
        );
        processor.process(
            Message::RaiVoteRequest(RaiVoteRequest {
                sequence: rsnano_messages::RAI_SLOT_REPAIR_SEQUENCE_FLAG | 2,
                epoch: RaiEpoch::ZERO.number(),
                hash,
                root: Root::ZERO,
                close_version: None,
            }),
            &Channel::new_test_instance().into(),
        );

        let responses = sent.output();
        assert_eq!(
            responses
                .iter()
                .filter(|response| {
                    matches!(&response.message, Message::Publish(publish) if publish.block.hash() == hash)
                        && response.traffic_type
                            == rsnano_network::TrafficType::ConfirmationRequests
                })
                .count(),
            2,
        );
        assert!(
            !responses
                .iter()
                .any(|response| matches!(&response.message, Message::ConfirmAck(_)))
        );
    }

    #[test]
    fn zero_hash_close_request_has_no_epoch_wide_reply() {
        let message_sender = crate::transport::MessageSender::new_null();
        let sent = message_sender.track();
        let processor = processor_with_rai_state(
            Arc::new(crate::consensus::AecService::new_null()),
            message_sender,
        );
        let epoch = RaiEpoch::new(7);
        processor.process(
            Message::RaiVoteRequest(RaiVoteRequest {
                sequence: rsnano_messages::RAI_CLOSE_REPAIR_SEQUENCE_FLAG | 1,
                epoch: epoch.number(),
                hash: BlockHash::ZERO,
                root: crate::consensus::rai::rai_close_cut_root(epoch, 0).root,
                close_version: None,
            }),
            &Channel::new_test_instance().into(),
        );

        assert!(sent.output().is_empty());
    }

    #[test]
    fn exact_close_request_returns_only_matching_preimage_and_no_votes() {
        use crate::consensus::rai::{RaiCloseCut, RaiReport};

        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
        ];
        let committee = Arc::new(RepWeights::from(
            keys.each_ref()
                .map(|key| (key.public_key(), Amount::raw(1))),
        ));
        let ledger = Arc::new(Ledger::new_null());
        let active_elections = Arc::new(crate::consensus::AecService::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_millis(25),
            committee.clone(),
            BlockHash::from(7),
            ledger,
        ));
        let epoch = RaiEpoch::ZERO;
        let now = Timestamp::new_test_instance();
        active_elections.rai_tick(now, &keys[0], Duration::ZERO);
        for key in &keys[1..] {
            active_elections.rai_report_received(RaiReport::new(key, epoch, []));
        }
        active_elections.rai_tick(now + Duration::from_millis(1), &keys[0], Duration::ZERO);

        let root = crate::consensus::rai::rai_close_cut_root(epoch, 0);
        let requested_cut = active_elections
            .rai_close_cut_versions(epoch)
            .into_iter()
            .next()
            .expect("full report coverage starts the cut election");
        let requested_hash = requested_cut.hash();
        let unrelated_cut = RaiCloseCut::new(
            epoch,
            [RaiSlotId {
                epoch,
                root: QualifiedRoot::new_test_instance(),
            }],
        );
        let unrelated_hash = unrelated_cut.hash();
        assert_ne!(unrelated_hash, requested_hash);
        assert!(active_elections.reconcile_rai_close_cut(unrelated_cut, root.root));

        // Keep real signed evidence in the cache. The former close-repair
        // path would replay it as a ConfirmAck alongside every cut version.
        let metadata = RaiVoteMetadata {
            election_id: RaiElectionId::CloseCut { epoch, round: 0 },
            phase: RaiVotePhase::First,
            epoch,
            scope: RaiCommitteeScope::All,
        };
        let vote = Arc::new(Vote::new_rai(
            &keys[0],
            UnixMillisTimestamp::new(1),
            0,
            requested_hash,
            metadata,
        ));
        let vote = FilteredVote::new(
            ReceivedVote::new(vote, VoteDelivery::Direct, None),
            BlockHash::ZERO,
        );
        assert_eq!(
            active_elections.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: committee.as_ref(),
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            })[&requested_hash],
            Ok(())
        );

        let message_sender = crate::transport::MessageSender::new_null();
        let sent = message_sender.track();
        let processor = processor_with_rai_state(active_elections, message_sender);
        processor.process(
            Message::RaiVoteRequest(RaiVoteRequest {
                sequence: rsnano_messages::RAI_CLOSE_REPAIR_SEQUENCE_FLAG | 1,
                epoch: epoch.number(),
                hash: requested_hash,
                root: root.root,
                close_version: None,
            }),
            &Channel::new_test_instance().into(),
        );

        let responses = sent.output();
        assert_eq!(responses.len(), 1);
        assert!(
            !responses
                .iter()
                .any(|response| matches!(&response.message, Message::ConfirmAck(_)))
        );
        let Message::RaiVoteRequest(reply) = &responses[0].message else {
            panic!("exact close repair must return only its preimage envelope");
        };
        assert_eq!(reply.hash, requested_hash);
        assert_ne!(reply.hash, unrelated_hash);
        assert!(matches!(
            &reply.close_version,
            Some(RaiCloseVersionWire::Cut(cut))
                if cut.obligations == requested_cut.obligations.iter().cloned().collect::<Vec<_>>()
        ));
    }

    #[test]
    fn reassembles_out_of_order_cut_chunks_and_ignores_retries() {
        let expected = (0..=rsnano_messages::MAX_RAI_CLOSE_CUT_CHUNK_ENTRIES)
            .map(|i| RaiSlotId {
                epoch: RaiEpoch::new(7),
                root: QualifiedRoot::new(Root::from(i as u64 + 1), BlockHash::from(4)),
            })
            .collect::<Vec<_>>();
        let request = request_with_cut(expected.clone());
        let mut chunks = request.into_chunks();
        assert_eq!(chunks.len(), 2);
        chunks.reverse();

        let mut assembler = RaiCloseRepairAssembler::default();
        let channel_id = rsnano_network::ChannelId::from(11);
        let last = chunks.remove(0);
        let RaiCloseVersionWire::CutChunk(last_chunk) = last.close_version.unwrap() else {
            panic!("expected a cut chunk");
        };
        assert!(
            assembler
                .push_cut(channel_id, last.hash, last.root, last_chunk.clone())
                .is_none()
        );
        // A retry replaces the same indexed fragment without increasing the
        // completion count.
        assert!(
            assembler
                .push_cut(channel_id, last.hash, last.root, last_chunk)
                .is_none()
        );
        let first = chunks.remove(0);
        let RaiCloseVersionWire::CutChunk(first_chunk) = first.close_version.unwrap() else {
            panic!("expected a cut chunk");
        };
        let completed = assembler.push_cut(channel_id, first.hash, first.root, first_chunk);

        assert_eq!(completed.unwrap().obligations, expected);
    }

    #[test]
    fn does_not_mix_chunks_from_different_peers() {
        let expected = (0..=rsnano_messages::MAX_RAI_CLOSE_CUT_CHUNK_ENTRIES)
            .map(|i| RaiSlotId {
                epoch: RaiEpoch::new(7),
                root: QualifiedRoot::new(Root::from(i as u64 + 1), BlockHash::from(4)),
            })
            .collect::<Vec<_>>();
        let chunks = request_with_cut(expected.clone()).into_chunks();
        let first = chunks[0].clone();
        let second = chunks[1].clone();
        let mut assembler = RaiCloseRepairAssembler::default();

        let RaiCloseVersionWire::CutChunk(first_chunk) = first.close_version.unwrap() else {
            panic!("expected a cut chunk");
        };
        assert!(
            assembler
                .push_cut(
                    rsnano_network::ChannelId::from(11),
                    first.hash,
                    first.root,
                    first_chunk.clone(),
                )
                .is_none()
        );
        let RaiCloseVersionWire::CutChunk(second_chunk) = second.close_version.unwrap() else {
            panic!("expected a cut chunk");
        };
        assert!(
            assembler
                .push_cut(
                    rsnano_network::ChannelId::from(12),
                    second.hash,
                    second.root,
                    second_chunk.clone(),
                )
                .is_none()
        );
        let completed = assembler.push_cut(
            rsnano_network::ChannelId::from(11),
            second.hash,
            second.root,
            second_chunk,
        );

        assert_eq!(completed.unwrap().obligations, expected);
    }

    #[test]
    fn reassembles_record_chunks_with_previous_hash() {
        let expected = (0..=rsnano_messages::MAX_RAI_CLOSE_RECORD_CHUNK_ENTRIES)
            .map(|i| (Account::from(i as u64 + 1), i as u64, BlockHash::from(5)))
            .collect::<Vec<_>>();
        let previous = BlockHash::from(6);
        let chunks = RaiVoteRequest {
            sequence: 2,
            epoch: 8,
            hash: BlockHash::from(7),
            root: Root::from(8),
            close_version: Some(RaiCloseVersionWire::Record(
                rsnano_messages::RaiCloseRecordWire {
                    epoch: 8,
                    previous,
                    frontiers: expected.clone(),
                },
            )),
        }
        .into_chunks();
        let mut assembler = RaiCloseRepairAssembler::default();
        let mut completed = None;

        for request in chunks {
            let RaiCloseVersionWire::RecordChunk(chunk) = request.close_version.unwrap() else {
                panic!("expected a record chunk");
            };
            completed = assembler.push_record(
                rsnano_network::ChannelId::from(13),
                request.hash,
                request.root,
                chunk,
            );
        }

        let completed = completed.unwrap();
        assert_eq!(completed.previous, previous);
        assert_eq!(completed.frontiers, expected);
    }

    #[test]
    fn report_retry_windows_are_bounded_and_eventually_lead_with_every_chunk() {
        use crate::consensus::rai::{RaiReport, RaiReportStore};

        let report_count = MAX_RAI_REPORT_CHUNKS_PER_RESPONSE * 5 + 1;
        let epoch = RaiEpoch::new(5);
        let mut reports = RaiReportStore::default();
        for key in 1..=report_count as u64 {
            reports
                .insert(RaiReport::new(&PrivateKey::from(key), epoch, []))
                .unwrap();
        }
        let expected = reports
            .filtered_for_epoch(epoch, |_| true, usize::MAX)
            .into_iter()
            .map(|report| report.reporter)
            .collect::<HashSet<_>>();
        let mut first_seen = HashSet::new();

        for sequence in 0..report_count as u64 {
            let window =
                reports.response_window(epoch, sequence, MAX_RAI_REPORT_CHUNKS_PER_RESPONSE);
            assert_eq!(window.len(), MAX_RAI_REPORT_CHUNKS_PER_RESPONSE);
            first_seen.insert(window[0].reporter);
        }

        assert_eq!(first_seen, expected);
    }

    #[test]
    fn sequence_flag_separates_close_slot_and_legacy_repair() {
        let epoch = RaiEpoch::new(7);
        let mut request = RaiVoteRequest {
            sequence: rsnano_messages::RAI_CLOSE_REPAIR_SEQUENCE_FLAG | 1,
            epoch: 7,
            hash: BlockHash::ZERO,
            root: Root::from(123),
            close_version: None,
        };
        assert_eq!(
            classify_rai_vote_request(&request, epoch),
            Some(RaiVoteRequestKind::Close)
        );
        assert!(!RaiVoteRequestKind::Close.permits_cached_vote_replay());
        assert!(!RaiVoteRequestKind::Close.permits_vote_generation());

        // A marked slot request is payload repair only.
        request.sequence = rsnano_messages::RAI_SLOT_REPAIR_SEQUENCE_FLAG | 1;
        assert_eq!(
            classify_rai_vote_request(&request, epoch),
            Some(RaiVoteRequestKind::MarkedSlot)
        );
        assert!(!RaiVoteRequestKind::MarkedSlot.permits_cached_vote_replay());
        assert!(!RaiVoteRequestKind::MarkedSlot.permits_vote_generation());

        // A nonzero marked slot value requests an exact transitive dependency
        // without replaying or generating vote evidence.
        request.hash = BlockHash::from(1);
        assert_eq!(
            classify_rai_vote_request(&request, epoch),
            Some(RaiVoteRequestKind::MarkedSlot)
        );

        // Current close requests name the exact nonzero missing digest, and
        // replies use the same envelope plus close_version.
        request.sequence = rsnano_messages::RAI_CLOSE_REPAIR_SEQUENCE_FLAG | 1;
        assert_eq!(
            classify_rai_vote_request(&request, epoch),
            Some(RaiVoteRequestKind::Close)
        );

        // An unmarked nonzero envelope retains legacy compatibility.
        request.sequence = 1;
        assert_eq!(
            classify_rai_vote_request(&request, epoch),
            Some(RaiVoteRequestKind::Legacy)
        );

        // Older senders remain interoperable through the synthetic-root
        // fallback even without the marker.
        request.sequence = 1;
        request.hash = BlockHash::ZERO;
        request.root = crate::consensus::rai::rai_close_cut_root(epoch, 3).root;
        assert_eq!(
            classify_rai_vote_request(&request, epoch),
            Some(RaiVoteRequestKind::Close)
        );
    }

    #[test]
    fn retry_windows_eventually_cover_every_chunk() {
        let chunk_count = MAX_RAI_CLOSE_REPAIR_CHUNKS_PER_RESPONSE * 4 + 3;
        let mut seen = HashSet::new();
        for sequence in 0..chunk_count as u64 {
            seen.extend(rai_close_repair_response_window(
                (0..chunk_count).collect::<Vec<_>>(),
                sequence,
            ));
        }

        assert_eq!(seen.len(), chunk_count);
    }

    #[test]
    fn exact_close_preimage_response_has_one_global_chunk_bound() {
        let sequence = rsnano_messages::RAI_CLOSE_REPAIR_SEQUENCE_FLAG | 3;
        let epoch = 9;
        let hash = BlockHash::from(91);
        let root = crate::consensus::rai::rai_close_record_root(epoch.into(), 2).root;
        let entry_count = rsnano_messages::MAX_RAI_CLOSE_RECORD_CHUNK_ENTRIES
            * (MAX_RAI_CLOSE_REPAIR_CHUNKS_PER_RESPONSE + 4)
            + 1;
        let frontiers = (0..entry_count)
            .map(|i| {
                (
                    Account::from(i as u64 + 1),
                    i as u64,
                    BlockHash::from(i as u64 + 2),
                )
            })
            .collect();

        let replies = rai_close_preimage_response(
            sequence,
            epoch,
            hash,
            root,
            Some(RaiCloseVersionWire::Record(
                rsnano_messages::RaiCloseRecordWire {
                    epoch,
                    previous: BlockHash::from(90),
                    frontiers,
                },
            )),
        );

        assert_eq!(replies.len(), MAX_RAI_CLOSE_REPAIR_CHUNKS_PER_RESPONSE);
        assert!(replies.iter().all(|reply| {
            reply.hash == hash
                && reply.root == root
                && matches!(
                    reply.close_version,
                    Some(RaiCloseVersionWire::RecordChunk(_))
                )
        }));
    }

    #[test]
    fn retry_windows_cover_every_chunk_when_only_first_send_survives() {
        let chunk_count = MAX_RAI_CLOSE_REPAIR_CHUNKS_PER_RESPONSE * 4;
        let mut seen = HashSet::new();
        for sequence in 0..chunk_count as u64 {
            let first_queued =
                rai_close_repair_response_window((0..chunk_count).collect::<Vec<_>>(), sequence)
                    .into_iter()
                    .next()
                    .unwrap();
            seen.insert(first_queued);
        }

        assert_eq!(seen.len(), chunk_count);
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
