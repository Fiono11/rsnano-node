use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_ledger::{AnySet, BlockSource, LedgerSet};
use rsnano_messages::{
    ConfirmAck, EvidenceReq, LedgerSketchReply, LedgerSketchReq, Message, Publish, ReconReply,
    ReconReq, Report,
};
use rsnano_network::{Channel, ChannelId, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{ConsensusEpoch, PublicKey};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use super::{CertificateSource, ReconRefusal, ReconcileResult, ReportExchange, ReportMessage};
use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
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
    /// When the signed votes behind unjustified entries were last asked
    /// for, per epoch
    evidence_requested: Mutex<HashMap<ConsensusEpoch, Timestamp>>,
    /// When each epoch's own G data was last re-gossiped, and the epochs
    /// whose G blocks and ancestry are retained already
    votes_repeated_at: Mutex<HashMap<ConsensusEpoch, Timestamp>>,
    retained_epochs: Mutex<std::collections::HashSet<ConsensusEpoch>>,
    /// When each epoch's live projection was last refreshed
    projected: Mutex<HashMap<ConsensusEpoch, Timestamp>>,
    /// Report-protocol requests and replies, handled on the report thread:
    /// on the network thread their state differences and sketches delayed
    /// the votes and blocks queued behind them
    inbound: Mutex<std::collections::VecDeque<(Message, Arc<Channel>)>>,
    /// The vote count a reporter's G was last derived from, and when
    derived_at: Mutex<HashMap<(ConsensusEpoch, PublicKey), (usize, Timestamp)>>,
    /// Diagnostic: the blocks others first-voted and this node did not at
    /// the last look, and when that was
    unvoted: Mutex<(
        Option<Timestamp>,
        std::collections::HashSet<rsnano_types::BlockHash>,
    )>,
    /// Final certificates assembled while checking reports, by epoch and
    /// block: they stay valid, and the reports of an epoch share them
    final_certificates: Mutex<
        HashMap<
            (ConsensusEpoch, rsnano_types::BlockHash),
            crate::consensus::election::CertificateKinds,
        >,
    >,
    /// Evidence blocks waiting to be checked and retained
    evidence_blocks: Mutex<std::collections::VecDeque<rsnano_types::Block>>,
    data: super::residual_data::ResidualData,
    ledger: Arc<rsnano_ledger::Ledger>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    /// When the retained branches were last checked against the ledger
    retained_checked: Mutex<Option<Timestamp>>,
}

#[allow(dead_code)] // reconciled_count is read by the close, Section 9.1
impl ReportService {
    const LOG_INTERVAL: Duration = Duration::from_secs(1);
    const UNDECIDED_REPEAT_INTERVAL: Duration = Duration::from_secs(3);
    const DECIDED_REPEAT_INTERVAL: Duration = Duration::from_secs(15);
    const PROJECTION_INTERVAL: Duration = Duration::from_secs(1);
    /// Hashes per AEC read-lock acquisition when assembling certificates
    const CERTIFICATE_CHUNK: usize = 2048;
    /// Report-protocol messages waiting for the report thread
    const MAX_INBOUND: usize = 16_384;
    /// A G derivation with no new vote of its reporter is retried this often:
    /// a G block fetched since the last attempt places a member
    const RESIDUAL_IDLE_RETRY: Duration = Duration::from_secs(1);

    /// Queue a report-protocol request or reply for the report thread. The
    /// oldest is dropped when the queue is full; a peer repeats its requests.
    pub fn enqueue(&self, message: Message, channel: &Arc<Channel>) {
        let mut inbound = self.inbound.lock().unwrap();
        if inbound.len() >= Self::MAX_INBOUND {
            inbound.pop_front();
        }
        inbound.push_back((message, channel.clone()));
    }

    fn process_inbound(&self) {
        let blocks: Vec<rsnano_types::Block> =
            self.evidence_blocks.lock().unwrap().drain(..).collect();
        for block in blocks {
            // Re-gossiped copies of blocks retained already cost a map lookup
            if !self.data.has_received(&block.hash()) && !self.data.ledger_holds(&block.hash()) {
                self.data.receive(&block);
            }
        }
        let queued: Vec<(Message, Arc<Channel>)> = self.inbound.lock().unwrap().drain(..).collect();
        for (message, channel) in queued {
            match message {
                Message::ReconReq(request) => self.handle_request(request, &channel),
                Message::ReconReply(reply) => self.handle_reply(reply, &channel),
                Message::EvidenceReq(request) => self.handle_evidence_request(request, &channel),
                Message::LedgerSketchReq(request) => self.handle_ledger_sketch(request, &channel),
                Message::LedgerSketchReply(reply) => {
                    self.handle_ledger_sketch_reply(reply, &channel)
                }
                _ => {}
            }
        }
    }

    pub(crate) fn new(
        active_elections: Arc<AecService>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        sender: MessageSender,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
        ledger: Arc<rsnano_ledger::Ledger>,
        forks: Arc<std::sync::RwLock<crate::consensus::ForkCache>>,
        block_processor_queue: Arc<BlockProcessorQueue>,
    ) -> Self {
        Self {
            exchange: Arc::new(Mutex::new(ReportExchange::new())),
            data: super::residual_data::ResidualData::new(
                ledger.clone(),
                forks,
                active_elections.clone(),
            ),
            ledger,
            block_processor_queue,
            retained_checked: Mutex::new(None),
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
            evidence_requested: Mutex::new(HashMap::new()),
            votes_repeated_at: Mutex::new(HashMap::new()),
            retained_epochs: Mutex::new(std::collections::HashSet::new()),
            projected: Mutex::new(HashMap::new()),
            inbound: Mutex::new(std::collections::VecDeque::new()),
            derived_at: Mutex::new(HashMap::new()),
            evidence_blocks: Mutex::new(std::collections::VecDeque::new()),
            unvoted: Mutex::new((None, std::collections::HashSet::new())),
            final_certificates: Mutex::new(HashMap::new()),
        }
    }

    /// An evidence block arrived: retained on the report thread (see
    /// `process_inbound`), where its owner signature is checked, unless the
    /// ledger already holds it
    pub(crate) fn retain_received_block(&self, block: &rsnano_types::Block) {
        let mut queued = self.evidence_blocks.lock().unwrap();
        if queued.len() >= Self::MAX_INBOUND {
            queued.pop_front();
        }
        queued.push_back(block.clone());
    }

    /// RAI: whether a block arriving as evidence is one the latest decided
    /// checkpoint retains and this ledger lacks: it then replaces the omitted
    /// rival the ledger holds at its position (see
    /// `AecFactProcessor::follow_retained_branches`)
    pub(crate) fn follows_retained_branch(&self, block: &rsnano_types::Block) -> bool {
        let hash = block.hash();
        (self.active_elections.awaits_checkpoint_block(&hash)
            || self
                .active_elections
                .latest_checkpoint()
                .is_some_and(|state| state.retains_block(&hash)))
            && !self.data.ledger_holds(&hash)
    }

    /// RAI: whether a published block is a fork candidate this node already
    /// holds in its fork cache. Processing it again only checks its
    /// signature and finds the same fork; an election that starts later
    /// takes it from the cache, and the paths that make the ledger follow a
    /// checkpoint read it from there too.
    pub(crate) fn known_fork(&self, block: &rsnano_types::Block) -> bool {
        self.data.cached_fork(block)
    }

    /// RAI: whether an evidence block goes to the block processor as well
    /// as to the report data. Reporters re-gossip their G blocks and the
    /// ancestry of them every few seconds while an epoch closes; processing
    /// all of them again checked every signature on the live block path and
    /// queued live blocks behind them. Only a block the ledger lacks where
    /// it can take it is processed: a retained or checkpoint-finalized one,
    /// one at a position the ledger holds nothing at, or a candidate of an
    /// election active here.
    pub(crate) fn evidence_for_ledger(&self, block: &rsnano_types::Block) -> bool {
        let hash = block.hash();
        let any = self.ledger.any();
        if any.block_exists(&hash) {
            return false;
        }
        if self
            .active_elections
            .is_active_root(&block.qualified_root())
        {
            return true;
        }
        let previous = block.previous();
        if previous.is_zero() {
            any.get_account(&block.account_field().unwrap_or_default())
                .is_none()
        } else {
            any.block_exists(&previous) && any.block_successor(&previous).is_none()
        }
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
                    "usable": r.is_usable(), "evidence": format!("{:?}", r.evidence),
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
            Arc::new(BlockProcessorQueue::new_null()),
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
        // Own G placed from block data, as the validators deriving it do
        let own_residual = report
            .residual
            .clone()
            .replaced(|hash| self.data.placement(hash));
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
                own_residual,
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
        // A report binds the committee that issued the epoch's votes; one
        // bound to another is not a report of this epoch
        let committee = self
            .active_elections
            .epoch_committee(report.epoch)
            .map(|committee| committee.digest());
        let mut exchange = self.exchange.lock().unwrap();
        if let Some(digest) = committee {
            exchange.set_committee(report.epoch, digest);
            if digest != report.committee {
                if self.log_due(report.epoch, true) {
                    crate::utils::diagnostic!(
                        "EPOCH_REPORT_REFUSED epoch={} reporter={} reason=committee",
                        report.epoch,
                        report.reporter
                    );
                }
                return;
            }
        }
        exchange.handle_report(report);
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
                .is_some_and(|last| last.elapsed(now) < Self::PROJECTION_INTERVAL)
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
    fn derive_residual(
        &self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
        missing: &mut Vec<rsnano_types::BlockHash>,
    ) {
        // G members whose blocks are not placed here: the block, or the
        // first ancestor this node lacks, is fetched with the evidence
        let progress = self
            .exchange
            .lock()
            .unwrap()
            .residual_progress(epoch, &reporter);
        if let Some((_, unplaced)) = progress {
            missing.extend(
                unplaced
                    .iter()
                    .filter_map(|hash| self.data.missing_ancestor(hash)),
            );
        }
        // A derived G that does not hash to the signed root lacks some of
        // the reporter's votes: the signed root asks the reporter for them
        // (see `handle_evidence_request`)
        let unmatched = self
            .exchange
            .lock()
            .unwrap()
            .unmatched_residual_root(epoch, &reporter);
        missing.extend(unmatched);
        let now = self.clock.now();
        if !self
            .exchange
            .lock()
            .unwrap()
            .needs_residual(epoch, &reporter, now)
        {
            return;
        }
        // Deriving copies every vote of the reporter: only when a new one
        // arrived since the last attempt, or rarely otherwise
        let count = self.active_elections.vote_record_count(epoch, &reporter);
        {
            let mut derived = self.derived_at.lock().unwrap();
            if derived.get(&(epoch, reporter)).is_some_and(|(last, at)| {
                *last == count && at.elapsed(now) < Self::RESIDUAL_IDLE_RETRY
            }) {
                return;
            }
            derived.insert((epoch, reporter), (count, now));
        }
        // RAI, "Reconstruction and report usability": Ĝ is the set of
        // hashes the reporter signed an epoch vote for, minus keys(T). Only
        // those outside T are placed, from owner-signed block data; the rest
        // of the reporter's votes are not copied.
        let signed = self.active_elections.signed_keys_of(epoch, &reporter);
        let held = signed.len();
        let outside = self
            .exchange
            .lock()
            .unwrap()
            .outside_certified(epoch, &reporter, signed);
        let mut votes = Vec::with_capacity(outside.len());
        let mut unplaced = Vec::new();
        let mut placements: HashMap<rsnano_types::BlockHash, Option<_>> = HashMap::new();
        for (hash, kind) in outside {
            let placed = *placements
                .entry(hash)
                .or_insert_with(|| self.data.placement(&hash));
            match placed {
                Some((block, previous)) => votes.push((block, kind, previous)),
                None => unplaced.push(hash),
            }
        }
        unplaced.sort();
        unplaced.dedup();
        let unplaced_count = unplaced.len();
        let placed_count = votes.len();
        let result = {
            let mut exchange = self.exchange.lock().unwrap();
            let result =
                exchange.derive_residual_with_unplaced(epoch, reporter, votes, unplaced, now);
            result.map(|result| (result, exchange.residual_progress(epoch, &reporter)))
        };
        if let Some((result, progress)) = result {
            let (root_matches, g_unplaced) = progress
                .map(|(matches, unplaced)| (matches, unplaced.len()))
                .unwrap_or((result.complete, 0));
            crate::utils::diagnostic!(
                "EPOCH_RESIDUAL epoch={} reporter={} votes={} derived={} complete={} root_matches={} g_unplaced={} unplaced={} placed={}",
                epoch,
                reporter,
                held,
                result.total,
                result.complete,
                root_matches,
                g_unplaced,
                unplaced_count,
                placed_count
            );
        }
        // Retry from validated reporter votes. A hash-only G commitment
        // cannot authenticate vote-kind metadata supplied by a sketch.
    }

    /// RAI, "Reconstructing a report": "an entry tagged N must have a valid
    /// NC, an entry tagged F must have an explicit valid finality proof".
    /// Check the memberships of a reconstructed report against the
    /// certificates assembled here from retained signed votes, and collect
    /// the hashes whose votes are missing so that they can be asked for.
    fn verify_evidence(
        &self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
        missing: &mut Vec<rsnano_types::BlockHash>,
    ) {
        let now = self.clock.now();
        let Some(hashes) = self
            .exchange
            .lock()
            .unwrap()
            .evidence_to_check(epoch, &reporter, now)
        else {
            return;
        };
        // The certificates are assembled outside the exchange lock: an epoch
        // and the one before it, for finality exposed late
        let mut kinds: HashMap<
            (ConsensusEpoch, rsnano_types::BlockHash),
            crate::consensus::election::CertificateKinds,
        > = HashMap::new();
        // Every retained epoch: a certificate exposed late may be older
        // than the epoch before the closing one
        let epochs: Vec<ConsensusEpoch> = (0..4)
            .filter_map(|back| epoch.as_u64().checked_sub(back).map(ConsensusEpoch::new))
            .collect();
        for vote_epoch in epochs {
            // A final certificate assembled from retained votes stays valid:
            // the reports of one epoch share most of their entries, so what
            // one reporter's check found final is not assembled again
            let wanted: Vec<rsnano_types::BlockHash> = {
                let cache = self.final_certificates.lock().unwrap();
                hashes
                    .iter()
                    .filter(|hash| match cache.get(&(vote_epoch, **hash)) {
                        Some(kind) => {
                            kinds.insert((vote_epoch, **hash), *kind);
                            false
                        }
                        None => true,
                    })
                    .copied()
                    .collect()
            };
            // In chunks: each call holds the AEC read lock, which vote
            // processing needs for writing
            for chunk in wanted.chunks(Self::CERTIFICATE_CHUNK) {
                let found = self.active_elections.certificate_kinds(vote_epoch, chunk);
                let mut cache = self.final_certificates.lock().unwrap();
                for (hash, kind) in found {
                    if kind.fc || kind.ff {
                        cache.insert((vote_epoch, hash), kind);
                    }
                    kinds.insert((vote_epoch, hash), kind);
                }
            }
        }
        // Kept for the epochs still checked
        if let Some(oldest) = epoch.as_u64().checked_sub(4) {
            self.final_certificates
                .lock()
                .unwrap()
                .retain(|(held, _), _| held.as_u64() > oldest);
        }
        let checked = hashes.len();
        let result = self.exchange.lock().unwrap().verify_evidence(
            epoch,
            &reporter,
            &kinds as &dyn CertificateSource,
            now,
        );
        let Some(unjustified) = result else {
            return;
        };
        if checked > 0 && self.log_due(epoch, false) {
            let sample: Vec<String> = unjustified
                .iter()
                .take(3)
                .map(|hash| {
                    let status = self
                        .exchange
                        .lock()
                        .unwrap()
                        .reported_status(epoch, &reporter, hash)
                        .map(|(block, status)| {
                            format!("{:?}@{}:{}", status, block.account, block.height)
                        })
                        .unwrap_or_default();
                    // How many retained signed batches this node holds for
                    // it, per retained epoch: whether the votes ever arrived
                    let held: Vec<usize> = (0..4)
                        .filter_map(|back| epoch.as_u64().checked_sub(back))
                        .map(|vote_epoch| {
                            self.active_elections
                                .signed_votes_for_hashes(
                                    ConsensusEpoch::new(vote_epoch),
                                    std::slice::from_ref(hash),
                                )
                                .len()
                        })
                        .collect();
                    format!("{}={} votes={:?}", &hash.to_string()[..8], status, held)
                })
                .collect();
            crate::utils::diagnostic!(
                "EPOCH_EVIDENCE epoch={} reporter={} checked={} missing={} sample={:?}",
                epoch,
                reporter,
                checked,
                unjustified.len(),
                sample
            );
        }
        missing.extend(unjustified);
    }

    /// RAI: ask every replica for the retained signed votes behind the
    /// entries this node can not justify, at most once a second per epoch
    /// and a bounded number of hashes at a time
    fn request_evidence(&self, epoch: ConsensusEpoch, missing: Vec<rsnano_types::BlockHash>) {
        if missing.is_empty() {
            return;
        }
        let now = self.clock.now();
        {
            let mut requested = self.evidence_requested.lock().unwrap();
            if requested
                .get(&epoch)
                .is_some_and(|last| last.elapsed(now) < ReportExchange::REPEAT_INTERVAL)
            {
                return;
            }
            requested.insert(epoch, now);
        }
        self.send_evidence_request(epoch, missing);
    }

    /// Asks every replica for the signed votes and the blocks behind these
    /// hashes, a bounded number at a time
    fn send_evidence_request(
        &self,
        epoch: ConsensusEpoch,
        mut missing: Vec<rsnano_types::BlockHash>,
    ) {
        if missing.is_empty() {
            return;
        }
        missing.sort();
        missing.dedup();
        missing.truncate(EvidenceReq::MAX_HASHES);
        self.stats
            .inc_dir(StatType::Message, DetailType::EvidenceReq, Direction::Out);
        self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
            &Message::EvidenceReq(EvidenceReq {
                epoch,
                hashes: missing,
            }),
            TrafficType::Generic,
            1.0,
        );
    }

    /// RAI: relay the original signed vote batches this node retains for the
    /// hashes asked for, in the epoch named and the one before it, as
    /// certificate evidence. Only signed votes are relayed; the requester
    /// assembles its certificates and verifies every signature itself.
    pub fn handle_evidence_request(&self, request: EvidenceReq, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::EvidenceReq, Direction::In);
        let mut epochs = vec![request.epoch];
        if let Some(before) = request.epoch.as_u64().checked_sub(1) {
            epochs.push(ConsensusEpoch::new(before));
        }
        // The blocks asked for, when held: a G member a requester can not
        // place, or an ancestor it lacks (RAI, "It then fetches blocks,
        // ancestry, admission witnesses, and dependencies")
        let mut blocks = 0;
        {
            let mut sender = self.sender.lock().unwrap();
            for hash in &request.hashes {
                if let Some(block) = self.data.evidence_block(hash) {
                    sender.try_send(
                        channel,
                        &Message::Publish(Publish::new_evidence(block)),
                        TrafficType::BlockBroadcastInitial,
                    );
                    blocks += 1;
                }
            }
        }
        // A G root of this node's own report: the validator could not derive
        // that G and gets every retained signed vote of the reporter for it
        let own: Vec<(PublicKey, Vec<rsnano_types::BlockHash>)> = {
            let exchange = self.exchange.lock().unwrap();
            request
                .hashes
                .iter()
                .filter_map(|hash| exchange.own_residual(request.epoch, hash))
                .collect()
        };
        let mut residual_batches = 0;
        for (reporter, hashes) in own {
            let votes = self
                .active_elections
                .signed_votes_for(request.epoch, &reporter, &hashes);
            let mut sender = self.sender.lock().unwrap();
            for vote in votes {
                sender.try_send(
                    channel,
                    &Message::ConfirmAck(ConfirmAck::new_with_certificate_evidence(
                        (*vote).clone(),
                    )),
                    TrafficType::Vote,
                );
                residual_batches += 1;
            }
        }
        let mut sent = 0;
        for epoch in epochs {
            let votes = self
                .active_elections
                .signed_votes_for_hashes(epoch, &request.hashes);
            let mut sender = self.sender.lock().unwrap();
            for vote in votes {
                sender.try_send(
                    channel,
                    &Message::ConfirmAck(ConfirmAck::new_with_certificate_evidence(
                        (*vote).clone(),
                    )),
                    TrafficType::Vote,
                );
                sent += 1;
            }
        }
        if self.log_due(request.epoch, true) {
            crate::utils::diagnostic!(
                "EPOCH_EVIDENCE_SERVED epoch={} hashes={} batches={} blocks={} own_residual_batches={}",
                request.epoch,
                request.hashes.len(),
                sent,
                blocks,
                residual_batches
            );
        }
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
        // Every few seconds while the epoch is undecided here; rarely once it
        // is, for replicas still catching up. Flooding every G block and vote
        // each second competed with the open epoch's traffic.
        let due = |epoch: &ConsensusEpoch| {
            let interval = if self.active_elections.epoch_decided_state(*epoch).is_some() {
                Self::DECIDED_REPEAT_INTERVAL
            } else {
                Self::UNDECIDED_REPEAT_INTERVAL
            };
            let mut at = self.votes_repeated_at.lock().unwrap();
            if at
                .get(epoch)
                .is_some_and(|last| last.elapsed(now) < interval)
            {
                return false;
            }
            at.insert(*epoch, now);
            true
        };
        let epochs: Vec<ConsensusEpoch> = {
            let exchange = self.exchange.lock().unwrap();
            exchange.epochs.keys().copied().collect()
        };
        let due_epochs: Vec<ConsensusEpoch> = epochs.into_iter().filter(|e| due(e)).collect();
        if due_epochs.is_empty() {
            return;
        }
        let requests: Vec<_> = {
            let exchange = self.exchange.lock().unwrap();
            exchange
                .epochs
                .iter()
                .filter(|(epoch, _)| due_epochs.contains(epoch))
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
            // The G blocks and their ancestry are copied and gossiped once
            // per epoch; a validator that still lacks one asks for it (see
            // `handle_evidence_request`). The votes are repeated.
            let first = self.retained_epochs.lock().unwrap().insert(epoch);
            if first {
                self.data.retain(epoch, hashes.iter().copied());
            }
            let block_count = self.data.retained(epoch, &hashes).len();
            let blocks = if first {
                self.data.with_ancestry(epoch, &hashes)
            } else {
                Vec::new()
            };
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
    /// Returns the time spent per part, for the slow-tick diagnostic
    /// Diagnostic, every 2 s: the current epoch's blocks others first-voted
    /// and this node did not, by reason, counting only those already
    /// unvoted at the previous look
    fn log_unvoted(&self) {
        const INTERVAL: Duration = Duration::from_secs(2);
        let now = self.clock.now();
        let mut unvoted = self.unvoted.lock().unwrap();
        if unvoted.0.is_some_and(|last| last.elapsed(now) < INTERVAL) {
            return;
        }
        unvoted.0 = Some(now);
        let voters: Vec<PublicKey> = {
            let mut keys = Vec::new();
            self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
            keys.iter().map(|key| key.public_key()).collect()
        };
        if voters.is_empty() {
            return;
        }
        let epoch = self.active_elections.current_epoch();
        let checkpoint = self.active_elections.latest_checkpoint();
        let current = self.active_elections.first_voted_elsewhere(epoch, &voters);
        let mut reasons: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut next = std::collections::HashSet::new();
        for (hash, active) in current {
            next.insert(hash);
            if unvoted.1.contains(&hash) {
                let reason = self
                    .data
                    .unvoted_reason(&hash, active, checkpoint.as_deref());
                *reasons.entry(reason).or_default() += 1;
            }
        }
        unvoted.1 = next;
        if !reasons.is_empty() {
            crate::utils::diagnostic!("EPOCH_UNVOTED epoch={} persistent={:?}", epoch, reasons);
        }
    }

    /// RAI: the ledger follows the branches the latest checkpoint retains.
    /// A retained block the ledger lost after the checkpoint was installed,
    /// or whose forced insert did not take, leaves the owner's extension of
    /// the lock unattachable here for good. Every 2 s: force it in again,
    /// unless the ledger holds another block the checkpoint retains at that
    /// position or the rival it holds there is final; fetch it, or the
    /// ancestor it lacks, when it is not held at all.
    fn recheck_retained(&self) -> Vec<rsnano_types::BlockHash> {
        const INTERVAL: Duration = Duration::from_secs(2);
        let now = self.clock.now();
        {
            let mut checked = self.retained_checked.lock().unwrap();
            if checked.is_some_and(|last| last.elapsed(now) < INTERVAL) {
                return Vec::new();
            }
            *checked = Some(now);
        }
        let any = self.ledger.any();
        let mut missing = Vec::new();
        // Checkpoint-finalized blocks never received here: fetched, and
        // forced in on arrival (see `follows_retained_branch`)
        let mut awaited_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for hash in self.active_elections.awaited_checkpoint_blocks() {
            if any.block_exists(&hash) {
                continue;
            }
            match self.data.evidence_block(&hash) {
                Some(block)
                    if block.previous().is_zero() || any.block_exists(&block.previous()) =>
                {
                    *awaited_counts.entry("forced").or_default() += 1;
                    self.block_processor_queue.push(BlockContext::new(
                        block,
                        BlockSource::Forced,
                        ChannelId::LOOPBACK,
                    ));
                }
                _ => {
                    *awaited_counts.entry("fetching").or_default() += 1;
                    missing.extend(self.data.missing_ancestor(&hash));
                }
            }
        }
        if !awaited_counts.is_empty() {
            crate::utils::diagnostic!("EPOCH_AWAITED_FINALIZED {:?}", awaited_counts);
        }
        let retained = self.active_elections.retained_to_follow();
        if retained.is_empty() {
            return missing;
        }
        let mut at_position: HashMap<(rsnano_types::Account, u64), Vec<rsnano_types::BlockHash>> =
            HashMap::new();
        for (account, height, hash) in &retained {
            at_position
                .entry((*account, *height))
                .or_default()
                .push(*hash);
        }
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for (account, height, hash) in &retained {
            if any.block_exists(hash) {
                continue;
            }
            let sibling_held = at_position[&(*account, *height)]
                .iter()
                .any(|other| other != hash && any.block_exists(other));
            if sibling_held {
                *counts.entry("sibling_held").or_default() += 1;
                continue;
            }
            let Some(block) = self.data.evidence_block(hash) else {
                *counts.entry("unavailable").or_default() += 1;
                missing.push(*hash);
                continue;
            };
            let previous = block.previous();
            if !previous.is_zero() && !any.block_exists(&previous) {
                *counts.entry("ancestor_missing").or_default() += 1;
                missing.extend(self.data.missing_ancestor(hash));
                continue;
            }
            let rival = if previous.is_zero() {
                any.get_account(account).map(|info| info.open_block)
            } else {
                any.block_successor(&previous)
            };
            if rival.is_some_and(|rival| {
                any.confirmed().block_exists(&rival) || self.active_elections.is_finalized(&rival)
            }) {
                *counts.entry("rival_final").or_default() += 1;
                continue;
            }
            *counts
                .entry(if rival.is_some() {
                    "forced_over_rival"
                } else {
                    "forced_empty_position"
                })
                .or_default() += 1;
            self.block_processor_queue.push(BlockContext::new(
                block,
                BlockSource::Forced,
                ChannelId::LOOPBACK,
            ));
        }
        if !counts.is_empty() {
            crate::utils::diagnostic!(
                "EPOCH_RETAINED_RECHECK retained={} {:?}",
                retained.len(),
                counts
            );
        }
        missing
    }

    pub fn tick(&self) -> Vec<(&'static str, u128)> {
        let mut spent: Vec<(&'static str, u128)> = Vec::new();
        let mut mark = std::time::Instant::now();
        let mut lap = |name: &'static str, spent: &mut Vec<(&'static str, u128)>| {
            let now = std::time::Instant::now();
            let ms = (now - mark).as_millis();
            if ms > 0 {
                if let Some(entry) = spent.iter_mut().find(|(n, _)| *n == name) {
                    entry.1 += ms;
                } else {
                    spent.push((name, ms));
                }
            }
            mark = now;
        };
        self.process_inbound();
        lap("inbound", &mut spent);
        self.log_unvoted();
        lap("unvoted_diagnostic", &mut spent);
        let missing_blocks = self.recheck_retained();
        self.send_evidence_request(self.active_elections.current_epoch(), missing_blocks);
        lap("retained_recheck", &mut spent);
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
        lap("deferred", &mut spent);
        // This node's own reports go out again for as long as it holds them:
        // a replica that missed the broadcast at the boundary can not select
        // them, and one still deriving an old epoch's state needs them
        let repeated = self
            .exchange
            .lock()
            .unwrap()
            .repeat_reports(self.clock.now());
        self.send(repeated, None);
        lap("repeat_reports", &mut spent);
        self.repeat_residual_votes();
        lap("repeat_votes", &mut spent);
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
            if let Some(committee) = self.active_elections.epoch_committee(epoch) {
                self.exchange
                    .lock()
                    .unwrap()
                    .set_committee(epoch, committee.digest());
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
            lap("select", &mut spent);
            let now = self.clock.now();
            let project = {
                let mut projected = self.projected.lock().unwrap();
                let due = projected
                    .get(&epoch)
                    .is_none_or(|last| last.elapsed(now) >= Self::PROJECTION_INTERVAL);
                if due {
                    projected.insert(epoch, now);
                }
                due
            };
            if project {
                let live = self.active_elections.epoch_certified(epoch);
                lap("projection", &mut spent);
                self.exchange.lock().unwrap().refresh_live(epoch, live);
                lap("refresh_live", &mut spent);
            }
            let mut missing = Vec::new();
            for reporter in reporters {
                self.reconcile(epoch, reporter);
                lap("reconcile", &mut spent);
                self.derive_residual(epoch, reporter, &mut missing);
                lap("residual", &mut spent);
                self.verify_evidence(epoch, reporter, &mut missing);
                lap("evidence", &mut spent);
            }
            self.request_evidence(epoch, missing);
            lap("evidence_request", &mut spent);
        }
        spent
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

    /// Re-gossiped report evidence the ledger already holds, or whose
    /// position the ledger fills with another block and no election decides
    /// here, stays out of the block processor. A block that extends the
    /// ledger's frontier goes in.
    #[test]
    fn only_evidence_the_ledger_can_take_goes_to_the_block_processor() {
        use rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder;
        let ledger = Arc::new(rsnano_ledger::Ledger::new_null());
        let mut lattice = UnsavedBlockLatticeBuilder::with_stub_work();
        let key = PrivateKey::from(1);
        let mut fork_lattice = lattice.clone();
        let held = lattice.genesis().send(&key, 1);
        let next = lattice.genesis().send(&key, 1);
        let rival_of_held = fork_lattice.genesis().send(&PrivateKey::from(2), 1);
        ledger.process_one(&held).unwrap();
        let service = service_with(ledger.clone());

        assert!(!service.evidence_for_ledger(&held), "already held");
        assert!(
            !service.evidence_for_ledger(&rival_of_held),
            "its position is taken and no election decides it here"
        );
        assert!(service.evidence_for_ledger(&next), "extends the frontier");
    }

    #[test]
    fn a_block_held_in_the_fork_cache_is_a_known_fork() {
        let forks = Arc::new(std::sync::RwLock::new(crate::consensus::ForkCache::new()));
        let service = ReportService::new(
            Arc::new(AecService::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            MessageFlooder::new_null(),
            MessageSender::new_null(),
            Arc::new(SteadyClock::new_null()),
            Arc::new(Stats::default()),
            Arc::new(rsnano_ledger::Ledger::new_null()),
            forks.clone(),
            Arc::new(BlockProcessorQueue::new_null()),
        );
        let block = rsnano_types::Block::new_test_instance();
        assert!(!service.known_fork(&block));

        forks.write().unwrap().add(block.clone());

        assert!(service.known_fork(&block));
    }

    /// A checkpoint-finalized block the ledger lacked at installation is
    /// asked for, and forced in when it arrives as evidence
    #[test]
    fn an_awaited_checkpoint_block_is_fetched_and_followed() {
        let service = ReportService::new_null();
        let block = rsnano_types::Block::new_test_instance();
        let hash = block.hash();
        assert!(!service.follows_retained_branch(&block));

        service.active_elections.await_checkpoint_blocks([hash]);
        assert_eq!(service.recheck_retained(), vec![hash]);
        assert!(service.follows_retained_branch(&block));

        service.active_elections.checkpoint_block_arrived(&hash);
        assert!(!service.follows_retained_branch(&block));
    }

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

    /* Test helpers */

    fn service_with(ledger: Arc<rsnano_ledger::Ledger>) -> ReportService {
        ReportService::new(
            Arc::new(AecService::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            MessageFlooder::new_null(),
            MessageSender::new_null(),
            Arc::new(SteadyClock::new_null()),
            Arc::new(Stats::default()),
            ledger,
            Arc::new(std::sync::RwLock::new(crate::consensus::ForkCache::new())),
            Arc::new(BlockProcessorQueue::new_null()),
        )
    }
}
