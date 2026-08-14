use std::{collections::HashMap, sync::RwLock, time::Duration};

#[cfg(all(test, feature = "rai_protocol"))]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "rai_protocol")]
use std::collections::{HashSet, VecDeque};

#[cfg(feature = "rai_protocol")]
use rsnano_ledger::AnySet;
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Account, Amount, Block, BlockHash, PublicKey, QualifiedRoot, SavedBlock, VoteError,
};
#[cfg(feature = "rai_protocol")]
use rsnano_utils::{CancellationToken, ticker::Tickable};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};

#[cfg(feature = "rai_protocol")]
use super::RaiCloseElectionSpec;
use super::{
    ActiveElectionsConfig, ActiveElectionsContainer, ActiveElectionsInfo, AecCooldownReason,
    AecFact, AecInsertError, AecInsertRequest, ApplyVoteArgs,
};

#[cfg(feature = "rai_protocol")]
fn close_preimage_request_indices(
    candidate_count: usize,
    start_sequence: u64,
    limit: usize,
) -> Vec<usize> {
    if candidate_count == 0 || limit == 0 {
        return Vec::new();
    }
    let start = start_sequence as usize % candidate_count;
    (0..candidate_count.min(limit))
        .map(|offset| (start + offset) % candidate_count)
        .collect()
}
use crate::consensus::{
    ElectionCandidateSource,
    election::{ConfirmedElection, Election, ElectionBehavior, ElectionState},
};

#[cfg(feature = "rai_protocol")]
type RaiReportIdentity = (u64, PublicKey, u16, rsnano_types::Signature);

pub struct AecService {
    aec: RwLock<ActiveElectionsContainer>,
    clock: SteadyClock,
    #[cfg(all(test, feature = "rai_protocol"))]
    lookup_read_locks: AtomicUsize,
}

#[cfg(feature = "rai_protocol")]
pub struct RaiEpochTicker {
    aec: std::sync::Arc<AecService>,
    clock: std::sync::Arc<SteadyClock>,
    wallet_reps: std::sync::Arc<std::sync::Mutex<crate::wallets::WalletRepresentatives>>,
    ledger: std::sync::Arc<rsnano_ledger::Ledger>,
    epoch_duration: Duration,
    flooder: crate::transport::MessageFlooder,
    vote_history: std::sync::Arc<crate::consensus::LocalVoteHistory>,
    known_reports: HashSet<RaiReportIdentity>,
    report_rebroadcast_queue: VecDeque<crate::consensus::rai::RaiReport>,
    last_slot_evidence_repair: Option<Timestamp>,
    slot_evidence_repair_cursor: usize,
    last_close_evidence_repair: Option<Timestamp>,
    close_evidence_repair_cursor: usize,
    local_key: Option<rsnano_types::PrivateKey>,
    last_report_request: Option<Timestamp>,
    last_close_preimage_request: Option<Timestamp>,
    last_slot_payload_request: Option<Timestamp>,
    slot_payload_request_epoch: Option<rsnano_types::RaiEpoch>,
    slot_payload_request_cursor: usize,
    initialized_drain_frontiers_epoch: Option<rsnano_types::RaiEpoch>,
    request_sequence: u64,
    close_request_sequence: u64,
    close_preimage_response_sequences: HashMap<(u64, BlockHash, rsnano_types::Root), u64>,
}

#[cfg(feature = "rai_protocol")]
impl RaiEpochTicker {
    pub fn new(
        aec: std::sync::Arc<AecService>,
        clock: std::sync::Arc<SteadyClock>,
        wallet_reps: std::sync::Arc<std::sync::Mutex<crate::wallets::WalletRepresentatives>>,
        ledger: std::sync::Arc<rsnano_ledger::Ledger>,
        epoch_duration: Duration,
        flooder: crate::transport::MessageFlooder,
        vote_history: std::sync::Arc<crate::consensus::LocalVoteHistory>,
    ) -> Self {
        Self {
            aec,
            clock,
            wallet_reps,
            ledger,
            epoch_duration,
            flooder,
            vote_history,
            known_reports: Default::default(),
            report_rebroadcast_queue: Default::default(),
            last_slot_evidence_repair: None,
            slot_evidence_repair_cursor: 0,
            last_close_evidence_repair: None,
            close_evidence_repair_cursor: 0,
            local_key: None,
            last_report_request: None,
            last_close_preimage_request: None,
            last_slot_payload_request: None,
            slot_payload_request_epoch: None,
            slot_payload_request_cursor: 0,
            initialized_drain_frontiers_epoch: None,
            request_sequence: 0,
            close_request_sequence: 0,
            close_preimage_response_sequences: Default::default(),
        }
    }
}

#[cfg(feature = "rai_protocol")]
// Payload repair must stay below the request and block queues' sustained
// capacity. Rotating over the sorted missing-root list bounds one tick without
// starving an obligation which remains unresolved.
const MAX_RAI_SLOT_PAYLOAD_REQUESTS_PER_TICK: usize = 64;

/// A large close cut can contain tens of thousands of slots, while only a
/// small tail normally needs evidence repair. Keep both replay and request
/// work proportional to that unresolved tail.
#[cfg(feature = "rai_protocol")]
const MAX_RAI_SLOT_EVIDENCE_REPAIRS_PER_TICK: usize = 16;

#[cfg(feature = "rai_protocol")]
const MAX_RAI_CLOSE_EVIDENCE_REPAIRS_PER_TICK: usize = 16;

#[cfg(feature = "rai_protocol")]
// A lost First/Notar/Final leaf can require several repair passes. Keeping the
// pass interval aligned with the default close-loop tick prevents one unlucky
// workload election at an epoch boundary from adding multiple seconds of tail
// latency while retaining the same bounded per-pass work.
const RAI_SLOT_EVIDENCE_REPAIR_INTERVAL: Duration = Duration::from_millis(100);

#[cfg(feature = "rai_protocol")]
const RAI_SLOT_EVIDENCE_REPAIR_FANOUT_SCALE: f32 = 8.0;

// Capacity checks are intentionally made at the ordinary fanout. The repair
// flood itself is wider, but Network::fanout is not capped by the number of
// channels; using the repair scale for this gate would disable repair forever
// on a healthy six-replica mesh with only five peer channels.
#[cfg(feature = "rai_protocol")]
const RAI_SLOT_EVIDENCE_CAPACITY_SCALE: f32 = 1.0;

#[cfg(feature = "rai_protocol")]
fn rai_slot_payload_repair_window<T: Clone>(requests: &[T], cursor: &mut usize) -> Vec<T> {
    if requests.is_empty() {
        *cursor = 0;
        return Vec::new();
    }

    let start = *cursor % requests.len();
    let count = requests.len().min(MAX_RAI_SLOT_PAYLOAD_REQUESTS_PER_TICK);
    let result = (0..count)
        .map(|offset| requests[(start + offset) % requests.len()].clone())
        .collect();
    *cursor = (start + count) % requests.len();
    result
}

#[cfg(feature = "rai_protocol")]
fn rai_replay_slot_vote_window<T>(votes: &[T], mut try_replay: impl FnMut(&T) -> bool) -> usize {
    let mut replayed = 0;
    for vote in votes {
        if replayed >= MAX_RAI_SLOT_EVIDENCE_REPAIRS_PER_TICK || !try_replay(vote) {
            break;
        }
        replayed += 1;
    }
    replayed
}

#[cfg(feature = "rai_protocol")]
fn rai_drain_frontier_snapshot_needed(
    initialized_epoch: &mut Option<rsnano_types::RaiEpoch>,
    closing: Option<crate::consensus::rai::RaiClosingEpoch>,
) -> bool {
    let Some(closing) = closing else {
        *initialized_epoch = None;
        return false;
    };
    if initialized_epoch.is_some_and(|epoch| epoch != closing.epoch) {
        *initialized_epoch = None;
    }
    if closing.phase != crate::consensus::rai::RaiClosingPhase::Draining
        || *initialized_epoch == Some(closing.epoch)
    {
        return false;
    }
    *initialized_epoch = Some(closing.epoch);
    true
}

#[cfg(feature = "rai_protocol")]
fn rai_close_repair_phase_active(phase: crate::consensus::rai::RaiClosingPhase) -> bool {
    matches!(
        phase,
        crate::consensus::rai::RaiClosingPhase::ElectingCut
            | crate::consensus::rai::RaiClosingPhase::ElectingRecord
    )
}

#[cfg(feature = "rai_protocol")]
fn rai_slot_payload_repair_phase_active(phase: crate::consensus::rai::RaiClosingPhase) -> bool {
    matches!(
        phase,
        crate::consensus::rai::RaiClosingPhase::Draining
            | crate::consensus::rai::RaiClosingPhase::ElectingRecord
    )
}

#[cfg(feature = "rai_protocol")]
fn rai_report_epidemic_phase_active(
    closing: Option<crate::consensus::rai::RaiClosingEpoch>,
) -> bool {
    closing.is_some_and(|closing| {
        matches!(
            closing.phase,
            crate::consensus::rai::RaiClosingPhase::CollectingReports
                | crate::consensus::rai::RaiClosingPhase::ElectingCut
        )
    })
}

#[cfg(feature = "rai_protocol")]
fn genuinely_missing_slot_payload_requests(
    any: &dyn AnySet,
    candidates: Vec<(QualifiedRoot, Option<BlockHash>)>,
) -> Vec<(BlockHash, rsnano_types::Root)> {
    let mut requests = candidates
        .into_iter()
        .filter(|(root, hash)| match hash {
            Some(hash) => any.get_block(hash).is_none(),
            None => any
                .block_successor_by_qualified_root(root)
                .and_then(|hash| any.get_block(&hash))
                .is_none(),
        })
        .map(|(root, _)| (BlockHash::ZERO, root.root))
        .collect::<Vec<_>>();
    requests.sort_unstable();
    requests.dedup();
    requests
}

#[cfg(feature = "rai_protocol")]
impl Tickable for RaiEpochTicker {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        if self.local_key.is_none() {
            let mut keys = Vec::new();
            self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
            self.local_key = keys.into_iter().next();
        }
        let Some(local_key) = self.local_key.as_ref() else {
            // Reports are committee votes and must be signed by a voting
            // representative. A node-id signature has no committee weight.
            return;
        };
        let now = self.clock.now();
        self.aec.rai_tick(now, local_key, self.epoch_duration);
        let closing = self.aec.rai_epoch_status().0.closing;
        if let Some(closing) = closing {
            let initial_frontiers = rai_drain_frontier_snapshot_needed(
                &mut self.initialized_drain_frontiers_epoch,
                Some(closing),
            )
            .then(|| self.ledger.rai_preceding_frontiers(closing.epoch));
            self.aec
                .rai_progress_close(initial_frontiers, &self.ledger, now);
            self.replay_retained_slot_votes(closing);
            self.replay_retained_close_votes(closing);
            // A report quorum is only enough to propose a cut.  Peers may have
            // reached quorum from different subsets and therefore be voting on
            // different cut hashes.  Keep repairing the report set until the
            // cut election itself has a terminal certificate, so every peer can
            // validate the winning candidate preimage and apply its votes.
            if matches!(
                closing.phase,
                crate::consensus::rai::RaiClosingPhase::CollectingReports
                    | crate::consensus::rai::RaiClosingPhase::ElectingCut
            ) && self
                .last_report_request
                .is_none_or(|last| last.elapsed(now) >= Duration::from_millis(500))
            {
                self.request_sequence = self.request_sequence.wrapping_add(1);
                self.flooder.flood_prs_and_some_non_prs(
                    &rsnano_messages::Message::RaiReportRequest(
                        rsnano_messages::RaiReportRequest {
                            epoch: closing.epoch,
                            sequence: self.request_sequence,
                        },
                    ),
                    rsnano_network::TrafficType::ConfirmationRequests,
                    1.0,
                );
                self.last_report_request = Some(now);
            }
            // Active close votes use the ordinary batched ConfirmReq/
            // ConfirmAck path. The custom envelope is only for a precise
            // signed close digest whose canonical preimage is absent here.
            const CLOSE_REPAIR_INTERVAL: Duration = Duration::from_millis(100);
            if rai_close_repair_phase_active(closing.phase)
                && self
                    .last_close_preimage_request
                    .is_none_or(|last| last.elapsed(now) >= CLOSE_REPAIR_INTERVAL)
            {
                let requests = self.aec.rai_missing_close_preimage_requests(closing.epoch);
                self.close_preimage_response_sequences
                    .retain(|(epoch, _, _), _| *epoch == closing.epoch.number());
                if !requests.is_empty() {
                    // A split close round can expose one distinct digest per
                    // representative.  Request a bounded window, rather than
                    // only one digest every repair interval, so all signed
                    // First values become reconstructible before a partial
                    // chunk assembly expires.  The rotating start preserves
                    // fairness when more candidates are retained than fit in
                    // one pass.
                    const CLOSE_PREIMAGE_REQUESTS_PER_PASS: usize = 4;
                    let indices = close_preimage_request_indices(
                        requests.len(),
                        self.close_request_sequence,
                        CLOSE_PREIMAGE_REQUESTS_PER_PASS,
                    );
                    self.close_request_sequence = self
                        .close_request_sequence
                        .wrapping_add(indices.len() as u64);
                    for index in indices {
                        let (hash, root) = requests[index];
                        debug_assert!(!hash.is_zero());
                        let response_sequence = self
                            .close_preimage_response_sequences
                            .entry((closing.epoch.number(), hash, root))
                            .or_default();
                        *response_sequence = response_sequence.wrapping_add(1);
                        self.flooder.flood_prs_and_some_non_prs(
                            &rsnano_messages::Message::RaiVoteRequest(
                                rsnano_messages::RaiVoteRequest {
                                    // Consecutive close-only sequences rotate a
                                    // bounded response window across a large exact
                                    // preimage without gaps from unrelated repair.
                                    sequence: rsnano_messages::RAI_CLOSE_REPAIR_SEQUENCE_FLAG
                                        | *response_sequence
                                            & rsnano_messages::RAI_REPAIR_SEQUENCE_COUNTER_MASK,
                                    epoch: closing.epoch.number(),
                                    hash,
                                    root,
                                    close_version: None,
                                },
                            ),
                            rsnano_network::TrafficType::ConfirmationRequests,
                            8.0,
                        );
                    }
                }
                self.last_close_preimage_request = Some(now);
            }
            if rai_slot_payload_repair_phase_active(closing.phase)
                && self
                    .last_slot_payload_request
                    .is_none_or(|last| last.elapsed(now) >= Duration::from_secs(2))
            {
                if self.slot_payload_request_epoch != Some(closing.epoch) {
                    self.slot_payload_request_epoch = Some(closing.epoch);
                    self.slot_payload_request_cursor = 0;
                }
                // Candidate discovery is maintained by the bounded drain
                // worklist. Recheck every candidate against one ledger read
                // snapshot so the custom ZERO request is sent only when the
                // payload itself is genuinely absent.
                let requests = {
                    let candidates = self.aec.rai_missing_slot_payload_candidates(closing.epoch);
                    let any = self.ledger.any();
                    genuinely_missing_slot_payload_requests(&any, candidates)
                };
                for (hash, root) in
                    rai_slot_payload_repair_window(&requests, &mut self.slot_payload_request_cursor)
                {
                    debug_assert!(hash.is_zero());
                    self.request_sequence = self.request_sequence.wrapping_add(1);
                    self.flooder.flood_prs_and_some_non_prs(
                        &rsnano_messages::Message::RaiVoteRequest(
                            rsnano_messages::RaiVoteRequest {
                                sequence: rsnano_messages::RAI_SLOT_REPAIR_SEQUENCE_FLAG
                                    | self.request_sequence
                                        & rsnano_messages::RAI_REPAIR_SEQUENCE_COUNTER_MASK,
                                epoch: closing.epoch.number(),
                                hash,
                                root,
                                close_version: None,
                            },
                        ),
                        rsnano_network::TrafficType::ConfirmationRequests,
                        8.0,
                    );
                }
                self.last_slot_payload_request = Some(now);
            }
        } else {
            rai_drain_frontier_snapshot_needed(&mut self.initialized_drain_frontiers_epoch, None);
            self.last_slot_evidence_repair = None;
            self.slot_evidence_repair_cursor = 0;
            self.last_close_evidence_repair = None;
            self.close_evidence_repair_cursor = 0;
            self.last_close_preimage_request = None;
            self.close_preimage_response_sequences.clear();
            self.last_slot_payload_request = None;
            self.slot_payload_request_epoch = None;
            self.slot_payload_request_cursor = 0;
        }
        if rai_report_epidemic_phase_active(closing) {
            // Reports use the same epidemic dissemination model as legacy
            // votes while they can still affect the active cut. Once this
            // replica enters Draining, lagging peers use the explicit report
            // request path; repeatedly cloning every retained report here
            // would monopolize the AEC read lock at the boundary.
            const MAX_REPORT_QUEUE: usize = 16 * 1024;
            let available = MAX_REPORT_QUEUE.saturating_sub(self.report_rebroadcast_queue.len());
            let epoch = closing
                .expect("active report phase has a closing epoch")
                .epoch;
            self.report_rebroadcast_queue
                .extend(self.aec.rai_new_reports_for_epoch(
                    epoch,
                    &mut self.known_reports,
                    available,
                ));
            while !self.report_rebroadcast_queue.is_empty()
                && self
                    .flooder
                    .check_capacity(rsnano_network::TrafficType::Generic, 1.0)
            {
                let report = self.report_rebroadcast_queue.pop_front().unwrap();
                tracing::trace!(
                    epoch = report.epoch.number(),
                    reporter = ?report.reporter,
                    chunk_index = report.chunk_index,
                    chunk_count = report.chunk_count,
                    obligations = report.visible_obligations.len(),
                    "RAI report epidemic send"
                );
                self.flooder.flood_prs_and_some_non_prs(
                    &rsnano_messages::Message::RaiReport(report.clone().into()),
                    rsnano_network::TrafficType::Generic,
                    1.0,
                );
            }
        } else {
            // Explicit report repair supersedes any epidemic work which could
            // not be sent before the cut was decided.
            self.report_rebroadcast_queue.clear();
        }
    }
}

#[cfg(feature = "rai_protocol")]
impl RaiEpochTicker {
    fn replay_retained_close_votes(&mut self, closing: crate::consensus::rai::RaiClosingEpoch) {
        if !rai_close_repair_phase_active(closing.phase) {
            self.last_close_evidence_repair = None;
            self.close_evidence_repair_cursor = 0;
            return;
        }
        let now = self.clock.now();
        if self
            .last_close_evidence_repair
            .is_some_and(|last| last.elapsed(now) < RAI_SLOT_EVIDENCE_REPAIR_INTERVAL)
        {
            return;
        }

        // Close leaves are durable authenticated objects.  Re-gossip retained
        // batches while their election is active: a cached ConfirmReq reply is
        // point-to-point and cannot by itself converge replicas which learned
        // disjoint First values at the epoch boundary.
        let requests = self
            .aec
            .rai_active_close_vote_requests(closing.epoch, MAX_RAI_CLOSE_EVIDENCE_REPAIRS_PER_TICK);
        let active_ids = requests
            .iter()
            .map(|(_, _, id)| id.clone())
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        let mut votes = self
            .aec
            .rai_close_votes_for_epoch(closing.epoch)
            .into_iter()
            .filter(|vote| seen.insert((vote.voter, vote.signature.clone())))
            .collect::<Vec<_>>();
        // The locally generated leaf may have entered LocalVoteHistory while
        // its one-shot vote-processor enqueue was dropped under saturation.
        // Include that authoritative signed history in epidemic repair.
        votes.extend(
            requests
                .iter()
                .flat_map(|(_, root, id)| self.vote_history.rai_votes_for_election(root, id))
                .filter(|vote| seen.insert((vote.voter, vote.signature.clone())))
                .map(|vote| (*vote).clone()),
        );
        let (active_votes, archived_votes): (Vec<_>, Vec<_>) =
            votes.into_iter().partition(|vote| {
                vote.rai_entries()
                    .any(|(metadata, _)| active_ids.contains(&metadata.election_id))
            });
        if active_votes.is_empty() && archived_votes.is_empty() {
            self.close_evidence_repair_cursor = 0;
        } else {
            let mut replayed = 0;
            for vote in active_votes
                .iter()
                .take(MAX_RAI_CLOSE_EVIDENCE_REPAIRS_PER_TICK)
            {
                if !self.flooder.check_capacity(
                    rsnano_network::TrafficType::VoteReply,
                    RAI_SLOT_EVIDENCE_CAPACITY_SCALE,
                ) {
                    break;
                }
                self.flooder.flood_prs_and_some_non_prs(
                    &rsnano_messages::Message::ConfirmAck(
                        rsnano_messages::ConfirmAck::new_with_own_vote(vote.clone()),
                    ),
                    rsnano_network::TrafficType::VoteReply,
                    RAI_SLOT_EVIDENCE_REPAIR_FANOUT_SCALE,
                );
                replayed += 1;
            }
            if !archived_votes.is_empty() && replayed < MAX_RAI_CLOSE_EVIDENCE_REPAIRS_PER_TICK {
                let start = self.close_evidence_repair_cursor % archived_votes.len();
                let remaining = MAX_RAI_CLOSE_EVIDENCE_REPAIRS_PER_TICK - replayed;
                let mut archived_replayed = 0;
                for offset in 0..archived_votes.len().min(remaining) {
                    let vote = &archived_votes[(start + offset) % archived_votes.len()];
                    if !self.flooder.check_capacity(
                        rsnano_network::TrafficType::VoteReply,
                        RAI_SLOT_EVIDENCE_CAPACITY_SCALE,
                    ) {
                        break;
                    }
                    self.flooder.flood_prs_and_some_non_prs(
                        &rsnano_messages::Message::ConfirmAck(
                            rsnano_messages::ConfirmAck::new_with_own_vote(vote.clone()),
                        ),
                        rsnano_network::TrafficType::VoteReply,
                        RAI_SLOT_EVIDENCE_REPAIR_FANOUT_SCALE,
                    );
                    archived_replayed += 1;
                }
                self.close_evidence_repair_cursor =
                    (start + archived_replayed) % archived_votes.len();
            }
        }

        // Replaying First leaves can change the active request target to the
        // timeout value. Solicit the current target after every repair pass so
        // committee members produce the next phase vote instead of waiting
        // indefinitely for an unrelated request.
        if !requests.is_empty()
            && self.flooder.check_capacity(
                rsnano_network::TrafficType::ConfirmationRequests,
                RAI_SLOT_EVIDENCE_CAPACITY_SCALE,
            )
        {
            self.flooder.flood_prs_and_some_non_prs(
                &rsnano_messages::Message::ConfirmReq(rsnano_messages::ConfirmReq::new(
                    requests
                        .into_iter()
                        .map(|(hash, root, _)| (hash, root))
                        .collect(),
                )),
                rsnano_network::TrafficType::ConfirmationRequests,
                RAI_SLOT_EVIDENCE_REPAIR_FANOUT_SCALE,
            );
        }
        self.last_close_evidence_repair = Some(now);
    }

    fn replay_retained_slot_votes(&mut self, closing: crate::consensus::rai::RaiClosingEpoch) {
        if !matches!(
            closing.phase,
            crate::consensus::rai::RaiClosingPhase::Draining
                | crate::consensus::rai::RaiClosingPhase::ElectingRecord
        ) {
            self.last_slot_evidence_repair = None;
            self.slot_evidence_repair_cursor = 0;
            return;
        }
        let now = self.clock.now();
        if self
            .last_slot_evidence_repair
            .is_some_and(|last| last.elapsed(now) < RAI_SLOT_EVIDENCE_REPAIR_INTERVAL)
        {
            return;
        }

        let slots = self
            .aec
            .rai_unresolved_drain_slots(closing.epoch, MAX_RAI_SLOT_EVIDENCE_REPAIRS_PER_TICK);
        if closing.phase == crate::consensus::rai::RaiClosingPhase::ElectingRecord {
            // Ordinary-finalized slots outside the cut are exact fresh-record
            // locks.  Their durable vote batches must keep converging while a
            // split record round retries, even though the cut drain itself is
            // already complete.
            let votes = self.aec.rai_slot_votes_for_epoch(closing.epoch);
            if votes.is_empty() {
                self.slot_evidence_repair_cursor = 0;
            } else {
                let start = self.slot_evidence_repair_cursor % votes.len();
                let mut replayed = 0;
                for offset in 0..votes.len().min(MAX_RAI_SLOT_EVIDENCE_REPAIRS_PER_TICK) {
                    let vote = &votes[(start + offset) % votes.len()];
                    if !self.flooder.check_capacity(
                        rsnano_network::TrafficType::VoteRebroadcast,
                        RAI_SLOT_EVIDENCE_CAPACITY_SCALE,
                    ) {
                        break;
                    }
                    self.flooder.flood_prs_and_some_non_prs(
                        &rsnano_messages::Message::ConfirmAck(
                            rsnano_messages::ConfirmAck::new_with_own_vote(vote.clone()),
                        ),
                        rsnano_network::TrafficType::VoteRebroadcast,
                        RAI_SLOT_EVIDENCE_REPAIR_FANOUT_SCALE,
                    );
                    replayed += 1;
                }
                self.slot_evidence_repair_cursor = (start + replayed) % votes.len();
            }
        }
        if slots.is_empty() {
            self.last_slot_evidence_repair = Some(now);
            return;
        }

        // A vectorized transport may cover several unresolved slots. Replay
        // it once, keeping First-before-Final order from LocalVoteHistory.
        let mut seen = HashSet::new();
        let votes = slots
            .iter()
            .flat_map(|slot| {
                self.vote_history.rai_votes_for_election(
                    &slot.root.root,
                    &rsnano_types::RaiElectionId::Slot(slot.clone()),
                )
            })
            .filter(|vote| seen.insert((vote.voter, vote.signature.clone())))
            .collect::<Vec<_>>();
        rai_replay_slot_vote_window(&votes, |vote| {
            if !self.flooder.check_capacity(
                rsnano_network::TrafficType::VoteRebroadcast,
                RAI_SLOT_EVIDENCE_CAPACITY_SCALE,
            ) {
                return false;
            }
            self.flooder.flood_prs_and_some_non_prs(
                &rsnano_messages::Message::ConfirmAck(
                    rsnano_messages::ConfirmAck::new_with_own_vote((**vote).clone()),
                ),
                rsnano_network::TrafficType::VoteRebroadcast,
                RAI_SLOT_EVIDENCE_REPAIR_FANOUT_SCALE,
            );
            true
        });

        // Resolved peers no longer list this slot locally, but they may retain
        // the precise First leaf a lagging replica needs. A bounded ZERO-root
        // ConfirmReq asks those peers to replay the exact epoch-qualified
        // history (and, if still active, their current phase) back to us.
        if self.flooder.check_capacity(
            rsnano_network::TrafficType::ConfirmationRequests,
            RAI_SLOT_EVIDENCE_CAPACITY_SCALE,
        ) {
            let request = rsnano_messages::ConfirmReq::new(
                slots
                    .iter()
                    .map(|slot| (BlockHash::ZERO, slot.root.root))
                    .collect(),
            );
            self.flooder.flood_prs_and_some_non_prs(
                &rsnano_messages::Message::ConfirmReq(request),
                rsnano_network::TrafficType::ConfirmationRequests,
                RAI_SLOT_EVIDENCE_REPAIR_FANOUT_SCALE,
            );
        }
        self.last_slot_evidence_repair = Some(now);
    }
}

#[cfg(feature = "rai_protocol")]
fn rai_report_identity(report: &crate::consensus::rai::RaiReport) -> RaiReportIdentity {
    (
        report.epoch.number(),
        report.reporter,
        report.chunk_index,
        report.signature.clone(),
    )
}

impl AecService {
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::new(config, base_latency)),
            clock: SteadyClock::default(),
            #[cfg(all(test, feature = "rai_protocol"))]
            lookup_read_locks: AtomicUsize::new(0),
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_with_rai_committee(
        config: ActiveElectionsConfig,
        base_latency: Duration,
        genesis_committee: std::sync::Arc<rsnano_ledger::RepWeights>,
        genesis_governing_hash: BlockHash,
        ledger: std::sync::Arc<rsnano_ledger::Ledger>,
    ) -> Self {
        let clock = SteadyClock::default();
        let mut aec = ActiveElectionsContainer::new_with_rai_committee(
            config,
            base_latency,
            genesis_committee,
            genesis_governing_hash,
        );
        aec.set_rai_ledger(ledger);
        // Epoch zero begins when this node initializes RAI. Leaving the
        // manager at Timestamp::default() makes the first ticker invocation
        // close epoch zero immediately, before nanospam can publish work.
        aec.rai_set_open_started_at(clock.now());
        Self {
            aec: RwLock::new(aec),
            clock,
            #[cfg(all(test, feature = "rai_protocol"))]
            lookup_read_locks: AtomicUsize::new(0),
        }
    }

    pub fn new_null() -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::default()),
            clock: SteadyClock::new_null(),
            #[cfg(all(test, feature = "rai_protocol"))]
            lookup_read_locks: AtomicUsize::new(0),
        }
    }

    fn read_for_lookup(&self) -> std::sync::RwLockReadGuard<'_, ActiveElectionsContainer> {
        #[cfg(all(test, feature = "rai_protocol"))]
        self.lookup_read_locks.fetch_add(1, Ordering::Relaxed);
        self.aec.read().unwrap()
    }

    #[cfg(all(test, feature = "rai_protocol"))]
    fn lookup_read_lock_count(&self) -> usize {
        self.lookup_read_locks.load(Ordering::Relaxed)
    }

    // --- Read forwarding ---

    pub fn check_vacancy<T>(&self, source: &T) -> bool
    where
        T: ElectionCandidateSource,
    {
        self.aec.read().unwrap().check_vacancy(source)
    }

    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<Election> {
        self.aec.read().unwrap().election_for_root(root).cloned()
    }

    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<Election> {
        self.aec
            .read()
            .unwrap()
            .election_for_block(block_hash)
            .cloned()
    }

    #[cfg(all(feature = "rai_protocol", test))]
    pub(crate) fn rai_vote_context(
        &self,
        block_hash: &BlockHash,
    ) -> Option<(rsnano_types::RaiVoteMetadata, bool)> {
        self.read_for_lookup().rai_vote_context(block_hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_vote_contexts(
        &self,
        block_hashes: &[BlockHash],
    ) -> Vec<Option<(rsnano_types::RaiVoteMetadata, bool)>> {
        self.read_for_lookup().rai_vote_contexts(block_hashes)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_zero_hash_vote_requests(
        &self,
        roots: &[rsnano_types::Root],
    ) -> Vec<Option<super::RaiZeroHashVoteRequest>> {
        self.read_for_lookup().rai_zero_hash_vote_requests(roots)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_terminal_notarized_target_for_root(
        &self,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Option<(BlockHash, rsnano_types::RaiVoteMetadata)> {
        self.aec
            .read()
            .unwrap()
            .rai_terminal_notarized_target_for_root(root, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_active_slot_vote_target_for_root(
        &self,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Option<rsnano_ledger::RaiFinalizedVoteTarget> {
        self.aec
            .read()
            .unwrap()
            .rai_active_slot_vote_target_for_root(root, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_votes_for_root(
        &self,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<rsnano_types::Vote> {
        self.aec.read().unwrap().rai_votes_for_root(root, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_blocks_for_request(
        &self,
        hash: BlockHash,
        root: rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<Block> {
        self.aec
            .read()
            .unwrap()
            .rai_blocks_for_request(hash, root, epoch)
    }

    pub fn max_len(&self) -> usize {
        self.aec.read().unwrap().max_len()
    }

    pub fn len(&self) -> usize {
        self.aec.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.aec.read().unwrap().is_empty()
    }

    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.aec.read().unwrap().is_active_root(root)
    }

    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.read_for_lookup().is_active_hash(block_hash)
    }

    pub fn any_active_hash(&self, block_hashes: &[BlockHash]) -> bool {
        self.read_for_lookup().any_active_hash(block_hashes)
    }

    pub fn was_recently_confirmed(&self, block_hash: &BlockHash) -> bool {
        self.aec.read().unwrap().was_recently_confirmed(block_hash)
    }

    pub fn count_by_behavior(&self, behavior: ElectionBehavior) -> usize {
        self.aec.read().unwrap().count_by_behavior(behavior)
    }

    pub fn vacancy(&self) -> i64 {
        self.aec.read().unwrap().vacancy()
    }

    pub fn info(&self) -> ActiveElectionsInfo {
        let now = self.clock.now();
        self.aec.read().unwrap().info(now)
    }

    pub fn round_robin<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&mut dyn Iterator<Item = &Election>) -> T,
    {
        let guard = self.aec.read().unwrap();
        f(&mut guard.iter_round_robin())
    }

    // --- Write forwarding ---

    pub fn set_observer(&self, observer: Sender<AecFact>) {
        self.aec.write().unwrap().set_observer(observer)
    }

    pub fn insert(&self, request: AecInsertRequest, now: Timestamp) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert(request, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_cut(
        &self,
        spec: RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert_close_cut(spec, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_record(
        &self,
        spec: RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert_close_record(spec, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_record_versions(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<crate::consensus::rai::RaiCloseRecord> {
        self.aec.read().unwrap().rai_close_record_versions(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_record_version(
        &self,
        epoch: rsnano_types::RaiEpoch,
        hash: &BlockHash,
    ) -> Option<crate::consensus::rai::RaiCloseRecord> {
        self.aec
            .read()
            .unwrap()
            .rai_close_record_version(epoch, hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_record_versions_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Vec<crate::consensus::rai::RaiCloseRecord> {
        self.aec
            .read()
            .unwrap()
            .rai_close_record_versions_for_root(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_cut_versions_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Vec<crate::consensus::rai::RaiCloseCut> {
        self.aec
            .read()
            .unwrap()
            .rai_close_cut_versions_for_root(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_cut_versions(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<crate::consensus::rai::RaiCloseCut> {
        self.aec.read().unwrap().rai_close_cut_versions(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_cut_version(
        &self,
        epoch: rsnano_types::RaiEpoch,
        hash: &BlockHash,
    ) -> Option<crate::consensus::rai::RaiCloseCut> {
        self.aec.read().unwrap().rai_close_cut_version(epoch, hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_missing_close_preimage_requests(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<(BlockHash, rsnano_types::Root)> {
        self.aec
            .read()
            .unwrap()
            .rai_missing_close_preimage_requests(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_votes_for_epoch(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<rsnano_types::Vote> {
        self.aec.read().unwrap().rai_close_votes_for_epoch(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_slot_votes_for_epoch(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<rsnano_types::Vote> {
        self.aec.read().unwrap().rai_slot_votes_for_epoch(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_active_close_vote_requests(
        &self,
        epoch: rsnano_types::RaiEpoch,
        limit: usize,
    ) -> Vec<(BlockHash, rsnano_types::Root, rsnano_types::RaiElectionId)> {
        self.aec
            .read()
            .unwrap()
            .rai_active_close_vote_requests(epoch, limit)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_diagnostics(&self) -> std::collections::BTreeMap<String, String> {
        self.aec.read().unwrap().rai_close_diagnostics()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn reconcile_rai_close_cut(
        &self,
        cut: crate::consensus::rai::RaiCloseCut,
        root: rsnano_types::Root,
    ) -> bool {
        self.aec
            .write()
            .unwrap()
            .reconcile_rai_close_cut(cut, root, self.clock.now())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn reconcile_rai_close_record(
        &self,
        record: crate::consensus::rai::RaiCloseRecord,
        root: rsnano_types::Root,
    ) -> bool {
        self.aec
            .write()
            .unwrap()
            .reconcile_rai_close_record(record, root, self.clock.now())
    }

    pub fn try_add_fork(&self, fork: &Block, fork_tally: Amount) -> bool {
        self.aec.write().unwrap().try_add_fork(fork, fork_tally)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn published_block_available(&self, block: Block) {
        self.aec.write().unwrap().published_block_available(block)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn admit_candidate(
        &self,
        slot: crate::consensus::election::RaiSlotId,
        candidate: BlockHash,
    ) -> Result<(), super::CandidateError> {
        self.aec.write().unwrap().admit_candidate(slot, candidate)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn has_published_block(&self, hash: &BlockHash) -> bool {
        self.aec.read().unwrap().known_block(hash).is_some()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn published_blocks_at_root(&self, root: &QualifiedRoot) -> Vec<BlockHash> {
        self.aec
            .read()
            .unwrap()
            .candidate_hashes_at_root(root)
            .copied()
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn slot_contains_candidate(
        &self,
        slot: &crate::consensus::election::RaiSlotId,
        hash: &BlockHash,
    ) -> bool {
        self.aec.read().unwrap().slot_contains_candidate(slot, hash)
    }

    pub fn apply_vote<'a>(
        &self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        self.aec.write().unwrap().apply_vote(args)
    }

    pub fn transition_time(&self, now: Timestamp) {
        self.aec.write().unwrap().transition_time(now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_tick(
        &self,
        now: Timestamp,
        local_key: &rsnano_types::PrivateKey,
        epoch_duration: Duration,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.aec
            .write()
            .unwrap()
            .rai_tick(now, local_key, epoch_duration)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_report_received(&self, report: crate::consensus::rai::RaiReport) {
        tracing::trace!(
            epoch = report.epoch.number(),
            reporter = ?report.reporter,
            chunk_index = report.chunk_index,
            chunk_count = report.chunk_count,
            obligations = report.visible_obligations.len(),
            "RAI report receive"
        );
        let now = self.clock.now();
        self.aec.write().unwrap().rai_report_received(
            report,
            &rsnano_types::PrivateKey::from(0),
            Duration::from_secs(1),
            now,
        );
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_reports(&self) -> Vec<crate::consensus::rai::RaiReport> {
        self.aec.read().unwrap().rai_reports()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_current_close_root(&self) -> Option<rsnano_types::Root> {
        self.aec.read().unwrap().rai_current_close_root()
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_new_reports_for_epoch(
        &self,
        epoch: rsnano_types::RaiEpoch,
        known: &mut HashSet<RaiReportIdentity>,
        limit: usize,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.aec.read().unwrap().rai_reports_for_epoch_filtered(
            epoch,
            |report| known.insert(rai_report_identity(report)),
            limit,
        )
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_report_response_window(
        &self,
        epoch: rsnano_types::RaiEpoch,
        sequence: u64,
        limit: usize,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.aec
            .read()
            .unwrap()
            .rai_report_response_window(epoch, sequence, limit)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_finalized_vote_target(
        &self,
        ledger: &rsnano_ledger::Ledger,
        hash: &BlockHash,
        root: &rsnano_types::Root,
        requested_epoch: rsnano_types::RaiEpoch,
    ) -> Option<rsnano_ledger::RaiFinalizedVoteTarget> {
        let aec = self.aec.read().unwrap();
        if let Some(target) = aec.rai_finalized_close_vote_target(root)
            && target.metadata.epoch == requested_epoch
        {
            return Some(target);
        }
        if let Some(target) = aec.rai_certificate_finalized_vote_target(hash, root, requested_epoch)
        {
            return Some(target);
        }
        drop(aec);
        let target = ledger.rai_finalized_vote_target(hash, root)?;
        let epoch = target.metadata.epoch;
        let aec = self.aec.read().unwrap();
        if epoch != requested_epoch
            || !aec.rai_has_governing_context(epoch)
            || !aec.rai_election_vote_enabled(&target.election_id)
        {
            return None;
        }
        Some(target)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_has_active_request_target(
        &self,
        hash: &BlockHash,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> bool {
        self.aec
            .read()
            .unwrap()
            .rai_has_active_request_target(hash, root, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_missing_slot_payload_candidates(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<(QualifiedRoot, Option<BlockHash>)> {
        self.aec
            .read()
            .unwrap()
            .rai_missing_slot_payload_candidates(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_unresolved_drain_slots(
        &self,
        epoch: rsnano_types::RaiEpoch,
        limit: usize,
    ) -> Vec<crate::consensus::election::RaiSlotId> {
        self.aec
            .write()
            .unwrap()
            .rai_unresolved_drain_slots(epoch, limit)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_progress_close(
        &self,
        frontiers: Option<crate::consensus::rai::RaiFrontierMap>,
        ledger: &rsnano_ledger::Ledger,
        now: Timestamp,
    ) {
        self.aec
            .write()
            .unwrap()
            .rai_progress_close(frontiers, ledger, now);
    }

    pub fn transition_active(&self, block_hash: &BlockHash) -> bool {
        self.aec.write().unwrap().transition_active(block_hash)
    }

    pub fn refill<T>(&self, source: &mut T, now: Timestamp)
    where
        T: ElectionCandidateSource,
    {
        self.aec.write().unwrap().refill(source, now);
    }

    pub fn remove_votes<'a>(
        &self,
        root: &QualifiedRoot,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        self.aec.write().unwrap().remove_votes(root, voters)
    }

    pub fn erase(&self, root: &QualifiedRoot) -> bool {
        self.aec.write().unwrap().erase(root)
    }

    pub fn confirm_dependent_elections(
        &self,
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>)>,
        now: Timestamp,
    ) {
        self.aec
            .write()
            .unwrap()
            .confirm_dependent_elections(confirmed, now)
    }

    pub fn remove_recently_confirmed(&self, block_hash: &BlockHash) {
        self.aec
            .write()
            .unwrap()
            .remove_recently_confirmed(block_hash)
    }

    pub fn set_cooldown(&self, cool_down: bool, reason: AecCooldownReason) {
        self.aec.write().unwrap().set_cooldown(cool_down, reason)
    }

    pub fn cancel(&self, root: &QualifiedRoot) {
        self.aec.write().unwrap().cancel(root)
    }

    pub fn cancel_all(&self) {
        self.aec.write().unwrap().cancel_all()
    }

    pub fn clear_recently_confirmed(&self) {
        self.aec.write().unwrap().clear_recently_confirmed()
    }

    pub fn stop(&self) {
        self.aec.write().unwrap().stop()
    }

    pub fn force_confirm(&self, block_hash: &BlockHash, now: Timestamp) {
        self.aec.write().unwrap().force_confirm(block_hash, now)
    }

    pub fn simulate_event(&self, event: AecFact) {
        self.aec.read().unwrap().simulate_event(event)
    }

    pub fn snapshot(&self) -> AecSnapshot {
        let now = self.clock.now();
        self.aec.read().unwrap().snapshot(now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_epoch_status(
        &self,
    ) -> (
        crate::consensus::rai::RaiEpochState,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, BlockHash>,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, BlockHash>,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, (usize, usize)>,
    ) {
        let aec = self.aec.read().unwrap();
        let state = *aec.rai_epoch_state();
        let mut hashes = std::collections::BTreeMap::new();
        if let Some(last) = state.closed_through {
            for number in 0..=last.number() {
                let epoch = rsnano_types::RaiEpoch::new(number);
                if let Some(hash) = aec.rai_installed_close_hash(epoch) {
                    hashes.insert(epoch, hash);
                }
            }
        }
        let cut_hashes = aec.rai_decided_cut_hashes().clone();
        let drains = aec
            .rai_happy_path_drains()
            .iter()
            .map(|(epoch, drain)| {
                (
                    *epoch,
                    (
                        drain.obligations.len(),
                        drain.finalized.len() + drain.selected.len() + drain.released.len(),
                    ),
                )
            })
            .collect();
        (state, hashes, cut_hashes, drains)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_genesis_committee(&self) -> std::sync::Arc<rsnano_ledger::RepWeights> {
        self.aec.read().unwrap().rai_genesis_committee()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_election_durations(
        &self,
    ) -> (
        std::collections::BTreeMap<rsnano_types::RaiEpoch, Duration>,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, Duration>,
    ) {
        let aec = self.aec.read().unwrap();
        let (cut, record) = aec.rai_close_election_durations();
        (cut.clone(), record.clone())
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_epoch_ticker_tests {
    use super::*;

    #[test]
    fn split_close_preimage_repair_requests_a_bounded_rotating_window() {
        assert_eq!(close_preimage_request_indices(0, 0, 4), Vec::<usize>::new());
        assert_eq!(close_preimage_request_indices(3, 0, 4), vec![0, 1, 2]);
        assert_eq!(close_preimage_request_indices(6, 0, 4), vec![0, 1, 2, 3]);
        assert_eq!(close_preimage_request_indices(6, 4, 4), vec![4, 5, 0, 1]);
        assert_eq!(close_preimage_request_indices(6, 8, 4), vec![2, 3, 4, 5]);
    }

    #[test]
    fn payload_repair_selects_a_bounded_window_in_order() {
        let requests = (0..70).collect::<Vec<_>>();
        let mut cursor = 0;

        let selected = rai_slot_payload_repair_window(&requests, &mut cursor);

        assert_eq!(selected, (0..64).collect::<Vec<_>>());
        assert_eq!(cursor, 64);
    }

    #[test]
    fn payload_repair_window_wraps_to_the_start() {
        let requests = (0..70).collect::<Vec<_>>();
        let mut cursor = 64;

        let selected = rai_slot_payload_repair_window(&requests, &mut cursor);

        assert_eq!(selected, (64..70).chain(0..58).collect::<Vec<_>>());
        assert_eq!(cursor, 58);
    }

    #[test]
    fn repeated_payload_repair_windows_do_not_starve_requests() {
        let requests = (0..130).collect::<Vec<_>>();
        let mut cursor = 0;
        let mut seen = std::collections::HashSet::new();

        for _ in 0..3 {
            seen.extend(rai_slot_payload_repair_window(&requests, &mut cursor));
        }

        assert_eq!(seen.len(), requests.len());
        assert!(requests.iter().all(|request| seen.contains(request)));
    }

    #[test]
    fn retained_slot_vote_replay_is_bounded() {
        let votes = (0..20).collect::<Vec<_>>();
        let mut sent = Vec::new();

        let replayed = rai_replay_slot_vote_window(&votes, |vote| {
            sent.push(*vote);
            true
        });

        assert_eq!(replayed, MAX_RAI_SLOT_EVIDENCE_REPAIRS_PER_TICK);
        assert_eq!(sent, (0..16).collect::<Vec<_>>());
    }

    #[test]
    fn retained_slot_vote_replay_does_not_skip_on_backpressure() {
        let votes = (0..20).collect::<Vec<_>>();
        let mut sent = Vec::new();
        let mut capacity = 3;

        let replayed = rai_replay_slot_vote_window(&votes, |vote| {
            if capacity == 0 {
                return false;
            }
            capacity -= 1;
            sent.push(*vote);
            true
        });

        assert_eq!(replayed, 3);
        assert_eq!(sent, vec![0, 1, 2]);
    }

    #[test]
    fn slot_evidence_repair_capacity_gate_accepts_a_small_healthy_topology() {
        let flooder = crate::transport::MessageFlooder::new_null();

        assert!(flooder.check_capacity(
            rsnano_network::TrafficType::VoteRebroadcast,
            RAI_SLOT_EVIDENCE_CAPACITY_SCALE,
        ));
        assert!(!flooder.check_capacity(
            rsnano_network::TrafficType::VoteRebroadcast,
            RAI_SLOT_EVIDENCE_REPAIR_FANOUT_SCALE,
        ));
    }

    #[test]
    fn drain_frontier_snapshot_is_taken_once_per_closing_epoch() {
        use crate::consensus::rai::{RaiClosingEpoch, RaiClosingPhase};
        use rsnano_types::RaiEpoch;

        let closing = |epoch, phase| {
            Some(RaiClosingEpoch {
                epoch: RaiEpoch::new(epoch),
                phase,
            })
        };
        let mut initialized_epoch = None;

        assert!(!rai_drain_frontier_snapshot_needed(
            &mut initialized_epoch,
            closing(0, RaiClosingPhase::ElectingCut),
        ));
        assert!(initialized_epoch.is_none());
        assert!(rai_drain_frontier_snapshot_needed(
            &mut initialized_epoch,
            closing(0, RaiClosingPhase::Draining),
        ));
        assert!(!rai_drain_frontier_snapshot_needed(
            &mut initialized_epoch,
            closing(0, RaiClosingPhase::Draining),
        ));
        assert_eq!(initialized_epoch, Some(RaiEpoch::ZERO));

        assert!(!rai_drain_frontier_snapshot_needed(
            &mut initialized_epoch,
            closing(1, RaiClosingPhase::ElectingCut),
        ));
        assert!(initialized_epoch.is_none());
        assert!(rai_drain_frontier_snapshot_needed(
            &mut initialized_epoch,
            closing(1, RaiClosingPhase::Draining),
        ));
        assert_eq!(initialized_epoch, Some(RaiEpoch::new(1)));

        assert!(!rai_drain_frontier_snapshot_needed(
            &mut initialized_epoch,
            None,
        ));
        assert!(initialized_epoch.is_none());
    }

    #[test]
    fn close_vote_repair_runs_only_during_an_active_close_election() {
        use crate::consensus::rai::RaiClosingPhase;

        assert!(!rai_close_repair_phase_active(
            RaiClosingPhase::CollectingReports
        ));
        assert!(rai_close_repair_phase_active(RaiClosingPhase::ElectingCut));
        assert!(!rai_close_repair_phase_active(RaiClosingPhase::Draining));
        assert!(rai_close_repair_phase_active(
            RaiClosingPhase::ElectingRecord
        ));
    }

    #[test]
    fn missing_slot_payload_repair_continues_until_the_record_is_decided() {
        use crate::consensus::rai::RaiClosingPhase;

        assert!(!rai_slot_payload_repair_phase_active(
            RaiClosingPhase::CollectingReports
        ));
        assert!(!rai_slot_payload_repair_phase_active(
            RaiClosingPhase::ElectingCut
        ));
        assert!(rai_slot_payload_repair_phase_active(
            RaiClosingPhase::Draining
        ));
        assert!(rai_slot_payload_repair_phase_active(
            RaiClosingPhase::ElectingRecord
        ));
    }

    #[test]
    fn report_epidemic_runs_only_while_reports_can_change_the_cut() {
        use crate::consensus::rai::{RaiClosingEpoch, RaiClosingPhase};
        use rsnano_types::RaiEpoch;

        let closing = |phase| {
            Some(RaiClosingEpoch {
                epoch: RaiEpoch::ZERO,
                phase,
            })
        };
        assert!(!rai_report_epidemic_phase_active(None));
        assert!(rai_report_epidemic_phase_active(closing(
            RaiClosingPhase::CollectingReports
        )));
        assert!(rai_report_epidemic_phase_active(closing(
            RaiClosingPhase::ElectingCut
        )));
        assert!(!rai_report_epidemic_phase_active(closing(
            RaiClosingPhase::Draining
        )));
        assert!(!rai_report_epidemic_phase_active(closing(
            RaiClosingPhase::ElectingRecord
        )));
    }

    #[test]
    fn zero_payload_repair_rechecks_exact_and_root_candidates_in_one_ledger_snapshot() {
        let ledger = rsnano_ledger::Ledger::new_null();
        let known = ledger.genesis();
        let unknown_root = QualifiedRoot::new(BlockHash::from(10).into(), BlockHash::from(11));
        let exact_unknown_root =
            QualifiedRoot::new(BlockHash::from(20).into(), BlockHash::from(21));
        let exact_known_root = QualifiedRoot::new(BlockHash::from(30).into(), BlockHash::from(31));
        let candidates = vec![
            (known.qualified_root(), None),
            (unknown_root.clone(), None),
            (exact_known_root, Some(known.hash())),
            (exact_unknown_root.clone(), Some(BlockHash::from(999_999))),
        ];

        let any = ledger.any();
        let requests = genuinely_missing_slot_payload_requests(&any, candidates);
        let mut expected = vec![
            (BlockHash::ZERO, unknown_root.root),
            (BlockHash::ZERO, exact_unknown_root.root),
        ];
        expected.sort_unstable();

        assert_eq!(requests, expected);
    }
}

impl StatsSource for AecService {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.aec.read().unwrap().collect_stats(result)
    }
}

impl ContainerInfoProvider for AecService {
    fn container_info(&self) -> ContainerInfo {
        self.aec.read().unwrap().container_info()
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use std::{sync::Arc, time::Duration};

    use rsnano_ledger::RepWeights;
    use rsnano_types::{Amount, BlockHash, BlockPriority, PrivateKey, RaiEpoch, SavedBlock};

    use super::*;

    #[test]
    fn batch_vote_lookups_match_scalar_results_with_one_read_lock() {
        let service = AecService::new_null();
        let blocks = [
            SavedBlock::new_test_instance_with_key(10),
            SavedBlock::new_test_instance_with_key(11),
        ];
        for block in &blocks {
            service
                .insert(
                    AecInsertRequest {
                        block: block.clone(),
                        behavior: ElectionBehavior::Priority,
                        priority: BlockPriority::new_test_instance(),
                    },
                    Timestamp::new_test_instance(),
                )
                .unwrap();
        }
        let hashes = vec![
            blocks[0].hash(),
            BlockHash::from(999),
            blocks[1].hash(),
            blocks[0].hash(),
        ];

        let locks_before_batch = service.lookup_read_lock_count();
        let batch_contexts = service.rai_vote_contexts(&hashes);
        assert_eq!(service.lookup_read_lock_count() - locks_before_batch, 1);

        let locks_before_scalar = service.lookup_read_lock_count();
        let scalar_contexts = hashes
            .iter()
            .map(|hash| service.rai_vote_context(hash))
            .collect::<Vec<_>>();
        assert_eq!(
            service.lookup_read_lock_count() - locks_before_scalar,
            hashes.len()
        );
        assert_eq!(batch_contexts, scalar_contexts);
        assert!(batch_contexts[0].is_some());
        assert!(batch_contexts[1].is_none());
        assert_eq!(batch_contexts[0], batch_contexts[3]);

        let locks_before_batch = service.lookup_read_lock_count();
        let any_active = service.any_active_hash(&hashes);
        assert_eq!(service.lookup_read_lock_count() - locks_before_batch, 1);

        let locks_before_scalar = service.lookup_read_lock_count();
        let scalar_membership = hashes
            .iter()
            .map(|hash| service.is_active_hash(hash))
            .collect::<Vec<_>>();
        assert_eq!(
            service.lookup_read_lock_count() - locks_before_scalar,
            hashes.len()
        );
        assert_eq!(
            any_active,
            scalar_membership.into_iter().any(|active| active)
        );
    }

    #[test]
    fn zero_hash_batch_resolves_active_slots_and_duplicates_with_one_read_lock() {
        let service = AecService::new_null();
        let blocks = [
            SavedBlock::new_test_instance_with_key(20),
            SavedBlock::new_test_instance_with_key(21),
        ];
        for block in &blocks {
            service
                .insert(
                    AecInsertRequest {
                        block: block.clone(),
                        behavior: ElectionBehavior::Priority,
                        priority: BlockPriority::new_test_instance(),
                    },
                    Timestamp::new_test_instance(),
                )
                .unwrap();
        }
        let roots = vec![
            blocks[0].root(),
            blocks[1].root(),
            blocks[0].root(),
            BlockHash::from(999).into(),
        ];

        let locks_before = service.lookup_read_lock_count();
        let resolutions = service.rai_zero_hash_vote_requests(&roots);
        assert_eq!(service.lookup_read_lock_count() - locks_before, 1);
        assert_eq!(resolutions.len(), roots.len());
        assert_eq!(resolutions[0], resolutions[2]);
        assert!(resolutions[3].is_none());

        for (resolution, block) in resolutions[..2].iter().zip(&blocks) {
            let Some(crate::consensus::RaiZeroHashVoteRequest::Slot {
                metadata,
                target: Some(target),
            }) = resolution
            else {
                panic!("active slot did not resolve to a vote target");
            };
            assert_eq!(metadata.epoch, RaiEpoch::ZERO);
            assert_eq!(target.hash, block.hash());
            assert_eq!(target.root, block.root());
            assert_eq!(target.metadata, *metadata);
        }
    }

    #[test]
    fn unseen_report_snapshot_preserves_order_cap_and_deduplication_state() {
        use crate::consensus::rai::RaiReport;

        let reports = (1..=3)
            .map(|key| RaiReport::new(&PrivateKey::from(key), RaiEpoch::ZERO, []))
            .collect::<Vec<_>>();
        let service = AecService::new_null();
        for report in &reports {
            service.rai_report_received(report.clone());
        }
        let mut expected = reports.clone();
        expected.sort_by_key(|report| (report.reporter, report.chunk_index));
        let mut known = HashSet::new();

        let bounded = service.rai_new_reports_for_epoch(RaiEpoch::ZERO, &mut known, 1);
        assert_eq!(bounded, expected[..1]);
        // Preserve the former queue-cap behavior: identities which did not fit
        // are still known and are recovered, if needed, by explicit repair.
        assert_eq!(known.len(), reports.len());
        assert!(
            service
                .rai_new_reports_for_epoch(RaiEpoch::ZERO, &mut known, usize::MAX)
                .is_empty()
        );
    }

    #[test]
    fn unseen_report_snapshot_is_epoch_scoped_and_observes_a_later_chunk() {
        use crate::consensus::rai::{MAX_REPORT_CHUNK_OBLIGATIONS, RaiReport};

        let epoch = RaiEpoch::new(7);
        let key = PrivateKey::from(1);
        let chunks = RaiReport::new_chunks(
            &key,
            epoch,
            (0..=MAX_REPORT_CHUNK_OBLIGATIONS as u64).map(|value| {
                crate::consensus::election::RaiSlotId {
                    epoch,
                    root: QualifiedRoot::new(value.into(), (value + 1).into()),
                }
            }),
        );
        assert_eq!(chunks.len(), 2);
        let other_epoch = RaiReport::new(&PrivateKey::from(2), RaiEpoch::new(8), []);
        let service = AecService::new_null();
        service.rai_report_received(chunks[0].clone());
        service.rai_report_received(other_epoch.clone());

        let mut known = HashSet::new();
        assert_eq!(
            service.rai_new_reports_for_epoch(epoch, &mut known, usize::MAX),
            vec![chunks[0].clone()]
        );
        assert!(
            service
                .rai_new_reports_for_epoch(epoch, &mut known, usize::MAX)
                .is_empty()
        );

        service.rai_report_received(chunks[1].clone());
        assert_eq!(
            service.rai_new_reports_for_epoch(epoch, &mut known, usize::MAX),
            vec![chunks[1].clone()]
        );
        assert_eq!(
            service.rai_new_reports_for_epoch(RaiEpoch::new(8), &mut known, usize::MAX),
            vec![other_epoch]
        );
    }

    #[test]
    fn live_tick_opens_epoch_one_at_the_deadline() {
        let key = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(key.public_key(), Amount::raw(1));
        let service = AecService::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_millis(25),
            Arc::new(weights),
            BlockHash::ZERO,
            Arc::new(rsnano_ledger::Ledger::new_null()),
        );
        let duration = Duration::from_secs(30);
        let start = service.clock.now();

        service.rai_tick(start, &key, duration);
        assert_eq!(service.rai_epoch_status().0.open_epoch, RaiEpoch::ZERO);

        service.rai_tick(start + duration, &key, duration);
        let state = service.rai_epoch_status().0;
        assert_eq!(state.open_epoch, RaiEpoch::new(1));
        assert_eq!(state.closing.unwrap().epoch, RaiEpoch::ZERO);
    }

    #[test]
    fn received_report_quorum_starts_the_live_cut_election() {
        use crate::consensus::rai::{RaiClosingPhase, RaiReport, rai_close_cut_root};

        let keys = (1..=4).map(PrivateKey::from).collect::<Vec<_>>();
        let mut weights = RepWeights::default();
        for key in &keys {
            weights.put(key.public_key(), Amount::raw(1));
        }
        let service = AecService::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_millis(25),
            Arc::new(weights),
            BlockHash::ZERO,
            Arc::new(rsnano_ledger::Ledger::new_null()),
        );
        let duration = Duration::from_secs(30);
        let deadline = service.clock.now() + duration;

        let reports = service.rai_tick(deadline, &keys[0], duration);
        assert_eq!(reports.len(), 1);
        assert_eq!(
            service.rai_epoch_status().0.closing.unwrap().phase,
            RaiClosingPhase::CollectingReports
        );
        assert!(!service.is_active_root(&rai_close_cut_root(RaiEpoch::ZERO, 0)));

        service.rai_report_received(RaiReport::new(&keys[1], RaiEpoch::ZERO, []));
        service.rai_report_received(RaiReport::new(&keys[2], RaiEpoch::ZERO, []));
        assert_eq!(service.rai_reports().len(), 3);
        assert_eq!(
            service.rai_epoch_status().0.closing.unwrap().phase,
            RaiClosingPhase::CollectingReports
        );
        service.rai_report_received(RaiReport::new(&keys[3], RaiEpoch::ZERO, []));
        service.rai_tick(deadline + Duration::from_millis(1), &keys[0], duration);

        assert_eq!(
            service.rai_epoch_status().0.closing.unwrap().phase,
            RaiClosingPhase::ElectingCut
        );
        assert!(service.is_active_root(&rai_close_cut_root(RaiEpoch::ZERO, 0)));
    }
}

#[derive(Default)]
pub struct AecSnapshot {
    pub buckets: Vec<BucketSnapshot>,
}

pub struct BucketSnapshot {
    pub bucket_index: usize,
    pub election_count: usize,
    pub elections: Vec<ElectionSnapshot>,
}

pub struct ElectionSnapshot {
    pub winner_hash: BlockHash,
    pub non_final_tally: Amount,
    pub final_tally: Amount,
    pub root: QualifiedRoot,
    pub account: Account,
    pub state: ElectionState,
    pub candidate_blocks: Vec<BlockHash>,
    pub is_final: bool,
    pub elapsed: Duration,
}
