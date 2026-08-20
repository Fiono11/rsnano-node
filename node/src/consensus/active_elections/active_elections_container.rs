use std::{cmp::max, collections::HashMap, time::Duration};

#[cfg(feature = "rai_protocol")]
use std::sync::{Arc, atomic::AtomicBool};

use strum::EnumCount;

use rsnano_ledger::RepWeights;
#[cfg(feature = "rai_protocol")]
use rsnano_ledger::{AnySet, CementingObserver, Ledger};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Amount, Block, BlockHash, PublicKey, QualifiedRoot, SavedBlock, TimePriority, VoteError,
};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};

#[cfg(feature = "rai_protocol")]
#[derive(Clone)]
struct RaiTerminalSlot {
    outcome: crate::consensus::rai::RaiOutcome,
    account: rsnano_types::Account,
    frontier: Option<rsnano_types::ConfirmationHeightInfo>,
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone)]
struct RaiPendingVoteLeaf {
    voter: PublicKey,
    timestamp: rsnano_types::UnixMillisTimestamp,
    metadata: rsnano_types::RaiVoteMetadata,
    hash: BlockHash,
}

/// Resolution of a legacy-wire `(ZERO, root)` ConfirmReq in RAI mode.
/// Keeping the optional active target beside its context lets the request
/// aggregator resolve a whole wire batch under one AEC read guard.
#[cfg(feature = "rai_protocol")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RaiZeroHashVoteRequest {
    Close {
        metadata: rsnano_types::RaiVoteMetadata,
        target: Option<rsnano_ledger::RaiFinalizedVoteTarget>,
    },
    Slot {
        metadata: rsnano_types::RaiVoteMetadata,
        target: Option<rsnano_ledger::RaiFinalizedVoteTarget>,
    },
}

#[cfg(feature = "rai_protocol")]
impl RaiTerminalSlot {
    fn hashes(&self) -> impl Iterator<Item = BlockHash> {
        let outcome = match self.outcome {
            crate::consensus::rai::RaiOutcome::Notarized(hash)
            | crate::consensus::rai::RaiOutcome::Confirmed(hash) => Some(hash),
            crate::consensus::rai::RaiOutcome::Pending
            | crate::consensus::rai::RaiOutcome::TimedOut => None,
        };
        let frontier = self.frontier.as_ref().map(|info| info.frontier);
        [outcome, frontier].into_iter().flatten()
    }
}

#[cfg(feature = "rai_protocol")]
// The whole slice shares one ledger read snapshot. Keeping this large lets a
// saturated epoch drain promptly without paying one transaction acquisition
// per slot while the AEC write lock is held.
const RAI_DRAIN_CHECKS_PER_TICK: usize = 256;

/// Late payload/evidence repair may refine the fresh close-record frontier
/// after round zero has already started. Keep that replay bounded under the
/// AEC writer just like the initial drain worklist.
#[cfg(feature = "rai_protocol")]
const RAI_CLOSE_RECORD_REFRESHES_PER_PASS: usize = 256;

/// Keep close-ledger installation small enough that a busy block processor
/// cannot hide all progress behind one long cementation and callback batch.
/// The close remains unpublished until every window is durable.
#[cfg(feature = "rai_protocol")]
const RAI_CLOSE_CEMENT_ROOTS_PER_PASS: usize = 64;

/// A dead close round must remain addressable long enough for its signed
/// candidate leaves to drive several preimage-repair passes. Without this
/// gate, a 100 ms epoch tick can create successor rounds as fast as the repair
/// loop discovers the data which caused the split.
#[cfg(feature = "rai_protocol")]
const RAI_CLOSE_DATA_REPAIR_GRACE: Duration = Duration::from_millis(300);

/// Missing losing-candidate preimages are useful for reconstructing the split,
/// but they must not turn a signed timeout certificate into a permanent
/// liveness dependency. Give the repair exchange several lifecycle passes,
/// then let the timeout certificate advance the logical close round.
#[cfg(feature = "rai_protocol")]
const RAI_CLOSE_PREIMAGE_REPAIR_LIMIT: Duration = Duration::from_secs(2);

/// Evidence repair only performs in-memory membership checks, so it can
/// cheaply discard a large settled prefix without inheriting the much smaller
/// ledger-read budget. This reaches a live tail behind a 25K cut in at most
/// four 500 ms repair passes while keeping each pass strictly bounded.
#[cfg(feature = "rai_protocol")]
const RAI_EVIDENCE_REPAIR_SCANS_PER_PASS: usize = 8 * 1024;

/// Persistent close-drain worklist. While certificates are incomplete it
/// rotates only unresolved slots. Once every slot has a certificate outcome,
/// it performs exactly one fresh, bounded durable-upgrade sweep before the
/// close record may be created.
#[cfg(feature = "rai_protocol")]
#[derive(Debug)]
struct RaiDrainCheckSchedule {
    epoch: rsnano_types::RaiEpoch,
    pending: std::collections::VecDeque<crate::consensus::election::RaiSlotId>,
    evidence_repair_pending: std::collections::VecDeque<crate::consensus::election::RaiSlotId>,
    durable_upgrade: bool,
}

#[cfg(feature = "rai_protocol")]
#[derive(Debug, Default)]
struct RaiDrainDiagnostics {
    passes: u64,
    checks: u64,
    last_checks: usize,
    max_checks: usize,
    ledger_snapshot_wait_us: u64,
    max_ledger_snapshot_wait_us: u64,
    last_pass_us: u64,
    max_pass_us: u64,
    slot_state: std::collections::BTreeMap<crate::consensus::election::RaiSlotId, &'static str>,
}

#[cfg(feature = "rai_protocol")]
impl RaiDrainCheckSchedule {
    fn resolving(
        epoch: rsnano_types::RaiEpoch,
        unresolved: impl IntoIterator<Item = crate::consensus::election::RaiSlotId>,
    ) -> Self {
        let pending = unresolved
            .into_iter()
            .collect::<std::collections::VecDeque<_>>();
        Self {
            epoch,
            evidence_repair_pending: pending.clone(),
            pending,
            durable_upgrade: false,
        }
    }

    fn take_window(&mut self, limit: usize) -> Vec<crate::consensus::election::RaiSlotId> {
        let count = limit.min(self.pending.len());
        self.pending.drain(..count).collect()
    }

    fn is_for_epoch(&self, epoch: rsnano_types::RaiEpoch) -> bool {
        self.epoch == epoch
    }

    fn requeue_unresolved(&mut self, slot: crate::consensus::election::RaiSlotId) {
        debug_assert!(!self.durable_upgrade);
        self.pending.push_back(slot);
    }

    fn take_evidence_repair_window(
        &mut self,
        limit: usize,
        mut is_unresolved: impl FnMut(&crate::consensus::election::RaiSlotId) -> bool,
    ) -> Vec<crate::consensus::election::RaiSlotId> {
        if self.durable_upgrade {
            return Vec::new();
        }
        let scan_count = RAI_EVIDENCE_REPAIR_SCANS_PER_PASS.min(self.evidence_repair_pending.len());
        let mut result = Vec::with_capacity(limit.min(scan_count));
        for _ in 0..scan_count {
            let slot = self.evidence_repair_pending.pop_front().unwrap();
            if is_unresolved(&slot) {
                result.push(slot.clone());
                self.evidence_repair_pending.push_back(slot);
                if result.len() == limit {
                    break;
                }
            }
        }
        result
    }

    fn begin_durable_upgrade(
        &mut self,
        candidates: impl IntoIterator<Item = crate::consensus::election::RaiSlotId>,
    ) {
        self.pending = candidates.into_iter().collect();
        self.evidence_repair_pending.clear();
        self.durable_upgrade = true;
    }

    fn ready_for_close(&self) -> bool {
        self.durable_upgrade && self.pending.is_empty()
    }
}

#[cfg(feature = "rai_protocol")]
#[derive(Default)]
struct RaiCloseCementingObserver {
    failed: bool,
}

#[cfg(feature = "rai_protocol")]
impl CementingObserver for RaiCloseCementingObserver {
    fn already_confirmed(&mut self, _hash: &BlockHash) {}

    fn cementing_failed(&mut self, _hash: &BlockHash) {
        self.failed = true;
    }
}

#[cfg(feature = "rai_protocol")]
fn rai_close_install_diagnostic_window(
    frontiers: &crate::consensus::rai::RaiFrontierMap,
    after: Option<rsnano_types::Account>,
    max_entries: usize,
) -> (crate::consensus::rai::RaiFrontierMap, bool) {
    assert!(max_entries > 0);
    let start = after
        .map(std::ops::Bound::Excluded)
        .unwrap_or(std::ops::Bound::Unbounded);
    let mut entries = frontiers
        .range((start, std::ops::Bound::Unbounded))
        .take(max_entries + 1)
        .map(|(account, frontier)| (*account, frontier.clone()))
        .collect::<Vec<_>>();
    let truncated = entries.len() > max_entries;
    entries.truncate(max_entries);
    (entries.into_iter().collect(), truncated)
}

use crate::{
    consensus::{
        AecSnapshot, ElectionCandidateSource,
        election::{
            AddForkResult, ConfirmationType, ConfirmedElection, Election, ElectionBehavior,
        },
        election_schedulers::priority::bucket_count,
        filtered_vote::FilteredVote,
    },
    representatives::QuorumSnapshot,
};

use super::{
    ActiveElectionsConfig, ActiveElectionsInfo, AecFact, AecInsertError, AecInsertRequest, Entry,
    RootContainer,
    apply_vote_helper::ApplyVoteHelper,
    cooldown_controller::{AecCooldownReason, CooldownController, CooldownResult},
    recently_confirmed_cache::RecentlyConfirmedCache,
    stats::AecStats,
};

pub(crate) struct ActiveElectionsContainer {
    roots: RootContainer,
    observer: Option<Sender<AecFact>>,
    stopped: bool,
    count_by_behavior: [usize; ElectionBehavior::COUNT],
    base_latency: Duration,
    recently_confirmed: RecentlyConfirmedCache,
    cooldown: CooldownController,
    max_elections: usize,
    max_elections_per_bucket: usize,
    #[cfg(feature = "rai_protocol")]
    retry_released_slots: bool,
    stats: AecStats,
    #[cfg(feature = "rai_protocol")]
    rai_epoch_manager: crate::consensus::rai::RaiEpochManager,
    #[cfg(feature = "rai_protocol")]
    rai_drain_check_schedule: Option<RaiDrainCheckSchedule>,
    #[cfg(feature = "rai_protocol")]
    rai_drain_diagnostics: RaiDrainDiagnostics,
    #[cfg(feature = "rai_protocol")]
    rai_visible_obligations: std::collections::BTreeMap<
        rsnano_types::RaiEpoch,
        std::collections::BTreeSet<crate::consensus::election::RaiSlotId>,
    >,
    /// Process-lifetime attribution used by bounded close diagnostics to
    /// distinguish slots activated locally from obligations learned only via
    /// the certified union cut.
    #[cfg(feature = "rai_protocol")]
    rai_locally_inserted_slots: std::collections::BTreeSet<crate::consensus::election::RaiSlotId>,
    /// Earliest locally visible epoch for each qualified root. This preserves
    /// the retry policy without searching all visible slots on every insert.
    #[cfg(feature = "rai_protocol")]
    rai_first_visible_epoch_by_root: HashMap<QualifiedRoot, rsnano_types::RaiEpoch>,
    #[cfg(feature = "rai_protocol")]
    rai_terminal_slots:
        std::collections::BTreeMap<crate::consensus::election::RaiSlotId, RaiTerminalSlot>,
    /// Terminal slot lookup by either certified outcome or persisted frontier.
    /// One hash may be retained by multiple epoch-qualified retry slots.
    #[cfg(feature = "rai_protocol")]
    rai_terminal_slots_by_hash:
        HashMap<BlockHash, std::collections::BTreeSet<crate::consensus::election::RaiSlotId>>,
    /// Terminal lookup for the unqualified root carried by ConfirmReq.
    #[cfg(feature = "rai_protocol")]
    rai_terminal_slots_by_request_root: HashMap<
        rsnano_types::Root,
        std::collections::BTreeSet<crate::consensus::election::RaiSlotId>,
    >,
    /// Validated block data is deliberately independent of election state.
    /// The second map is only a discovery index; neither map classifies a
    /// block into an epoch.
    #[cfg(feature = "rai_protocol")]
    rai_blocks: HashMap<BlockHash, Block>,
    #[cfg(feature = "rai_protocol")]
    rai_blocks_by_qualified_root: HashMap<QualifiedRoot, std::collections::BTreeSet<BlockHash>>,
    /// Epoch-qualified references whose payload has not arrived yet.
    #[cfg(feature = "rai_protocol")]
    rai_payload_incomplete:
        HashMap<crate::consensus::election::RaiSlotId, std::collections::HashSet<BlockHash>>,
    /// Reverse lookup used by Publish delivery. A payload arrival should wake
    /// only authenticated references to that exact digest, never scan every
    /// incomplete slot under the AEC writer.
    #[cfg(feature = "rai_protocol")]
    rai_payload_incomplete_by_hash:
        HashMap<BlockHash, std::collections::BTreeSet<crate::consensus::election::RaiSlotId>>,
    /// Cut obligations which the bounded drain scheduler has inspected but
    /// cannot yet reconstruct because no payload is locally available. Exact
    /// missing candidate hashes remain in `rai_payload_incomplete`; this set
    /// represents the hash-unknown cut case.
    #[cfg(feature = "rai_protocol")]
    rai_missing_drain_payloads: std::collections::BTreeSet<crate::consensus::election::RaiSlotId>,
    /// Reverse lookup for the genuinely hash-unknown drain case. Qualified
    /// roots, rather than their root field alone, preserve fork/base safety.
    #[cfg(feature = "rai_protocol")]
    rai_missing_drain_payloads_by_root:
        HashMap<QualifiedRoot, std::collections::BTreeSet<crate::consensus::election::RaiSlotId>>,
    /// Certificate-resolved slots whose exact selected payload must be
    /// replayed before constructing another fresh close-record candidate.
    /// Entries are added by late payload/terminal events, so record retries do
    /// not scan every frozen obligation under the AEC writer.
    #[cfg(feature = "rai_protocol")]
    rai_close_record_refresh_slots:
        std::collections::BTreeSet<crate::consensus::election::RaiSlotId>,
    #[cfg(feature = "rai_protocol")]
    rai_candidate_hashes:
        HashMap<crate::consensus::election::RaiSlotId, std::collections::HashSet<BlockHash>>,
    /// Process-lifetime vote evidence, including votes received before an
    /// election starts and votes for elections evicted from the active set.
    #[cfg(feature = "rai_protocol")]
    rai_pending_votes:
        HashMap<crate::consensus::election::RaiElectionId, Vec<Arc<rsnano_types::Vote>>>,
    /// Leaves extracted once from signed vector transports. Replay for one
    /// election must not rescan every leaf in every retained batch.
    #[cfg(feature = "rai_protocol")]
    rai_pending_vote_leaves:
        HashMap<crate::consensus::election::RaiElectionId, Vec<RaiPendingVoteLeaf>>,
    #[cfg(feature = "rai_protocol")]
    rai_pending_vote_replay_complete:
        std::collections::HashSet<crate::consensus::election::RaiElectionId>,
    /// Compact slot votes intentionally omit the qualified root. If their
    /// block has not arrived yet, retain the signed transport by epoch/hash
    /// and resolve its election identity when Publish supplies the block.
    #[cfg(feature = "rai_protocol")]
    rai_pending_compact_slot_votes:
        HashMap<(rsnano_types::RaiEpoch, BlockHash), Vec<Arc<rsnano_types::Vote>>>,
    #[cfg(feature = "rai_protocol")]
    rai_pending_timeout_slot_votes: HashMap<
        (rsnano_types::RaiEpoch, rsnano_types::RaiTimeoutSlot),
        Vec<Arc<rsnano_types::Vote>>,
    >,
    /// Exact close candidate lookup for serving vote repair after an election
    /// has been removed. Slot-vote traffic can be much larger and must not be
    /// searched for every requested hash.
    #[cfg(feature = "rai_protocol")]
    rai_pending_close_contexts_by_hash: HashMap<BlockHash, Vec<rsnano_types::RaiVoteMetadata>>,
    /// The live ledger is mandatory for publishing a close decision. Direct
    /// container unit tests omit it and use the in-memory state machine only.
    #[cfg(feature = "rai_protocol")]
    rai_ledger: Option<Arc<Ledger>>,
    /// Last committed account in each close record. Close retries resume here
    /// rather than walking the already durable prefix under the AEC writer.
    #[cfg(feature = "rai_protocol")]
    rai_close_commit_cursors:
        std::collections::BTreeMap<rsnano_types::RaiEpoch, rsnano_types::Account>,
    #[cfg(feature = "rai_protocol")]
    rai_pending_close_commits: std::collections::BTreeMap<
        rsnano_types::RaiEpoch,
        (BlockHash, Arc<crate::consensus::rai::RaiFrontierMap>),
    >,
    #[cfg(feature = "rai_protocol")]
    rai_completed_close_commits: std::collections::BTreeSet<(rsnano_types::RaiEpoch, BlockHash)>,
    #[cfg(feature = "rai_protocol")]
    rai_cut_election_durations: std::collections::BTreeMap<rsnano_types::RaiEpoch, Duration>,
    #[cfg(feature = "rai_protocol")]
    rai_record_election_durations: std::collections::BTreeMap<rsnano_types::RaiEpoch, Duration>,
    #[cfg(feature = "rai_protocol")]
    rai_close_election_starts:
        std::collections::BTreeMap<crate::consensus::election::RaiElectionId, Timestamp>,
    /// First time this node observed a close round reach notarization. The
    /// final-vote window starts here, not when the election was created.
    #[cfg(feature = "rai_protocol")]
    rai_close_notarized_at:
        std::collections::BTreeMap<crate::consensus::election::RaiElectionId, Timestamp>,
}

impl ActiveElectionsContainer {
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_election_vote_enabled(
        &self,
        id: &crate::consensus::election::RaiElectionId,
    ) -> bool {
        match id {
            crate::consensus::election::RaiElectionId::Slot(slot) => self
                .rai_epoch_manager
                .slot_election_enabled(slot.epoch, &slot.root),
            crate::consensus::election::RaiElectionId::CloseCut { .. }
            | crate::consensus::election::RaiElectionId::CloseRecord { .. } => true,
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_mark_missing_drain_payload(&mut self, slot: crate::consensus::election::RaiSlotId) {
        if self.rai_missing_drain_payloads.insert(slot.clone()) {
            self.rai_missing_drain_payloads_by_root
                .entry(slot.root.clone())
                .or_default()
                .insert(slot);
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_clear_missing_drain_payload(
        &mut self,
        slot: &crate::consensus::election::RaiSlotId,
    ) -> bool {
        if !self.rai_missing_drain_payloads.remove(slot) {
            return false;
        }
        let remove_root = self
            .rai_missing_drain_payloads_by_root
            .get_mut(&slot.root)
            .is_some_and(|slots| {
                slots.remove(slot);
                slots.is_empty()
            });
        if remove_root {
            self.rai_missing_drain_payloads_by_root.remove(&slot.root);
        }
        true
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_mark_payload_incomplete(
        &mut self,
        slot: crate::consensus::election::RaiSlotId,
        hash: BlockHash,
    ) {
        if self
            .rai_payload_incomplete
            .entry(slot.clone())
            .or_default()
            .insert(hash)
        {
            self.rai_payload_incomplete_by_hash
                .entry(hash)
                .or_default()
                .insert(slot);
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_clear_payload_incomplete_hash(
        &mut self,
        slot: &crate::consensus::election::RaiSlotId,
        hash: &BlockHash,
    ) -> bool {
        let (removed, remove_slot) = self
            .rai_payload_incomplete
            .get_mut(slot)
            .map(|hashes| {
                let removed = hashes.remove(hash);
                (removed, hashes.is_empty())
            })
            .unwrap_or_default();
        if !removed {
            return false;
        }
        if remove_slot {
            self.rai_payload_incomplete.remove(slot);
        }
        let remove_hash = self
            .rai_payload_incomplete_by_hash
            .get_mut(hash)
            .is_some_and(|slots| {
                slots.remove(slot);
                slots.is_empty()
            });
        if remove_hash {
            self.rai_payload_incomplete_by_hash.remove(hash);
        }
        true
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_clear_payload_incomplete_slot(
        &mut self,
        slot: &crate::consensus::election::RaiSlotId,
    ) -> bool {
        let Some(hashes) = self.rai_payload_incomplete.remove(slot) else {
            return false;
        };
        for hash in hashes {
            let remove_hash = self
                .rai_payload_incomplete_by_hash
                .get_mut(&hash)
                .is_some_and(|slots| {
                    slots.remove(slot);
                    slots.is_empty()
                });
            if remove_hash {
                self.rai_payload_incomplete_by_hash.remove(&hash);
            }
        }
        true
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_prune_payload_tracking_through(&mut self, closed_epoch: rsnano_types::RaiEpoch) {
        let root_only = self
            .rai_missing_drain_payloads
            .iter()
            .take_while(|slot| slot.epoch <= closed_epoch)
            .cloned()
            .collect::<Vec<_>>();
        for slot in root_only {
            self.rai_clear_missing_drain_payload(&slot);
        }
        let exact = self
            .rai_payload_incomplete
            .keys()
            .filter(|slot| slot.epoch <= closed_epoch)
            .cloned()
            .collect::<Vec<_>>();
        for slot in exact {
            self.rai_clear_payload_incomplete_slot(&slot);
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_clear_payload_tracking(&mut self) {
        self.rai_missing_drain_payloads.clear();
        self.rai_missing_drain_payloads_by_root.clear();
        self.rai_payload_incomplete.clear();
        self.rai_payload_incomplete_by_hash.clear();
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_retain_pending_vote(
        &mut self,
        election_id: crate::consensus::election::RaiElectionId,
        vote: Arc<rsnano_types::Vote>,
    ) {
        let added = {
            let retained = self
                .rai_pending_votes
                .entry(election_id.clone())
                .or_default();
            if retained.iter().any(|existing| {
                existing.voter == vote.voter && existing.signature == vote.signature
            }) {
                false
            } else {
                retained.push(vote.clone());
                true
            }
        };
        if !added {
            return;
        }
        self.rai_pending_vote_replay_complete.remove(&election_id);
        self.rai_pending_vote_leaves
            .entry(election_id.clone())
            .or_default()
            .extend(
                vote.rai_entries()
                    .filter(|(metadata, _)| metadata.election_id == election_id)
                    .map(|(metadata, hash)| RaiPendingVoteLeaf {
                        voter: vote.voter,
                        timestamp: vote.timestamp(),
                        metadata: metadata.clone(),
                        hash: *hash,
                    }),
            );
        if !matches!(
            election_id,
            crate::consensus::election::RaiElectionId::CloseCut { .. }
                | crate::consensus::election::RaiElectionId::CloseRecord { .. }
        ) {
            return;
        }
        for (metadata, hash) in vote
            .rai_entries()
            .filter(|(metadata, _)| metadata.election_id == election_id)
        {
            let contexts = self
                .rai_pending_close_contexts_by_hash
                .entry(*hash)
                .or_default();
            if !contexts.contains(metadata) {
                contexts.push(metadata.clone());
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_resolve_pending_timeout_votes(
        &mut self,
        election_id: &crate::consensus::election::RaiElectionId,
    ) {
        let crate::consensus::election::RaiElectionId::Slot(slot) = election_id else {
            return;
        };
        let Some(election) = self.roots.election_for_rai_id(election_id) else {
            return;
        };
        let locator = rsnano_types::RaiTimeoutSlot {
            account: election.account(),
            height: election.rai_slot_height(),
        };
        let Some(votes) = self
            .rai_pending_timeout_slot_votes
            .remove(&(slot.epoch, locator))
        else {
            return;
        };
        for vote in votes {
            let mut resolved = (*vote).clone();
            for index in 0..resolved.len() {
                if resolved.rai_timeout_slot(index) == Some(locator)
                    && resolved.rai_metadata(index).unwrap().epoch == slot.epoch
                {
                    let target = resolved.rai_timeout_slot(index);
                    if target == Some(locator) {
                        resolved.rai_entries_mut().nth(index).unwrap().0.election_id =
                            election_id.clone();
                    }
                }
            }
            self.rai_retain_pending_vote(election_id.clone(), Arc::new(resolved));
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(super) fn commit_rai_close_frontiers(
        ledger: Option<&Ledger>,
        epoch: rsnano_types::RaiEpoch,
        frontiers: &crate::consensus::rai::RaiFrontierMap,
        cursors: &mut std::collections::BTreeMap<rsnano_types::RaiEpoch, rsnano_types::Account>,
    ) -> bool {
        let Some(ledger) = ledger else {
            debug_assert!(cfg!(test), "live RAI close installation requires a ledger");
            return cfg!(test);
        };
        // Installing a close is retried on every lifecycle tick until the
        // callback succeeds. Advance a large frontier map in bounded passes so
        // ledger work does not monopolize the global AEC write lock for the
        // entire close record.
        // Ledger installation runs after releasing the AEC lock, but retaining
        // bounded passes keeps one lagging replica from monopolizing its epoch
        // ticker while it catches up thousands of certified frontiers.
        const CLOSE_CEMENT_BLOCKS_PER_TXN: usize = 512;
        let pending = ledger.rai_uncommitted_close_frontiers_after(
            epoch,
            frontiers,
            cursors.get(&epoch).copied(),
            RAI_CLOSE_CEMENT_ROOTS_PER_PASS,
        );
        if pending.is_empty() {
            cursors.remove(&epoch);
            return true;
        }

        // A gap in a later account must not prevent an earlier complete
        // window from advancing the durable cursor. Preflight exactly the
        // roots this pass will cement; the later window will request its own
        // missing dependency when it reaches the cursor.
        let pending_frontiers = pending
            .iter()
            .map(|(account, _)| (*account, frontiers[account].clone()))
            .collect();
        // Do not monopolize the AEC writer retrying confirmation paths which
        // cannot succeed yet. The ticker requests these exact dependency
        // hashes on the bounded payload-repair lane; yielding here lets the
        // block processor deliver those replies before the next attempt.
        if !ledger
            .rai_missing_close_dependencies(epoch, &pending_frontiers, 1)
            .is_empty()
        {
            return false;
        }

        let stopped = AtomicBool::new(false);
        let mut observer = RaiCloseCementingObserver::default();
        ledger.confirm_batch_rai(
            pending.iter().map(|(_, frontier)| (frontier, Some(epoch))),
            &stopped,
            CLOSE_CEMENT_BLOCKS_PER_TXN,
            &mut observer,
        );
        if observer.failed {
            // A failed root can become commit-ready later; restart so it is
            // not permanently skipped by the monotonic cursor.
            cursors.remove(&epoch);
            return false;
        }
        let mut last_committed = cursors.get(&epoch).copied();
        for (account, hash) in &pending {
            let frontier = &frontiers[account];
            debug_assert_eq!(frontier.frontier, *hash);
            if !ledger.rai_close_frontier_is_committed(epoch, account, frontier) {
                match last_committed {
                    Some(account) => {
                        cursors.insert(epoch, account);
                    }
                    None => {
                        cursors.remove(&epoch);
                    }
                }
                return false;
            }
            last_committed = Some(*account);
        }
        cursors.insert(epoch, last_committed.unwrap());
        let complete = ledger
            .rai_uncommitted_close_frontiers_after(
                epoch,
                frontiers,
                cursors.get(&epoch).copied(),
                1,
            )
            .is_empty();
        if complete {
            cursors.remove(&epoch);
        }
        complete
    }

    #[cfg(feature = "rai_protocol")]
    fn install_close_record_with_commit(
        &mut self,
        epoch: rsnano_types::RaiEpoch,
        round: u32,
        hash: BlockHash,
        certified_weights: Option<RepWeights>,
    ) -> Result<
        crate::consensus::rai::RaiFrontierMap,
        crate::consensus::rai::CloseRecordDecisionError,
    > {
        let weights = match certified_weights {
            Some(weights) => weights,
            None => self
                .rai_epoch_manager
                .close_committee(epoch)
                .ok_or(crate::consensus::rai::CloseRecordDecisionError::MissingPreimage)?
                .as_ref()
                .clone(),
        };
        let ledger = self.rai_ledger.clone();
        let ledger_commit_complete =
            ledger.is_none() || self.rai_completed_close_commits.contains(&(epoch, hash));
        let mut pending_frontiers = None;
        let result = self
            .rai_epoch_manager
            .install_certified_close_record_after(epoch, round, hash, weights, |_, frontiers| {
                if ledger_commit_complete {
                    true
                } else {
                    pending_frontiers = Some(Arc::new(frontiers.clone()));
                    false
                }
            })
            .cloned();
        if let Some(frontiers) = pending_frontiers {
            self.rai_pending_close_commits
                .entry(epoch)
                .or_insert((hash, frontiers));
        }
        if result.is_ok() {
            self.rai_pending_close_commits.remove(&epoch);
            self.rai_close_commit_cursors.remove(&epoch);
            self.rai_completed_close_commits.remove(&(epoch, hash));
        }
        result
    }

    #[cfg(feature = "rai_protocol")]
    pub(super) fn rai_pending_close_commit(
        &self,
    ) -> Option<(
        rsnano_types::RaiEpoch,
        BlockHash,
        Arc<crate::consensus::rai::RaiFrontierMap>,
        Option<rsnano_types::Account>,
    )> {
        self.rai_pending_close_commits
            .iter()
            .next()
            .map(|(epoch, (hash, frontiers))| {
                (
                    *epoch,
                    *hash,
                    frontiers.clone(),
                    self.rai_close_commit_cursors.get(epoch).copied(),
                )
            })
    }

    #[cfg(feature = "rai_protocol")]
    pub(super) fn rai_close_commit_pass_finished(
        &mut self,
        epoch: rsnano_types::RaiEpoch,
        hash: BlockHash,
        complete: bool,
        cursor: Option<rsnano_types::Account>,
    ) {
        if self
            .rai_pending_close_commits
            .get(&epoch)
            .is_none_or(|(pending_hash, _)| *pending_hash != hash)
        {
            return;
        }
        match cursor {
            Some(account) => {
                self.rai_close_commit_cursors.insert(epoch, account);
            }
            None => {
                self.rai_close_commit_cursors.remove(&epoch);
            }
        }
        if complete {
            self.rai_completed_close_commits.insert((epoch, hash));
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_replay_frontier(
        &self,
        hash: BlockHash,
        root: &QualifiedRoot,
        ledger: &rsnano_ledger::Ledger,
    ) -> Option<(rsnano_types::Account, rsnano_types::ConfirmationHeightInfo)> {
        let any = ledger.any();
        self.rai_replay_frontier_from(hash, root, &any)
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_replay_frontier_from(
        &self,
        hash: BlockHash,
        root: &QualifiedRoot,
        any: &rsnano_ledger::OwningAnySet<'_>,
    ) -> Option<(rsnano_types::Account, rsnano_types::ConfirmationHeightInfo)> {
        if let Some(saved) = any.get_block(&hash) {
            return (saved.qualified_root() == *root).then(|| {
                (
                    saved.account(),
                    rsnano_types::ConfirmationHeightInfo::new(saved.height(), hash),
                )
            });
        }
        let block = self.rai_blocks.get(&hash)?;
        if block.qualified_root() != *root {
            return None;
        }
        let previous = block.previous();
        let predecessor = (!previous.is_zero())
            .then(|| any.get_block(&previous))
            .flatten();
        let account = block
            .account_field()
            .or_else(|| predecessor.as_ref().map(|saved| saved.account()))?;
        let height = predecessor.map_or(1, |saved| saved.height().saturating_add(1));
        Some((
            account,
            rsnano_types::ConfirmationHeightInfo::new(height, hash),
        ))
    }

    /// Replays only slots made dirty by late authenticated evidence or block
    /// delivery. A received close-record map is never consulted here: the
    /// expected hash comes from this replica's certificate-derived drain and
    /// the frontier comes from its validated block/ledger state.
    ///
    /// Returning false defers a fresh successor round. The next lifecycle
    /// pass continues from the persistent set, bounded under the AEC writer.
    #[cfg(feature = "rai_protocol")]
    fn rai_refresh_close_record_frontiers(
        &mut self,
        epoch: rsnano_types::RaiEpoch,
        ledger: &rsnano_ledger::Ledger,
    ) -> bool {
        let slots = self
            .rai_close_record_refresh_slots
            .iter()
            .skip_while(|slot| slot.epoch < epoch)
            .take_while(|slot| slot.epoch == epoch)
            .take(RAI_CLOSE_RECORD_REFRESHES_PER_PASS)
            .cloned()
            .collect::<Vec<_>>();

        for slot in slots {
            let resolution = self
                .rai_epoch_manager
                .happy_path_drain(epoch)
                .and_then(|drain| {
                    drain
                        .finalized
                        .get(&slot)
                        .copied()
                        .map(|hash| (hash, true))
                        .or_else(|| drain.selected.get(&slot).copied().map(|hash| (hash, false)))
                });
            let Some((hash, finalized)) = resolution else {
                // A newer certificate-derived resolution made the old dirty
                // entry irrelevant (for example, a durable upgrade).
                self.rai_close_record_refresh_slots.remove(&slot);
                continue;
            };
            let Some(frontier) = self.rai_replay_frontier(hash, &slot.root, ledger) else {
                continue;
            };
            let refreshed = if finalized {
                self.rai_epoch_manager
                    .record_finalized_drain(epoch, &slot, hash, [frontier])
            } else {
                self.rai_epoch_manager
                    .record_notarized_drain(epoch, &slot, hash, [frontier])
                    .is_some()
            };
            if refreshed {
                self.rai_close_record_refresh_slots.remove(&slot);
                self.rai_clear_missing_drain_payload(&slot);
                self.rai_clear_payload_incomplete_slot(&slot);
            }
        }

        !self
            .rai_close_record_refresh_slots
            .iter()
            .skip_while(|slot| slot.epoch < epoch)
            .any(|slot| slot.epoch == epoch)
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_refresh_close_record_frontiers_from_attached_ledger(
        &mut self,
        epoch: rsnano_types::RaiEpoch,
    ) -> bool {
        let Some(ledger) = self.rai_ledger.clone() else {
            // Container-only tests have no mutable ledger and construct their
            // already-validated frontier maps directly.
            return true;
        };
        self.rai_refresh_close_record_frontiers(epoch, &ledger)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_has_governing_context(&self, epoch: rsnano_types::RaiEpoch) -> bool {
        self.rai_epoch_manager.governing_hash(epoch).is_some()
    }

    /// Reconstructs a Final-vote target from an already validated close-drain
    /// certificate. Protocol finality precedes asynchronous ledger cementation,
    /// so repair must not depend exclusively on the durable finalization index.
    #[cfg(feature = "rai_protocol")]
    fn rai_certificate_finalized_slot(
        &self,
        root: &rsnano_types::Root,
        requested_epoch: rsnano_types::RaiEpoch,
    ) -> Option<(&crate::consensus::election::RaiSlotId, BlockHash)> {
        self.rai_epoch_manager
            .happy_path_drain(requested_epoch)?
            .finalized
            .iter()
            .find(|(slot, _)| slot.epoch == requested_epoch && slot.root.root == *root)
            .map(|(slot, hash)| (slot, *hash))
    }

    /// A zero hash is a wildcard used by a lagging drain replica which no
    /// longer knows the selected candidate. Only a certified drain result may
    /// resolve it; active timeout elections continue to use zero literally.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_certificate_finalized_vote_target(
        &self,
        hash: &BlockHash,
        root: &rsnano_types::Root,
        requested_epoch: rsnano_types::RaiEpoch,
    ) -> Option<rsnano_ledger::RaiFinalizedVoteTarget> {
        let (slot, finalized_hash) = self.rai_certificate_finalized_slot(root, requested_epoch)?;
        if !hash.is_zero() && *hash != finalized_hash {
            return None;
        }
        self.rai_epoch_manager.governing_hash(slot.epoch)?;
        let election_id = crate::consensus::election::RaiElectionId::Slot(slot.clone());
        Some(rsnano_ledger::RaiFinalizedVoteTarget {
            election_id: election_id.clone(),
            hash: finalized_hash,
            root: *root,
            metadata: rsnano_types::RaiVoteMetadata {
                election_id,
                epoch: slot.epoch,
                phase: rsnano_types::RaiVotePhase::Final,
                ..Default::default()
            },
        })
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_finalized_close_vote_target(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<rsnano_ledger::RaiFinalizedVoteTarget> {
        let (election_id, epoch, hash) = if let Some((epoch, round, hash)) =
            self.rai_epoch_manager.installed_close_cut_for_root(root)
        {
            (
                crate::consensus::election::RaiElectionId::CloseCut { epoch, round },
                epoch,
                hash,
            )
        } else {
            let (epoch, round, hash) = self
                .rai_epoch_manager
                .installed_close_record_for_root(root)?;
            (
                crate::consensus::election::RaiElectionId::CloseRecord { epoch, round },
                epoch,
                hash,
            )
        };
        self.rai_epoch_manager.close_committee(epoch)?;
        Some(rsnano_ledger::RaiFinalizedVoteTarget {
            election_id: election_id.clone(),
            hash,
            root: *root,
            metadata: rsnano_types::RaiVoteMetadata {
                election_id,
                epoch,
                phase: rsnano_types::RaiVotePhase::Final,
                ..Default::default()
            },
        })
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_has_active_request_target(
        &self,
        hash: &BlockHash,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> bool {
        self.roots.iter_rai().any(|entry| {
            entry.root.root == *root
                && entry.election.rai_epoch() == epoch
                && (entry.election.voting_hash() == *hash
                    || (hash.is_zero() && entry.election.is_rai_close()))
        })
    }
    #[cfg(feature = "rai_protocol")]
    pub fn rai_tick(
        &mut self,
        now: Timestamp,
        local_key: &rsnano_types::PrivateKey,
        epoch_duration: Duration,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.process_rai_event(
            crate::consensus::rai::RaiEpochEvent::Tick(now),
            local_key,
            epoch_duration,
            now,
        )
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_report_received(
        &mut self,
        report: crate::consensus::rai::RaiReport,
        local_key: &rsnano_types::PrivateKey,
        epoch_duration: Duration,
        now: Timestamp,
    ) {
        self.process_rai_event(
            crate::consensus::rai::RaiEpochEvent::ReportReceived(report),
            local_key,
            epoch_duration,
            now,
        );
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_reports(&self) -> Vec<crate::consensus::rai::RaiReport> {
        self.rai_epoch_manager.reports().all()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_reports_for_epoch_filtered(
        &self,
        epoch: rsnano_types::RaiEpoch,
        predicate: impl FnMut(&crate::consensus::rai::RaiReport) -> bool,
        limit: usize,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.rai_epoch_manager
            .reports()
            .filtered_for_epoch(epoch, predicate, limit)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_report_response_window(
        &self,
        epoch: rsnano_types::RaiEpoch,
        sequence: u64,
        limit: usize,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.rai_epoch_manager
            .reports()
            .response_window(epoch, sequence, limit)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_current_close_root(&self) -> Option<rsnano_types::Root> {
        let closing = self.rai_epoch_manager.state().closing?;
        match closing.phase {
            crate::consensus::rai::RaiClosingPhase::ElectingCut => {
                let round = self.rai_epoch_manager.close_cut_round(closing.epoch)?;
                Some(crate::consensus::rai::rai_close_cut_root(closing.epoch, round).root)
            }
            crate::consensus::rai::RaiClosingPhase::ElectingRecord => {
                let round = self.rai_epoch_manager.close_record_round(closing.epoch)?;
                Some(crate::consensus::rai::rai_close_record_root(closing.epoch, round).root)
            }
            crate::consensus::rai::RaiClosingPhase::Draining => {
                Some(crate::consensus::rai::rai_close_record_root(closing.epoch, 0).root)
            }
            _ => None,
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_replay_terminal_drain_slot(
        &mut self,
        epoch: rsnano_types::RaiEpoch,
        slot: &crate::consensus::election::RaiSlotId,
        any: &rsnano_ledger::OwningAnySet<'_>,
    ) -> bool {
        let Some(terminal) = self.rai_terminal_slots.get(slot).cloned() else {
            return false;
        };
        let terminal_hash = match terminal.outcome {
            crate::consensus::rai::RaiOutcome::Notarized(hash)
            | crate::consensus::rai::RaiOutcome::Confirmed(hash) => Some(hash),
            _ => None,
        };
        // The election may have ended while its winner was still an unsaved
        // publish. Resolve the certified digest against the ledger now;
        // close-local replay must not freeze an empty segment merely because
        // block processing lagged the vote.
        let segment = terminal_hash
            .and_then(|hash| self.rai_replay_frontier_from(hash, &slot.root, any))
            .map(|frontier| vec![frontier])
            .or_else(|| terminal.frontier.map(|info| vec![(terminal.account, info)]))
            .unwrap_or_default();
        let payload_ready = !segment.is_empty();
        let outcome = match terminal.outcome {
            crate::consensus::rai::RaiOutcome::Notarized(hash) => self
                .rai_epoch_manager
                .record_notarized_drain(epoch, slot, hash, segment),
            crate::consensus::rai::RaiOutcome::Confirmed(hash) => self
                .rai_epoch_manager
                .record_finalized_drain(epoch, slot, hash, segment)
                .then_some(crate::consensus::rai::RaiDrainOutcome::Finalized(hash)),
            crate::consensus::rai::RaiOutcome::Pending
            | crate::consensus::rai::RaiOutcome::TimedOut => None,
        };
        if outcome.is_some() {
            if payload_ready {
                self.rai_close_record_refresh_slots.remove(slot);
                self.rai_clear_missing_drain_payload(slot);
                self.rai_clear_payload_incomplete_slot(slot);
            } else {
                // The certificate settles the logical slot, but a fresh close
                // record cannot omit its selected segment. Keep requesting and
                // defer close construction/advancement until the exact payload
                // can be replayed.
                self.rai_close_record_refresh_slots.insert(slot.clone());
                self.rai_mark_missing_drain_payload(slot.clone());
            }
            if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                eprintln!(
                    "RAI_MSG pr={pr} event=drain_settled source=terminal slot={slot:?} outcome={outcome:?}"
                );
            }
            return true;
        }
        debug_assert!(
            false,
            "pending RAI slot was removed before obtaining terminal certificate evidence"
        );
        false
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_check_drain_slot_from(
        &mut self,
        epoch: rsnano_types::RaiEpoch,
        slot: &crate::consensus::election::RaiSlotId,
        any: &rsnano_ledger::OwningAnySet<'_>,
        now: Timestamp,
        durable_upgrade_only: bool,
    ) {
        // Use one ledger snapshot for both payload discovery and durable
        // attribution. The selected SavedBlock is retained after the read
        // transaction is dropped so election insertion stays inside this
        // already-bounded AEC writer pass.
        let local_block = any
            .block_successor_by_qualified_root(&slot.root)
            .and_then(|hash| {
                any.get_block(&hash)
                    .filter(|block| block.qualified_root() == slot.root)
                    .map(|block| (hash, block))
            });
        if let Some((hash, block)) = local_block.as_ref()
            && any.rai_finalization_epoch(hash) == Some(slot.epoch)
            && self.rai_epoch_manager.record_finalized_drain(
                epoch,
                slot,
                *hash,
                [(
                    block.account(),
                    rsnano_types::ConfirmationHeightInfo::new(block.height(), *hash),
                )],
            )
        {
            self.rai_drain_diagnostics
                .slot_state
                .insert(slot.clone(), "durably_finalized");
            self.rai_clear_missing_drain_payload(slot);
            self.rai_clear_payload_incomplete_slot(slot);
            return;
        }
        // A release has no frontier to replay after its durable probe fails.
        // A selected block can have been notarized while it was still an
        // unsaved publish, however, so the completion sweep must also give it
        // one fresh terminal/election replay opportunity before the close
        // record freezes the frontier map.
        if durable_upgrade_only
            && !self
                .rai_epoch_manager
                .happy_path_drain(epoch)
                .is_some_and(|drain| drain.selected.contains_key(slot))
        {
            self.rai_clear_missing_drain_payload(slot);
            self.rai_clear_payload_incomplete_slot(slot);
            return;
        }

        if self.rai_replay_terminal_drain_slot(epoch, slot, any) {
            self.rai_drain_diagnostics
                .slot_state
                .insert(slot.clone(), "terminal_replayed");
            return;
        }

        let id = crate::consensus::election::RaiElectionId::Slot(slot.clone());
        if self.roots.election_for_rai_id(&id).is_none() {
            if let Some((_, block)) = local_block {
                self.rai_clear_missing_drain_payload(slot);
                let state = match self.insert_drain_election(block, epoch, now) {
                    Ok(()) => "recreated",
                    Err(AecInsertError::Duplicate) => "recreate_duplicate",
                    Err(AecInsertError::MissingRaiGoverningClose) => "recreate_no_governing_close",
                    Err(AecInsertError::InvalidRaiCloseElection) => "recreate_invalid_close",
                    Err(AecInsertError::RecentlyConfirmed) => "recreate_recently_confirmed",
                    Err(AecInsertError::Stopped) => "recreate_stopped",
                };
                self.rai_drain_diagnostics
                    .slot_state
                    .insert(slot.clone(), state);
            } else if self
                .rai_blocks_by_qualified_root
                .get(&slot.root)
                .is_some_and(|hashes| !hashes.is_empty())
            {
                // The payload is known but has not reached the durable block
                // table yet. Keep the slot on the drain worklist so ordinary
                // block processing can make it insertable; it is not a reason
                // to send the custom missing-payload request.
                self.rai_clear_missing_drain_payload(slot);
                self.rai_drain_diagnostics
                    .slot_state
                    .insert(slot.clone(), "cached_without_ledger_successor");
            } else {
                self.rai_mark_missing_drain_payload(slot.clone());
                self.rai_drain_diagnostics
                    .slot_state
                    .insert(slot.clone(), "missing_payload");
            }
        } else {
            self.rai_clear_missing_drain_payload(slot);
            self.rai_drain_diagnostics
                .slot_state
                .insert(slot.clone(), "active_without_certificate");
        }

        // Insertion replays retained votes and can make the election terminal
        // immediately. Re-evaluate both representations before deciding that
        // this bounded work item still needs another pass.
        if self.rai_replay_terminal_drain_slot(epoch, slot, any) {
            self.rai_drain_diagnostics
                .slot_state
                .insert(slot.clone(), "terminal_after_recreate");
            return;
        }
        // A payload can make previously retained leaves admissible after the
        // election's initial replay. This is now an indexed per-election leaf
        // replay, not a rescan of every vectorized signed transport.
        self.apply_pending_rai_votes(&id, now);
        let Some(election) = self.roots.election_for_rai_id(&id) else {
            return;
        };
        let evidence = election.rai_votes.clone();
        let winner_hash = election.winner().hash();
        let confirmed = self.rai_replay_frontier_from(winner_hash, &slot.root, any);
        let outcome = self
            .rai_epoch_manager
            .happy_path_drain(epoch)
            .and_then(|drain| drain.persistent_evidence_outcome(slot, &evidence));
        if let Some(outcome) = outcome {
            let segment = match outcome {
                crate::consensus::rai::RaiDrainOutcome::Finalized(hash)
                | crate::consensus::rai::RaiDrainOutcome::Selected(hash) => confirmed
                    .filter(|(_, info)| info.frontier == hash)
                    .map(|(account, info)| vec![(account, info)])
                    .unwrap_or_default(),
                crate::consensus::rai::RaiDrainOutcome::ReleasedTimeout
                | crate::consensus::rai::RaiDrainOutcome::ReleasedConflict => Vec::new(),
            };
            let needs_payload = matches!(
                outcome,
                crate::consensus::rai::RaiDrainOutcome::Finalized(_)
                    | crate::consensus::rai::RaiDrainOutcome::Selected(_)
            );
            let payload_ready = !needs_payload || !segment.is_empty();
            let recorded = self
                .rai_epoch_manager
                .record_drain_evidence(epoch, slot, &evidence, segment);
            if recorded.is_some() {
                self.rai_drain_diagnostics
                    .slot_state
                    .insert(slot.clone(), "certificate_applied");
                if payload_ready {
                    self.rai_close_record_refresh_slots.remove(slot);
                    self.rai_clear_missing_drain_payload(slot);
                    self.rai_clear_payload_incomplete_slot(slot);
                } else {
                    self.rai_close_record_refresh_slots.insert(slot.clone());
                    self.rai_mark_missing_drain_payload(slot.clone());
                }
                if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                    eprintln!(
                        "RAI_MSG pr={pr} event=drain_settled source=active slot={slot:?} outcome={recorded:?}"
                    );
                }
            }
        }
    }

    #[cfg(all(test, feature = "rai_protocol"))]
    fn rai_check_drain_slot(
        &mut self,
        epoch: rsnano_types::RaiEpoch,
        slot: &crate::consensus::election::RaiSlotId,
        ledger: &rsnano_ledger::Ledger,
        now: Timestamp,
        durable_upgrade_only: bool,
    ) {
        let any = ledger.any();
        self.rai_check_drain_slot_from(epoch, slot, &any, now, durable_upgrade_only);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_progress_close(
        &mut self,
        frontiers: Option<crate::consensus::rai::RaiFrontierMap>,
        ledger: &rsnano_ledger::Ledger,
        now: Timestamp,
    ) {
        use crate::consensus::rai::{RaiCloseElectionId, RaiCloseKind, RaiClosingPhase};

        let Some(closing) = self.rai_epoch_manager.closing_epoch() else {
            return;
        };
        if closing.phase == RaiClosingPhase::ElectingCut {
            // Fresh visibility may change while a close round is active, but
            // the round's first-vote value is immutable. A different fresh
            // value is eligible only in a successor round after positive
            // death evidence, as enforced by RaiCloseRoundTracker::next.
            let round = self
                .rai_epoch_manager
                .close_cut_round(closing.epoch)
                .unwrap_or(0);
            let election_id = crate::consensus::election::RaiElectionId::CloseCut {
                epoch: closing.epoch,
                round,
            };
            // Round advancement and election cleanup are driven by different
            // event queues. If cleanup wins the race, the epoch manager can
            // already point at the successor while no active election exists,
            // leaving this replica unable to contribute its First vote.
            // Recreate that deterministic successor from durable tracker state.
            if self.roots.election_for_rai_id(&election_id).is_none() {
                let candidate = self
                    .rai_epoch_manager
                    .close_cut_tracker(closing.epoch)
                    .and_then(|tracker| tracker.round(round))
                    .map(|state| state.selected);
                let committee = self.rai_epoch_manager.close_committee(closing.epoch);
                if let (Some(candidate), Some(committee)) = (candidate, committee) {
                    let _ = self.insert_close_election(
                        super::RaiCloseElectionSpec {
                            id: RaiCloseElectionId {
                                kind: RaiCloseKind::Cut,
                                epoch: closing.epoch,
                                round,
                            },
                            root: crate::consensus::rai::rai_close_cut_root(closing.epoch, round),
                            candidate,
                            committee,
                        },
                        now,
                    );
                }
            }
            if let Some(hash) = self.rai_epoch_manager.refresh_close_cut_candidate(
                closing.epoch,
                round,
                std::iter::empty(),
            ) {
                // Admit the reconstructed preimage for validating remote
                // votes. The local vote-history lock still prevents signing
                // this changed value in the current round.
                self.roots.add_rai_hash_candidate_for_id(&election_id, hash);
            }
            return;
        }
        if closing.phase != RaiClosingPhase::Draining {
            return;
        }
        // CloseInput_e starts from the preceding certified frontiers. Exact
        // terminal slot frontiers are merged below as cut obligations settle.
        // Use the installed certificate directly: ledger cementation is
        // asynchronous and may otherwise expose different partial bases to
        // different replicas at the epoch boundary.
        if self
            .rai_epoch_manager
            .drain_frontiers(closing.epoch)
            .is_none()
        {
            let frontiers = closing
                .epoch
                .number()
                .checked_sub(1)
                .and_then(|epoch| {
                    self.rai_epoch_manager
                        .durable_close_state(rsnano_types::RaiEpoch::new(epoch))
                })
                .map(|state| state.frontiers)
                .or(frontiers);
            let Some(frontiers) = frontiers else {
                // The ticker supplies the external ledger snapshot exactly
                // once after observing Draining. If the phase advanced between
                // its status read and this write lock, retry on the next tick.
                return;
            };
            self.rai_epoch_manager
                .initialize_drain_frontiers(closing.epoch, frontiers);

            // Ordinary-finalized slots are intentionally absent from reports
            // and the cut. Their exact finality locks must nevertheless be
            // included in the fresh close-record frontier map.
            let finalized_outside_cut = self
                .rai_terminal_slots
                .iter()
                .filter_map(|(slot, terminal)| {
                    (slot.epoch == closing.epoch
                        && matches!(
                            terminal.outcome,
                            crate::consensus::rai::RaiOutcome::Confirmed(_)
                        ))
                    .then_some((terminal.account, terminal.frontier.clone()?))
                })
                .collect::<Vec<_>>();
            for (account, info) in finalized_outside_cut {
                self.rai_epoch_manager.record_ordinary_finalized_frontier(
                    closing.epoch,
                    account,
                    info,
                );
            }
        }
        let reset_schedule = self
            .rai_drain_check_schedule
            .as_ref()
            .is_none_or(|schedule| !schedule.is_for_epoch(closing.epoch));
        if reset_schedule {
            let unresolved = self
                .rai_epoch_manager
                .unresolved_drain_obligations(closing.epoch)
                .unwrap_or_default();
            self.rai_drain_check_schedule =
                Some(RaiDrainCheckSchedule::resolving(closing.epoch, unresolved));
        }

        let durable_upgrade = self
            .rai_drain_check_schedule
            .as_ref()
            .is_some_and(|schedule| schedule.durable_upgrade);
        let obligations = self
            .rai_drain_check_schedule
            .as_mut()
            .map(|schedule| schedule.take_window(RAI_DRAIN_CHECKS_PER_TICK))
            .unwrap_or_default();
        let checks_this_pass = obligations.len();
        let pass_started = std::time::Instant::now();
        // One consistent ledger snapshot serves the entire bounded window.
        // Opening a read transaction per slot while holding the AEC write lock
        // made a drain pass monopolize that lock for several seconds.
        let snapshot_started = std::time::Instant::now();
        let any = ledger.any();
        let snapshot_wait_us = snapshot_started.elapsed().as_micros() as u64;
        for slot in obligations {
            // A certificate may have settled a queued slot asynchronously.
            // Do not spend a ledger read on that stale resolving entry.
            let should_check = self
                .rai_epoch_manager
                .happy_path_drain(closing.epoch)
                .is_some_and(|drain| {
                    !drain.finalized.contains_key(&slot)
                        && (durable_upgrade
                            || (!drain.selected.contains_key(&slot)
                                && !drain.released.contains_key(&slot)))
                });
            if should_check {
                self.rai_check_drain_slot_from(closing.epoch, &slot, &any, now, durable_upgrade);
            }
            if !durable_upgrade
                && self
                    .rai_epoch_manager
                    .happy_path_drain(closing.epoch)
                    .is_some_and(|drain| {
                        !drain.finalized.contains_key(&slot)
                            && !drain.selected.contains_key(&slot)
                            && !drain.released.contains_key(&slot)
                    })
            {
                self.rai_drain_check_schedule
                    .as_mut()
                    .expect("schedule was initialized above")
                    .requeue_unresolved(slot);
            }
        }
        let pass_us = pass_started.elapsed().as_micros() as u64;
        let diagnostics = &mut self.rai_drain_diagnostics;
        diagnostics.passes = diagnostics.passes.saturating_add(1);
        diagnostics.checks = diagnostics.checks.saturating_add(checks_this_pass as u64);
        diagnostics.last_checks = checks_this_pass;
        diagnostics.max_checks = diagnostics.max_checks.max(checks_this_pass);
        diagnostics.ledger_snapshot_wait_us = snapshot_wait_us;
        diagnostics.max_ledger_snapshot_wait_us = diagnostics
            .max_ledger_snapshot_wait_us
            .max(snapshot_wait_us);
        diagnostics.last_pass_us = pass_us;
        diagnostics.max_pass_us = diagnostics.max_pass_us.max(pass_us);

        let drain_complete = self
            .rai_epoch_manager
            .happy_path_drain(closing.epoch)
            .is_some_and(|drain| drain.is_complete());
        if !drain_complete {
            return;
        }
        if !durable_upgrade {
            // Completion is certificate-derived. Before freezing the close
            // record, start one fresh bounded pass over every selected or
            // released slot so a concurrent durable ledger advance can still
            // upgrade its close-local frontier.
            let candidates = self
                .rai_epoch_manager
                .obligations_requiring_durable_check(closing.epoch)
                .unwrap_or_default();
            self.rai_drain_check_schedule
                .as_mut()
                .expect("schedule was initialized above")
                .begin_durable_upgrade(candidates);
            return;
        }
        if !self
            .rai_drain_check_schedule
            .as_ref()
            .is_some_and(RaiDrainCheckSchedule::ready_for_close)
        {
            return;
        }
        // The close-record construction order is normative: after draining
        // the decided cut, first derive and apply every available ordinary
        // fast/final certificate for this epoch, including elections outside
        // the cut, and only then hash the fresh frontier map. Importing these
        // locks after round zero had started made correct replicas first-vote
        // different incomplete records and needlessly churn close rounds.
        self.import_persistent_ordinary_finality(closing.epoch);
        if !self.rai_refresh_close_record_frontiers(closing.epoch, ledger) {
            return;
        }
        let Some(close_frontiers) = self
            .rai_epoch_manager
            .drain_frontiers(closing.epoch)
            .cloned()
        else {
            return;
        };
        let committee = ledger.rai_rep_weights_at_frontiers(&close_frontiers);
        let Some((root, candidate)) = self.rai_epoch_manager.begin_close_record(committee) else {
            return;
        };
        let Some(committee) = self.rai_epoch_manager.close_committee(closing.epoch) else {
            return;
        };
        let _ = self.insert_close_record(
            super::RaiCloseElectionSpec {
                id: RaiCloseElectionId {
                    kind: RaiCloseKind::Record,
                    epoch: closing.epoch,
                    round: 0,
                },
                root,
                candidate,
                committee,
            },
            now,
        );
    }

    #[cfg(feature = "rai_protocol")]
    fn process_rai_event(
        &mut self,
        event: crate::consensus::rai::RaiEpochEvent,
        local_key: &rsnano_types::PrivateKey,
        epoch_duration: Duration,
        now: Timestamp,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        use crate::consensus::rai::{RaiEpochLoop, RaiEpochLoopDriver};

        // Live close elections have additional persistence, preimage repair,
        // ledger-refresh, and bounded retry requirements implemented by
        // `progress_close_election`. Sending the same event through the
        // generic epoch loop as well advanced the round a second time and
        // bypassed its repair grace.
        if let crate::consensus::rai::RaiEpochEvent::CloseElectionChanged { kind, epoch, round } =
            &event
        {
            let id = match kind {
                crate::consensus::rai::RaiCloseKind::Cut => {
                    crate::consensus::election::RaiElectionId::CloseCut {
                        epoch: *epoch,
                        round: *round,
                    }
                }
                crate::consensus::rai::RaiCloseKind::Record => {
                    crate::consensus::election::RaiElectionId::CloseRecord {
                        epoch: *epoch,
                        round: *round,
                    }
                }
            };
            self.progress_close_election(&id, now);
            return Vec::new();
        }

        // Network callbacks retain signed leaves before all candidate
        // preimages or earlier phases necessarily exist. Re-evaluate the
        // active close certificate on every lifecycle tick so progress does
        // not depend on another packet arriving after the missing material.
        if matches!(event, crate::consensus::rai::RaiEpochEvent::Tick(_)) {
            self.process_persistent_close_certificates(now);
            if let Some(closing) = self.rai_epoch_manager.closing_epoch() {
                let id = match closing.phase {
                    crate::consensus::rai::RaiClosingPhase::ElectingCut => self
                        .rai_epoch_manager
                        .close_cut_round(closing.epoch)
                        .map(
                            |round| crate::consensus::election::RaiElectionId::CloseCut {
                                epoch: closing.epoch,
                                round,
                            },
                        ),
                    crate::consensus::rai::RaiClosingPhase::ElectingRecord => self
                        .rai_epoch_manager
                        .close_record_round(closing.epoch)
                        .map(
                            |round| crate::consensus::election::RaiElectionId::CloseRecord {
                                epoch: closing.epoch,
                                round,
                            },
                        ),
                    _ => None,
                };
                if let Some(id) = id {
                    self.apply_pending_rai_votes(&id, now);
                    self.progress_close_election(&id, now);
                }
            }
        }

        #[derive(Default)]
        struct LiveDriver {
            reports: Vec<crate::consensus::rai::RaiReport>,
            visible: std::collections::BTreeMap<
                rsnano_types::RaiEpoch,
                std::collections::BTreeSet<crate::consensus::election::RaiSlotId>,
            >,
            close_evidence: Option<crate::consensus::rai::RaiElectionVoteState>,
            close_winner: Option<BlockHash>,
            close_elections: Vec<(
                crate::consensus::rai::RaiCloseKind,
                rsnano_types::RaiEpoch,
                u32,
                QualifiedRoot,
                BlockHash,
            )>,
        }

        impl RaiEpochLoopDriver for LiveDriver {
            fn visible_obligations(
                &self,
                epoch: rsnano_types::RaiEpoch,
            ) -> std::collections::BTreeSet<crate::consensus::election::RaiSlotId> {
                self.visible.get(&epoch).cloned().unwrap_or_default()
            }

            fn vote_visible_obligations(
                &self,
                _epoch: rsnano_types::RaiEpoch,
            ) -> std::collections::BTreeSet<crate::consensus::election::RaiSlotId> {
                // Local election presence is not the authenticated >F
                // vote-visibility witness required by the protocol.
                Default::default()
            }

            fn start_close_election(
                &mut self,
                kind: crate::consensus::rai::RaiCloseKind,
                epoch: rsnano_types::RaiEpoch,
                round: u32,
                root: QualifiedRoot,
                hash: BlockHash,
            ) {
                tracing::warn!(
                    ?kind,
                    ?epoch,
                    round,
                    ?root,
                    ?hash,
                    "RAI_CLOSE_TRACE close election start"
                );
                self.close_elections.push((kind, epoch, round, root, hash));
            }

            fn close_election_winner(
                &self,
                _kind: crate::consensus::rai::RaiCloseKind,
                _epoch: rsnano_types::RaiEpoch,
                _round: u32,
            ) -> Option<BlockHash> {
                self.close_winner
            }

            fn close_election_evidence(
                &self,
                _kind: crate::consensus::rai::RaiCloseKind,
                _epoch: rsnano_types::RaiEpoch,
                _round: u32,
            ) -> Option<crate::consensus::rai::RaiElectionVoteState> {
                self.close_evidence.clone()
            }

            fn commit_close_record(
                &mut self,
                _epoch: rsnano_types::RaiEpoch,
                _frontiers: &crate::consensus::rai::RaiFrontierMap,
            ) -> bool {
                // Live close decisions are installed by the container's
                // ledger-backed paths after this generic lifecycle pass.
                false
            }

            fn broadcast_report(&mut self, report: crate::consensus::rai::RaiReport) {
                self.reports.push(report);
            }
        }

        // Snapshot the changed election before the loop may ask the manager to
        // derive a decision, death proof, or live carry from it.
        let (close_evidence, close_winner) = match &event {
            crate::consensus::rai::RaiEpochEvent::CloseElectionChanged { kind, epoch, round } => {
                let id = match kind {
                    crate::consensus::rai::RaiCloseKind::Cut => {
                        crate::consensus::election::RaiElectionId::CloseCut {
                            epoch: *epoch,
                            round: *round,
                        }
                    }
                    crate::consensus::rai::RaiCloseKind::Record => {
                        crate::consensus::election::RaiElectionId::CloseRecord {
                            epoch: *epoch,
                            round: *round,
                        }
                    }
                };
                let snapshot =
                    self.roots
                        .election_for_rai_id(&id)
                        .map_or((None, None, None), |election| {
                            let evidence = election.rai_votes.clone();
                            let winner = match evidence.outcome {
                                crate::consensus::rai::RaiOutcome::Confirmed(hash) => Some(hash),
                                _ => None,
                            };
                            (Some(evidence), winner, Some(election.start()))
                        });
                if snapshot.1.is_some()
                    && let Some(started_at) = snapshot.2
                {
                    let duration = started_at.elapsed(now);
                    match kind {
                        crate::consensus::rai::RaiCloseKind::Cut => {
                            self.rai_cut_election_durations
                                .entry(*epoch)
                                .or_insert(duration);
                        }
                        crate::consensus::rai::RaiCloseKind::Record => {
                            self.rai_record_election_durations
                                .entry(*epoch)
                                .or_insert(duration);
                        }
                    }
                }
                let result = (snapshot.0, snapshot.1);
                tracing::warn!(
                    ?kind,
                    ?epoch,
                    round,
                    election_id = ?id,
                    evidence = ?result.0,
                    winner = ?result.1,
                    "RAI_CLOSE_TRACE close election update"
                );
                result
            }
            _ => (None, None),
        };
        // Visibility is consumed only by the tick which starts an epoch
        // close. Pre-boundary ticks, report delivery, and ticks for an already
        // closing epoch must not walk every slot accumulated by the node.
        let visibility_epoch = match &event {
            crate::consensus::rai::RaiEpochEvent::Tick(tick_now) => {
                let state = self.rai_epoch_manager.state();
                (state.closing.is_none() && *tick_now >= state.open_started_at + epoch_duration)
                    .then_some(state.open_epoch)
            }
            _ => None,
        };
        let mut visible = std::collections::BTreeMap::new();
        if let Some(epoch) = visibility_epoch {
            // Every slot is indexed when its election is inserted. Consume
            // only this epoch's bucket: scanning the process-lifetime slot
            // history and then scanning every active election again made a
            // boundary hold the AEC write lock for seconds under sustained
            // load.
            let epoch_visible = self
                .rai_visible_obligations
                .remove(&epoch)
                .unwrap_or_default()
                .into_iter()
                .filter(|slot| {
                    !self.rai_terminal_slots.get(slot).is_some_and(|terminal| {
                        matches!(
                            terminal.outcome,
                            crate::consensus::rai::RaiOutcome::Confirmed(_)
                        ) && terminal.frontier.is_some()
                    }) && self
                        .rai_epoch_manager
                        .slot_election_enabled(slot.epoch, &slot.root)
                })
                .collect::<std::collections::BTreeSet<_>>();
            visible.insert(epoch, epoch_visible);
        }

        // Keep one source of truth: this is the same manager used by active
        // elections and the rai_status RPC, moved through the loop for a tick.
        let replacement = crate::consensus::rai::RaiEpochManager::new(
            std::sync::Arc::new(RepWeights::default()),
            BlockHash::ZERO,
        );
        let manager = std::mem::replace(&mut self.rai_epoch_manager, replacement);
        let started_at = manager.state().open_started_at;
        let mut epoch_loop = RaiEpochLoop::new(
            manager,
            LiveDriver {
                close_evidence,
                close_winner,
                visible,
                ..Default::default()
            },
            local_key.clone(),
            epoch_duration,
            started_at,
        );
        epoch_loop.process(event);
        let (manager, driver) = epoch_loop.into_parts();
        self.rai_epoch_manager = manager;
        for (kind, epoch, round, root, candidate) in driver.close_elections {
            let Some(committee) = self.rai_epoch_manager.close_committee(epoch) else {
                continue;
            };
            let spec = super::RaiCloseElectionSpec {
                id: crate::consensus::rai::RaiCloseElectionId { kind, epoch, round },
                root,
                candidate,
                committee,
            };
            let _ = self.insert_close_election(spec, now);
        }
        driver.reports
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_epoch_state(&self) -> &crate::consensus::rai::RaiEpochState {
        self.rai_epoch_manager.state()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_genesis_committee(&self) -> std::sync::Arc<RepWeights> {
        self.rai_epoch_manager
            .committee_at(-1)
            .expect("the genesis committee is always defined")
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_current_voting_weight(&self, representative: PublicKey) -> Amount {
        let epoch = self.rai_epoch_manager.current_epoch();
        let mut weight = Amount::ZERO;
        if let Some(committees) = self.rai_epoch_manager.slot_committees(epoch) {
            for committee in committees {
                weight = std::cmp::max(weight, committee.weight(&representative));
            }
        }
        if let Some(committee) = self.rai_epoch_manager.close_committee(epoch) {
            weight = std::cmp::max(weight, committee.weight(&representative));
        }
        weight
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_installed_close_hash(&self, epoch: rsnano_types::RaiEpoch) -> Option<BlockHash> {
        self.rai_epoch_manager.installed_close_hash(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_decided_cut_hashes(
        &self,
    ) -> &std::collections::BTreeMap<rsnano_types::RaiEpoch, BlockHash> {
        self.rai_epoch_manager.decided_cut_hashes()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_happy_path_drains(
        &self,
    ) -> &std::collections::BTreeMap<rsnano_types::RaiEpoch, crate::consensus::rai::RaiHappyPathDrain>
    {
        self.rai_epoch_manager.happy_path_drains()
    }

    /// Returns a bounded, fair window of slots which still need certificate
    /// evidence during the resolving pass. Evidence repair has its own queue
    /// so its stride cannot alias with the 256-slot ledger-check rotation.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_unresolved_drain_slots(
        &mut self,
        epoch: rsnano_types::RaiEpoch,
        limit: usize,
    ) -> Vec<crate::consensus::election::RaiSlotId> {
        let Some(drain) = self.rai_epoch_manager.happy_path_drain(epoch) else {
            return Vec::new();
        };
        let Some(schedule) = self
            .rai_drain_check_schedule
            .as_mut()
            .filter(|schedule| schedule.is_for_epoch(epoch))
        else {
            return Vec::new();
        };
        schedule.take_evidence_repair_window(limit, |slot| {
            drain.obligations.contains(slot)
                && !drain.finalized.contains_key(slot)
                && !drain.selected.contains_key(slot)
                && !drain.released.contains_key(slot)
        })
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_missing_slot_payload_candidates(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<(QualifiedRoot, Option<BlockHash>)> {
        let Some(drain) = self.rai_epoch_manager.happy_path_drain(epoch) else {
            return Vec::new();
        };
        let unresolved = |slot: &crate::consensus::election::RaiSlotId| {
            drain.obligations.contains(slot)
                && (self.rai_close_record_refresh_slots.contains(slot)
                    || (!drain.finalized.contains_key(slot)
                        && !drain.selected.contains_key(slot)
                        && !drain.released.contains_key(slot)))
        };
        let mut candidates = std::collections::BTreeSet::new();

        // Root-only misses are added only by the bounded drain worklist. Do
        // not turn ordinary ended/evicted elections into an O(N) repair scan.
        for slot in self
            .rai_missing_drain_payloads
            .iter()
            .filter(|slot| slot.epoch == epoch && unresolved(slot))
        {
            let id = crate::consensus::election::RaiElectionId::Slot(slot.clone());
            let payload_known = self
                .rai_blocks_by_qualified_root
                .get(&slot.root)
                .is_some_and(|hashes| !hashes.is_empty());
            if self.roots.election_for_rai_id(&id).is_none()
                && (self.rai_close_record_refresh_slots.contains(slot)
                    || !self.rai_terminal_slots.contains_key(slot))
                && !payload_known
            {
                candidates.insert((slot.root.clone(), None));
            }
        }

        // A signed leaf can name a certified fork whose payload is absent
        // even while another candidate/election at the same root is local.
        // Preserve that exact digest; the ticker rechecks it against one
        // ledger snapshot before emitting a root-only ZERO request.
        for (slot, hashes) in self
            .rai_payload_incomplete
            .iter()
            .filter(|(slot, _)| slot.epoch == epoch && unresolved(slot))
        {
            for hash in hashes
                .iter()
                .filter(|hash| !hash.is_zero() && !self.rai_blocks.contains_key(*hash))
            {
                candidates.insert((slot.root.clone(), Some(*hash)));
            }
        }

        candidates.into_iter().collect()
    }

    #[cfg(all(feature = "rai_protocol", test))]
    fn rai_missing_slot_payload_requests(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<(BlockHash, rsnano_types::Root)> {
        let mut requests = self
            .rai_missing_slot_payload_candidates(epoch)
            .into_iter()
            .map(|(root, _)| (BlockHash::ZERO, root.root))
            .collect::<Vec<_>>();
        requests.sort_unstable();
        requests.dedup();
        requests
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_blocks_for_request(
        &self,
        hash: BlockHash,
        root: rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<Block> {
        let selected = if hash.is_zero() {
            self.rai_certificate_finalized_slot(&root, epoch)
                .map(|(_, hash)| hash)
                .or_else(|| {
                    self.rai_terminal_slots
                        .iter()
                        .find(|(slot, _)| slot.epoch == epoch && slot.root.root == root)
                        .and_then(|(_, terminal)| match terminal.outcome {
                            crate::consensus::rai::RaiOutcome::Notarized(hash)
                            | crate::consensus::rai::RaiOutcome::Confirmed(hash) => Some(hash),
                            _ => None,
                        })
                })
                .or_else(|| {
                    self.rai_blocks_by_qualified_root
                        .iter()
                        .find(|(qualified_root, _)| qualified_root.root == root)
                        .and_then(|(_, hashes)| hashes.first().copied())
                })
        } else {
            Some(hash)
        };
        let Some(hash) = selected else {
            return Vec::new();
        };
        // Slot elections are deliberately one-block elections. Ancestors are
        // repaired and confirmed by their own qualified-root elections, just
        // as in the non-RAI path; a tip request must never implicitly turn
        // into segment transport or confirmation.
        self.rai_blocks.get(&hash).cloned().into_iter().collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_slot_vote_context_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<rsnano_types::RaiVoteMetadata> {
        if let Some(election) = self
            .roots
            .rai_elections_for_request_root(root)
            .find(|election| {
                election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot
                    && self.rai_election_vote_enabled(election.rai_id())
            })
        {
            return Some(election.rai_vote_metadata());
        }
        self.rai_terminal_slots_by_request_root
            .get(root)
            .into_iter()
            .flatten()
            .find(|slot| {
                self.rai_epoch_manager
                    .slot_election_enabled(slot.epoch, &slot.root)
            })
            .and_then(|slot| {
                self.rai_epoch_manager.governing_hash(slot.epoch)?;
                Some(rsnano_types::RaiVoteMetadata {
                    election_id: crate::consensus::election::RaiElectionId::Slot(slot.clone()),
                    epoch: slot.epoch,
                    ..Default::default()
                })
            })
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_terminal_notarized_target_for_root(
        &self,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Option<(BlockHash, rsnano_types::RaiVoteMetadata)> {
        if let Some(election) = self
            .roots
            .rai_elections_for_request_root(root)
            .find(|election| {
                election.rai_epoch() == epoch
                    && election.rai_request_hash().is_zero()
                    && !election.state().has_ended()
                    && self.rai_election_vote_enabled(election.rai_id())
            })
        {
            return Some((BlockHash::ZERO, election.rai_vote_metadata()));
        }
        let slot = self
            .rai_terminal_slots_by_request_root
            .get(root)?
            .iter()
            .find(|slot| {
                slot.epoch == epoch
                    && self
                        .rai_epoch_manager
                        .slot_election_enabled(slot.epoch, &slot.root)
                    && self.rai_terminal_slots.get(*slot).is_some_and(|terminal| {
                        matches!(
                            terminal.outcome,
                            crate::consensus::rai::RaiOutcome::Notarized(_)
                        )
                    })
            })?;
        let terminal = self.rai_terminal_slots.get(slot)?;
        self.rai_epoch_manager.governing_hash(slot.epoch)?;
        let crate::consensus::rai::RaiOutcome::Notarized(hash) = terminal.outcome else {
            return None;
        };
        Some((
            hash,
            rsnano_types::RaiVoteMetadata {
                election_id: crate::consensus::election::RaiElectionId::Slot(slot.clone()),
                epoch: slot.epoch,
                phase: rsnano_types::RaiVotePhase::Notar,
                ..Default::default()
            },
        ))
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_active_slot_vote_target_for_root(
        &self,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Option<rsnano_ledger::RaiFinalizedVoteTarget> {
        let election = self
            .roots
            .rai_elections_for_request_root(root)
            .find(|election| {
                election.rai_epoch() == epoch
                    && election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot
                    && !election.state().has_ended()
                    && self.rai_election_vote_enabled(election.rai_id())
            })?;
        let election_id = election.rai_id().clone();
        Some(rsnano_ledger::RaiFinalizedVoteTarget {
            election_id: election_id.clone(),
            hash: election.voting_hash(),
            root: *root,
            metadata: election.rai_vote_metadata(),
        })
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_votes_for_root(
        &self,
        root: &rsnano_types::Root,
        requested_epoch: rsnano_types::RaiEpoch,
    ) -> Vec<rsnano_types::Vote> {
        let election_id = self
            .roots
            .iter_rai()
            .find(|entry| entry.root.root == *root && entry.election.rai_epoch() == requested_epoch)
            .map(|entry| entry.election.rai_id().clone())
            .or_else(|| {
                self.rai_terminal_slots
                    .keys()
                    .find(|slot| slot.root.root == *root && slot.epoch == requested_epoch)
                    .cloned()
                    .map(crate::consensus::election::RaiElectionId::Slot)
            })
            .or_else(|| {
                self.rai_pending_votes
                    .keys()
                    .find(|id| match id {
                        crate::consensus::election::RaiElectionId::CloseCut { epoch, round } => {
                            *epoch == requested_epoch
                                && crate::consensus::rai::rai_close_cut_root(*epoch, *round).root
                                    == *root
                        }
                        crate::consensus::election::RaiElectionId::CloseRecord { epoch, round } => {
                            *epoch == requested_epoch
                                && crate::consensus::rai::rai_close_record_root(*epoch, *round).root
                                    == *root
                        }
                        crate::consensus::election::RaiElectionId::Slot(slot) => {
                            slot.root.root == *root && slot.epoch == requested_epoch
                        }
                    })
                    .cloned()
            });
        election_id
            .filter(|id| self.rai_election_vote_enabled(id))
            .and_then(|id| self.rai_pending_votes.get(&id))
            .map(|votes| votes.iter().map(|vote| (**vote).clone()).collect())
            .unwrap_or_default()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_record_versions(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<crate::consensus::rai::RaiCloseRecord> {
        self.rai_epoch_manager
            .close_record_versions()
            .into_iter()
            .filter(|record| record.epoch == epoch)
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_decided_close_record(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Option<crate::consensus::rai::RaiCloseRecord> {
        let (_, hash) = self
            .rai_epoch_manager
            .close_record_tracker(epoch)?
            .decision()?;
        self.rai_epoch_manager.close_record(&hash).cloned()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_record_version(
        &self,
        epoch: rsnano_types::RaiEpoch,
        hash: &BlockHash,
    ) -> Option<crate::consensus::rai::RaiCloseRecord> {
        self.rai_epoch_manager
            .close_record(hash)
            .filter(|record| record.epoch == epoch)
            .cloned()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_cut_versions_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Vec<crate::consensus::rai::RaiCloseCut> {
        self.rai_epoch_manager
            .close_cut_versions()
            .into_iter()
            .filter(|cut| {
                self.rai_epoch_manager
                    .close_cut_round(cut.epoch)
                    .is_some_and(|round| {
                        crate::consensus::rai::rai_close_cut_root(cut.epoch, round).root == *root
                    })
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_cut_versions(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<crate::consensus::rai::RaiCloseCut> {
        self.rai_epoch_manager
            .close_cut_versions()
            .into_iter()
            .filter(|cut| cut.epoch == epoch)
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_cut_version(
        &self,
        epoch: rsnano_types::RaiEpoch,
        hash: &BlockHash,
    ) -> Option<crate::consensus::rai::RaiCloseCut> {
        self.rai_epoch_manager
            .close_cut(hash)
            .filter(|cut| cut.epoch == epoch)
            .cloned()
    }

    /// Returns exact nonzero close candidates for which authenticated vote
    /// leaves are retained but this replica cannot yet validate the preimage.
    /// Only rounds belonging to the currently active logical close election
    /// are eligible; historical closed epochs and the other close kind do not
    /// create repair traffic.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_missing_close_preimage_requests(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<(BlockHash, rsnano_types::Root)> {
        use crate::consensus::rai::{RaiCloseKind, RaiClosingPhase};

        let Some(closing) = self.rai_epoch_manager.closing_epoch() else {
            return Vec::new();
        };
        if closing.epoch != epoch {
            return Vec::new();
        }
        let active_kind = match closing.phase {
            RaiClosingPhase::ElectingCut => RaiCloseKind::Cut,
            RaiClosingPhase::ElectingRecord => RaiCloseKind::Record,
            _ => return Vec::new(),
        };

        let mut requests = std::collections::BTreeSet::new();
        for (id, votes) in &self.rai_pending_votes {
            let (kind, validated_preimages, root) = match id {
                crate::consensus::election::RaiElectionId::CloseCut {
                    epoch: vote_epoch,
                    round,
                } if *vote_epoch == epoch => {
                    let Some(state) = self
                        .rai_epoch_manager
                        .close_cut_tracker(epoch)
                        .and_then(|tracker| tracker.round(*round))
                    else {
                        continue;
                    };
                    (
                        RaiCloseKind::Cut,
                        &state.validated_preimages,
                        crate::consensus::rai::rai_close_cut_root(epoch, *round).root,
                    )
                }
                crate::consensus::election::RaiElectionId::CloseRecord {
                    epoch: vote_epoch,
                    round,
                } if *vote_epoch == epoch => {
                    let Some(state) = self
                        .rai_epoch_manager
                        .close_record_tracker(epoch)
                        .and_then(|tracker| tracker.round(*round))
                    else {
                        continue;
                    };
                    (
                        RaiCloseKind::Record,
                        &state.validated_preimages,
                        crate::consensus::rai::rai_close_record_root(epoch, *round).root,
                    )
                }
                _ => continue,
            };
            if kind != active_kind {
                continue;
            }
            for vote in votes {
                for (metadata, hash) in vote.rai_entries() {
                    if &metadata.election_id == id
                        && metadata.epoch == epoch
                        && !hash.is_zero()
                        && !validated_preimages.contains(hash)
                    {
                        requests.insert((*hash, root));
                    }
                }
            }
        }
        requests.into_iter().collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_votes_for_epoch(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<rsnano_types::Vote> {
        let mut seen = std::collections::HashSet::new();
        self.rai_pending_votes
            .iter()
            .filter(|(id, _)| {
                matches!(id,
                    crate::consensus::election::RaiElectionId::CloseCut { epoch: vote_epoch, .. }
                    | crate::consensus::election::RaiElectionId::CloseRecord { epoch: vote_epoch, .. }
                    if *vote_epoch == epoch)
            })
            .flat_map(|(_, votes)| votes)
            // One signed batch is retained under every close ID represented
            // by one of its leaves. Return the transport once, not once per
            // retention key. These votes were signature-validated before
            // retention, so signer/signature is the exact transport identity
            // without re-hashing a full batch once per retained leaf.
            .filter(|vote| seen.insert((vote.voter, vote.signature.clone())))
            .map(|vote| (**vote).clone())
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_slot_votes_for_epoch(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<rsnano_types::Vote> {
        let mut seen = std::collections::HashSet::new();
        self.rai_pending_votes
            .iter()
            .filter(|(id, _)| {
                matches!(id, crate::consensus::election::RaiElectionId::Slot(slot) if slot.epoch == epoch)
            })
            .flat_map(|(_, votes)| votes)
            .filter(|vote| seen.insert((vote.voter, vote.signature.clone())))
            .map(|vote| (**vote).clone())
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_active_close_vote_requests(
        &self,
        epoch: rsnano_types::RaiEpoch,
        limit: usize,
    ) -> Vec<(BlockHash, rsnano_types::Root, rsnano_types::RaiElectionId)> {
        // Close elections can live in `rai_entries` when their qualified root
        // is already occupied.  Repair must cover both storage locations;
        // otherwise a split round can learn enough First votes to request the
        // timeout value but never actually solicit the timeout Notar leaves.
        self.roots
            .iter_rai()
            .map(|entry| &entry.election)
            .filter(|election| {
                election.is_rai_close()
                    && election.rai_epoch() == epoch
                    && election.rai_requires_retention()
            })
            .take(limit)
            .map(|election| {
                (
                    election.rai_request_hash(),
                    election.qualified_root().root,
                    election.rai_id().clone(),
                )
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_diagnostics(&self) -> std::collections::BTreeMap<String, String> {
        let mut result = std::collections::BTreeMap::new();
        let Some(closing) = self.rai_epoch_manager.closing_epoch() else {
            return result;
        };
        if closing.phase == crate::consensus::rai::RaiClosingPhase::Draining {
            if let Some(drain) = self.rai_epoch_manager.happy_path_drain(closing.epoch) {
                let resolved = drain.finalized.len() + drain.selected.len() + drain.released.len();
                result.insert("obligations".into(), drain.obligations.len().to_string());
                result.insert("resolved".into(), resolved.to_string());
                result.insert(
                    "unresolved".into(),
                    drain.obligations.len().saturating_sub(resolved).to_string(),
                );
                result.insert("finalized".into(), drain.finalized.len().to_string());
                result.insert("selected".into(), drain.selected.len().to_string());
                result.insert("released".into(), drain.released.len().to_string());
                let unresolved = drain.obligations.iter().filter(|slot| {
                    !drain.finalized.contains_key(*slot)
                        && !drain.selected.contains_key(*slot)
                        && !drain.released.contains_key(*slot)
                });
                let mut payload_incomplete = 0usize;
                let mut missing_payload = 0usize;
                let mut active = 0usize;
                let mut locally_inserted = 0usize;
                let mut state_counts = std::collections::BTreeMap::<&str, usize>::new();
                let mut first_signer_counts = [0usize; 7];
                let mut fast_ready = 0usize;
                let mut leading_first_min: Option<u128> = None;
                let mut leading_first_max = 0u128;
                let mut fast_threshold_min: Option<u128> = None;
                let mut fast_threshold_max = 0u128;
                let mut samples = Vec::new();
                for slot in unresolved {
                    payload_incomplete += self.rai_payload_incomplete.contains_key(slot) as usize;
                    missing_payload += self.rai_missing_drain_payloads.contains(slot) as usize;
                    if let Some(election) = self.roots.election_for_rai_id(
                        &crate::consensus::election::RaiElectionId::Slot(slot.clone()),
                    ) {
                        active += 1;
                        if let Some(committee) = election.rai_votes.committees.first() {
                            let signers = committee.votes.first.len().min(6);
                            first_signer_counts[signers] += 1;
                            let mut values = std::collections::HashSet::new();
                            values.extend(committee.votes.first.values().copied());
                            let leading = values
                                .into_iter()
                                .map(|value| election.rai_votes.first_tally(0, value).number())
                                .max()
                                .unwrap_or_default();
                            let fast = committee.thresholds.fast.number();
                            leading_first_min =
                                Some(leading_first_min.map_or(leading, |v| v.min(leading)));
                            leading_first_max = leading_first_max.max(leading);
                            fast_threshold_min =
                                Some(fast_threshold_min.map_or(fast, |v| v.min(fast)));
                            fast_threshold_max = fast_threshold_max.max(fast);
                            fast_ready += (leading >= fast) as usize;
                        }
                    }
                    locally_inserted += self.rai_locally_inserted_slots.contains(slot) as usize;
                    let state = self
                        .rai_drain_diagnostics
                        .slot_state
                        .get(slot)
                        .copied()
                        .unwrap_or("not_yet_scanned");
                    *state_counts.entry(state).or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!("{}:{}:{state}", slot.root.previous, slot.root.root));
                    }
                }
                result.insert("payload_incomplete".into(), payload_incomplete.to_string());
                result.insert("missing_payload".into(), missing_payload.to_string());
                result.insert("active_unresolved".into(), active.to_string());
                result.insert(
                    "first_signers".into(),
                    first_signer_counts
                        .iter()
                        .enumerate()
                        .filter(|(_, count)| **count != 0)
                        .map(|(signers, count)| format!("{signers}:{count}"))
                        .collect::<Vec<_>>()
                        .join(","),
                );
                result.insert("fast_ready_active".into(), fast_ready.to_string());
                result.insert(
                    "leading_first_weight".into(),
                    format!(
                        "min:{},max:{}",
                        leading_first_min.unwrap_or_default(),
                        leading_first_max
                    ),
                );
                result.insert(
                    "fast_threshold".into(),
                    format!(
                        "min:{},max:{}",
                        fast_threshold_min.unwrap_or_default(),
                        fast_threshold_max
                    ),
                );
                result.insert("locally_inserted".into(), locally_inserted.to_string());
                result.insert(
                    "remote_only".into(),
                    drain
                        .obligations
                        .len()
                        .saturating_sub(resolved + locally_inserted)
                        .to_string(),
                );
                result.insert(
                    "unresolved_states".into(),
                    state_counts
                        .into_iter()
                        .map(|(state, count)| format!("{state}:{count}"))
                        .collect::<Vec<_>>()
                        .join(","),
                );
                if !samples.is_empty() {
                    result.insert("unresolved_sample".into(), samples.join(","));
                }
            }
            if let Some(schedule) = &self.rai_drain_check_schedule {
                result.insert("worklist".into(), schedule.pending.len().to_string());
                result.insert(
                    "durable_upgrade".into(),
                    schedule.durable_upgrade.to_string(),
                );
            }
            let diagnostics = &self.rai_drain_diagnostics;
            result.insert("passes".into(), diagnostics.passes.to_string());
            result.insert("checks".into(), diagnostics.checks.to_string());
            result.insert("last_checks".into(), diagnostics.last_checks.to_string());
            result.insert("max_checks".into(), diagnostics.max_checks.to_string());
            result.insert(
                "snapshot_wait_us".into(),
                diagnostics.ledger_snapshot_wait_us.to_string(),
            );
            result.insert(
                "max_snapshot_wait_us".into(),
                diagnostics.max_ledger_snapshot_wait_us.to_string(),
            );
            result.insert("pass_us".into(), diagnostics.last_pass_us.to_string());
            result.insert("max_pass_us".into(), diagnostics.max_pass_us.to_string());
            return result;
        }
        let (round, id, tracker) = match closing.phase {
            crate::consensus::rai::RaiClosingPhase::ElectingCut => {
                let Some(round) = self.rai_epoch_manager.close_cut_round(closing.epoch) else {
                    return result;
                };
                (
                    round,
                    crate::consensus::election::RaiElectionId::CloseCut {
                        epoch: closing.epoch,
                        round,
                    },
                    self.rai_epoch_manager.close_cut_tracker(closing.epoch),
                )
            }
            crate::consensus::rai::RaiClosingPhase::ElectingRecord => {
                let Some(round) = self.rai_epoch_manager.close_record_round(closing.epoch) else {
                    return result;
                };
                (
                    round,
                    crate::consensus::election::RaiElectionId::CloseRecord {
                        epoch: closing.epoch,
                        round,
                    },
                    self.rai_epoch_manager.close_record_tracker(closing.epoch),
                )
            }
            _ => return result,
        };
        result.insert("round".into(), round.to_string());
        if let Some(state) = tracker.and_then(|tracker| tracker.round(round)) {
            result.insert(
                "preimages".into(),
                state.validated_preimages.len().to_string(),
            );
            result.insert("round_result".into(), format!("{:?}", state.derive()));
        }
        result.insert(
            "retained_transports".into(),
            self.rai_pending_votes
                .get(&id)
                .map_or(0, Vec::len)
                .to_string(),
        );
        if let Some(election) = self.roots.election_for_rai_id(&id) {
            result.insert(
                "outcome".into(),
                format!("{:?}", election.rai_votes.outcome),
            );
            result.insert(
                "request_hash".into(),
                election.rai_request_hash().to_string(),
            );
            if let Some(committee) = election.rai_votes.committees.first() {
                result.insert(
                    "first_signers".into(),
                    committee.votes.first.len().to_string(),
                );
                result.insert(
                    "notar_signers".into(),
                    committee.votes.notar.len().to_string(),
                );
                result.insert(
                    "timeout_ready".into(),
                    election.rai_votes.timeout_ready(0).to_string(),
                );
                result.insert(
                    "local_result".into(),
                    format!("{:?}", election.rai_votes.local_result(0)),
                );
                result.insert(
                    "thresholds".into(),
                    format!(
                        "progression:{},notarization:{},fast:{},finalization:{}",
                        committee.thresholds.progression.number(),
                        committee.thresholds.notarization.number(),
                        committee.thresholds.fast.number(),
                        committee.thresholds.finalization.number(),
                    ),
                );

                let mut candidates = std::collections::HashSet::new();
                for value in committee.votes.first.values() {
                    if let crate::consensus::rai::BlockHashOrTimeout::Block(hash) = value {
                        candidates.insert(*hash);
                    }
                }
                for values in committee.votes.notar.values() {
                    for value in values {
                        if let crate::consensus::rai::BlockHashOrTimeout::Block(hash) = value {
                            candidates.insert(*hash);
                        }
                    }
                }
                candidates.extend(committee.votes.final_votes.values().copied());
                let mut candidates = candidates.into_iter().collect::<Vec<_>>();
                candidates.sort();
                for (index, hash) in candidates.into_iter().take(8).enumerate() {
                    let value = crate::consensus::rai::BlockHashOrTimeout::Block(hash);
                    result.insert(
                        format!("candidate_{index}"),
                        format!(
                            "hash:{hash},first:{},notar:{},final:{}",
                            election.rai_votes.first_tally(0, value).number(),
                            election.rai_votes.notarization_tally(0, value).number(),
                            election.rai_votes.final_tally(0, hash).number(),
                        ),
                    );
                }
                let timeout = crate::consensus::rai::BlockHashOrTimeout::Timeout;
                result.insert(
                    "timeout_tally".into(),
                    format!(
                        "first:{},notar:{}",
                        election.rai_votes.first_tally(0, timeout).number(),
                        election.rai_votes.notarization_tally(0, timeout).number(),
                    ),
                );
            }
        }

        // A close certificate can be complete while installation repeatedly
        // fails because this replica does not yet have every block named by a
        // received frontier map. Status is polled frequently by operators and
        // benchmarks, so inspect only a small window after the install cursor.
        // An exact full-record count here can consume most of a 100 ms polling
        // interval and starve the ledger writer which advances that cursor.
        if matches!(
            closing.phase,
            crate::consensus::rai::RaiClosingPhase::ElectingRecord
        ) && let Some(state) = self
            .rai_epoch_manager
            .close_record_tracker(closing.epoch)
            .and_then(|tracker| tracker.round(round))
            && let crate::consensus::rai::RaiCloseRoundResult::Decided(hash) = state.derive()
            && let Some(record) = self.rai_epoch_manager.close_record(&hash)
            && let Some(ledger) = self.rai_ledger.as_deref()
        {
            const INSTALL_DIAGNOSTIC_WINDOW: usize = 64;
            const INSTALL_DIAGNOSTIC_SAMPLES: usize = 8;
            let cursor = self.rai_close_commit_cursors.get(&closing.epoch).copied();
            let (window, truncated) = rai_close_install_diagnostic_window(
                &record.frontiers,
                cursor,
                INSTALL_DIAGNOSTIC_WINDOW,
            );
            let pending = ledger.rai_uncommitted_close_frontier_details(
                closing.epoch,
                &window,
                window.len().max(1),
            );
            let missing = pending
                .iter()
                .filter(|(_, _, _, exists, _)| !exists)
                .count();
            result.insert(
                "install_pending_frontiers".into(),
                if truncated {
                    format!("{} in next {}", pending.len(), window.len())
                } else {
                    pending.len().to_string()
                },
            );
            result.insert(
                "install_missing_blocks".into(),
                if truncated {
                    format!("{missing} in next {}", window.len())
                } else {
                    missing.to_string()
                },
            );
            result.insert("install_scan_truncated".into(), truncated.to_string());
            result.insert("install_scanned_frontiers".into(), window.len().to_string());
            result.insert(
                "install_cursor".into(),
                cursor
                    .map(|account| account.to_string())
                    .unwrap_or_else(|| "start".to_string()),
            );
            result.insert("record_previous".into(), record.previous.to_string());
            result.insert(
                "record_frontiers".into(),
                record.frontiers.len().to_string(),
            );
            let samples = pending
                .into_iter()
                .take(INSTALL_DIAGNOSTIC_SAMPLES)
                .map(|(account, expected, local, exists, attributed_epoch)| {
                    let local = local
                        .map(|info| format!("{}@{}", info.frontier, info.height))
                        .unwrap_or_else(|| "none".to_string());
                    format!(
                        "account:{account},expected:{}@{},local:{local},exists:{exists},attributed:{attributed_epoch:?}",
                        expected.frontier, expected.height,
                    )
                })
                .collect::<Vec<_>>();
            if !samples.is_empty() {
                result.insert("install_pending_sample".into(), samples.join(";"));
            }
        }
        result
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn reconcile_rai_close_cut(
        &mut self,
        cut: crate::consensus::rai::RaiCloseCut,
        root: rsnano_types::Root,
        now: Timestamp,
    ) -> bool {
        let Some(current_round) = self.rai_epoch_manager.close_cut_round(cut.epoch) else {
            return false;
        };
        let Some(round) = (0..=current_round).find(|round| {
            crate::consensus::rai::rai_close_cut_root(cut.epoch, *round).root == root
        }) else {
            return false;
        };
        let Some((epoch, round, hash)) = self.rai_epoch_manager.reconcile_close_cut(cut, round)
        else {
            return false;
        };
        let id = crate::consensus::election::RaiElectionId::CloseCut { epoch, round };
        self.roots.add_rai_hash_candidate_for_id(&id, hash);
        // Preimage insertion is idempotent, but the signed certificate may
        // have grown since this candidate was first learned. Always replay and
        // re-evaluate so a later repair wave can complete the election.
        self.apply_pending_rai_votes(&id, now);
        self.progress_close_election(&id, now);
        true
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_record_versions_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Vec<crate::consensus::rai::RaiCloseRecord> {
        self.rai_epoch_manager
            .close_record_versions()
            .into_iter()
            .filter(|record| {
                self.rai_epoch_manager
                    .close_record_round(record.epoch)
                    .is_some_and(|round| {
                        crate::consensus::rai::rai_close_record_root(record.epoch, round).root
                            == *root
                    })
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn reconcile_rai_close_record(
        &mut self,
        record: crate::consensus::rai::RaiCloseRecord,
        root: rsnano_types::Root,
        now: Timestamp,
    ) -> bool {
        let current_round = self
            .rai_epoch_manager
            .close_record_round(record.epoch)
            .unwrap_or(0);
        let Some(round) = (0..=current_round).find(|round| {
            crate::consensus::rai::rai_close_record_root(record.epoch, *round).root == root
        }) else {
            return false;
        };
        let Some((epoch, round, hash)) =
            self.rai_epoch_manager.reconcile_close_record(record, round)
        else {
            return false;
        };
        let id = crate::consensus::election::RaiElectionId::CloseRecord { epoch, round };
        self.roots.add_rai_hash_candidate_for_id(&id, hash);
        self.apply_pending_rai_votes(&id, now);
        self.progress_close_election(&id, now);
        true
    }

    /// Rebuilds one close round from the process-lifetime signed vote store.
    /// This is also used after the active source round has been retired, so a
    /// delayed fast/final certificate remains authoritative in later rounds.
    #[cfg(feature = "rai_protocol")]
    fn persistent_close_vote_evidence(
        &self,
        id: &crate::consensus::election::RaiElectionId,
    ) -> Option<crate::consensus::rai::RaiElectionVoteState> {
        use crate::consensus::rai::{BlockHashOrTimeout, RaiElectionVoteState};

        let (epoch, validated_preimages) = match id {
            crate::consensus::election::RaiElectionId::CloseCut { epoch, round } => (
                *epoch,
                &self
                    .rai_epoch_manager
                    .close_cut_tracker(*epoch)?
                    .round(*round)?
                    .validated_preimages,
            ),
            crate::consensus::election::RaiElectionId::CloseRecord { epoch, round } => (
                *epoch,
                &self
                    .rai_epoch_manager
                    .close_record_tracker(*epoch)?
                    .round(*round)?
                    .validated_preimages,
            ),
            crate::consensus::election::RaiElectionId::Slot(_) => return None,
        };
        let committee = self.rai_epoch_manager.close_committee(epoch)?;
        let mut evidence = RaiElectionVoteState::new(vec![committee]);
        let mut entries = self
            .rai_pending_votes
            .get(id)?
            .iter()
            .flat_map(|vote| {
                vote.rai_entries()
                    .filter(|(metadata, _)| metadata.election_id == *id && metadata.epoch == epoch)
                    .map(move |(metadata, hash)| (vote, metadata, hash))
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|(_, metadata, _)| match metadata.phase {
            rsnano_types::RaiVotePhase::First => 0,
            rsnano_types::RaiVotePhase::Notar => 1,
            rsnano_types::RaiVotePhase::Final => 2,
        });
        for (vote, metadata, hash) in entries {
            let value = if hash.is_zero() && metadata.phase == rsnano_types::RaiVotePhase::Notar {
                BlockHashOrTimeout::Timeout
            } else {
                if metadata.phase != rsnano_types::RaiVotePhase::First
                    && !validated_preimages.contains(hash)
                {
                    continue;
                }
                BlockHashOrTimeout::Block(*hash)
            };
            let _ = evidence.record_vote(vote.voter, value, metadata.phase, metadata.scope);
        }
        Some(evidence)
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_close_round_has_missing_preimages(
        &self,
        id: &crate::consensus::election::RaiElectionId,
    ) -> bool {
        let validated = match id {
            crate::consensus::election::RaiElectionId::CloseCut { epoch, round } => self
                .rai_epoch_manager
                .close_cut_tracker(*epoch)
                .and_then(|tracker| tracker.round(*round))
                .map(|state| &state.validated_preimages),
            crate::consensus::election::RaiElectionId::CloseRecord { epoch, round } => self
                .rai_epoch_manager
                .close_record_tracker(*epoch)
                .and_then(|tracker| tracker.round(*round))
                .map(|state| &state.validated_preimages),
            crate::consensus::election::RaiElectionId::Slot(_) => None,
        };
        let Some(validated) = validated else {
            return false;
        };
        self.rai_pending_votes.get(id).is_some_and(|votes| {
            votes.iter().any(|vote| {
                vote.rai_entries().any(|(metadata, hash)| {
                    metadata.election_id == *id && !hash.is_zero() && !validated.contains(hash)
                })
            })
        })
    }

    /// Materializes ordinary-finalized slots from the durable vote store even
    /// after their short-lived active elections have been retired. Close
    /// records are built from exact finality locks, not from which elections
    /// happened to remain resident at the epoch boundary.
    #[cfg(feature = "rai_protocol")]
    fn import_persistent_ordinary_finality(&mut self, epoch: rsnano_types::RaiEpoch) {
        use crate::consensus::rai::{
            BlockHashOrTimeout, RaiElectionVoteState, RaiLocalResult, RaiOutcome,
        };

        let Some(committees) = self.rai_epoch_manager.slot_committees(epoch) else {
            return;
        };
        let Some(ledger) = self.rai_ledger.clone() else {
            return;
        };
        let ids = self
            .rai_pending_votes
            .keys()
            .filter_map(|id| match id {
                crate::consensus::election::RaiElectionId::Slot(slot)
                    if slot.epoch == epoch && !self.rai_terminal_slots.contains_key(slot) =>
                {
                    Some(slot.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        for slot in ids {
            let id = crate::consensus::election::RaiElectionId::Slot(slot.clone());
            let mut evidence = RaiElectionVoteState::new(committees.clone());
            let mut entries = self
                .rai_pending_votes
                .get(&id)
                .into_iter()
                .flatten()
                .flat_map(|vote| {
                    vote.rai_entries()
                        .filter(|(metadata, _)| metadata.election_id == id)
                        .map(move |(metadata, hash)| (vote, metadata, hash))
                })
                .collect::<Vec<_>>();
            entries.sort_by_key(|(_, metadata, _)| match metadata.phase {
                rsnano_types::RaiVotePhase::First => 0,
                rsnano_types::RaiVotePhase::Notar => 1,
                rsnano_types::RaiVotePhase::Final => 2,
            });
            for (vote, metadata, hash) in entries {
                let value = if hash.is_zero() {
                    BlockHashOrTimeout::Timeout
                } else {
                    BlockHashOrTimeout::Block(*hash)
                };
                let _ = evidence.record_vote(vote.voter, value, metadata.phase, metadata.scope);
            }
            let results = (0..evidence.committees.len())
                .map(|index| evidence.local_result(index))
                .collect::<Option<Vec<_>>>();
            let Some(results) = results else { continue };
            let Some(hash) = results.first().and_then(|result| match result {
                RaiLocalResult::Fast(hash) | RaiLocalResult::Final(hash) => Some(*hash),
                _ => None,
            }) else {
                continue;
            };
            if !results.iter().all(|result| matches!(result, RaiLocalResult::Fast(h) | RaiLocalResult::Final(h) if *h == hash)) {
                continue;
            }
            let Some((account, frontier)) = self.rai_replay_frontier(hash, &slot.root, &ledger)
            else {
                continue;
            };
            self.insert_rai_terminal_slot(
                slot,
                RaiTerminalSlot {
                    outcome: RaiOutcome::Confirmed(hash),
                    account,
                    frontier: Some(frontier),
                },
            );
        }
    }

    /// Processes fast/final certificates from every retained round of the
    /// active logical close instance. The specification makes such a
    /// certificate decisive even after this replica has entered a later round.
    #[cfg(feature = "rai_protocol")]
    fn process_persistent_close_certificates(&mut self, now: Timestamp) {
        use crate::consensus::rai::{RaiCloseKind, RaiClosingPhase, RaiLocalResult};

        let Some(closing) = self.rai_epoch_manager.closing_epoch() else {
            return;
        };
        let kind = match closing.phase {
            RaiClosingPhase::ElectingCut => RaiCloseKind::Cut,
            RaiClosingPhase::ElectingRecord => RaiCloseKind::Record,
            _ => return,
        };
        let mut ids = self
            .rai_pending_votes
            .keys()
            .filter(|id| match (kind, *id) {
                (
                    RaiCloseKind::Cut,
                    crate::consensus::election::RaiElectionId::CloseCut { epoch, .. },
                )
                | (
                    RaiCloseKind::Record,
                    crate::consensus::election::RaiElectionId::CloseRecord { epoch, .. },
                ) => *epoch == closing.epoch,
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        ids.sort_by_key(|id| match id {
            crate::consensus::election::RaiElectionId::CloseCut { round, .. }
            | crate::consensus::election::RaiElectionId::CloseRecord { round, .. } => *round,
            crate::consensus::election::RaiElectionId::Slot(_) => 0,
        });

        for id in ids {
            let Some(evidence) = self.persistent_close_vote_evidence(&id) else {
                continue;
            };
            let Some(result) = evidence.local_result(0) else {
                continue;
            };
            let hash = match result {
                RaiLocalResult::Fast(hash) | RaiLocalResult::Final(hash) => hash,
                RaiLocalResult::Notarized(_) | RaiLocalResult::Timeout => continue,
            };
            let round = match &id {
                crate::consensus::election::RaiElectionId::CloseCut { round, .. }
                | crate::consensus::election::RaiElectionId::CloseRecord { round, .. } => *round,
                crate::consensus::election::RaiElectionId::Slot(_) => continue,
            };
            match kind {
                RaiCloseKind::Cut => {
                    self.rai_epoch_manager
                        .store_close_cut_evidence(closing.epoch, round, evidence);
                    if self
                        .rai_epoch_manager
                        .decide_close_cut(closing.epoch, round, hash)
                        .is_ok()
                    {
                        self.record_rai_close_election_duration(&id, now);
                        let removed = self.roots.drain_filter(|entry| {
                            matches!(
                                entry.election.rai_id(),
                                crate::consensus::election::RaiElectionId::CloseCut { epoch, .. }
                                    if *epoch == closing.epoch
                            )
                        });
                        for entry in removed {
                            self.cleanup_election(entry);
                        }
                        return;
                    }
                }
                RaiCloseKind::Record => {
                    self.rai_epoch_manager.store_close_record_evidence(
                        closing.epoch,
                        round,
                        evidence,
                    );
                    if self
                        .install_close_record_with_commit(closing.epoch, round, hash, None)
                        .is_ok()
                    {
                        self.record_rai_close_election_duration(&id, now);
                        let removed = self.roots.drain_filter(|entry| {
                            matches!(
                                entry.election.rai_id(),
                                crate::consensus::election::RaiElectionId::CloseRecord { epoch, .. }
                                    if *epoch == closing.epoch
                            ) || matches!(
                                entry.election.rai_id(),
                                crate::consensus::election::RaiElectionId::Slot(slot)
                                    if self.rai_epoch_manager.certified_release(slot).is_some()
                            )
                        });
                        for entry in removed {
                            self.cleanup_election(entry);
                        }
                        self.prune_rai_evidence_through(closing.epoch);
                        return;
                    }
                }
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn cleanup_rai_close_rounds(
        &mut self,
        kind: crate::consensus::rai::RaiCloseKind,
        epoch: rsnano_types::RaiEpoch,
    ) {
        let removed = self.roots.drain_filter(|entry| match kind {
            crate::consensus::rai::RaiCloseKind::Cut => matches!(
                entry.election.rai_id(),
                crate::consensus::election::RaiElectionId::CloseCut {
                    epoch: entry_epoch,
                    ..
                } if *entry_epoch == epoch
            ),
            crate::consensus::rai::RaiCloseKind::Record => matches!(
                entry.election.rai_id(),
                crate::consensus::election::RaiElectionId::CloseRecord {
                    epoch: entry_epoch,
                    ..
                } if *entry_epoch == epoch
            ),
        });
        for entry in removed {
            self.cleanup_election(entry);
        }
    }

    /// Projects persistent close-election votes into the logical round
    /// tracker. A compatible notarization creates a carried-value successor;
    /// only fast/final evidence decides the close instance.
    #[cfg(feature = "rai_protocol")]
    fn progress_close_election(
        &mut self,
        id: &crate::consensus::election::RaiElectionId,
        now: Timestamp,
    ) {
        use crate::consensus::rai::{RaiCloseElectionId, RaiCloseKind, RaiOutcome};

        let Some(election) = self.roots.election_for_rai_id(id) else {
            return;
        };
        let (kind, epoch, round) = match id {
            crate::consensus::election::RaiElectionId::CloseCut { epoch, round } => {
                (RaiCloseKind::Cut, *epoch, *round)
            }
            crate::consensus::election::RaiElectionId::CloseRecord { epoch, round } => {
                (RaiCloseKind::Record, *epoch, *round)
            }
            crate::consensus::election::RaiElectionId::Slot(_) => return,
        };
        let outcome = election.rai_votes.outcome;
        let active_evidence = election.rai_votes.clone();

        if matches!(outcome, RaiOutcome::Confirmed(_)) {
            self.finish_reconciled_close_election(id, now);
            return;
        }

        let evidence = self
            .persistent_close_vote_evidence(id)
            .unwrap_or(active_evidence);
        let current_round = match kind {
            RaiCloseKind::Cut => self.rai_epoch_manager.close_cut_round(epoch),
            RaiCloseKind::Record => self.rai_epoch_manager.close_record_round(epoch),
        };
        if current_round != Some(round) {
            // Retained rounds remain live and their evidence remains
            // decisive, but only the tracker's current round may start a
            // successor. In particular, a repair event for an older carried
            // round must not consume the current round's result using the old
            // round's already-elapsed grace window.
            match kind {
                RaiCloseKind::Cut => {
                    self.rai_epoch_manager
                        .store_close_cut_evidence(epoch, round, evidence);
                }
                RaiCloseKind::Record => {
                    self.rai_epoch_manager
                        .store_close_record_evidence(epoch, round, evidence);
                }
            }
            return;
        }
        let local_result = evidence.local_result(0);
        // Store the terminal evidence before waiting so the repair loop can
        // replay the exact signed leaves which explain this transition.
        match kind {
            RaiCloseKind::Cut => {
                self.rai_epoch_manager
                    .store_close_cut_evidence(epoch, round, evidence);
            }
            RaiCloseKind::Record => {
                self.rai_epoch_manager
                    .store_close_record_evidence(epoch, round, evidence);
            }
        }

        match local_result {
            Some(crate::consensus::rai::RaiLocalResult::Notarized(_)) => {
                let observed_at = *self.rai_close_notarized_at.entry(id.clone()).or_insert(now);
                if observed_at.elapsed(now) < self.base_latency {
                    return;
                }
            }
            Some(crate::consensus::rai::RaiLocalResult::Timeout) => {
                let observed_at = *self.rai_close_notarized_at.entry(id.clone()).or_insert(now);
                let repair_window = if self.rai_close_round_has_missing_preimages(id) {
                    RAI_CLOSE_PREIMAGE_REPAIR_LIMIT
                } else {
                    RAI_CLOSE_DATA_REPAIR_GRACE
                };
                if observed_at.elapsed(now) < repair_window {
                    return;
                }
            }
            _ => {
                self.rai_close_notarized_at.remove(id);
            }
        }

        if kind == RaiCloseKind::Record
            && !self.rai_refresh_close_record_frontiers_from_attached_ledger(epoch)
        {
            return;
        }

        let next = match kind {
            RaiCloseKind::Cut => self.rai_epoch_manager.advance_close_cut_round(),
            RaiCloseKind::Record => self
                .rai_epoch_manager
                .advance_close_record_round(std::iter::empty()),
        };
        let Some((root, candidate)) = next else {
            return;
        };
        let Some(committee) = self.rai_epoch_manager.close_committee(epoch) else {
            return;
        };
        let next_round = match kind {
            RaiCloseKind::Cut => self.rai_epoch_manager.close_cut_round(epoch),
            RaiCloseKind::Record => self.rai_epoch_manager.close_record_round(epoch),
        };
        let Some(next_round) = next_round else {
            return;
        };
        let next_id = match kind {
            RaiCloseKind::Cut => crate::consensus::election::RaiElectionId::CloseCut {
                epoch,
                round: next_round,
            },
            RaiCloseKind::Record => crate::consensus::election::RaiElectionId::CloseRecord {
                epoch,
                round: next_round,
            },
        };
        // A notarization enables the carried successor, but does not disable
        // the source round. Keep both active: delayed First votes may still
        // form a fast certificate, and the local voter may still emit the
        // source round's Final vote. A decision in either round retires every
        // active round of this logical close instance.
        let successor_exists = self.roots.election_for_rai_id(&next_id).is_some();
        if !successor_exists {
            let _ = self.insert_close_election(
                super::RaiCloseElectionSpec {
                    id: RaiCloseElectionId {
                        kind,
                        epoch,
                        round: next_round,
                    },
                    root,
                    candidate,
                    committee,
                },
                now,
            );
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn finish_reconciled_close_election(
        &mut self,
        id: &crate::consensus::election::RaiElectionId,
        now: Timestamp,
    ) {
        let Some(election) = self.roots.election_for_rai_id(id) else {
            return;
        };
        let crate::consensus::rai::RaiOutcome::Confirmed(hash) = election.rai_votes.outcome else {
            return;
        };
        let evidence = election.rai_votes.clone();
        let (kind, epoch, round) = match id {
            crate::consensus::election::RaiElectionId::CloseCut { epoch, round } => {
                (crate::consensus::rai::RaiCloseKind::Cut, *epoch, *round)
            }
            crate::consensus::election::RaiElectionId::CloseRecord { epoch, round } => {
                (crate::consensus::rai::RaiCloseKind::Record, *epoch, *round)
            }
            crate::consensus::election::RaiElectionId::Slot(_) => return,
        };
        match kind {
            crate::consensus::rai::RaiCloseKind::Cut => {
                self.rai_epoch_manager
                    .store_close_cut_evidence(epoch, round, evidence);
                if self
                    .rai_epoch_manager
                    .decide_close_cut(epoch, round, hash)
                    .is_ok()
                {
                    let Some(entry) = self.roots.erase_rai_id(id) else {
                        return;
                    };
                    self.rai_close_notarized_at.remove(id);
                    self.record_rai_close_election_duration(id, now);
                    self.cleanup_rai_close_rounds(kind, epoch);
                    self.cleanup_election(entry);
                }
            }
            crate::consensus::rai::RaiCloseKind::Record => {
                self.rai_epoch_manager
                    .store_close_record_evidence(epoch, round, evidence);
                if self
                    .install_close_record_with_commit(epoch, round, hash, None)
                    .is_ok()
                {
                    let Some(entry) = self.roots.erase_rai_id(id) else {
                        return;
                    };
                    self.rai_close_notarized_at.remove(id);
                    self.record_rai_close_election_duration(id, now);
                    self.cleanup_rai_close_rounds(kind, epoch);
                    let removed = self.roots.drain_filter(|entry| {
                        let crate::consensus::election::RaiElectionId::Slot(slot) =
                            entry.election.rai_id()
                        else {
                            return false;
                        };
                        self.rai_epoch_manager.certified_release(slot).is_some()
                    });
                    for released in removed {
                        self.cleanup_election(released);
                    }
                    self.prune_rai_evidence_through(epoch);
                    self.cleanup_election(entry);
                }
            }
        }
    }
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            roots: RootContainer::new(config.max_elections),
            observer: None,
            stopped: false,
            count_by_behavior: Default::default(),
            base_latency,
            recently_confirmed: RecentlyConfirmedCache::new(config.confirmation_cache),
            cooldown: CooldownController::default(),
            max_elections: config.max_elections,
            max_elections_per_bucket: max(config.max_elections / bucket_count(), 1),
            #[cfg(feature = "rai_protocol")]
            retry_released_slots: config.retry_released_slots,
            stats: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_epoch_manager: crate::consensus::rai::RaiEpochManager::new(
                std::sync::Arc::new(RepWeights::default()),
                BlockHash::ZERO,
            ),
            #[cfg(feature = "rai_protocol")]
            rai_drain_check_schedule: None,
            #[cfg(feature = "rai_protocol")]
            rai_drain_diagnostics: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_visible_obligations: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_locally_inserted_slots: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_first_visible_epoch_by_root: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_terminal_slots: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_terminal_slots_by_hash: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_terminal_slots_by_request_root: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_blocks: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_blocks_by_qualified_root: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_payload_incomplete: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_payload_incomplete_by_hash: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_missing_drain_payloads: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_missing_drain_payloads_by_root: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_close_record_refresh_slots: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_candidate_hashes: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_pending_votes: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_pending_vote_leaves: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_pending_vote_replay_complete: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_pending_compact_slot_votes: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_pending_timeout_slot_votes: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_pending_close_contexts_by_hash: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_ledger: None,
            #[cfg(feature = "rai_protocol")]
            rai_close_commit_cursors: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_pending_close_commits: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_completed_close_commits: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_cut_election_durations: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_record_election_durations: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_close_election_starts: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_close_notarized_at: Default::default(),
        }
    }

    /// Records block data after the normal ledger processing path has checked
    /// it. Arrival only wakes epoch-qualified references which were already
    /// made by a vote/report; it never chooses the open epoch.
    #[cfg(feature = "rai_protocol")]
    pub fn published_block_available(&mut self, block: Block) {
        let hash = block.hash();
        let root = block.qualified_root();
        self.rai_blocks.entry(hash).or_insert(block);
        self.rai_blocks_by_qualified_root
            .entry(root.clone())
            .or_default()
            .insert(hash);

        // Resolve vote-before-block compact leaves without inventing a root
        // on the wire. The block's qualified root is the sole authoritative
        // mapping from hash to the ordinary election/cache key.
        let compact_keys = self
            .rai_pending_compact_slot_votes
            .keys()
            .filter(|(_, pending_hash)| *pending_hash == hash)
            .cloned()
            .collect::<Vec<_>>();
        for (epoch, pending_hash) in compact_keys {
            let votes = self
                .rai_pending_compact_slot_votes
                .remove(&(epoch, pending_hash))
                .unwrap_or_default();
            let slot = crate::consensus::election::RaiSlotId {
                epoch,
                root: root.clone(),
            };
            for vote in votes {
                let mut resolved = (*vote).clone();
                for (metadata, target) in resolved.rai_entries_mut() {
                    if matches!(target, rsnano_types::RaiVoteTarget::Hash(candidate) if *candidate == hash)
                        && matches!(
                            &metadata.election_id,
                            crate::consensus::election::RaiElectionId::Slot(id)
                                if id.epoch == epoch && id.root == QualifiedRoot::ZERO
                        )
                    {
                        metadata.election_id =
                            crate::consensus::election::RaiElectionId::Slot(slot.clone());
                    }
                }
                self.rai_retain_pending_vote(
                    crate::consensus::election::RaiElectionId::Slot(slot.clone()),
                    Arc::new(resolved),
                );
            }
            let _ = self.admit_candidate(slot, hash);
        }
        // The terminal marker commonly predates both close-drain selection
        // and the late Publish. Use its reverse hash index to wake only slots
        // whose authenticated drain resolution selected this exact payload;
        // no all-obligation scan is needed.
        let referenced_slots = self
            .rai_terminal_slots_by_hash
            .get(&hash)
            .cloned()
            .unwrap_or_default();
        for slot in referenced_slots {
            if slot.root == root
                && self
                    .rai_epoch_manager
                    .happy_path_drain(slot.epoch)
                    .is_some_and(|drain| {
                        drain.finalized.get(&slot) == Some(&hash)
                            || drain.selected.get(&slot) == Some(&hash)
                    })
            {
                self.rai_close_record_refresh_slots.insert(slot);
            }
        }
        // A root-only drain miss is satisfied by any locally validated
        // payload at that exact qualified root. The reverse index keeps this
        // proportional to the slots waiting on this root.
        let root_waiting = self
            .rai_missing_drain_payloads_by_root
            .get(&root)
            .cloned()
            .unwrap_or_default();
        for slot in root_waiting {
            self.rai_clear_missing_drain_payload(&slot);
        }

        // An exact signed reference is satisfied by payload availability, not
        // by election availability. If the election has not started yet, its
        // retained vote will admit this already-known fork during insertion.
        let waiting = self
            .rai_payload_incomplete_by_hash
            .get(&hash)
            .cloned()
            .unwrap_or_default();
        for slot in waiting {
            if slot.root == root {
                let _ = self.admit_candidate(slot.clone(), hash);
                self.rai_clear_payload_incomplete_hash(&slot, &hash);
            }
        }
    }

    /// Makes a durable block an election-local candidate.  The slot identity,
    /// rather than block arrival time, supplies the epoch classification.
    #[cfg(feature = "rai_protocol")]
    pub fn admit_candidate(
        &mut self,
        slot: crate::consensus::election::RaiSlotId,
        candidate: BlockHash,
    ) -> Result<(), super::CandidateError> {
        use super::CandidateError;
        use crate::consensus::election::RaiElectionId;

        let Some(block) = self.rai_blocks.get(&candidate).cloned() else {
            self.rai_mark_payload_incomplete(slot, candidate);
            return Err(CandidateError::UnknownBlock);
        };
        if block.qualified_root() != slot.root {
            return Err(CandidateError::InvalidSegment);
        }
        let election_id = RaiElectionId::Slot(slot.clone());
        if self.rai_epoch_manager.certified_release(&slot).is_some() {
            return Err(CandidateError::ElectionDisabled);
        }
        let Some(entry) = self.roots.election_for_rai_id_mut(&election_id) else {
            return Err(CandidateError::ElectionNotFound);
        };
        if !self
            .rai_epoch_manager
            .slot_election_enabled(slot.epoch, &slot.root)
            && !self
                .rai_epoch_manager
                .obligations_to_drain(slot.epoch)
                .is_some_and(|roots| roots.contains(&slot))
        {
            return Err(CandidateError::ElectionDisabled);
        }
        if self.rai_terminal_slots.contains_key(&slot) {
            return Err(CandidateError::FinalizedSlotConflict);
        }

        self.rai_candidate_hashes
            .entry(slot.clone())
            .or_default()
            .insert(candidate);

        // Exactly one block belongs to this election. Its predecessor, when
        // any, is handled by a distinct qualified-root election.
        let result = entry.try_add_fork(&block, Amount::ZERO);
        match result {
            AddForkResult::Added | AddForkResult::Duplicate => {
                self.roots.vote_router.connect(candidate, slot.root);
                Ok(())
            }
            AddForkResult::Replaced(removed) => {
                self.roots.vote_router.disconnect(&removed.hash());
                self.roots.vote_router.connect(candidate, slot.root);
                Ok(())
            }
            AddForkResult::ElectionEnded => Err(CandidateError::FinalizedSlotConflict),
            AddForkResult::TallyTooLow => Err(CandidateError::InvalidSegment),
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn known_block(&self, hash: &BlockHash) -> Option<&Block> {
        self.rai_blocks.get(hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn candidate_hashes_at_root(
        &self,
        root: &QualifiedRoot,
    ) -> impl Iterator<Item = &BlockHash> {
        self.rai_blocks_by_qualified_root
            .get(root)
            .into_iter()
            .flatten()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn slot_contains_candidate(
        &self,
        slot: &crate::consensus::election::RaiSlotId,
        hash: &BlockHash,
    ) -> bool {
        self.rai_candidate_hashes
            .get(slot)
            .is_some_and(|hashes| hashes.contains(hash))
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_with_rai_committee(
        config: ActiveElectionsConfig,
        base_latency: Duration,
        genesis_committee: std::sync::Arc<RepWeights>,
        genesis_governing_hash: BlockHash,
    ) -> Self {
        let mut result = Self::new(config, base_latency);
        result.rai_epoch_manager =
            crate::consensus::rai::RaiEpochManager::new(genesis_committee, genesis_governing_hash);
        result
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_set_open_started_at(&mut self, started_at: Timestamp) {
        self.rai_epoch_manager.set_open_started_at(started_at);
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn set_rai_ledger(&mut self, ledger: Arc<Ledger>) {
        self.rai_ledger = Some(ledger);
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_election_durations(
        &self,
    ) -> (
        &std::collections::BTreeMap<rsnano_types::RaiEpoch, Duration>,
        &std::collections::BTreeMap<rsnano_types::RaiEpoch, Duration>,
    ) {
        (
            &self.rai_cut_election_durations,
            &self.rai_record_election_durations,
        )
    }

    #[cfg(feature = "rai_protocol")]
    fn record_rai_close_election_duration(
        &mut self,
        id: &crate::consensus::election::RaiElectionId,
        now: Timestamp,
    ) {
        let Some(started_at) = self.rai_close_election_starts.get(id).copied() else {
            return;
        };
        let duration = started_at.elapsed(now);
        match id {
            crate::consensus::election::RaiElectionId::CloseCut { epoch, .. } => {
                self.rai_cut_election_durations
                    .entry(*epoch)
                    .or_insert(duration);
            }
            crate::consensus::election::RaiElectionId::CloseRecord { epoch, .. } => {
                self.rai_record_election_durations
                    .entry(*epoch)
                    .or_insert(duration);
            }
            crate::consensus::election::RaiElectionId::Slot(_) => {}
        }
    }

    pub fn set_observer(&mut self, observer: Sender<AecFact>) {
        self.observer = Some(observer);
    }

    pub fn max_len(&self) -> usize {
        self.max_elections
    }

    pub fn count_by_behavior(&self, behavior: ElectionBehavior) -> usize {
        self.count_by_behavior[behavior as usize]
    }

    fn count_by_behavior_mut(&mut self, behavior: ElectionBehavior) -> &mut usize {
        &mut self.count_by_behavior[behavior as usize]
    }

    pub fn bucket_len(&self, bucket_id: usize) -> usize {
        self.roots.bucket_len(bucket_id)
    }

    pub fn find_bucket(&self, root: &QualifiedRoot) -> Option<usize> {
        self.roots.find_bucket(root)
    }

    pub fn lowest_priority(&self, bucket_id: usize) -> Option<(QualifiedRoot, TimePriority)> {
        self.roots.lowest_priority(bucket_id)
    }

    /// Iterates over all elections in round robin fashion starting at the highest bucket
    pub fn iter_round_robin(&self) -> impl Iterator<Item = &Election> {
        self.roots
            .round_robin()
            .map(|i| &i.election)
            .filter(|election| {
                #[cfg(feature = "rai_protocol")]
                {
                    self.rai_election_vote_enabled(election.rai_id())
                }
                #[cfg(not(feature = "rai_protocol"))]
                {
                    let _ = election;
                    true
                }
            })
    }

    pub fn check_vacancy<T>(&self, source: &T) -> bool
    where
        T: ElectionCandidateSource,
    {
        let bucket_infos = self.roots.bucket_infos();
        source.should_schedule(&bucket_infos)
    }

    pub fn insert(
        &mut self,
        request: AecInsertRequest,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.ensure_not_stopped()?;
        self.ensure_not_recently_confirmed(&request)?;

        #[cfg(not(feature = "rai_protocol"))]
        if self.try_upgrade_priority_election(&request)? {
            return Ok(());
        }
        #[cfg(feature = "rai_protocol")]
        {
            let slot = crate::consensus::election::RaiSlotId {
                epoch: self.rai_epoch_manager.state().open_epoch,
                root: request.block.qualified_root(),
            };
            if !self
                .rai_epoch_manager
                .slot_election_enabled(slot.epoch, &slot.root)
            {
                return Err(AecInsertError::Duplicate);
            }
            if !self.retry_released_slots
                && self
                    .rai_first_visible_epoch_by_root
                    .get(&slot.root)
                    .is_some_and(|first_epoch| *first_epoch < slot.epoch)
            {
                return Err(AecInsertError::Duplicate);
            }
            let id = crate::consensus::election::RaiElectionId::Slot(slot);
            if self.roots.election_for_rai_id(&id).is_some() {
                return Err(AecInsertError::Duplicate);
            }
        }

        self.insert_new_election(request, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_election(
        &mut self,
        spec: super::RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        use crate::consensus::rai::{RaiCloseKind, rai_close_cut_root, rai_close_record_root};

        self.ensure_not_stopped()?;
        let tracker = match spec.id.kind {
            RaiCloseKind::Cut => self.rai_epoch_manager.close_cut_tracker(spec.id.epoch),
            RaiCloseKind::Record => self.rai_epoch_manager.close_record_tracker(spec.id.epoch),
        };
        let expected_root = match spec.id.kind {
            RaiCloseKind::Cut => rai_close_cut_root(spec.id.epoch, spec.id.round),
            RaiCloseKind::Record => rai_close_record_root(spec.id.epoch, spec.id.round),
        };
        if spec.root != expected_root
            || tracker
                .and_then(|tracker| tracker.round(spec.id.round))
                .is_none_or(|round| {
                    round.id != spec.id || !round.validated_preimages.contains(&spec.candidate)
                })
            || self
                .rai_epoch_manager
                .close_committee(spec.id.epoch)
                .is_none_or(|committee| committee != spec.committee)
        {
            return Err(AecInsertError::InvalidRaiCloseElection);
        }
        if self.roots.get(&spec.root).is_some() {
            return Err(AecInsertError::Duplicate);
        }

        let root = spec.root;
        let candidate = spec.candidate;
        let election = Election::new_close(
            spec.id,
            root.clone(),
            candidate,
            spec.committee,
            self.base_latency,
            now,
        );
        let election_id = election.rai_id().clone();
        self.rai_close_election_starts
            .entry(election_id.clone())
            .or_insert(now);
        if !self.roots.insert_rai(Entry {
            root: root.clone(),
            election,
            priority: rsnano_types::BlockPriority::default(),
        }) {
            return Err(AecInsertError::Duplicate);
        }
        // Close elections bypass ManualScheduler, which normally activates a
        // newly inserted manual election immediately. Match slot-election
        // scheduling so the confirmation solicitor can request close votes on
        // its next pass instead of waiting through the passive-duration gate.
        self.roots
            .election_for_rai_id_mut(&election_id)
            .expect("the close election was just inserted")
            .transition_active();
        if spec.id.kind == RaiCloseKind::Record
            && let Some(round) = self
                .rai_epoch_manager
                .close_record_tracker(spec.id.epoch)
                .and_then(|tracker| tracker.round(spec.id.round))
        {
            for hash in &round.validated_preimages {
                self.roots
                    .add_rai_hash_candidate_for_id(&election_id, *hash);
            }
        }
        self.rai_resolve_pending_timeout_votes(&election_id);
        self.apply_pending_rai_votes(&election_id, now);
        *self.count_by_behavior_mut(ElectionBehavior::Manual) += 1;
        self.stats.started(ElectionBehavior::Manual);
        self.notify(AecFact::ElectionStarted(candidate, root));
        Ok(())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_cut(
        &mut self,
        spec: super::RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        if spec.id.kind != crate::consensus::rai::RaiCloseKind::Cut {
            return Err(AecInsertError::InvalidRaiCloseElection);
        }
        self.insert_close_election(spec, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_record(
        &mut self,
        spec: super::RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        if spec.id.kind != crate::consensus::rai::RaiCloseKind::Record {
            return Err(AecInsertError::InvalidRaiCloseElection);
        }
        self.insert_close_election(spec, now)
    }

    /// Re-opens a certified-cut obligation which this replica did not have
    /// active when the cut was installed. This is deliberately tied to the
    /// closing epoch (rather than the successor epoch) so replayed durable
    /// votes pass their epoch/governing-hash checks and can complete drain.
    #[cfg(feature = "rai_protocol")]
    pub fn insert_drain_election(
        &mut self,
        block: SavedBlock,
        epoch: rsnano_types::RaiEpoch,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.ensure_not_stopped()?;
        let root = block.qualified_root();
        let rai_id = crate::consensus::election::RaiElectionId::Slot(
            crate::consensus::election::RaiSlotId {
                epoch,
                root: root.clone(),
            },
        );
        if self.roots.election_for_rai_id(&rai_id).is_some()
            || self
                .rai_terminal_slots
                .contains_key(&crate::consensus::election::RaiSlotId {
                    epoch,
                    root: root.clone(),
                })
        {
            return Err(AecInsertError::Duplicate);
        }
        if self
            .rai_epoch_manager
            .obligations_to_drain(epoch)
            .is_none_or(|obligations| {
                !obligations.contains(&crate::consensus::election::RaiSlotId {
                    epoch,
                    root: root.clone(),
                })
            })
        {
            return Err(AecInsertError::InvalidRaiCloseElection);
        }
        self.rai_epoch_manager
            .governing_hash(epoch)
            .ok_or(AecInsertError::MissingRaiGoverningClose)?;
        let committees = self
            .rai_epoch_manager
            .slot_committees(epoch)
            .ok_or(AecInsertError::MissingRaiGoverningClose)?;
        let hash = block.hash();
        let election = Election::new_slot(
            block,
            ElectionBehavior::Manual,
            self.base_latency,
            now,
            epoch,
        )
        .with_rai_committees(committees);
        let election_id = election.rai_id().clone();
        if !self.roots.insert_rai(Entry {
            root: root.clone(),
            election,
            priority: rsnano_types::BlockPriority::default(),
        }) {
            return Err(AecInsertError::Duplicate);
        }
        self.rai_resolve_pending_timeout_votes(&election_id);
        self.apply_pending_rai_votes(&election_id, now);
        *self.count_by_behavior_mut(ElectionBehavior::Manual) += 1;
        self.stats.started(ElectionBehavior::Manual);
        self.notify(AecFact::ElectionStarted(hash, root));
        Ok(())
    }

    fn ensure_not_stopped(&self) -> Result<(), AecInsertError> {
        if self.stopped {
            Err(AecInsertError::Stopped)
        } else {
            Ok(())
        }
    }

    fn ensure_not_recently_confirmed(
        &self,
        request: &AecInsertRequest,
    ) -> Result<(), AecInsertError> {
        let root = request.block.qualified_root();

        if self.recently_confirmed.root_exists(&root) {
            return Err(AecInsertError::RecentlyConfirmed);
        }
        Ok(())
    }

    #[cfg(not(feature = "rai_protocol"))]
    fn try_upgrade_priority_election(
        &mut self,
        request: &AecInsertRequest,
    ) -> Result<bool, AecInsertError> {
        let (upgraded, previous_behavior) = self.roots.try_upgrade_to_priority_election(request);

        if upgraded {
            *self.count_by_behavior_mut(previous_behavior.unwrap()) -= 1;
            *self.count_by_behavior_mut(request.behavior) += 1;
            Ok(true)
        } else if previous_behavior.is_some() {
            Err(AecInsertError::Duplicate)
        } else {
            Ok(false)
        }
    }

    fn insert_new_election(
        &mut self,
        request: AecInsertRequest,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        let root = request.block.qualified_root();
        let hash = request.block.hash();
        #[cfg(not(feature = "rai_protocol"))]
        let election = Election::new(request.block, request.behavior, self.base_latency, now);
        #[cfg(feature = "rai_protocol")]
        let epoch_state = self.rai_epoch_manager.state();
        #[cfg(feature = "rai_protocol")]
        let epoch = epoch_state.open_epoch;
        #[cfg(feature = "rai_protocol")]
        self.rai_epoch_manager
            .governing_hash(epoch)
            .ok_or(AecInsertError::MissingRaiGoverningClose)?;
        #[cfg(feature = "rai_protocol")]
        let committees = self
            .rai_epoch_manager
            .slot_committees(epoch)
            .ok_or(AecInsertError::MissingRaiGoverningClose)?;
        #[cfg(feature = "rai_protocol")]
        let election = Election::new_slot(
            request.block,
            request.behavior,
            self.base_latency,
            now,
            epoch,
        )
        .with_rai_committees(committees);

        #[cfg(feature = "rai_protocol")]
        self.rai_visible_obligations
            .entry(epoch)
            .or_default()
            .insert(crate::consensus::election::RaiSlotId {
                epoch,
                root: root.clone(),
            });

        #[cfg(feature = "rai_protocol")]
        self.rai_locally_inserted_slots
            .insert(crate::consensus::election::RaiSlotId {
                epoch,
                root: root.clone(),
            });

        #[cfg(feature = "rai_protocol")]
        self.rai_first_visible_epoch_by_root
            .entry(root.clone())
            .and_modify(|first_epoch| *first_epoch = (*first_epoch).min(epoch))
            .or_insert(epoch);

        #[cfg(feature = "rai_protocol")]
        self.rai_epoch_manager
            .record_known_slot(crate::consensus::election::RaiSlotId {
                epoch,
                root: root.clone(),
            });

        #[cfg(not(feature = "rai_protocol"))]
        self.roots.insert(Entry {
            root: root.clone(),
            election,
            priority: request.priority,
        });
        #[cfg(feature = "rai_protocol")]
        {
            let election_id = election.rai_id().clone();
            self.roots.insert_rai(Entry {
                root: root.clone(),
                election,
                priority: request.priority,
            });
            // A recreated election needs the complete durable archive again,
            // not the prior incarnation's consumed replay queue.
            self.rai_pending_vote_replay_complete.remove(&election_id);
            self.rai_pending_vote_leaves.remove(&election_id);
            self.rai_resolve_pending_timeout_votes(&election_id);
            self.apply_pending_rai_votes(&election_id, now);
        }

        *self.count_by_behavior_mut(request.behavior) += 1;
        self.stats.started(request.behavior);
        self.notify(AecFact::ElectionStarted(hash, root));
        Ok(())
    }

    #[cfg(feature = "rai_protocol")]
    fn apply_pending_rai_votes(
        &mut self,
        election_id: &crate::consensus::election::RaiElectionId,
        now: Timestamp,
    ) {
        if self.rai_pending_vote_replay_complete.contains(election_id) {
            return;
        }
        // Restart images and a few direct container callers may contain the
        // durable signed transports without the derived leaf index. Build it
        // lazily once; live retention maintains it incrementally thereafter.
        if !self.rai_pending_vote_leaves.contains_key(election_id) {
            let leaves = self
                .rai_pending_votes
                .get(election_id)
                .into_iter()
                .flatten()
                .flat_map(|vote| {
                    vote.rai_entries()
                        .filter(|(metadata, _)| metadata.election_id == *election_id)
                        .map(|(metadata, hash)| RaiPendingVoteLeaf {
                            voter: vote.voter,
                            timestamp: vote.timestamp(),
                            metadata: metadata.clone(),
                            hash: *hash,
                        })
                })
                .collect::<Vec<_>>();
            if !leaves.is_empty() {
                self.rai_pending_vote_leaves
                    .insert(election_id.clone(), leaves);
            }
        }
        // Vote-before-block-before-election is a valid delivery order. Publish
        // clears the payload-missing marker immediately; the signed transport
        // remains the durable slot reference and admits every now-known exact
        // fork before its cached leaves are replayed.
        if let crate::consensus::election::RaiElectionId::Slot(slot) = election_id {
            let known_candidates = self
                .rai_pending_vote_leaves
                .get(election_id)
                .into_iter()
                .flatten()
                .filter(|leaf| !leaf.hash.is_zero() && self.rai_blocks.contains_key(&leaf.hash))
                .map(|leaf| leaf.hash)
                .collect::<std::collections::HashSet<_>>();
            for candidate in known_candidates {
                let _ = self.admit_candidate(slot.clone(), candidate);
            }
        }
        let Some(leaves) = self.rai_pending_vote_leaves.get(election_id) else {
            return;
        };
        let mut entries = leaves
            .iter()
            .map(|leaf| (leaf.voter, leaf.timestamp, leaf.metadata.clone(), leaf.hash))
            .collect::<Vec<_>>();
        let Some(election) = self.roots.election_for_rai_id_mut(election_id) else {
            return;
        };
        // A close election can be created after its signed traffic arrives.
        // Replay First evidence before Notar evidence so a timeout vote can be
        // checked against the complete split certificate, independent of
        // network arrival order.
        entries.sort_by_key(|(_, _, metadata, _)| match metadata.phase {
            rsnano_types::RaiVotePhase::First => 0,
            rsnano_types::RaiVotePhase::Notar => 1,
            rsnano_types::RaiVotePhase::Final => 2,
        });
        let mut applied = std::collections::HashSet::new();
        for (voter, timestamp, metadata, hash) in entries {
            let close_first =
                election.is_rai_close() && metadata.phase == rsnano_types::RaiVotePhase::First;
            let timeout_vote =
                hash.is_zero() && metadata.phase != rsnano_types::RaiVotePhase::Final;
            if close_first || timeout_vote || election.contains_candidate(&hash) {
                let phase = metadata.phase;
                let _ = election.add_rai_vote(voter, hash, metadata, timestamp, now);
                applied.insert((voter, phase, hash));
            }
        }
        // Reconciliation may make an already cached final certificate
        // applicable without another vote arriving to drive ApplyVoteHelper.
        // RAI tallying ignores the legacy weight/quorum arguments and derives
        // its result from the election's frozen committee snapshots.
        if !applied.is_empty() {
            election.update_tallies(&Default::default(), Amount::ZERO);
        }
        if let Some(leaves) = self.rai_pending_vote_leaves.get_mut(election_id) {
            leaves.retain(|leaf| !applied.contains(&(leaf.voter, leaf.metadata.phase, leaf.hash)));
            if leaves.is_empty() {
                self.rai_pending_vote_leaves.remove(election_id);
                self.rai_pending_vote_replay_complete
                    .insert(election_id.clone());
            }
        }
    }

    pub fn try_add_fork(&mut self, fork: &Block, fork_tally: Amount) -> bool {
        let Some(entry) = self.roots.get_mut(&fork.qualified_root()) else {
            return false;
        };

        let result = entry.election.try_add_fork(fork, fork_tally);
        let added = match result {
            AddForkResult::Added => {
                self.notify(AecFact::BlockAddedToElection(fork.hash()));
                true
            }
            AddForkResult::Replaced(removed) => {
                self.roots.vote_router.disconnect(&removed.hash());
                self.notify(AecFact::BlockDiscarded(removed.into()));
                self.notify(AecFact::BlockAddedToElection(fork.hash()));
                true
            }
            AddForkResult::TallyTooLow => {
                self.notify(AecFact::BlockDiscarded(fork.clone()));
                false
            }
            AddForkResult::Duplicate | AddForkResult::ElectionEnded => false,
        };

        if added {
            self.roots
                .vote_router
                .connect(fork.hash(), fork.qualified_root());
            self.stats.conflicts += 1;
        }

        added
    }

    /// How many election slots are available
    /// This is a soft limit and can be negative!
    pub fn vacancy(&self) -> i64 {
        if self.cooldown.is_cooling_down() {
            return 0;
        }
        let current_size = self.roots.len() as i64;
        self.max_elections as i64 - current_size
    }

    pub fn set_cooldown(&mut self, cool_down: bool, reason: AecCooldownReason) {
        let result = self.cooldown.set_cooldown(cool_down, reason);
        if result == CooldownResult::Recovered {
            self.notify(AecFact::Recovered);
        }
    }

    pub fn stop(&mut self) {
        // destroy send queue so that the receiver thread will be stopped too
        drop(self.observer.take());
        self.stopped = true;
        self.roots.clear();
        #[cfg(feature = "rai_protocol")]
        self.rai_clear_payload_tracking();
    }

    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.roots.get(root).is_some()
    }

    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.roots.vote_router.is_active(block_hash)
    }

    /// Returns whether any hash in the batch is currently routed to an
    /// active election. Callers which already hold the AEC read guard can use
    /// this to scan an entire vote without re-entering the service lock for
    /// each leaf.
    pub fn any_active_hash(&self, block_hashes: &[BlockHash]) -> bool {
        block_hashes.iter().any(|hash| self.is_active_hash(hash))
    }

    pub fn was_recently_confirmed(&self, block_hash: &BlockHash) -> bool {
        self.recently_confirmed.hash_exists(block_hash)
    }

    pub fn clear_recently_confirmed(&mut self) {
        self.recently_confirmed.clear();
    }

    /// Returns the current active elections after transitioning
    pub fn transition_time(&mut self, now: Timestamp) {
        self.stats.ticked += 1;
        for entry in self.roots.iter_mut() {
            #[cfg(feature = "rai_protocol")]
            if entry.election.rai_requires_retention()
                && entry.election.state() == crate::consensus::election::ElectionState::Active
            {
                continue;
            }
            entry.election.transition_time(now);
        }
        self.erase_ended_elections();
    }

    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<&Election> {
        self.roots.election_for_root(root)
    }

    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<&Election> {
        self.roots.election_for_block(block_hash)
    }

    #[cfg(feature = "rai_protocol")]
    fn insert_rai_terminal_slot(
        &mut self,
        slot: crate::consensus::election::RaiSlotId,
        terminal: RaiTerminalSlot,
    ) {
        if matches!(
            terminal.outcome,
            crate::consensus::rai::RaiOutcome::Confirmed(_)
        ) && let Some(info) = terminal.frontier.clone()
        {
            self.rai_epoch_manager.record_ordinary_finalized_frontier(
                slot.epoch,
                terminal.account,
                info,
            );
        }
        let needs_frontier_refresh = terminal.frontier.is_none()
            || self
                .rai_epoch_manager
                .closing_epoch()
                .is_some_and(|closing| {
                    closing.epoch == slot.epoch
                        && closing.phase == crate::consensus::rai::RaiClosingPhase::ElectingRecord
                });
        let terminal_hash = match terminal.outcome {
            crate::consensus::rai::RaiOutcome::Notarized(hash)
            | crate::consensus::rai::RaiOutcome::Confirmed(hash) => Some(hash),
            crate::consensus::rai::RaiOutcome::Pending
            | crate::consensus::rai::RaiOutcome::TimedOut => None,
        };
        let hashes = terminal.hashes().collect::<Vec<_>>();
        if let Some(previous) = self.rai_terminal_slots.insert(slot.clone(), terminal) {
            for hash in previous.hashes() {
                let remove_hash =
                    self.rai_terminal_slots_by_hash
                        .get_mut(&hash)
                        .is_some_and(|slots| {
                            slots.remove(&slot);
                            slots.is_empty()
                        });
                if remove_hash {
                    self.rai_terminal_slots_by_hash.remove(&hash);
                }
            }
        }
        for hash in hashes {
            self.rai_terminal_slots_by_hash
                .entry(hash)
                .or_default()
                .insert(slot.clone());
        }
        self.rai_terminal_slots_by_request_root
            .entry(slot.root.root)
            .or_default()
            .insert(slot.clone());

        // A slot may become terminal after the initial drain sweep or while a
        // split close-record round is active. Queue only the exact hash already
        // selected by the certificate-derived drain; the bounded refresher
        // will reconstruct its payload before another fresh close vote.
        if needs_frontier_refresh
            && let Some(hash) = terminal_hash
            && self
                .rai_epoch_manager
                .happy_path_drain(slot.epoch)
                .is_some_and(|drain| {
                    drain.finalized.get(&slot) == Some(&hash)
                        || drain.selected.get(&slot) == Some(&hash)
                })
        {
            self.rai_close_record_refresh_slots.insert(slot);
        }
    }

    #[cfg(all(feature = "rai_protocol", test))]
    pub(crate) fn rai_missing_drain_elections(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<QualifiedRoot> {
        self.rai_epoch_manager
            .happy_path_drain(epoch)
            .map(|drain| {
                drain
                    .obligations
                    .iter()
                    .filter(|slot| {
                        let id = crate::consensus::election::RaiElectionId::Slot((*slot).clone());
                        !drain.finalized.contains_key(*slot)
                            && !drain.selected.contains_key(*slot)
                            && !drain.released.contains_key(*slot)
                            && self.roots.election_for_rai_id(&id).is_none()
                    })
                    .map(|slot| slot.root.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_vote_context(
        &self,
        block_hash: &BlockHash,
    ) -> Option<(rsnano_types::RaiVoteMetadata, bool)> {
        // Prefer the RAI-id index explicitly. The same ledger block can still
        // have an ordinary election entry, whose default metadata would cause
        // a repair-generated vote to be rejected by the RAI slot election.
        if let Some(election) = self
            .roots
            .rai_elections_for_candidate(block_hash)
            .find(|election| self.rai_election_vote_enabled(election.rai_id()))
        {
            return Some((election.rai_vote_metadata(), election.is_rai_close()));
        }
        if let Some(election) = self.election_for_block(block_hash)
            && self.rai_election_vote_enabled(election.rai_id())
        {
            return Some((election.rai_vote_metadata(), election.is_rai_close()));
        }

        // Terminal slot evidence is both exact and O(1)-indexed. Prefer it to
        // archived close traffic: the same digest can legitimately occur in
        // a synthetic close leaf and a slot certificate, and repair must keep
        // the slot's election identity.
        if let Some(context) = self
            .rai_terminal_slots_by_hash
            .get(block_hash)
            .into_iter()
            .flatten()
            .find(|slot| {
                self.rai_epoch_manager
                    .slot_election_enabled(slot.epoch, &slot.root)
            })
            .and_then(|slot| {
                let terminal = self.rai_terminal_slots.get(slot)?;
                self.rai_epoch_manager.governing_hash(slot.epoch)?;
                Some((
                    rsnano_types::RaiVoteMetadata {
                        election_id: crate::consensus::election::RaiElectionId::Slot(slot.clone()),
                        epoch: slot.epoch,
                        phase: match terminal.outcome {
                            crate::consensus::rai::RaiOutcome::Notarized(_) => {
                                rsnano_types::RaiVotePhase::Notar
                            }
                            crate::consensus::rai::RaiOutcome::Confirmed(_) => {
                                rsnano_types::RaiVotePhase::Final
                            }
                            _ => return None,
                        },
                        ..Default::default()
                    },
                    matches!(
                        terminal.outcome,
                        crate::consensus::rai::RaiOutcome::Confirmed(_)
                    ),
                ))
            })
        {
            return Some(context);
        }

        // A peer may request missing close votes after this replica has
        // already installed and removed its close election. This exact index
        // avoids searching the process-lifetime slot-vote store.
        self.rai_pending_close_contexts_by_hash
            .get(block_hash)
            .and_then(|contexts| contexts.first())
            .cloned()
            .map(|metadata| (metadata, true))
    }

    /// Resolves RAI vote contexts in input order while the caller holds one
    /// AEC read guard. Missing hashes deliberately remain positional `None`s.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_vote_contexts(
        &self,
        block_hashes: &[BlockHash],
    ) -> Vec<Option<(rsnano_types::RaiVoteMetadata, bool)>> {
        block_hashes
            .iter()
            .map(|hash| self.rai_vote_context(hash))
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_vote_context_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<rsnano_types::RaiVoteMetadata> {
        if let Some(election) = self
            .roots
            .rai_elections_for_request_root(root)
            .find(|election| election.is_rai_close())
        {
            return Some(election.rai_vote_metadata());
        }
        self.rai_pending_votes
            .iter()
            .find_map(|(id, votes)| {
                let root_matches = match id {
                    crate::consensus::election::RaiElectionId::CloseCut { epoch, round } => {
                        crate::consensus::rai::rai_close_cut_root(*epoch, *round).root == *root
                    }
                    crate::consensus::election::RaiElectionId::CloseRecord { epoch, round } => {
                        crate::consensus::rai::rai_close_record_root(*epoch, *round).root == *root
                    }
                    crate::consensus::election::RaiElectionId::Slot(_) => false,
                };
                root_matches.then(|| {
                    votes.iter().find_map(|vote| {
                        vote.rai_entries()
                            .find(|(metadata, _)| metadata.election_id == *id)
                            .map(|(metadata, _)| metadata.clone())
                    })
                })?
            })
            .or_else(|| {
                // A terminal close election is removed before every peer has
                // necessarily received its certificate.  Its synthetic root
                // remains derivable from durable epoch state, so keep serving
                // the locally retained signed votes after active cleanup.
                (0..=self.rai_epoch_manager.state().open_epoch.number()).find_map(|epoch| {
                    let epoch = rsnano_types::RaiEpoch::new(epoch);
                    let election_id = if crate::consensus::rai::rai_close_cut_root(epoch, 0).root
                        == *root
                    {
                        Some(crate::consensus::election::RaiElectionId::CloseCut {
                            epoch,
                            round: 0,
                        })
                    } else if crate::consensus::rai::rai_close_record_root(epoch, 0).root == *root {
                        Some(crate::consensus::election::RaiElectionId::CloseRecord {
                            epoch,
                            round: 0,
                        })
                    } else {
                        None
                    }?;
                    Some(rsnano_types::RaiVoteMetadata {
                        election_id,
                        epoch,
                        ..Default::default()
                    })
                })
            })
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_active_close_vote_target_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<rsnano_ledger::RaiFinalizedVoteTarget> {
        let election = self
            .roots
            .rai_elections_for_request_root(root)
            .find(|election| election.is_rai_close() && election.rai_requires_retention())?;
        let metadata = election.rai_vote_metadata();
        Some(rsnano_ledger::RaiFinalizedVoteTarget {
            election_id: metadata.election_id.clone(),
            hash: election.rai_request_hash(),
            root: *root,
            metadata,
        })
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_zero_hash_vote_request(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<RaiZeroHashVoteRequest> {
        // Classify an active wire root from the maintained reverse index
        // before consulting archival fallbacks. This is the hot path for slot
        // solicitation and prevents an O(active elections) close probe for
        // every leaf in a ConfirmReq batch.
        if let Some(election) = self
            .roots
            .rai_elections_for_request_root(root)
            .find(|election| election.is_rai_close())
        {
            let metadata = election.rai_vote_metadata();
            let target =
                election
                    .rai_requires_retention()
                    .then(|| rsnano_ledger::RaiFinalizedVoteTarget {
                        election_id: metadata.election_id.clone(),
                        hash: election.rai_request_hash(),
                        root: *root,
                        metadata: metadata.clone(),
                    });
            return Some(RaiZeroHashVoteRequest::Close { metadata, target });
        }
        if let Some(election) = self
            .roots
            .rai_elections_for_request_root(root)
            .find(|election| {
                election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot
                    && self.rai_election_vote_enabled(election.rai_id())
            })
        {
            let metadata = election.rai_vote_metadata();
            let target =
                (!election.state().has_ended()).then(|| rsnano_ledger::RaiFinalizedVoteTarget {
                    election_id: metadata.election_id.clone(),
                    hash: election.voting_hash(),
                    root: *root,
                    metadata: metadata.clone(),
                });
            return Some(RaiZeroHashVoteRequest::Slot { metadata, target });
        }

        // Removed close elections retain signed repair evidence; terminal
        // slots retain their epoch-qualified context. These cold paths keep
        // the previous close-before-slot precedence.
        if let Some(metadata) = self.rai_close_vote_context_for_root(root) {
            return Some(RaiZeroHashVoteRequest::Close {
                target: self.rai_active_close_vote_target_for_root(root),
                metadata,
            });
        }
        let metadata = self.rai_slot_vote_context_for_root(root)?;
        Some(RaiZeroHashVoteRequest::Slot {
            target: self.rai_active_slot_vote_target_for_root(root, metadata.epoch),
            metadata,
        })
    }

    /// Resolves zero-hash ConfirmReq leaves in input order. Duplicate roots
    /// are memoized, so a vectorized legacy request pays for each wire root at
    /// most once while the caller holds one AEC read guard.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_zero_hash_vote_requests(
        &self,
        roots: &[rsnano_types::Root],
    ) -> Vec<Option<RaiZeroHashVoteRequest>> {
        let mut resolved = HashMap::new();
        roots
            .iter()
            .map(|root| {
                resolved
                    .entry(*root)
                    .or_insert_with(|| self.rai_zero_hash_vote_request(root))
                    .clone()
            })
            .collect()
    }

    pub fn transition_active(&mut self, block_hash: &BlockHash) -> bool {
        let Some(election) = self.roots.election_for_block_mut(block_hash) else {
            return false;
        };
        election.transition_active();
        true
    }

    pub fn refill<T>(&mut self, source: &mut T, now: Timestamp)
    where
        T: ElectionCandidateSource,
    {
        if self.cooldown.is_cooling_down() {
            return;
        }

        let mut any_inserted = true;
        while any_inserted {
            any_inserted = false;
            for bucket_index in (0..self.roots.bucket_count()).rev() {
                let bucket = &self.roots.bucket_infos()[bucket_index];
                let bucket_vacancy = if self.len() >= self.max_elections {
                    0
                } else {
                    self.max_elections_per_bucket as isize - bucket.election_count as isize
                };

                let Some(candidate) = source.next_candidate(
                    bucket_index,
                    bucket_vacancy,
                    bucket.lowest_priority.time,
                ) else {
                    continue;
                };

                any_inserted = true;
                let root = candidate.block.qualified_root();
                if self.find_bucket(&root) == Some(candidate.bucket_id) {
                    self.stats.activate_failed_duplicate += 1;
                    continue;
                }

                if self.bucket_len(candidate.bucket_id) >= self.max_elections_per_bucket {
                    self.erase_lowest_prio_election(candidate.bucket_id);
                    self.stats.replaced += 1;
                }

                // TODO: Don't hard code priority election!
                match self.insert(
                    AecInsertRequest::new_priority(candidate.block, candidate.priority),
                    now,
                ) {
                    Ok(_) => {
                        self.stats.activate_success += 1;
                    }
                    Err(AecInsertError::RecentlyConfirmed) => {
                        self.stats.activate_failed_confirmed += 1;
                    }
                    Err(AecInsertError::Duplicate) => {
                        self.stats.activate_failed_duplicate += 1;
                    }
                    Err(AecInsertError::MissingRaiGoverningClose) => {}
                    #[cfg(feature = "rai_protocol")]
                    Err(AecInsertError::InvalidRaiCloseElection) => {}
                    Err(AecInsertError::Stopped) => {}
                }
            }
        }
    }

    pub fn remove_votes<'a>(
        &mut self,
        root: &QualifiedRoot,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        let Some(election) = self.roots.election_for_root_mut(root) else {
            return;
        };
        for voter in voters {
            election.remove_vote(voter);
        }
    }

    pub fn erase_ended_elections(&mut self) {
        let removed = self.roots.drain_filter(|i| {
            if !i.election.state().has_ended() {
                return false;
            }
            #[cfg(feature = "rai_protocol")]
            if i.election.rai_requires_retention() {
                return false;
            }
            #[cfg(feature = "rai_protocol")]
            debug_assert!(
                i.election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot
                    || i.election.rai_is_terminal()
            );
            true
        });

        for entry in removed {
            self.cleanup_election(entry);
        }
    }

    pub fn erase(&mut self, root: &QualifiedRoot) -> bool {
        #[cfg(feature = "rai_protocol")]
        if self
            .roots
            .get(root)
            .is_some_and(|entry| entry.election.rai_requires_retention())
        {
            return false;
        }
        #[cfg(feature = "rai_protocol")]
        let entry = self
            .roots
            .get(root)
            .map(|entry| entry.election.rai_id().clone())
            .and_then(|id| self.roots.erase_rai_id(&id));
        #[cfg(not(feature = "rai_protocol"))]
        let entry = self.roots.erase(root);
        let Some(entry) = entry else {
            return false;
        };
        self.cleanup_election(entry);
        true
    }

    pub fn erase_lowest_prio_election(&mut self, bucket_id: usize) {
        let Some((root, _)) = self.lowest_priority(bucket_id) else {
            return;
        };
        self.erase(&root);
    }

    fn cleanup_election(&mut self, entry: Entry) {
        let election = &entry.election;

        #[cfg(feature = "rai_protocol")]
        if election.is_rai_close() {
            self.rai_close_notarized_at.remove(election.rai_id());
        }

        #[cfg(feature = "rai_protocol")]
        if election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot {
            let confirmed = match election.winner() {
                rsnano_types::MaybeSavedBlock::Saved(block) => Some(
                    rsnano_types::ConfirmationHeightInfo::new(block.height(), block.hash()),
                ),
                rsnano_types::MaybeSavedBlock::Unsaved(_) => None,
            };
            // Only certificate-terminal evidence may leave the active set.
            // Pending slots remain active and solicited until they terminate.
            let terminal_slot = matches!(
                election.rai_votes.outcome,
                crate::consensus::rai::RaiOutcome::Confirmed(_)
            ) || (matches!(
                election.rai_votes.outcome,
                crate::consensus::rai::RaiOutcome::Notarized(_)
            ) && election.state().has_ended());
            if terminal_slot {
                self.insert_rai_terminal_slot(
                    crate::consensus::election::RaiSlotId {
                        epoch: election.rai_epoch(),
                        root: entry.root.clone(),
                    },
                    RaiTerminalSlot {
                        outcome: election.rai_votes.outcome,
                        account: election.account(),
                        frontier: confirmed,
                    },
                );
            }
        }

        // Keep track of election count by election type
        *self.count_by_behavior_mut(election.behavior()) -= 1;

        self.stats.stopped(&entry.election);
        self.notify(AecFact::ElectionEnded(entry.election));
    }

    /// Dependent elections are implicitly confirmed when their block is confirmed
    pub fn confirm_dependent_elections(
        &mut self,
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>)>,
        now: Timestamp,
    ) {
        for (confirmed_block, source_election) in confirmed {
            let confirmed_election =
                self.confirm_dependent_election(&confirmed_block, source_election, now);

            self.block_confirmed(confirmed_block, confirmed_election);
        }
    }

    fn confirm_dependent_election(
        &mut self,
        confirmed_block: &SavedBlock,
        source_election: Option<ConfirmedElection>,
        now: Timestamp,
    ) -> ConfirmedElection {
        // Check if the currently confirmed block was part of an election that triggered
        // the block confirmation
        if let Some(source) = source_election
            && confirmed_block.hash() == source.winner.hash()
        {
            // This is the block that was directly confirmed by the source election.
            // The election is already confirmed, so there is nothing to do.
            return source;
        }

        let Some(corresponding) = self.roots.get_mut(&confirmed_block.qualified_root()) else {
            return ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::InactiveConfirmationHeight,
            );
        };

        if corresponding.election.winner().hash() == confirmed_block.hash() {
            corresponding.election.force_confirm();
            corresponding
                .election
                .into_confirmed_election(now, ConfirmationType::ActiveConfirmationHeight)
        } else {
            corresponding.election.cancel();
            ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::ActiveConfirmationHeight,
            )
        }
    }

    fn block_confirmed(&mut self, block: SavedBlock, election: ConfirmedElection) {
        self.stats.block_confirmations[election.confirmation_type as usize] += 1;
        self.notify(AecFact::BlockConfirmed(block, election));
    }

    pub fn remove_recently_confirmed(&mut self, block_hash: &BlockHash) {
        self.recently_confirmed.erase(block_hash);
    }

    pub fn apply_vote<'a>(
        &mut self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        #[cfg(feature = "rai_protocol")]
        let (rai_entries, invalid_rai_results) = {
            let mut valid_entries = Vec::new();
            let mut invalid_results = HashMap::new();
            let mut represented_ids = std::collections::HashSet::new();

            // ConfirmAck's compact slot representation carries no qualified
            // root. Resolve it from an already validated block before using
            // the same election and retained-vote maps as ordinary votes.
            let mut resolved_transport = (*args.vote.vote.vote).clone();
            let mut transport_changed = false;
            let timeout_resolutions = (0..resolved_transport.len())
                .map(|index| {
                    let locator = resolved_transport.rai_timeout_slot(index)?;
                    let epoch = resolved_transport.rai_metadata(index).unwrap().epoch;
                    self.roots
                        .iter_rai()
                        .find(|entry| {
                            entry.election.rai_epoch() == epoch
                                && entry.election.account() == locator.account
                                && entry.election.rai_slot_height() == locator.height
                        })
                        .map(|entry| entry.election.rai_id().clone())
                })
                .collect::<Vec<_>>();
            for (index, (metadata, target)) in resolved_transport.rai_entries_mut().enumerate() {
                let hash = match target {
                    rsnano_types::RaiVoteTarget::Hash(hash) => hash,
                    rsnano_types::RaiVoteTarget::Timeout(_) => &BlockHash::ZERO,
                };
                let crate::consensus::election::RaiElectionId::Slot(slot) = &metadata.election_id
                else {
                    continue;
                };
                if let Some(resolved_id) = &timeout_resolutions[index]
                    && slot.root == QualifiedRoot::ZERO
                    && args.vote.vote.vote.rai_timeout_slot(index).is_some()
                {
                    metadata.election_id = resolved_id.clone();
                    transport_changed = true;
                    continue;
                }
                if !args.vote.vote.vote.is_compact_rai_slot()
                    || slot.root != QualifiedRoot::ZERO
                    || hash.is_zero()
                {
                    continue;
                }
                if let Some(block) = self.rai_blocks.get(hash) {
                    metadata.election_id = crate::consensus::election::RaiElectionId::Slot(
                        crate::consensus::election::RaiSlotId {
                            epoch: slot.epoch,
                            root: block.qualified_root(),
                        },
                    );
                    transport_changed = true;
                }
            }
            let resolved_transport = if transport_changed {
                Arc::new(resolved_transport)
            } else {
                args.vote.vote.vote.clone()
            };

            for (index, (metadata, hash)) in resolved_transport
                .rai_entries()
                .enumerate()
                .filter(|(_, (_, hash))| args.vote.filter.is_zero() || **hash == args.vote.filter)
            {
                if let Some(locator) = resolved_transport.rai_timeout_slot(index)
                    && matches!(
                        &metadata.election_id,
                        crate::consensus::election::RaiElectionId::Slot(slot)
                            if slot.root == QualifiedRoot::ZERO
                    )
                {
                    let pending = self
                        .rai_pending_timeout_slot_votes
                        .entry((metadata.epoch, locator))
                        .or_default();
                    if !pending.iter().any(|existing| {
                        existing.voter == args.vote.voter
                            && existing.signature == args.vote.signature
                    }) {
                        pending.push(args.vote.vote.vote.clone());
                    }
                    invalid_results.insert(*hash, Err(VoteError::Indeterminate));
                    continue;
                }
                if let crate::consensus::election::RaiElectionId::Slot(slot) = &metadata.election_id
                    && resolved_transport.is_compact_rai_slot()
                    && slot.root == QualifiedRoot::ZERO
                    && !hash.is_zero()
                {
                    let pending = self
                        .rai_pending_compact_slot_votes
                        .entry((slot.epoch, *hash))
                        .or_default();
                    if !pending.iter().any(|existing| {
                        existing.voter == args.vote.voter
                            && existing.signature == args.vote.signature
                    }) {
                        pending.push(args.vote.vote.vote.clone());
                    }
                    invalid_results.insert(*hash, Err(VoteError::Indeterminate));
                    continue;
                }
                // Contextless ConfirmReq replies are signed only to prove
                // which representative owns the direct channel used by
                // RepCrawler. Reject each discovery leaf before candidate
                // references, persistent retention, or election tallies can
                // observe it. Other leaves in the signed batch remain usable.
                let election_epoch = match &metadata.election_id {
                    crate::consensus::election::RaiElectionId::Slot(slot) => slot.epoch,
                    crate::consensus::election::RaiElectionId::CloseCut { epoch, .. }
                    | crate::consensus::election::RaiElectionId::CloseRecord { epoch, .. } => {
                        *epoch
                    }
                };
                // The governing close is an implicit, deterministic part of
                // each leaf's epoch context. Never retain or apply that leaf
                // until the certified state is locally available.
                if metadata.is_discovery()
                    || election_epoch != metadata.epoch
                    || self
                        .rai_epoch_manager
                        .governing_hash(election_epoch)
                        .is_none()
                    || !self.rai_election_vote_enabled(&metadata.election_id)
                {
                    invalid_results.insert(*hash, Err(VoteError::Invalid));
                    continue;
                }

                if let crate::consensus::election::RaiElectionId::Slot(slot) = &metadata.election_id
                    && !hash.is_zero()
                {
                    // Preserve the signed slot identity even when the payload
                    // is absent. Deriving the root later from the block hash
                    // loses exactly the information ZERO-hash drain repair
                    // needs, and timeout leaves never denote payloads.
                    let _ = self.admit_candidate(slot.clone(), *hash);
                }
                represented_ids.insert(metadata.election_id.clone());
                valid_entries.push((metadata.clone(), *hash));
            }

            // Every signed vote is durable quorum material until its epoch's
            // close record is installed. In particular, a drain replica may
            // have missed a slot's First votes; regenerating only the current
            // Notar phase cannot repair the progression proof for those votes.
            // Retain the original signed transport under every represented ID;
            // replay filters its leaves by the map key.
            for election_id in represented_ids {
                // VoteProcessor verifies the signature before entering the
                // AEC. For a given signer the signature is therefore the
                // transport identity and already binds every metadata/hash
                // leaf. The helper also indexes close leaves once, without
                // mixing the much larger slot-vote store into hash lookup.
                self.rai_retain_pending_vote(election_id, resolved_transport.clone());
            }
            if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                eprintln!(
                    "RAI_MSG pr={pr} event=apply_vote entries={:?} voter={} vote_hash={} hashes={:?}",
                    valid_entries,
                    args.vote.voter,
                    args.vote.vote.hash(),
                    args.vote.vote.hashes().collect::<Vec<_>>()
                );
            }
            (valid_entries, invalid_results)
        };

        let apply_result = {
            let mut apply_helper = ApplyVoteHelper {
                args: &args,
                recently_confirmed: &mut self.recently_confirmed,
                vote_counter: &mut self.stats.vote_counter,
                observer: &self.observer,
                roots: &mut self.roots,
            };
            #[cfg(feature = "rai_protocol")]
            {
                apply_helper.apply_rai_entries(&rai_entries)
            }
            #[cfg(not(feature = "rai_protocol"))]
            {
                apply_helper.apply_vote()
            }
        };
        #[cfg(feature = "rai_protocol")]
        let mut result = apply_result;
        #[cfg(not(feature = "rai_protocol"))]
        let result = apply_result;
        #[cfg(feature = "rai_protocol")]
        for (hash, invalid_result) in invalid_rai_results {
            if let Some(existing) = result.per_block.get_mut(&hash) {
                // The compatibility result is hash-keyed, while every leaf is
                // processed independently. Any accepted context wins.
                if existing.is_err() {
                    *existing = invalid_result;
                }
            } else {
                result.per_block.insert(hash, invalid_result);
            }
        }
        for entry in result.confirmed {
            #[cfg(feature = "rai_protocol")]
            if entry.election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot {
                let confirmed = match entry.election.winner() {
                    rsnano_types::MaybeSavedBlock::Saved(block) => Some(
                        rsnano_types::ConfirmationHeightInfo::new(block.height(), block.hash()),
                    ),
                    rsnano_types::MaybeSavedBlock::Unsaved(_) => None,
                };
                if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                    eprintln!(
                        "RAI_MSG pr={pr} event=slot_terminal epoch={:?} root={:?} outcome={:?} confirmed={confirmed:?}",
                        entry.election.rai_epoch(),
                        entry.root,
                        entry.election.rai_votes.outcome
                    );
                }
                if matches!(
                    entry.election.rai_votes.outcome,
                    crate::consensus::rai::RaiOutcome::Notarized(_)
                        | crate::consensus::rai::RaiOutcome::Confirmed(_)
                ) {
                    self.insert_rai_terminal_slot(
                        crate::consensus::election::RaiSlotId {
                            epoch: entry.election.rai_epoch(),
                            root: entry.root.clone(),
                        },
                        RaiTerminalSlot {
                            outcome: entry.election.rai_votes.outcome,
                            account: entry.election.account(),
                            frontier: confirmed,
                        },
                    );
                }
            }
            #[cfg(feature = "rai_protocol")]
            if matches!(
                entry.election.rai_kind(),
                crate::consensus::election::RaiElectionKind::CloseCut
                    | crate::consensus::election::RaiElectionKind::CloseRecord
            ) {
                let mut successor = None;
                let epoch = entry.election.rai_epoch();
                let round = entry.election.rai_round();
                let candidate = entry.election.rai_votes.outcome;
                if matches!(candidate, crate::consensus::rai::RaiOutcome::Confirmed(_)) {
                    let duration = entry.election.start().elapsed(args.now);
                    match entry.election.rai_kind() {
                        crate::consensus::election::RaiElectionKind::CloseCut => {
                            self.rai_cut_election_durations
                                .entry(epoch)
                                .or_insert(duration);
                        }
                        crate::consensus::election::RaiElectionKind::CloseRecord => {
                            self.rai_record_election_durations
                                .entry(epoch)
                                .or_insert(duration);
                        }
                        crate::consensus::election::RaiElectionKind::Slot => unreachable!(),
                    }
                }
                let evidence = entry.election.rai_votes.clone();
                let stored = match entry.election.rai_kind() {
                    crate::consensus::election::RaiElectionKind::CloseCut => self
                        .rai_epoch_manager
                        .store_close_cut_evidence(epoch, round, evidence),
                    crate::consensus::election::RaiElectionKind::CloseRecord => self
                        .rai_epoch_manager
                        .store_close_record_evidence(epoch, round, evidence),
                    crate::consensus::election::RaiElectionKind::Slot => false,
                };
                if stored {
                    if let crate::consensus::rai::RaiOutcome::Confirmed(hash) = candidate {
                        match entry.election.rai_kind() {
                            crate::consensus::election::RaiElectionKind::CloseCut => {
                                let decided = self
                                    .rai_epoch_manager
                                    .decide_close_cut(epoch, round, hash)
                                    .map(|obligations| obligations.clone());
                                if decided.is_ok() {
                                    self.cleanup_rai_close_rounds(
                                        crate::consensus::rai::RaiCloseKind::Cut,
                                        epoch,
                                    );
                                }
                                if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                                    eprintln!(
                                        "RAI_MSG pr={pr} event=cut_decided epoch={epoch:?} round={round} hash={hash} result={decided:?}"
                                    );
                                }
                            }
                            crate::consensus::election::RaiElectionKind::CloseRecord => {
                                if self
                                    .install_close_record_with_commit(epoch, round, hash, None)
                                    .is_ok()
                                {
                                    self.cleanup_rai_close_rounds(
                                        crate::consensus::rai::RaiCloseKind::Record,
                                        epoch,
                                    );
                                    let removed = self.roots.drain_filter(|entry| {
                                        let crate::consensus::election::RaiElectionId::Slot(slot) =
                                            entry.election.rai_id()
                                        else {
                                            return false;
                                        };
                                        self.rai_epoch_manager.certified_release(slot).is_some()
                                    });
                                    for released in removed {
                                        self.cleanup_election(released);
                                    }
                                    self.prune_rai_evidence_through(epoch);
                                }
                            }
                            crate::consensus::election::RaiElectionKind::Slot => {}
                        }
                    } else if candidate == crate::consensus::rai::RaiOutcome::TimedOut {
                        let kind = match entry.election.rai_kind() {
                            crate::consensus::election::RaiElectionKind::CloseCut => {
                                crate::consensus::rai::RaiCloseKind::Cut
                            }
                            crate::consensus::election::RaiElectionKind::CloseRecord => {
                                crate::consensus::rai::RaiCloseKind::Record
                            }
                            crate::consensus::election::RaiElectionKind::Slot => unreachable!(),
                        };
                        let next = match kind {
                            crate::consensus::rai::RaiCloseKind::Cut => {
                                // Death may be learned between periodic close
                                // ticks. Recompute the fresh successor from
                                // the complete report store now, rather than
                                // carrying the replica-relative round-opening
                                // preference into the next round.
                                let _ = self.rai_epoch_manager.refresh_close_cut_candidate(
                                    epoch,
                                    round,
                                    std::iter::empty(),
                                );
                                self.rai_epoch_manager.advance_close_cut_round()
                            }
                            crate::consensus::rai::RaiCloseKind::Record => {
                                if self
                                    .rai_refresh_close_record_frontiers_from_attached_ledger(epoch)
                                {
                                    self.rai_epoch_manager
                                        .advance_close_record_round(std::iter::empty())
                                } else {
                                    None
                                }
                            }
                        };
                        if let Some((root, hash)) = next
                            && let Some(committee) = self.rai_epoch_manager.close_committee(epoch)
                        {
                            let round = match kind {
                                crate::consensus::rai::RaiCloseKind::Cut => {
                                    self.rai_epoch_manager.close_cut_round(epoch)
                                }
                                crate::consensus::rai::RaiCloseKind::Record => {
                                    self.rai_epoch_manager.close_record_round(epoch)
                                }
                            };
                            if let Some(round) = round {
                                successor = Some(super::RaiCloseElectionSpec {
                                    id: crate::consensus::rai::RaiCloseElectionId {
                                        kind,
                                        epoch,
                                        round,
                                    },
                                    root,
                                    candidate: hash,
                                    committee,
                                });
                            }
                        }
                    }
                }
                self.cleanup_election(entry);
                if let Some(spec) = successor {
                    let _ = self.insert_close_election(spec, args.now);
                }
                continue;
            }
            self.cleanup_election(entry);
        }
        result.per_block
    }

    #[cfg(feature = "rai_protocol")]
    fn prune_rai_evidence_through(&mut self, closed_epoch: rsnano_types::RaiEpoch) {
        self.rai_prune_payload_tracking_through(closed_epoch);
        self.rai_close_record_refresh_slots
            .retain(|slot| slot.epoch > closed_epoch);
        self.rai_terminal_slots
            .retain(|slot, _| slot.epoch > closed_epoch);
        self.rai_terminal_slots_by_hash.retain(|_, slots| {
            slots.retain(|slot| slot.epoch > closed_epoch);
            !slots.is_empty()
        });
        self.rai_terminal_slots_by_request_root.retain(|_, slots| {
            slots.retain(|slot| slot.epoch > closed_epoch);
            !slots.is_empty()
        });
        // Slot evidence is represented durably by the installed close state.
        // Close vote leaves, however, remain the only authenticated material
        // from which a lagging replica can derive the cut/record certificate;
        // retain them for process-lifetime archival repair.
        self.rai_pending_votes.retain(|id, _| match id {
            crate::consensus::election::RaiElectionId::Slot(slot) => slot.epoch > closed_epoch,
            crate::consensus::election::RaiElectionId::CloseCut { .. }
            | crate::consensus::election::RaiElectionId::CloseRecord { .. } => true,
        });
        self.rai_pending_vote_leaves.retain(|id, _| match id {
            crate::consensus::election::RaiElectionId::Slot(slot) => slot.epoch > closed_epoch,
            crate::consensus::election::RaiElectionId::CloseCut { .. }
            | crate::consensus::election::RaiElectionId::CloseRecord { .. } => true,
        });
        self.rai_pending_vote_replay_complete.retain(|id| match id {
            crate::consensus::election::RaiElectionId::Slot(slot) => slot.epoch > closed_epoch,
            crate::consensus::election::RaiElectionId::CloseCut { .. }
            | crate::consensus::election::RaiElectionId::CloseRecord { .. } => true,
        });
        self.rai_pending_compact_slot_votes
            .retain(|(epoch, _), _| *epoch > closed_epoch);
        self.rai_pending_timeout_slot_votes
            .retain(|(epoch, _), _| *epoch > closed_epoch);
    }

    pub fn force_confirm(&mut self, block_hash: &BlockHash, now: Timestamp) {
        let Some(election) = self.roots.election_for_block_mut(block_hash) else {
            panic!("Force confirm failed, because no active election was found");
        };
        if election.force_confirm() {
            let confirmed_election =
                election.into_confirmed_election(now, ConfirmationType::ActiveConfirmedQuorum);
            self.notify(AecFact::ElectionConfirmed(confirmed_election));
        }
    }

    pub fn cancel(&mut self, root: &QualifiedRoot) {
        if let Some(entry) = self.roots.get_mut(root) {
            entry.election.cancel();
        }
    }

    pub fn cancel_all(&mut self) {
        for entry in self.roots.iter_mut() {
            entry.election.cancel();
        }
    }

    pub fn len(&self) -> usize {
        self.roots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn info(&self, now: Timestamp) -> ActiveElectionsInfo {
        ActiveElectionsInfo {
            max_elections: self.max_elections,
            total: self.roots.len(),
            stale: self
                .roots
                .iter()
                .filter(|i| i.election.start().elapsed(now) >= Duration::from_secs(60))
                .count(),
            priority: self.count_by_behavior(ElectionBehavior::Priority),
            hinted: self.count_by_behavior(ElectionBehavior::Hinted),
            optimistic: self.count_by_behavior(ElectionBehavior::Optimistic),
        }
    }

    pub fn simulate_event(&self, event: AecFact) {
        self.notify(event);
    }

    pub fn snapshot(&self, now: Timestamp) -> AecSnapshot {
        self.roots.snapshot(now)
    }

    fn notify(&self, event: AecFact) {
        if let Some(sender) = &self.observer {
            sender.send(event).unwrap()
        }
    }
}

impl Default for ActiveElectionsContainer {
    fn default() -> Self {
        Self::new(ActiveElectionsConfig::default(), Duration::from_secs(1))
    }
}

impl StatsSource for ActiveElectionsContainer {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.cooldown.collect_stats(result);
        self.stats.collect_stats(result);
    }
}

impl ContainerInfoProvider for ActiveElectionsContainer {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .leaf("roots", self.roots.len(), RootContainer::ELEMENT_SIZE)
            .leaf(
                "normal",
                self.count_by_behavior(ElectionBehavior::Priority),
                0,
            )
            .leaf(
                "hinted".to_string(),
                self.count_by_behavior(ElectionBehavior::Hinted),
                0,
            )
            .leaf(
                "optimistic".to_string(),
                self.count_by_behavior(ElectionBehavior::Optimistic),
                0,
            )
            .node(
                "recently_confirmed",
                self.recently_confirmed.container_info(),
            )
            .node("vote_router", self.roots.vote_router.container_info())
            .node("buckets", self.roots.container_info())
            .finish()
    }
}

pub struct ApplyVoteArgs<'a> {
    pub vote: &'a FilteredVote,
    pub rep_weights: &'a RepWeights,
    pub quorum_snapshot: &'a QuorumSnapshot,
    pub now: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "rai_protocol"))]
    use crate::consensus::ReceivedVote;
    use rsnano_types::{BlockPriority, TimePriority};
    #[cfg(not(feature = "rai_protocol"))]
    use rsnano_types::{PrivateKey, Vote, VoteDelivery};
    #[cfg(not(feature = "rai_protocol"))]
    use std::sync::Arc;

    #[cfg(feature = "rai_protocol")]
    fn drain_slot(value: u64) -> crate::consensus::election::RaiSlotId {
        crate::consensus::election::RaiSlotId {
            epoch: rsnano_types::RaiEpoch::ZERO,
            root: QualifiedRoot::new(value.into(), (value + 100).into()),
        }
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_install_diagnostics_use_a_bounded_window_after_the_cursor() {
        let frontiers = (1..=100)
            .map(|value| {
                (
                    rsnano_types::Account::from(value),
                    rsnano_types::ConfirmationHeightInfo::new(value, BlockHash::from(value)),
                )
            })
            .collect();

        let (first, first_truncated) = rai_close_install_diagnostic_window(&frontiers, None, 8);
        assert_eq!(
            first.keys().copied().collect::<Vec<_>>(),
            (1..=8).map(rsnano_types::Account::from).collect::<Vec<_>>()
        );
        assert!(first_truncated);

        let (after_cursor, after_cursor_truncated) = rai_close_install_diagnostic_window(
            &frontiers,
            Some(rsnano_types::Account::from(95)),
            8,
        );
        assert_eq!(
            after_cursor.keys().copied().collect::<Vec<_>>(),
            (96..=100)
                .map(rsnano_types::Account::from)
                .collect::<Vec<_>>()
        );
        assert!(!after_cursor_truncated);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_install_advances_complete_window_before_later_missing_dependency() {
        let missing = BlockHash::from(999_999);
        let mut blocks = Vec::new();
        let mut frontiers = crate::consensus::rai::RaiFrontierMap::new();
        for value in 1..=RAI_CLOSE_CEMENT_ROOTS_PER_PASS + 1 {
            let account = rsnano_types::Account::from(value as u64);
            let previous = if value == RAI_CLOSE_CEMENT_ROOTS_PER_PASS + 1 {
                missing
            } else {
                BlockHash::ZERO
            };
            let block = rsnano_types::TestBlockBuilder::state()
                .account(account)
                .previous(previous)
                .representative(rsnano_types::PublicKey::from(value as u64 + 1_000))
                .link(rsnano_types::Link::ZERO)
                .is_send()
                .build_saved();
            frontiers.insert(
                account,
                rsnano_types::ConfirmationHeightInfo::new(block.height(), block.hash()),
            );
            blocks.push(block);
        }
        let ledger = Ledger::new_null_builder().blocks(&blocks).finish();
        let epoch = rsnano_types::RaiEpoch::ZERO;
        let expected_cursor = *frontiers
            .keys()
            .nth(RAI_CLOSE_CEMENT_ROOTS_PER_PASS - 1)
            .unwrap();
        let later_account = *frontiers.keys().next_back().unwrap();
        let mut cursors = std::collections::BTreeMap::new();

        assert!(!ActiveElectionsContainer::commit_rai_close_frontiers(
            Some(&ledger),
            epoch,
            &frontiers,
            &mut cursors,
        ));
        assert_eq!(cursors.get(&epoch), Some(&expected_cursor));
        for (account, frontier) in frontiers.iter().take(RAI_CLOSE_CEMENT_ROOTS_PER_PASS) {
            assert!(ledger.rai_close_frontier_is_committed(epoch, account, frontier));
        }
        assert!(!ledger.rai_close_frontier_is_committed(
            epoch,
            &later_account,
            &frontiers[&later_account],
        ));

        // The next pass reaches the incomplete root and preserves the last
        // durable cursor while repair fetches its missing predecessor.
        assert!(!ActiveElectionsContainer::commit_rai_close_frontiers(
            Some(&ledger),
            epoch,
            &frontiers,
            &mut cursors,
        ));
        assert_eq!(cursors.get(&epoch), Some(&expected_cursor));
        assert_eq!(
            ledger.rai_missing_close_dependencies(
                epoch,
                &std::collections::BTreeMap::from([(
                    later_account,
                    frontiers[&later_account].clone(),
                )]),
                1,
            ),
            vec![missing]
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn drain_check_schedule_rotates_unresolved_slots_fairly_and_is_epoch_scoped() {
        let first = drain_slot(1);
        let second = drain_slot(2);
        let third = drain_slot(3);
        let mut schedule = RaiDrainCheckSchedule::resolving(
            rsnano_types::RaiEpoch::ZERO,
            [first.clone(), second.clone(), third.clone()],
        );
        assert!(schedule.is_for_epoch(rsnano_types::RaiEpoch::ZERO));
        assert!(!schedule.is_for_epoch(rsnano_types::RaiEpoch::new(1)));

        let first_window = schedule.take_window(2);
        assert_eq!(first_window, vec![first.clone(), second.clone()]);
        for slot in first_window {
            schedule.requeue_unresolved(slot);
        }

        assert_eq!(schedule.take_window(2), vec![third, first]);
        assert!(!schedule.ready_for_close());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn drain_check_schedule_requires_the_complete_durable_upgrade_sweep() {
        let first = drain_slot(1);
        let second = drain_slot(2);
        let third = drain_slot(3);
        let mut schedule =
            RaiDrainCheckSchedule::resolving(rsnano_types::RaiEpoch::ZERO, std::iter::empty());

        assert!(!schedule.ready_for_close());
        schedule.begin_durable_upgrade([first.clone(), second.clone(), third.clone()]);
        assert!(!schedule.ready_for_close());
        assert_eq!(schedule.take_window(2), vec![first, second]);
        assert!(!schedule.ready_for_close());
        assert_eq!(schedule.take_window(2), vec![third]);
        assert!(schedule.ready_for_close());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn unresolved_drain_repair_window_is_bounded_and_excludes_settled_slots() {
        use rsnano_types::{Amount, BlockHash, PrivateKey, RaiEpoch};

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let slots = (1..=3).map(drain_slot).collect::<Vec<_>>();
        assert!(container.rai_epoch_manager.start_closing(now));
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(crate::consensus::rai::RaiReport::new(
                &key,
                RaiEpoch::ZERO,
                slots.clone(),
            ))
            .unwrap();
        let (_, cut_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut_hash)
            .unwrap();
        assert!(
            container
                .rai_epoch_manager
                .initialize_drain_frontiers(RaiEpoch::ZERO, [])
        );
        container.rai_drain_check_schedule = Some(RaiDrainCheckSchedule::resolving(
            RaiEpoch::ZERO,
            slots.clone(),
        ));

        assert_eq!(
            container.rai_unresolved_drain_slots(RaiEpoch::ZERO, 2),
            slots[..2]
        );
        assert_eq!(
            container.rai_epoch_manager.record_notarized_drain(
                RaiEpoch::ZERO,
                &slots[0],
                BlockHash::from(99),
                [],
            ),
            Some(crate::consensus::rai::RaiDrainOutcome::Selected(
                BlockHash::from(99)
            ))
        );
        assert_eq!(
            container.rai_unresolved_drain_slots(RaiEpoch::ZERO, 2),
            vec![slots[2].clone(), slots[1].clone()]
        );

        container
            .rai_drain_check_schedule
            .as_mut()
            .unwrap()
            .begin_durable_upgrade(slots);
        assert!(
            container
                .rai_unresolved_drain_slots(RaiEpoch::ZERO, 2)
                .is_empty()
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn evidence_repair_rotation_is_independent_and_fair() {
        let slots = (1..=512).map(drain_slot).collect::<Vec<_>>();
        let mut schedule =
            RaiDrainCheckSchedule::resolving(rsnano_types::RaiEpoch::ZERO, slots.clone());
        let mut repaired = std::collections::HashSet::new();

        for _ in 0..32 {
            // Model the production ledger-check stride which previously made
            // a first-16 repair sample alias forever for a 512-slot queue.
            for slot in schedule.take_window(RAI_DRAIN_CHECKS_PER_TICK) {
                schedule.requeue_unresolved(slot);
            }
            repaired.extend(schedule.take_evidence_repair_window(16, |_| true));
        }

        assert_eq!(repaired.len(), slots.len());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn evidence_repair_reaches_a_live_tail_behind_a_large_settled_prefix() {
        let slots = (1..=25_001).map(drain_slot).collect::<Vec<_>>();
        let tail = slots.last().unwrap().clone();
        let mut schedule = RaiDrainCheckSchedule::resolving(rsnano_types::RaiEpoch::ZERO, slots);

        let mut found = Vec::new();
        for _ in 0..4 {
            found.extend(schedule.take_evidence_repair_window(16, |slot| *slot == tail));
        }

        assert_eq!(found, vec![tail]);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_record_waits_for_all_upgrade_pages_and_refreshes_a_late_selected_frontier() {
        use rsnano_types::{
            Account, Amount, BlockHash, ConfirmationHeightInfo, PrivateKey, RaiEpoch,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let slots = (0..=RAI_DRAIN_CHECKS_PER_TICK)
            .map(|index| drain_slot(index as u64 + 1))
            .collect::<Vec<_>>();

        assert!(container.rai_epoch_manager.start_closing(now));
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(crate::consensus::rai::RaiReport::new(
                &key,
                RaiEpoch::ZERO,
                slots.clone(),
            ))
            .unwrap();
        let (_, cut_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut_hash)
            .unwrap();
        assert!(
            container
                .rai_epoch_manager
                .initialize_drain_frontiers(RaiEpoch::ZERO, [])
        );
        for (index, slot) in slots.iter().enumerate() {
            assert_eq!(
                container.rai_epoch_manager.record_notarized_drain(
                    RaiEpoch::ZERO,
                    slot,
                    BlockHash::from(index as u64 + 1),
                    [],
                ),
                Some(crate::consensus::rai::RaiDrainOutcome::Selected(
                    BlockHash::from(index as u64 + 1)
                ))
            );
        }

        // The last selected winner was initially unsaved. Its terminal marker
        // gains a frontier before the bounded completion sweep reaches it.
        let last_slot = slots.last().unwrap().clone();
        let last_hash = BlockHash::from(slots.len() as u64);
        let account = Account::from(9);
        let frontier = ConfirmationHeightInfo::new(3, last_hash);
        container.insert_rai_terminal_slot(
            last_slot,
            RaiTerminalSlot {
                outcome: crate::consensus::rai::RaiOutcome::Notarized(last_hash),
                account,
                frontier: Some(frontier.clone()),
            },
        );

        let ledger = rsnano_ledger::Ledger::new_null();
        // The first call observes certificate completion and starts a fresh
        // durable sweep. The second consumes exactly the first page.
        container.rai_progress_close(None, &ledger, now);
        container.rai_progress_close(None, &ledger, now);
        assert_eq!(
            container.rai_epoch_manager.closing_epoch().unwrap().phase,
            crate::consensus::rai::RaiClosingPhase::Draining
        );
        assert!(
            container
                .rai_epoch_manager
                .drain_frontiers(RaiEpoch::ZERO)
                .unwrap()
                .get(&account)
                .is_none()
        );

        // The final page refreshes the late selected frontier, and only then
        // may record election begin.
        container.rai_progress_close(None, &ledger, now);
        assert_eq!(
            container
                .rai_epoch_manager
                .drain_frontiers(RaiEpoch::ZERO)
                .unwrap()[&account],
            frontier
        );
        assert_eq!(
            container.rai_epoch_manager.closing_epoch().unwrap().phase,
            crate::consensus::rai::RaiClosingPhase::ElectingRecord
        );
    }

    #[test]
    fn empty() {
        let container = ActiveElectionsContainer::default();
        assert_eq!(container.len(), 0);
        assert!(!container.is_active_root(&QualifiedRoot::new_test_instance()));
        assert!(!container.is_active_hash(&BlockHash::from(1)));
    }

    #[test]
    fn insert_election() {
        let mut container = ActiveElectionsContainer::default();
        let request = AecInsertRequest {
            block: SavedBlock::new_test_instance(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        container
            .insert(request, Timestamp::new_test_instance())
            .unwrap();

        assert_eq!(container.len(), 1);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn inserted_block_election_is_a_slot_with_epoch_fixed_at_creation() {
        use crate::consensus::{election::RaiElectionKind, rai::RaiEpoch};

        let mut container = ActiveElectionsContainer::default();
        assert!(
            container
                .rai_epoch_manager
                .start_closing(Timestamp::new_test_instance())
        );
        assert_eq!(
            container.rai_epoch_manager.closing_epoch().unwrap().epoch,
            RaiEpoch::ZERO
        );
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();

        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                Timestamp::new_test_instance(),
            )
            .unwrap();

        container.rai_epoch_manager.open_epoch(RaiEpoch::new(2));
        let election = container.election_for_root(&root).unwrap();
        assert_eq!(election.qualified_root(), &root);
        assert_eq!(election.rai_kind(), RaiElectionKind::Slot);
        assert_eq!(election.rai_epoch(), RaiEpoch::new(1));
        assert_eq!(election.rai_round(), 0);
        assert_eq!(
            container.rai_first_visible_epoch_by_root.get(&root),
            Some(&RaiEpoch::new(1))
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn active_slot_vote_target_is_selected_by_root_and_epoch() {
        use rsnano_types::RaiEpoch;

        let mut container = ActiveElectionsContainer::default();
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        for epoch in [RaiEpoch::new(3), RaiEpoch::new(14)] {
            let election = Election::new_slot(
                block.clone(),
                ElectionBehavior::Manual,
                Duration::from_secs(1),
                now,
                epoch,
            );
            assert!(container.roots.insert_rai(Entry {
                root: root.clone(),
                election,
                priority: BlockPriority::default(),
            }));
        }

        let target = container
            .rai_active_slot_vote_target_for_root(&root.root, RaiEpoch::new(3))
            .unwrap();

        assert_eq!(target.metadata.epoch, RaiEpoch::new(3));
        assert!(matches!(
            target.election_id,
            crate::consensus::election::RaiElectionId::Slot(ref slot)
                if slot.epoch == RaiEpoch::new(3)
        ));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn published_block_is_epoch_neutral_until_slot_admission() {
        use crate::consensus::election::RaiSlotId;
        use rsnano_types::{Amount, PrivateKey, RaiEpoch, StateBlockArgs};

        let mut container = ActiveElectionsContainer::default();
        let key = PrivateKey::from(1);
        let make_block = |balance| {
            Block::from(StateBlockArgs {
                key: &key,
                previous: BlockHash::from_bytes(*key.account().as_bytes()),
                representative: 789.into(),
                balance: Amount::raw(balance),
                link: 111.into(),
                work: 69420.into(),
            })
        };
        let initial = SavedBlock::new_test_instance_with(make_block(420));
        let published = SavedBlock::new_test_instance_with(make_block(421));
        assert_eq!(initial.qualified_root(), published.qualified_root());
        let root = initial.qualified_root();
        let published_hash = published.hash();
        container
            .insert(
                AecInsertRequest {
                    block: initial,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                Timestamp::new_test_instance(),
            )
            .unwrap();

        container.published_block_available(published.into());

        assert!(container.known_block(&published_hash).is_some());
        assert!(!container.is_active_hash(&published_hash));
        assert_eq!(container.election_for_root(&root).unwrap().block_count(), 1);

        let slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: root.clone(),
        };
        container
            .admit_candidate(slot.clone(), published_hash)
            .unwrap();
        assert!(container.slot_contains_candidate(&slot, &published_hash));
        assert!(container.is_active_hash(&published_hash));
        assert_eq!(container.election_for_root(&root).unwrap().block_count(), 2);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn slot_payload_request_returns_only_the_requested_block() {
        use rsnano_types::{Amount, PrivateKey, RaiEpoch, StateBlockArgs};

        let key = PrivateKey::from(1);
        let first = Block::from(StateBlockArgs {
            key: &key,
            previous: BlockHash::from_bytes(*key.account().as_bytes()),
            representative: 789.into(),
            balance: Amount::raw(2),
            link: 111.into(),
            work: 69420.into(),
        });
        let second = Block::from(StateBlockArgs {
            key: &key,
            previous: first.hash(),
            representative: 789.into(),
            balance: Amount::raw(1),
            link: 112.into(),
            work: 69420.into(),
        });
        let second_hash = second.hash();
        let second_root = second.qualified_root().root;
        let mut container = ActiveElectionsContainer::default();
        container.published_block_available(first);
        container.published_block_available(second.clone());

        let response = container.rai_blocks_for_request(second_hash, second_root, RaiEpoch::ZERO);

        assert_eq!(response.len(), 1);
        assert_eq!(response[0].hash(), second_hash);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn confirmed_slot_retains_terminal_marker_and_vote_evidence() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiVoteMetadata, RaiVotePhase,
            UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let hash = block.hash();
        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();

        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                hash,
                RaiVoteMetadata {
                    election_id: crate::consensus::election::RaiElectionId::Slot(
                        crate::consensus::election::RaiSlotId {
                            epoch: 0.into(),
                            root: root.clone(),
                        },
                    ),
                    phase: RaiVotePhase::First,
                    epoch: 0.into(),
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        assert_eq!(
            container.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: &RepWeights::default(),
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            })[&hash],
            Ok(())
        );
        assert!(container.election_for_root(&root).is_none());
        let slot = crate::consensus::election::RaiSlotId {
            epoch: 0.into(),
            root: root.clone(),
        };
        let terminal = &container.rai_terminal_slots[&slot];
        assert_eq!(
            terminal.outcome,
            crate::consensus::rai::RaiOutcome::Confirmed(hash)
        );
        assert!(container.rai_pending_votes.contains_key(
            &crate::consensus::election::RaiElectionId::Slot(slot.clone())
        ));
        assert!(
            container
                .rai_terminal_slots_by_hash
                .get(&hash)
                .is_some_and(|slots| slots.contains(&slot))
        );
        let (metadata, final_vote) = container.rai_vote_context(&hash).unwrap();
        assert_eq!(metadata.phase, RaiVotePhase::Final);
        assert!(final_vote);

        let pending_block = SavedBlock::new_test_instance_with_key(2);
        let pending_slot = crate::consensus::election::RaiSlotId {
            epoch: 0.into(),
            root: pending_block.qualified_root(),
        };
        container
            .insert(
                AecInsertRequest {
                    block: pending_block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();

        let payload_incomplete_slot = crate::consensus::election::RaiSlotId {
            epoch: 0.into(),
            root: SavedBlock::new_test_instance_with_key(3).qualified_root(),
        };
        container
            .rai_visible_obligations
            .entry(rsnano_types::RaiEpoch::ZERO)
            .or_default()
            .insert(payload_incomplete_slot.clone());
        container
            .rai_epoch_manager
            .record_known_slot(payload_incomplete_slot.clone());
        container.insert_rai_terminal_slot(
            payload_incomplete_slot.clone(),
            RaiTerminalSlot {
                outcome: crate::consensus::rai::RaiOutcome::Confirmed(BlockHash::from(3)),
                account: rsnano_types::Account::from(3),
                frontier: None,
            },
        );

        let reports =
            container.rai_tick(now + Duration::from_secs(1), &key, Duration::from_secs(1));
        assert!(!reports.is_empty());
        assert!(reports.iter().all(|report| {
            !report.visible_obligations.contains(&slot)
                && report.visible_obligations.contains(&pending_slot)
                && report
                    .visible_obligations
                    .contains(&payload_incomplete_slot)
        }));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_batch_routes_and_retains_one_shared_signed_transport_per_id() {
        use crate::consensus::{FilteredVote, ReceivedVote, rai::BlockHashOrTimeout};
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId,
            RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
        ];
        let mut committee_weights = RepWeights::default();
        for key in &keys {
            committee_weights.put(key.public_key(), Amount::raw(1));
        }
        let committee = Arc::new(committee_weights);
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let blocks = [
            SavedBlock::new_test_instance_with_key(10),
            SavedBlock::new_test_instance_with_key(11),
        ];
        let mut entries = Vec::new();
        let mut ids = Vec::new();
        for block in &blocks {
            container
                .insert(
                    AecInsertRequest {
                        block: block.clone(),
                        behavior: ElectionBehavior::Priority,
                        priority: BlockPriority::new_test_instance(),
                    },
                    now,
                )
                .unwrap();
            let id = RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::ZERO,
                root: block.qualified_root(),
            });
            entries.push((
                RaiVoteMetadata {
                    election_id: id.clone(),
                    phase: RaiVotePhase::First,
                    epoch: RaiEpoch::ZERO,
                    scope: RaiCommitteeScope::All,
                },
                block.hash(),
            ));
            ids.push(id);
        }
        let signed = Arc::new(Vote::new_rai_batch(
            &keys[0],
            UnixMillisTimestamp::new(1),
            0,
            entries,
        ));
        signed.validate().unwrap();
        let signed_hash = signed.hash();
        let vote: FilteredVote =
            ReceivedVote::new(signed.clone(), VoteDelivery::Direct, None).into();

        let result = container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        for (block, id) in blocks.iter().zip(&ids) {
            assert_eq!(result[&block.hash()], Ok(()));
            let election = container.roots.election_for_rai_id(id).unwrap();
            assert_eq!(
                election.rai_votes.committees[0]
                    .votes
                    .first
                    .get(&keys[0].public_key()),
                Some(&BlockHashOrTimeout::Block(block.hash()))
            );
            let retained = &container.rai_pending_votes[id];
            assert_eq!(retained.len(), 1);
            assert!(Arc::ptr_eq(&retained[0], &signed));
            assert_eq!(retained[0].rai_entry_count(), 2);
        }
        let cached = container.rai_votes_for_root(&blocks[0].qualified_root().root, RaiEpoch::ZERO);
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].hash(), signed_hash);
        assert_eq!(cached[0].rai_entry_count(), 2);
        cached[0].validate().unwrap();
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn vote_before_block_before_election_replays_the_exact_known_fork() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId,
            RaiVoteMetadata, RaiVotePhase, StateBlockArgs, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee = Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let make_block = |balance| {
            SavedBlock::new_test_instance_with(Block::from(StateBlockArgs {
                key: &key,
                previous: BlockHash::from_bytes(*key.account().as_bytes()),
                representative: 789.into(),
                balance: Amount::raw(balance),
                link: 111.into(),
                work: 69420.into(),
            }))
        };
        let initial = make_block(1);
        let fork = make_block(2);
        assert_eq!(initial.qualified_root(), fork.qualified_root());
        let fork_hash = fork.hash();
        let slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: initial.qualified_root(),
        };
        let id = RaiElectionId::Slot(slot.clone());
        let signed = Vote::new_rai(
            &key,
            UnixMillisTimestamp::new(1),
            0,
            fork_hash,
            RaiVoteMetadata {
                election_id: id.clone(),
                phase: RaiVotePhase::First,
                epoch: RaiEpoch::ZERO,
                scope: RaiCommitteeScope::All,
            },
        );
        let mut wire = Vec::new();
        signed.serialize(&mut wire).unwrap();
        let compact = Vote::deserialize_rai_slot(&mut wire.as_slice()).unwrap();
        assert!(matches!(
            &compact.rai_metadata(0).unwrap().election_id,
            RaiElectionId::Slot(decoded) if decoded.root == QualifiedRoot::ZERO
        ));
        let vote: FilteredVote =
            ReceivedVote::new(compact.into(), VoteDelivery::Direct, None).into();
        let now = Timestamp::new_test_instance();

        assert_eq!(
            container.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: &RepWeights::default(),
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            })[&fork_hash],
            Err(VoteError::Indeterminate)
        );
        assert!(
            container
                .rai_pending_compact_slot_votes
                .contains_key(&(RaiEpoch::ZERO, fork_hash))
        );

        // There is still no election, so Publish cannot admit the candidate;
        // it nevertheless resolves payload absence immediately.
        container.published_block_available(fork.clone().into());
        assert!(
            !container
                .rai_pending_compact_slot_votes
                .contains_key(&(RaiEpoch::ZERO, fork_hash))
        );
        assert!(container.roots.election_for_rai_id(&id).is_none());

        container
            .insert(
                AecInsertRequest {
                    block: initial,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();
        let election = container.roots.election_for_rai_id(&id).unwrap();
        assert!(election.contains_candidate(&fork_hash));
        assert!(container.slot_contains_candidate(&slot, &fork_hash));
        assert!(container.rai_pending_votes.contains_key(&id));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn timeout_account_height_resolves_to_local_slot_election() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiTimeoutSlot,
            RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee = Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let block = SavedBlock::new_test_instance();
        let account = block.account();
        let height = block.height();
        let root = block.qualified_root();
        let now = Timestamp::new_test_instance();
        let slot = crate::consensus::election::RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root,
        };
        let id = RaiElectionId::Slot(slot.clone());
        let signed = Vote::new_rai_timeout_batch(
            &key,
            UnixMillisTimestamp::new(1),
            0,
            [(
                RaiVoteMetadata {
                    election_id: id.clone(),
                    phase: RaiVotePhase::First,
                    epoch: RaiEpoch::ZERO,
                    scope: RaiCommitteeScope::All,
                },
                RaiTimeoutSlot { account, height },
            )],
        );
        let mut wire = Vec::new();
        signed.serialize(&mut wire).unwrap();
        let decoded = Vote::deserialize_rai_timeout_slot(&wire).unwrap();
        let vote: FilteredVote =
            ReceivedVote::new(decoded.into(), VoteDelivery::Direct, None).into();

        assert_eq!(
            container.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: &RepWeights::default(),
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            })[&BlockHash::ZERO],
            Err(VoteError::Indeterminate)
        );
        assert!(
            container
                .rai_pending_timeout_slot_votes
                .contains_key(&(RaiEpoch::ZERO, RaiTimeoutSlot { account, height }))
        );

        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();
        assert!(
            !container
                .rai_pending_timeout_slot_votes
                .contains_key(&(RaiEpoch::ZERO, RaiTimeoutSlot { account, height }))
        );
        assert!(container.rai_pending_votes.contains_key(&id));
        assert!(container.rai_pending_votes[&id].iter().any(|vote| {
            vote.rai_metadata_iter()
                .any(|metadata| metadata.election_id == id)
                && vote.rai_timeout_slot(0) == Some(RaiTimeoutSlot { account, height })
        }));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn payload_reverse_indexes_erase_prune_and_stop_without_stale_entries() {
        use rsnano_types::{RaiEpoch, RaiSlotId};

        let mut container = ActiveElectionsContainer::default();
        let root = QualifiedRoot::new(11.into(), 12.into());
        let old = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: root.clone(),
        };
        let future = RaiSlotId {
            epoch: RaiEpoch::new(1),
            root: root.clone(),
        };
        let hash = BlockHash::from(99);
        for slot in [&old, &future] {
            container.rai_mark_missing_drain_payload(slot.clone());
            container.rai_mark_payload_incomplete(slot.clone(), hash);
        }

        assert!(container.rai_clear_missing_drain_payload(&old));
        assert!(container.rai_clear_payload_incomplete_slot(&old));
        assert_eq!(
            container.rai_missing_drain_payloads_by_root[&root],
            [future.clone()].into_iter().collect()
        );
        assert_eq!(
            container.rai_payload_incomplete_by_hash[&hash],
            [future.clone()].into_iter().collect()
        );

        container.rai_mark_missing_drain_payload(old.clone());
        container.rai_mark_payload_incomplete(old.clone(), hash);
        container.prune_rai_evidence_through(RaiEpoch::ZERO);
        assert!(!container.rai_missing_drain_payloads.contains(&old));
        assert!(!container.rai_payload_incomplete.contains_key(&old));
        assert_eq!(
            container.rai_missing_drain_payloads_by_root[&root],
            [future.clone()].into_iter().collect()
        );
        assert_eq!(
            container.rai_payload_incomplete_by_hash[&hash],
            [future].into_iter().collect()
        );

        container.stop();
        assert!(container.rai_missing_drain_payloads.is_empty());
        assert!(container.rai_missing_drain_payloads_by_root.is_empty());
        assert!(container.rai_payload_incomplete.is_empty());
        assert!(container.rai_payload_incomplete_by_hash.is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn terminal_vote_context_precedes_indexed_close_context_amid_slot_noise() {
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId,
            RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp, Vote,
        };

        let key = PrivateKey::from(1);
        let committee = Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        for value in 1..=4096 {
            container.rai_pending_votes.insert(
                RaiElectionId::Slot(RaiSlotId {
                    epoch: RaiEpoch::new(value),
                    root: QualifiedRoot::new(value.into(), (value + 10_000).into()),
                }),
                Vec::new(),
            );
        }

        let close_id = RaiElectionId::CloseCut {
            epoch: RaiEpoch::ZERO,
            round: 17,
        };
        let shared_hash = BlockHash::from(700);
        let close_only_hash = BlockHash::from(701);
        for (sequence, hash) in [(1, shared_hash), (2, close_only_hash)] {
            container.rai_retain_pending_vote(
                close_id.clone(),
                Arc::new(Vote::new_rai(
                    &key,
                    UnixMillisTimestamp::new(sequence),
                    0,
                    hash,
                    RaiVoteMetadata {
                        election_id: close_id.clone(),
                        phase: RaiVotePhase::First,
                        epoch: RaiEpoch::ZERO,
                        scope: RaiCommitteeScope::All,
                    },
                )),
            );
        }
        let terminal_slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: QualifiedRoot::new(21.into(), 22.into()),
        };
        container.insert_rai_terminal_slot(
            terminal_slot.clone(),
            RaiTerminalSlot {
                outcome: crate::consensus::rai::RaiOutcome::Confirmed(shared_hash),
                account: Default::default(),
                frontier: None,
            },
        );

        let (terminal, final_vote) = container.rai_vote_context(&shared_hash).unwrap();
        assert_eq!(terminal.election_id, RaiElectionId::Slot(terminal_slot));
        assert_eq!(terminal.phase, RaiVotePhase::Final);
        assert!(final_vote);

        let (close, is_close) = container.rai_vote_context(&close_only_hash).unwrap();
        assert_eq!(close.election_id, close_id);
        assert!(is_close);
        assert_eq!(container.rai_pending_close_contexts_by_hash.len(), 2);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn terminal_slot_hash_index_tracks_replacement_and_pruning() {
        use crate::consensus::election::RaiSlotId;
        use crate::consensus::rai::RaiOutcome;
        use rsnano_types::{ConfirmationHeightInfo, RaiEpoch, RaiVotePhase};

        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            std::sync::Arc::new(RepWeights::default()),
            BlockHash::from(7),
        );
        let slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: QualifiedRoot::new_test_instance(),
        };
        let outcome_hash = BlockHash::from(10);
        let frontier_hash = BlockHash::from(11);
        container.insert_rai_terminal_slot(
            slot.clone(),
            RaiTerminalSlot {
                outcome: RaiOutcome::Confirmed(outcome_hash),
                account: Default::default(),
                frontier: Some(ConfirmationHeightInfo::new(1, frontier_hash)),
            },
        );

        for hash in [outcome_hash, frontier_hash] {
            assert!(
                container
                    .rai_terminal_slots_by_hash
                    .get(&hash)
                    .is_some_and(|slots| slots.contains(&slot))
            );
            let (metadata, final_vote) = container.rai_vote_context(&hash).unwrap();
            assert_eq!(metadata.phase, RaiVotePhase::Final);
            assert!(final_vote);
        }
        assert_eq!(
            container.rai_terminal_slots_by_request_root[&slot.root.root],
            [slot.clone()].into_iter().collect()
        );

        let replacement_hash = BlockHash::from(12);
        container.insert_rai_terminal_slot(
            slot.clone(),
            RaiTerminalSlot {
                outcome: RaiOutcome::Notarized(replacement_hash),
                account: Default::default(),
                frontier: None,
            },
        );

        assert!(
            !container
                .rai_terminal_slots_by_hash
                .contains_key(&outcome_hash)
        );
        assert!(
            !container
                .rai_terminal_slots_by_hash
                .contains_key(&frontier_hash)
        );
        assert!(container.rai_vote_context(&outcome_hash).is_none());
        assert!(container.rai_vote_context(&frontier_hash).is_none());
        let (metadata, final_vote) = container.rai_vote_context(&replacement_hash).unwrap();
        assert_eq!(metadata.phase, RaiVotePhase::Notar);
        assert!(!final_vote);

        container.prune_rai_evidence_through(RaiEpoch::ZERO);
        assert!(container.rai_terminal_slots.is_empty());
        assert!(container.rai_terminal_slots_by_hash.is_empty());
        assert!(container.rai_terminal_slots_by_request_root.is_empty());
        assert!(container.rai_vote_context(&replacement_hash).is_none());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn active_slot_certificate_is_consumed_by_close_drain() {
        use crate::consensus::rai::RaiReport;
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiEpoch, RaiVoteMetadata, RaiVotePhase,
            UnixMillisTimestamp,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let hash = block.hash();
        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();
        let slot = crate::consensus::election::RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: root.clone(),
        };
        let id = crate::consensus::election::RaiElectionId::Slot(slot.clone());

        // Cached votes are restored directly into a recreated election and do
        // not pass through ApplyVoteToElectionHelper::confirm_if_quorum.
        container
            .roots
            .election_for_rai_id_mut(&id)
            .unwrap()
            .add_rai_vote(
                key.public_key(),
                hash,
                RaiVoteMetadata {
                    election_id: id.clone(),
                    phase: RaiVotePhase::First,
                    epoch: RaiEpoch::ZERO,
                    scope: RaiCommitteeScope::All,
                },
                UnixMillisTimestamp::new(1),
                now,
            )
            .unwrap();

        container.rai_epoch_manager.start_closing(now);
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(RaiReport::new(&key, RaiEpoch::ZERO, [slot.clone()]))
            .unwrap();
        let (_, cut) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut)
            .unwrap();

        container.rai_progress_close(
            Some(Default::default()),
            &rsnano_ledger::Ledger::new_null(),
            now,
        );

        let drain = container
            .rai_epoch_manager
            .happy_path_drain(RaiEpoch::ZERO)
            .unwrap();
        assert_eq!(drain.finalized.get(&slot), Some(&hash));
        assert!(drain.is_complete());
        let repair = container
            .rai_certificate_finalized_vote_target(&hash, &root.root, RaiEpoch::ZERO)
            .unwrap();
        assert_eq!(repair.hash, hash);
        assert_eq!(repair.root, root.root);
        assert_eq!(repair.election_id, id);
        assert_eq!(repair.metadata.phase, RaiVotePhase::Final);
        container.prune_rai_evidence_through(RaiEpoch::ZERO);
        assert!(container.rai_terminal_slots.is_empty());
        assert!(container.rai_terminal_slots_by_hash.is_empty());
        assert!(!container.rai_pending_votes.contains_key(&id));
        let wildcard_repair = container
            .rai_certificate_finalized_vote_target(&BlockHash::ZERO, &root.root, RaiEpoch::ZERO)
            .unwrap();
        assert_eq!(wildcard_repair.hash, hash);
        assert_eq!(wildcard_repair.root, root.root);
        assert_eq!(wildcard_repair.election_id, id);
        assert_eq!(wildcard_repair.metadata.phase, RaiVotePhase::Final);
        assert!(
            container
                .rai_certificate_finalized_vote_target(&hash, &root.root, RaiEpoch::new(1),)
                .is_none()
        );
        assert!(
            container
                .rai_certificate_finalized_vote_target(
                    &BlockHash::from(123),
                    &root.root,
                    RaiEpoch::ZERO,
                )
                .is_none()
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn transition_time_expires_ordinary_rai_slot() {
        let mut container = ActiveElectionsContainer::default();
        let start = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let hash = block.hash();
        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                start,
            )
            .unwrap();
        assert!(container.transition_active(&hash));
        assert!(
            !container
                .election_for_root(&root)
                .unwrap()
                .rai_requires_retention()
        );

        container.transition_time(start + Duration::from_secs(10 * 60));

        assert!(container.election_for_root(&root).is_none());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn evicted_slot_replays_archived_vote_when_reactivated() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiEpoch, RaiVoteMetadata, RaiVotePhase,
            UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let hash = block.hash();
        let slot = crate::consensus::election::RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: root.clone(),
        };
        let id = crate::consensus::election::RaiElectionId::Slot(slot.clone());
        let request = || AecInsertRequest {
            block: block.clone(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };
        container.insert(request(), now).unwrap();

        assert!(container.erase(&root));
        assert!(container.roots.election_for_rai_id(&id).is_none());
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                hash,
                RaiVoteMetadata {
                    election_id: id.clone(),
                    phase: RaiVotePhase::First,
                    epoch: RaiEpoch::ZERO,
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert!(container.rai_pending_votes.contains_key(&id));

        container.insert(request(), now).unwrap();

        assert_eq!(
            container
                .roots
                .election_for_rai_id(&id)
                .unwrap()
                .rai_votes
                .outcome,
            crate::consensus::rai::RaiOutcome::Confirmed(hash)
        );
        container.transition_time(now);
        assert!(container.roots.election_for_rai_id(&id).is_none());
        assert!(container.rai_terminal_slots.contains_key(&slot));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn pending_slot_can_be_erased() {
        let mut container = ActiveElectionsContainer::default();
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();
        assert!(
            container
                .election_for_root(&root)
                .unwrap()
                .rai_requires_retention()
                == false
        );
        assert!(container.erase(&root));
        assert!(container.election_for_root(&root).is_none());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn evicted_active_notarized_slot_is_not_marked_terminal() {
        let mut container = ActiveElectionsContainer::default();
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let hash = block.hash();
        let slot = crate::consensus::election::RaiSlotId {
            epoch: rsnano_types::RaiEpoch::ZERO,
            root: root.clone(),
        };
        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();
        let election = container.roots.election_for_root_mut(&root).unwrap();
        election.transition_active();
        election.rai_votes.outcome = crate::consensus::rai::RaiOutcome::Notarized(hash);

        assert!(container.erase(&root));

        assert!(!container.rai_terminal_slots.contains_key(&slot));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn missing_payload_requests_exclude_an_exact_active_timeout_slot() {
        use crate::consensus::rai::BlockHashOrTimeout;
        use rsnano_types::{Amount, PrivateKey, RaiElectionId, RaiEpoch, RaiSlotId};

        fn pending_requests(
            include_same_root_distinct_slot: bool,
        ) -> (rsnano_types::Root, Vec<(BlockHash, rsnano_types::Root)>) {
            let keys = [PrivateKey::from(1), PrivateKey::from(2)];
            let committee = std::sync::Arc::new(RepWeights::from([
                (keys[0].public_key(), Amount::raw(100)),
                (keys[1].public_key(), Amount::raw(100)),
            ]));
            let mut container = ActiveElectionsContainer::new_with_rai_committee(
                ActiveElectionsConfig::default(),
                Duration::from_secs(1),
                committee,
                BlockHash::from(7),
            );
            let now = Timestamp::new_test_instance();
            let block = SavedBlock::new_test_instance();
            let hash = block.hash();
            let active_slot = RaiSlotId {
                epoch: RaiEpoch::ZERO,
                root: block.qualified_root(),
            };
            let request_root = active_slot.root.root;
            container
                .insert(
                    AecInsertRequest {
                        block,
                        behavior: ElectionBehavior::Priority,
                        priority: BlockPriority::new_test_instance(),
                    },
                    now,
                )
                .unwrap();
            assert!(container.transition_active(&hash));
            let active_id = RaiElectionId::Slot(active_slot.clone());
            let timeout_at = now + Duration::from_secs(6);
            let election = container.roots.election_for_rai_id_mut(&active_id).unwrap();
            election
                .rai_votes
                .record_first_vote(
                    keys[0].public_key(),
                    BlockHashOrTimeout::Block(hash),
                    rsnano_types::RaiCommitteeScope::All,
                )
                .unwrap();
            election
                .rai_votes
                .record_first_vote(
                    keys[1].public_key(),
                    BlockHashOrTimeout::Timeout,
                    rsnano_types::RaiCommitteeScope::All,
                )
                .unwrap();
            assert_eq!(election.rai_request_hash(), BlockHash::ZERO);

            let mut obligations = vec![active_slot.clone()];
            if include_same_root_distinct_slot {
                obligations.push(RaiSlotId {
                    epoch: RaiEpoch::ZERO,
                    root: QualifiedRoot::new(request_root, BlockHash::from(999)),
                });
            }
            assert!(container.rai_epoch_manager.start_closing(timeout_at));
            for key in &keys {
                container
                    .rai_epoch_manager
                    .reports_mut()
                    .insert(crate::consensus::rai::RaiReport::new(
                        key,
                        RaiEpoch::ZERO,
                        obligations.clone(),
                    ))
                    .unwrap();
            }
            let (_, cut_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
            container
                .rai_epoch_manager
                .install_cut(RaiEpoch::ZERO, 0, cut_hash)
                .unwrap();
            let ledger = rsnano_ledger::Ledger::new_null();
            for slot in &obligations {
                container.rai_check_drain_slot(RaiEpoch::ZERO, slot, &ledger, timeout_at, false);
            }

            (
                request_root,
                container.rai_missing_slot_payload_requests(RaiEpoch::ZERO),
            )
        }

        let (_, requests) = pending_requests(false);
        assert!(requests.is_empty());

        let (root, requests) = pending_requests(true);
        assert_eq!(requests, vec![(BlockHash::ZERO, root)]);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn recovered_active_slot_removes_missing_payload_request() {
        use rsnano_types::{Amount, PrivateKey, RaiEpoch, RaiSlotId};

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: block.qualified_root(),
        };
        assert!(container.rai_epoch_manager.start_closing(now));
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(crate::consensus::rai::RaiReport::new(
                &key,
                RaiEpoch::ZERO,
                [slot.clone()],
            ))
            .unwrap();
        let (_, cut_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut_hash)
            .unwrap();
        container.rai_check_drain_slot(
            RaiEpoch::ZERO,
            &slot,
            &rsnano_ledger::Ledger::new_null(),
            now,
            false,
        );

        assert_eq!(
            container.rai_missing_slot_payload_requests(RaiEpoch::ZERO),
            vec![(BlockHash::ZERO, slot.root.root)]
        );

        container
            .insert_drain_election(block, RaiEpoch::ZERO, now)
            .unwrap();

        assert!(
            container
                .rai_missing_slot_payload_requests(RaiEpoch::ZERO)
                .is_empty()
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn aec_known_unsaved_payload_does_not_trigger_zero_hash_repair() {
        use rsnano_types::{Amount, PrivateKey, RaiEpoch, RaiSlotId};

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: block.qualified_root(),
        };
        container.published_block_available(block.clone().into());
        assert!(container.rai_epoch_manager.start_closing(now));
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(crate::consensus::rai::RaiReport::new(
                &key,
                RaiEpoch::ZERO,
                [slot.clone()],
            ))
            .unwrap();
        let (_, cut_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut_hash)
            .unwrap();

        container.rai_check_drain_slot(
            RaiEpoch::ZERO,
            &slot,
            &rsnano_ledger::Ledger::new_null(),
            now,
            false,
        );

        assert!(container.rai_missing_drain_payloads.is_empty());
        assert!(
            container
                .rai_missing_slot_payload_candidates(RaiEpoch::ZERO)
                .is_empty()
        );
        assert!(
            container
                .rai_epoch_manager
                .happy_path_drain(RaiEpoch::ZERO)
                .is_some_and(|drain| !drain.is_complete())
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn publish_after_terminal_and_record_start_dirties_and_refreshes_exact_frontier() {
        use rsnano_types::{Amount, PrivateKey, RaiEpoch, RaiSlotId};

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let account = block.account();
        let slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: block.qualified_root(),
        };

        assert!(container.rai_epoch_manager.start_closing(now));
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(crate::consensus::rai::RaiReport::new(
                &key,
                RaiEpoch::ZERO,
                [slot.clone()],
            ))
            .unwrap();
        let (_, cut_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut_hash)
            .unwrap();
        assert!(
            container
                .rai_epoch_manager
                .initialize_drain_frontiers(RaiEpoch::ZERO, [])
        );

        // The terminal marker predates both drain selection and Publish, so
        // insertion itself cannot yet know that this slot will need refresh.
        container.insert_rai_terminal_slot(
            slot.clone(),
            RaiTerminalSlot {
                outcome: crate::consensus::rai::RaiOutcome::Notarized(hash),
                account,
                frontier: None,
            },
        );
        assert_eq!(
            container
                .rai_epoch_manager
                .record_notarized_drain(RaiEpoch::ZERO, &slot, hash, [],),
            Some(crate::consensus::rai::RaiDrainOutcome::Selected(hash))
        );
        container
            .rai_epoch_manager
            .begin_close_record(committee.as_ref().clone())
            .unwrap();
        assert!(!container.rai_close_record_refresh_slots.contains(&slot));

        container.published_block_available(block.clone().into());
        assert!(container.rai_close_record_refresh_slots.contains(&slot));

        let ledger = rsnano_ledger::Ledger::new_null();
        assert!(container.rai_refresh_close_record_frontiers(RaiEpoch::ZERO, &ledger));
        assert!(!container.rai_close_record_refresh_slots.contains(&slot));
        assert_eq!(
            container
                .rai_epoch_manager
                .drain_frontiers(RaiEpoch::ZERO)
                .unwrap()[&account]
                .frontier,
            hash
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn bounded_drain_check_recreates_one_ledger_known_election() {
        use rsnano_types::{Amount, PrivateKey, RaiElectionId, RaiEpoch, RaiSlotId};

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let ledger = rsnano_ledger::Ledger::new_null();
        let block = ledger.genesis().clone();
        let slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: block.qualified_root(),
        };
        assert!(container.rai_epoch_manager.start_closing(now));
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(crate::consensus::rai::RaiReport::new(
                &key,
                RaiEpoch::ZERO,
                [slot.clone()],
            ))
            .unwrap();
        let (_, cut_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut_hash)
            .unwrap();

        container.rai_check_drain_slot(RaiEpoch::ZERO, &slot, &ledger, now, false);

        let id = RaiElectionId::Slot(slot);
        assert!(container.roots.election_for_rai_id(&id).is_some());
        assert!(container.rai_missing_drain_payloads.is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn active_slot_retains_an_exact_missing_certified_fork_for_payload_repair() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId,
            RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: block.qualified_root(),
        };
        let id = RaiElectionId::Slot(slot.clone());
        container.published_block_available(block.clone().into());
        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();
        let missing_hash = BlockHash::from(999_999);
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                missing_hash,
                RaiVoteMetadata {
                    election_id: id.clone(),
                    phase: RaiVotePhase::First,
                    epoch: RaiEpoch::ZERO,
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(result[&missing_hash], Err(VoteError::Indeterminate));
        assert!(container.roots.election_for_rai_id(&id).is_some());
        assert!(container.rai_payload_incomplete[&slot].contains(&missing_hash));

        assert!(container.rai_epoch_manager.start_closing(now));
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(crate::consensus::rai::RaiReport::new(
                &key,
                RaiEpoch::ZERO,
                [slot.clone()],
            ))
            .unwrap();
        let (_, cut_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut_hash)
            .unwrap();

        assert_eq!(
            container.rai_missing_slot_payload_candidates(RaiEpoch::ZERO),
            vec![(slot.root, Some(missing_hash))]
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn timeout_zero_leaf_is_never_classified_as_a_missing_payload() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId,
            RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: block.qualified_root(),
        };
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                BlockHash::ZERO,
                RaiVoteMetadata {
                    election_id: RaiElectionId::Slot(slot.clone()),
                    phase: RaiVotePhase::First,
                    epoch: RaiEpoch::ZERO,
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();
        let _ = container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        assert!(!container.rai_payload_incomplete.contains_key(&slot));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn remote_cut_obligation_blocks_successor_retry_and_keeps_drain_vote_context() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId,
            RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let hash = block.hash();
        let closing_slot = RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: root.clone(),
        };

        // This node never opened the old slot. It learns the obligation only
        // from the certified remote cut, so the container-local visibility set
        // cannot be the retry guard.
        assert!(container.rai_visible_obligations.is_empty());
        assert!(container.rai_epoch_manager.start_closing(now));
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(crate::consensus::rai::RaiReport::new(
                &key,
                RaiEpoch::ZERO,
                [closing_slot.clone()],
            ))
            .unwrap();
        let (_, cut) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut)
            .unwrap();

        assert!(
            !container
                .rai_epoch_manager
                .slot_election_enabled(RaiEpoch::new(1), &root)
        );
        assert_eq!(
            container.insert(
                AecInsertRequest {
                    block: block.clone(),
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            ),
            Err(AecInsertError::Duplicate)
        );

        // Model a successor election which raced with learning the cut. It may
        // remain indexed, but it is no longer vote-enabled after installation.
        let successor = Election::new_slot(
            block.clone(),
            ElectionBehavior::Manual,
            Duration::from_secs(1),
            now,
            RaiEpoch::new(1),
        )
        .with_rai_committees(vec![committee]);
        let successor_id = successor.rai_id().clone();
        assert!(container.roots.insert_rai(Entry {
            root: root.clone(),
            election: successor,
            priority: BlockPriority::default(),
        }));
        assert!(container.rai_vote_context(&hash).is_none());
        assert!(
            container
                .rai_slot_vote_context_for_root(&root.root)
                .is_none()
        );
        assert!(
            container
                .rai_active_slot_vote_target_for_root(&root.root, RaiEpoch::new(1))
                .is_none()
        );
        assert!(
            container
                .iter_round_robin()
                .all(|election| election.rai_id() != &successor_id)
        );
        assert!(
            container
                .rai_missing_slot_payload_requests(RaiEpoch::new(1))
                .is_empty()
        );

        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                hash,
                RaiVoteMetadata {
                    election_id: successor_id.clone(),
                    phase: RaiVotePhase::First,
                    epoch: RaiEpoch::new(1),
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(result[&hash], Err(VoteError::Invalid));
        assert!(!container.rai_pending_votes.contains_key(&successor_id));
        container
            .rai_pending_votes
            .insert(successor_id.clone(), vec![vote.vote.vote.clone()]);
        assert!(
            container
                .rai_votes_for_root(&root.root, RaiEpoch::new(1))
                .is_empty()
        );
        container.rai_pending_votes.remove(&successor_id);

        // The exact closing-epoch identity remains enabled for drain repair,
        // even while the disabled successor entry is still present.
        container
            .insert_drain_election(block.clone(), RaiEpoch::ZERO, now)
            .unwrap();
        let (metadata, _) = container.rai_vote_context(&hash).unwrap();
        assert_eq!(
            metadata.election_id,
            RaiElectionId::Slot(closing_slot.clone())
        );
        assert_eq!(metadata.epoch, RaiEpoch::ZERO);
        assert_eq!(
            container
                .rai_slot_vote_context_for_root(&root.root)
                .unwrap()
                .epoch,
            RaiEpoch::ZERO
        );
        assert_eq!(
            container
                .rai_active_slot_vote_target_for_root(&root.root, RaiEpoch::ZERO)
                .unwrap()
                .metadata
                .epoch,
            RaiEpoch::ZERO
        );

        // Once the old close is installed, the raced successor must not leak
        // into this replica's report when epoch 1 reaches its own boundary.
        assert!(
            container
                .rai_epoch_manager
                .initialize_drain_frontiers(RaiEpoch::ZERO, [])
        );
        assert!(container.rai_epoch_manager.record_finalized_drain(
            RaiEpoch::ZERO,
            &closing_slot,
            hash,
            [(
                block.account(),
                rsnano_types::ConfirmationHeightInfo::new(block.height(), hash),
            )],
        ));
        let (_, close) = container
            .rai_epoch_manager
            .begin_close_record(RepWeights::default())
            .unwrap();
        container
            .rai_epoch_manager
            .install_close_record(RaiEpoch::ZERO, 0, close)
            .unwrap();
        let successor_slot = RaiSlotId {
            epoch: RaiEpoch::new(1),
            root,
        };
        let reports =
            container.rai_tick(now + Duration::from_secs(1), &key, Duration::from_secs(1));
        assert!(!reports.is_empty());
        assert!(reports.iter().all(|report| report.epoch == RaiEpoch::new(1)
            && !report.visible_obligations.contains(&successor_slot)));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn successor_epoch_election_does_not_hide_missing_drain_election() {
        use rsnano_types::{Amount, PrivateKey, RaiEpoch};

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let closing_slot = crate::consensus::election::RaiSlotId {
            epoch: RaiEpoch::ZERO,
            root: root.clone(),
        };
        let closing_id = crate::consensus::election::RaiElectionId::Slot(closing_slot.clone());

        container
            .insert(
                AecInsertRequest {
                    block: block.clone(),
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();
        container.rai_epoch_manager.start_closing(now);
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(crate::consensus::rai::RaiReport::new(
                &key,
                RaiEpoch::ZERO,
                [closing_slot.clone()],
            ))
            .unwrap();
        let (_, cut) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(RaiEpoch::ZERO, 0, cut)
            .unwrap();

        container.roots.erase_rai_id(&closing_id).unwrap();
        let successor = Election::new_slot(
            block,
            ElectionBehavior::Manual,
            Duration::from_secs(1),
            now,
            RaiEpoch::new(1),
        )
        .with_rai_committees(vec![committee]);
        let successor_id = successor.rai_id().clone();
        assert!(container.roots.insert_rai(Entry {
            root: root.clone(),
            election: successor,
            priority: BlockPriority::default(),
        }));

        assert!(container.roots.election_for_rai_id(&successor_id).is_some());
        assert_eq!(
            container.rai_missing_drain_elections(RaiEpoch::ZERO),
            vec![root]
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epoch_two_election_requires_close_zero() {
        use crate::consensus::rai::RaiEpoch;

        let mut container = ActiveElectionsContainer::default();
        container.rai_epoch_manager.open_epoch(RaiEpoch::new(2));

        let result = container.insert(
            AecInsertRequest {
                block: SavedBlock::new_test_instance(),
                behavior: ElectionBehavior::Priority,
                priority: BlockPriority::new_test_instance(),
            },
            Timestamp::new_test_instance(),
        );

        assert_eq!(result, Err(AecInsertError::MissingRaiGoverningClose));
        assert_eq!(container.len(), 0);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_preimage_repair_is_requested_only_for_exact_unknown_signed_digest() {
        use crate::consensus::{
            RaiCloseElectionSpec,
            rai::{RaiCloseCut, RaiCloseElectionId, RaiCloseKind, RaiReport},
        };
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiSlotId, RaiVoteMetadata, RaiVotePhase,
            UnixMillisTimestamp, Vote,
        };

        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
        ];
        let committee = std::sync::Arc::new(RepWeights::from(
            keys.each_ref()
                .map(|key| (key.public_key(), Amount::raw(1))),
        ));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        assert!(container.rai_epoch_manager.start_closing(now));
        for key in &keys {
            container
                .rai_epoch_manager
                .reports_mut()
                .insert(RaiReport::new(key, 0.into(), []))
                .unwrap();
        }
        let (root, local_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        let id = RaiCloseElectionId {
            kind: RaiCloseKind::Cut,
            epoch: 0.into(),
            round: 0,
        };
        container
            .insert_close_cut(
                RaiCloseElectionSpec {
                    id,
                    root: root.clone(),
                    candidate: local_hash,
                    committee,
                },
                now,
            )
            .unwrap();

        let retain_first =
            |container: &mut ActiveElectionsContainer, key: &PrivateKey, hash: BlockHash| {
                let election_id = crate::consensus::election::RaiElectionId::CloseCut {
                    epoch: 0.into(),
                    round: 0,
                };
                container
                    .rai_pending_votes
                    .entry(election_id.clone())
                    .or_default()
                    .push(Arc::new(Vote::new_rai(
                        key,
                        UnixMillisTimestamp::new(1),
                        0,
                        hash,
                        RaiVoteMetadata {
                            election_id,
                            phase: RaiVotePhase::First,
                            epoch: 0.into(),
                            scope: RaiCommitteeScope::All,
                        },
                    )));
            };

        retain_first(&mut container, &keys[0], local_hash);
        assert!(
            container
                .rai_missing_close_preimage_requests(0.into())
                .is_empty(),
            "a retained leaf for a validated local preimage needs no repair"
        );

        let remote_cut = RaiCloseCut::new(
            0.into(),
            [RaiSlotId {
                epoch: 0.into(),
                root: QualifiedRoot::new_test_instance(),
            }],
        );
        let remote_hash = remote_cut.hash();
        assert_ne!(remote_hash, local_hash);
        retain_first(&mut container, &keys[1], remote_hash);
        assert_eq!(
            container.rai_missing_close_preimage_requests(0.into()),
            vec![(remote_hash, root.root)]
        );

        assert!(container.reconcile_rai_close_cut(remote_cut, root.root, now));
        assert!(
            container
                .rai_missing_close_preimage_requests(0.into())
                .is_empty(),
            "the exact request disappears after its hash-checked preimage arrives"
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_cut_uses_normal_vote_validation_and_enters_draining() {
        use crate::consensus::{
            FilteredVote, RaiCloseElectionSpec, ReceivedVote,
            election::RaiElectionKind,
            rai::{
                RaiCloseElectionId, RaiCloseKind, RaiClosingPhase, RaiReport, rai_close_cut_root,
            },
        };
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiVoteMetadata, RaiVotePhase,
            UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
        ];
        let committee = std::sync::Arc::new(RepWeights::from([
            (keys[0].public_key(), Amount::raw(1)),
            (keys[1].public_key(), Amount::raw(1)),
            (keys[2].public_key(), Amount::raw(1)),
            (keys[3].public_key(), Amount::raw(1)),
        ]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        assert!(container.rai_epoch_manager.start_closing(now));
        for key in &keys {
            container
                .rai_epoch_manager
                .reports_mut()
                .insert(RaiReport::new(key, 0.into(), []))
                .unwrap();
        }
        let (root, candidate) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        let id = RaiCloseElectionId {
            kind: RaiCloseKind::Cut,
            epoch: 0.into(),
            round: 0,
        };
        container
            .insert_close_cut(
                RaiCloseElectionSpec {
                    id,
                    root: root.clone(),
                    candidate,
                    committee,
                },
                now,
            )
            .unwrap();

        let election = container.election_for_root(&root).unwrap();
        assert_eq!(election.rai_kind(), RaiElectionKind::CloseCut);
        assert!(election.candidate_blocks().is_empty());
        assert_eq!(root, rai_close_cut_root(0.into(), 0));

        let rep_weights = RepWeights::default();
        let quorum = QuorumSnapshot::new_test_instance();
        for key in &keys {
            let vote: FilteredVote = ReceivedVote::new(
                Vote::new_rai(
                    key,
                    UnixMillisTimestamp::new(1),
                    0,
                    candidate,
                    RaiVoteMetadata {
                        election_id: crate::consensus::election::RaiElectionId::CloseCut {
                            epoch: 0.into(),
                            round: 0,
                        },
                        phase: RaiVotePhase::First,
                        epoch: 0.into(),
                        scope: RaiCommitteeScope::All,
                    },
                )
                .into(),
                VoteDelivery::Direct,
                None,
            )
            .into();
            assert_eq!(
                container.apply_vote(ApplyVoteArgs {
                    vote: &vote,
                    rep_weights: &rep_weights,
                    quorum_snapshot: &quorum,
                    now,
                })[&candidate],
                Ok(())
            );
        }

        assert_eq!(
            container.rai_epoch_manager.decided_close_hash(0.into()),
            Some(candidate)
        );
        assert_eq!(
            container.rai_epoch_manager.closing_epoch().unwrap().phase,
            RaiClosingPhase::Draining
        );
        assert_eq!(
            container
                .rai_epoch_manager
                .close_cut_tracker(0.into())
                .unwrap()
                .round(0)
                .unwrap()
                .id,
            id
        );
        assert_eq!(
            container
                .rai_epoch_manager
                .decide_close_cut(0.into(), 0, BlockHash::from(123)),
            Err(crate::consensus::rai::CloseCutDecisionError::ImmutableDecision)
        );
        container.prune_rai_evidence_through(0.into());
        assert!(container.rai_pending_votes.contains_key(
            &crate::consensus::election::RaiElectionId::CloseCut {
                epoch: 0.into(),
                round: 0,
            }
        ));
        let regenerated = container
            .rai_finalized_close_vote_target(&root.root)
            .unwrap();
        assert_eq!(regenerated.hash, candidate);
        assert_eq!(regenerated.root, root.root);
        assert_eq!(regenerated.metadata.phase, RaiVotePhase::Final);
        assert_eq!(
            regenerated.election_id,
            crate::consensus::election::RaiElectionId::CloseCut {
                epoch: 0.into(),
                round: 0,
            }
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_cut_notarization_starts_carried_successor_round() {
        use crate::consensus::{
            FilteredVote, RaiCloseElectionSpec, ReceivedVote,
            rai::{
                RaiCloseElectionId, RaiCloseKind, RaiCloseRoundResult, RaiLocalResult, RaiReport,
            },
        };
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiVoteMetadata, RaiVotePhase,
            UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
            PrivateKey::from(5),
            PrivateKey::from(6),
        ];
        let committee = std::sync::Arc::new(RepWeights::from(
            keys.each_ref()
                .map(|key| (key.public_key(), Amount::raw(1))),
        ));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        assert!(container.rai_epoch_manager.start_closing(now));
        for key in &keys {
            container
                .rai_epoch_manager
                .reports_mut()
                .insert(RaiReport::new(key, 0.into(), []))
                .unwrap();
        }
        let (root, candidate) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .insert_close_cut(
                RaiCloseElectionSpec {
                    id: RaiCloseElectionId {
                        kind: RaiCloseKind::Cut,
                        epoch: 0.into(),
                        round: 0,
                    },
                    root,
                    candidate,
                    committee,
                },
                now,
            )
            .unwrap();
        assert_eq!(
            container
                .roots
                .election_for_rai_id(&crate::consensus::election::RaiElectionId::CloseCut {
                    epoch: 0.into(),
                    round: 0,
                })
                .unwrap()
                .state(),
            crate::consensus::election::ElectionState::Active
        );

        let rep_weights = RepWeights::default();
        let quorum = QuorumSnapshot::new_test_instance();
        let mut apply = |key: &PrivateKey, phase, expected| {
            let vote: FilteredVote = ReceivedVote::new(
                Vote::new_rai(
                    key,
                    UnixMillisTimestamp::new(1),
                    0,
                    candidate,
                    RaiVoteMetadata {
                        election_id: crate::consensus::election::RaiElectionId::CloseCut {
                            epoch: 0.into(),
                            round: 0,
                        },
                        phase,
                        epoch: 0.into(),
                        scope: RaiCommitteeScope::All,
                    },
                )
                .into(),
                VoteDelivery::Direct,
                None,
            )
            .into();
            assert_eq!(
                container.apply_vote(ApplyVoteArgs {
                    vote: &vote,
                    rep_weights: &rep_weights,
                    quorum_snapshot: &quorum,
                    now,
                })[&candidate],
                expected
            );
        };
        for key in &keys[..3] {
            apply(key, RaiVotePhase::First, Ok(()));
        }
        for key in &keys[..4] {
            apply(key, RaiVotePhase::Notar, Ok(()));
        }
        drop(apply);

        let round_zero = crate::consensus::election::RaiElectionId::CloseCut {
            epoch: 0.into(),
            round: 0,
        };
        assert_eq!(
            container
                .roots
                .election_for_rai_id(&round_zero)
                .unwrap()
                .rai_votes
                .local_result(0),
            Some(RaiLocalResult::Notarized(candidate))
        );

        container.rai_tick(
            now + Duration::from_millis(100),
            &keys[0],
            Duration::from_secs(30),
        );

        // Give the normal voting pass a base-latency window to emit Final
        // votes before falling back to a carried successor round.
        assert_eq!(
            container
                .rai_epoch_manager
                .close_cut_tracker(0.into())
                .unwrap()
                .current_round(),
            0
        );
        assert!(container.roots.election_for_rai_id(&round_zero).is_some());

        container.rai_tick(
            now + Duration::from_millis(1100),
            &keys[0],
            Duration::from_secs(30),
        );

        let tracker = container
            .rai_epoch_manager
            .close_cut_tracker(0.into())
            .unwrap();
        assert_eq!(tracker.current_round(), 1);
        assert_eq!(
            tracker.round(0).unwrap().finished,
            RaiCloseRoundResult::LiveCarry(candidate)
        );
        assert_eq!(tracker.round(1).unwrap().carried, Some(candidate));
        let round_one = crate::consensus::election::RaiElectionId::CloseCut {
            epoch: 0.into(),
            round: 1,
        };
        assert!(container.roots.election_for_rai_id(&round_zero).is_some());
        assert!(container.roots.election_for_rai_id(&round_one).is_some());

        // A carried successor does not disable its source. These delayed
        // First votes can still form the source round's fast certificate.
        // These two delayed First votes bring round zero from three to five
        // signers, which is the six-member fast threshold.
        for key in &keys[3..5] {
            let vote: FilteredVote = ReceivedVote::new(
                Vote::new_rai(
                    key,
                    UnixMillisTimestamp::new(2),
                    0,
                    candidate,
                    RaiVoteMetadata {
                        election_id: crate::consensus::election::RaiElectionId::CloseCut {
                            epoch: 0.into(),
                            round: 0,
                        },
                        phase: RaiVotePhase::First,
                        epoch: 0.into(),
                        scope: RaiCommitteeScope::All,
                    },
                )
                .into(),
                VoteDelivery::Direct,
                None,
            )
            .into();
            assert_eq!(
                container.apply_vote(ApplyVoteArgs {
                    vote: &vote,
                    rep_weights: &rep_weights,
                    quorum_snapshot: &quorum,
                    now,
                })[&candidate],
                Ok(())
            );
        }
        assert_eq!(
            container.rai_epoch_manager.decided_close_hash(0.into()),
            Some(candidate)
        );
        assert_eq!(
            container.rai_epoch_manager.closing_epoch().unwrap().phase,
            crate::consensus::rai::RaiClosingPhase::Draining
        );
        assert!(container.roots.election_for_rai_id(&round_zero).is_none());
        assert!(container.roots.election_for_rai_id(&round_one).is_none());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn retained_old_close_round_cannot_advance_the_current_round() {
        use crate::consensus::{
            RaiCloseElectionSpec,
            rai::{
                BlockHashOrTimeout, RaiCloseElectionId, RaiCloseKind, RaiCloseRoundResult,
                RaiElectionVoteState, RaiLocalResult, RaiReport,
            },
        };
        use rsnano_types::{Amount, PrivateKey, RaiCommitteeScope};

        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
            PrivateKey::from(5),
            PrivateKey::from(6),
        ];
        let committee = std::sync::Arc::new(RepWeights::from(
            keys.each_ref()
                .map(|key| (key.public_key(), Amount::raw(1))),
        ));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        assert!(container.rai_epoch_manager.start_closing(now));
        for key in &keys {
            container
                .rai_epoch_manager
                .reports_mut()
                .insert(RaiReport::new(key, 0.into(), []))
                .unwrap();
        }
        let (root, candidate) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        let round_zero = crate::consensus::election::RaiElectionId::CloseCut {
            epoch: 0.into(),
            round: 0,
        };
        container
            .insert_close_cut(
                RaiCloseElectionSpec {
                    id: RaiCloseElectionId {
                        kind: RaiCloseKind::Cut,
                        epoch: 0.into(),
                        round: 0,
                    },
                    root,
                    candidate,
                    committee: committee.clone(),
                },
                now,
            )
            .unwrap();

        let notarized_evidence = || {
            let mut evidence = RaiElectionVoteState::new(vec![committee.clone()]);
            for key in &keys[..4] {
                evidence
                    .record_first_vote(
                        key.public_key(),
                        BlockHashOrTimeout::Block(candidate),
                        RaiCommitteeScope::All,
                    )
                    .unwrap();
            }
            assert_eq!(
                evidence.local_result(0),
                Some(RaiLocalResult::Notarized(candidate))
            );
            evidence
        };
        container
            .roots
            .election_for_rai_id_mut(&round_zero)
            .unwrap()
            .rai_votes = notarized_evidence();
        container.progress_close_election(&round_zero, now);
        container.progress_close_election(&round_zero, now + Duration::from_millis(1100));

        let round_one = crate::consensus::election::RaiElectionId::CloseCut {
            epoch: 0.into(),
            round: 1,
        };
        assert!(container.roots.election_for_rai_id(&round_one).is_some());
        assert!(container.rai_epoch_manager.store_close_cut_evidence(
            0.into(),
            1,
            notarized_evidence(),
        ));
        assert_eq!(
            container
                .rai_epoch_manager
                .close_cut_tracker(0.into())
                .unwrap()
                .round(1)
                .unwrap()
                .finished,
            RaiCloseRoundResult::LiveCarry(candidate)
        );

        // Reconciliation may revisit retained round zero after round one's
        // evidence has become terminal. Its elapsed grace belongs only to
        // round zero and must not be used to advance round one.
        container.progress_close_election(&round_zero, now + Duration::from_millis(2200));

        let tracker = container
            .rai_epoch_manager
            .close_cut_tracker(0.into())
            .unwrap();
        assert_eq!(tracker.current_round(), 1);
        assert!(tracker.round(2).is_none());
        assert!(
            container
                .roots
                .election_for_rai_id(&crate::consensus::election::RaiElectionId::CloseCut {
                    epoch: 0.into(),
                    round: 2,
                })
                .is_none()
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn timed_out_close_round_waits_for_data_repair_before_advancing() {
        use crate::consensus::{
            RaiCloseElectionSpec,
            rai::{BlockHashOrTimeout, RaiCloseElectionId, RaiCloseKind, RaiElectionVoteState},
        };
        use rsnano_types::{Amount, PrivateKey, RaiCommitteeScope, RaiVotePhase};

        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
            PrivateKey::from(5),
            PrivateKey::from(6),
        ];
        let committee = std::sync::Arc::new(RepWeights::from(
            keys.each_ref()
                .map(|key| (key.public_key(), Amount::raw(1))),
        ));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        assert!(container.rai_epoch_manager.start_closing(now));
        for key in &keys {
            container
                .rai_epoch_manager
                .reports_mut()
                .insert(crate::consensus::rai::RaiReport::new(key, 0.into(), []))
                .unwrap();
        }
        let (root, candidate) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        let id = crate::consensus::election::RaiElectionId::CloseCut {
            epoch: 0.into(),
            round: 0,
        };
        container
            .insert_close_cut(
                RaiCloseElectionSpec {
                    id: RaiCloseElectionId {
                        kind: RaiCloseKind::Cut,
                        epoch: 0.into(),
                        round: 0,
                    },
                    root,
                    candidate,
                    committee: committee.clone(),
                },
                now,
            )
            .unwrap();

        let mut evidence = RaiElectionVoteState::new(vec![committee]);
        for key in &keys[..2] {
            evidence
                .record_first_vote(
                    key.public_key(),
                    BlockHashOrTimeout::Block(candidate),
                    RaiCommitteeScope::All,
                )
                .unwrap();
        }
        for key in &keys[2..4] {
            evidence
                .record_first_vote(
                    key.public_key(),
                    BlockHashOrTimeout::Block(BlockHash::from(99)),
                    RaiCommitteeScope::All,
                )
                .unwrap();
        }
        evidence
            .record_first_vote(
                keys[4].public_key(),
                BlockHashOrTimeout::Block(BlockHash::from(100)),
                RaiCommitteeScope::All,
            )
            .unwrap();
        for key in &keys[..4] {
            evidence
                .record_vote(
                    key.public_key(),
                    BlockHashOrTimeout::Timeout,
                    RaiVotePhase::Notar,
                    RaiCommitteeScope::All,
                )
                .unwrap();
        }
        container
            .roots
            .election_for_rai_id_mut(&id)
            .unwrap()
            .rai_votes = evidence;

        container.progress_close_election(&id, now);
        container.progress_close_election(&id, now + Duration::from_millis(250));
        assert_eq!(
            container.rai_epoch_manager.close_cut_round(0.into()),
            Some(0)
        );
        assert!(
            container.roots.election_for_rai_id(&id).is_some(),
            "the dead source round remains available for data and vote repair"
        );

        container.progress_close_election(&id, now + Duration::from_millis(350));
        assert_eq!(
            container.rai_epoch_manager.close_cut_round(0.into()),
            Some(1)
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn reconciled_close_cut_replays_cached_final_certificate() {
        use crate::consensus::{
            RaiCloseElectionSpec,
            rai::{RaiCloseCut, RaiCloseElectionId, RaiCloseKind, RaiClosingPhase, RaiReport},
        };
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiSlotId, RaiVoteMetadata, RaiVotePhase,
            UnixMillisTimestamp, Vote,
        };

        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
        ];
        let committee = std::sync::Arc::new(RepWeights::from(
            keys.each_ref()
                .map(|key| (key.public_key(), Amount::raw(1))),
        ));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        assert!(container.rai_epoch_manager.start_closing(now));
        for key in &keys {
            container
                .rai_epoch_manager
                .reports_mut()
                .insert(RaiReport::new(key, 0.into(), []))
                .unwrap();
        }
        let (root, local_hash) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .insert_close_cut(
                RaiCloseElectionSpec {
                    id: RaiCloseElectionId {
                        kind: RaiCloseKind::Cut,
                        epoch: 0.into(),
                        round: 0,
                    },
                    root,
                    candidate: local_hash,
                    committee,
                },
                now,
            )
            .unwrap();

        let remote_cut = RaiCloseCut::new(
            0.into(),
            [RaiSlotId {
                epoch: 0.into(),
                root: QualifiedRoot::new_test_instance(),
            }],
        );
        let remote_hash = remote_cut.hash();
        assert_ne!(remote_hash, local_hash);
        // Learn the candidate before its certificate, matching the repair
        // ordering which exposed the live-network race.
        assert!(container.reconcile_rai_close_cut(
            remote_cut.clone(),
            crate::consensus::rai::rai_close_cut_root(0.into(), 0).root,
            now
        ));
        let id = crate::consensus::election::RaiElectionId::CloseCut {
            epoch: 0.into(),
            round: 0,
        };
        for key in &keys {
            container
                .rai_pending_votes
                .entry(id.clone())
                .or_default()
                .push(Arc::new(Vote::new_rai(
                    key,
                    UnixMillisTimestamp::new(1),
                    0,
                    remote_hash,
                    RaiVoteMetadata {
                        election_id: crate::consensus::election::RaiElectionId::CloseCut {
                            epoch: 0.into(),
                            round: 0,
                        },
                        phase: RaiVotePhase::Final,
                        epoch: 0.into(),
                        scope: RaiCommitteeScope::All,
                    },
                )));
        }

        assert_eq!(
            container.rai_epoch_manager.decided_close_hash(0.into()),
            None
        );
        // The candidate is a duplicate, but reconciliation must still replay
        // the certificate which arrived after the first preimage response.
        assert!(container.reconcile_rai_close_cut(
            remote_cut,
            crate::consensus::rai::rai_close_cut_root(0.into(), 0).root,
            now
        ));
        assert_eq!(
            container.rai_epoch_manager.decided_close_hash(0.into()),
            Some(remote_hash)
        );
        assert_eq!(
            container.rai_epoch_manager.closing_epoch().unwrap().phase,
            RaiClosingPhase::Draining
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_record_uses_normal_votes_and_closes_epoch_zero() {
        use crate::consensus::{
            FilteredVote, RaiCloseElectionSpec, ReceivedVote,
            election::RaiElectionKind,
            rai::{RaiCloseElectionId, RaiCloseKind, RaiReport, rai_close_record_root},
        };
        use rsnano_types::{
            Account, Amount, ConfirmationHeightInfo, PrivateKey, RaiCommitteeScope,
            RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        container.rai_epoch_manager.start_closing(now);
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(RaiReport::new(&key, 0.into(), []))
            .unwrap();
        let (_, cut) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(0.into(), 0, cut)
            .unwrap();
        container.rai_epoch_manager.initialize_drain_frontiers(
            0.into(),
            [(
                Account::from(1),
                ConfirmationHeightInfo::new(4, BlockHash::from(40)),
            )],
        );
        let (root, candidate) = container
            .rai_epoch_manager
            .begin_close_record(RepWeights::default())
            .unwrap();
        let id = RaiCloseElectionId {
            kind: RaiCloseKind::Record,
            epoch: 0.into(),
            round: 0,
        };
        container
            .insert_close_record(
                RaiCloseElectionSpec {
                    id,
                    root: root.clone(),
                    candidate,
                    committee,
                },
                now,
            )
            .unwrap();

        assert_eq!(root, rai_close_record_root(0.into(), 0));
        assert_eq!(
            container.election_for_root(&root).unwrap().rai_kind(),
            RaiElectionKind::CloseRecord
        );
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                candidate,
                RaiVoteMetadata {
                    election_id: crate::consensus::election::RaiElectionId::CloseRecord {
                        epoch: 0.into(),
                        round: 0,
                    },
                    phase: RaiVotePhase::First,
                    epoch: 0.into(),
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        assert_eq!(
            container.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: &RepWeights::default(),
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            })[&candidate],
            Ok(())
        );
        assert_eq!(
            container.rai_epoch_manager.installed_close_hash(0.into()),
            Some(candidate)
        );
        assert_eq!(
            container.rai_epoch_manager.state().closed_through,
            Some(crate::consensus::rai::RaiEpoch::ZERO)
        );
        assert_eq!(
            container.rai_epoch_manager.state().open_epoch,
            crate::consensus::rai::RaiEpoch::new(1)
        );
        assert_eq!(
            container.rai_epoch_manager.committee_at(0).unwrap(),
            std::sync::Arc::new(RepWeights::default())
        );
        assert!(container.election_for_root(&root).is_none());
        assert!(container.rai_pending_votes.contains_key(
            &crate::consensus::election::RaiElectionId::CloseRecord {
                epoch: 0.into(),
                round: 0,
            }
        ));
        let regenerated = container
            .rai_finalized_close_vote_target(&root.root)
            .unwrap();
        assert_eq!(regenerated.hash, candidate);
        assert_eq!(regenerated.root, root.root);
        assert_eq!(regenerated.metadata.phase, RaiVotePhase::Final);
        assert_eq!(
            regenerated.election_id,
            crate::consensus::election::RaiElectionId::CloseRecord {
                epoch: 0.into(),
                round: 0,
            }
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn reconciled_close_record_replays_cached_final_certificate() {
        use crate::consensus::{
            FilteredVote, RaiCloseElectionSpec, ReceivedVote,
            rai::{RaiCloseElectionId, RaiCloseKind, RaiCloseRecord, RaiReport},
        };
        use rsnano_types::{
            Account, Amount, ConfirmationHeightInfo, PrivateKey, RaiCommitteeScope,
            RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        container.rai_epoch_manager.start_closing(now);
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(RaiReport::new(&key, 0.into(), []))
            .unwrap();
        let (_, cut) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(0.into(), 0, cut)
            .unwrap();
        let account = Account::from(1);
        container.rai_epoch_manager.initialize_drain_frontiers(
            0.into(),
            [(account, ConfirmationHeightInfo::new(4, BlockHash::from(40)))],
        );
        let (root, local_hash) = container
            .rai_epoch_manager
            .begin_close_record(committee.as_ref().clone())
            .unwrap();
        container
            .insert_close_record(
                RaiCloseElectionSpec {
                    id: RaiCloseElectionId {
                        kind: RaiCloseKind::Record,
                        epoch: 0.into(),
                        round: 0,
                    },
                    root,
                    candidate: local_hash,
                    committee,
                },
                now,
            )
            .unwrap();

        let remote_record = RaiCloseRecord::new(
            0.into(),
            BlockHash::ZERO,
            [(account, ConfirmationHeightInfo::new(5, BlockHash::from(50)))],
        );
        let remote_hash = remote_record.hash();
        assert_ne!(remote_hash, local_hash);
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                remote_hash,
                RaiVoteMetadata {
                    election_id: crate::consensus::election::RaiElectionId::CloseRecord {
                        epoch: 0.into(),
                        round: 0,
                    },
                    phase: RaiVotePhase::Final,
                    epoch: 0.into(),
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        assert_eq!(
            container.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: &RepWeights::default(),
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            })[&remote_hash],
            Err(VoteError::Indeterminate)
        );

        assert!(container.reconcile_rai_close_record(
            remote_record,
            crate::consensus::rai::rai_close_record_root(0.into(), 0).root,
            now
        ));
        assert_eq!(
            container.rai_epoch_manager.installed_close_hash(0.into()),
            Some(remote_hash)
        );
        assert!(container.rai_epoch_manager.closing_epoch().is_none());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn unknown_close_cut_preimage_cannot_be_voted_for() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{PrivateKey, RaiVoteMetadata, UnixMillisTimestamp, Vote, VoteDelivery};

        let mut container = ActiveElectionsContainer::default();
        let unknown = BlockHash::from(999);
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &PrivateKey::from(1),
                UnixMillisTimestamp::new(1),
                0,
                unknown,
                RaiVoteMetadata::default(),
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now: Timestamp::new_test_instance(),
        });

        assert_eq!(result[&unknown], Err(VoteError::Invalid));
    }

    #[cfg(not(feature = "rai_protocol"))]
    #[test]
    fn confirm_election() {
        let mut container = ActiveElectionsContainer::default();

        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();

        let request = AecInsertRequest {
            block,
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        let now = Timestamp::new_test_instance();
        container.insert(request, now).unwrap();

        let rep_key = PrivateKey::from(1);
        let received_vote = test_final_vote(&rep_key, block_hash);

        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);

        let result = container.apply_vote(ApplyVoteArgs {
            vote: &received_vote.into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        assert_eq!(result.get(&block_hash), Some(&Ok(())));

        assert!(container.election_for_block(&block_hash).is_none());
    }

    #[test]
    fn iter_round_robin() {
        let block_a = SavedBlock::new_test_instance_with_key(1);
        let block_b = SavedBlock::new_test_instance_with_key(2);
        let block_c = SavedBlock::new_test_instance_with_key(3);
        let block_d = SavedBlock::new_test_instance_with_key(4);

        let prio_a = BlockPriority::new(Amount::nano(1), TimePriority::new(100));
        let prio_b = BlockPriority::new(Amount::nano(100), TimePriority::new(100));
        let prio_c = BlockPriority::new(Amount::nano(100), TimePriority::new(99));
        let prio_d = BlockPriority::new(Amount::nano(1_000_000), TimePriority::new(100));

        test_iter(&[], &[]);

        test_iter(&[(&block_a, prio_a)], &[&block_a]);

        test_iter(
            &[
                (&block_d, prio_d),
                (&block_a, prio_a),
                (&block_c, prio_c),
                (&block_b, prio_b),
            ],
            &[&block_d, &block_c, &block_a, &block_b],
        )
    }

    #[test]
    fn reports_stale_election_count() {
        let mut container = ActiveElectionsContainer::default();
        let request = AecInsertRequest {
            block: SavedBlock::new_test_instance(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        let start = Timestamp::new_test_instance();

        container.insert(request, start).unwrap();

        assert_eq!(container.info(start).stale, 0);
        assert_eq!(container.info(start + Duration::from_secs(60)).stale, 1);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn discovery_vote_is_not_rai_consensus_evidence() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{PrivateKey, RaiVoteMetadata, UnixMillisTimestamp, Vote, VoteDelivery};

        let mut container = ActiveElectionsContainer::default();
        let hash = BlockHash::from(1);
        let received: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &PrivateKey::from(1),
                UnixMillisTimestamp::new(16),
                0,
                hash,
                RaiVoteMetadata::default(),
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();

        let result = container.apply_vote(ApplyVoteArgs {
            vote: &received,
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now: Timestamp::new_test_instance(),
        });

        assert_eq!(result[&hash], Err(VoteError::Invalid));
        assert_eq!(container.len(), 0);
        assert!(container.rai_pending_votes.is_empty());
        assert!(container.rai_candidate_hashes.is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_vote_repair_finds_election_in_secondary_rai_index() {
        use crate::consensus::rai::{RaiCloseElectionId, RaiCloseKind, rai_close_record_root};
        use rsnano_types::{Amount, PrivateKey, RaiEpoch};

        let epoch = RaiEpoch::ZERO;
        let round = 0;
        let root = rai_close_record_root(epoch, round);
        let candidate = BlockHash::from(99);
        let committee = Arc::new(RepWeights::from([(
            PrivateKey::from(1).public_key(),
            Amount::raw(1),
        )]));
        let mut container = ActiveElectionsContainer::default();

        // Occupy the primary qualified-root map first, forcing the close
        // election into RootContainer::rai_entries.
        let blocker = SavedBlock::new_test_instance();
        container.roots.insert(Entry {
            root: root.clone(),
            election: Election::new(
                blocker,
                ElectionBehavior::Manual,
                Duration::ZERO,
                Timestamp::new_test_instance(),
            ),
            priority: BlockPriority::default(),
        });
        let id = RaiCloseElectionId {
            kind: RaiCloseKind::Record,
            epoch,
            round,
        };
        assert!(container.roots.insert_rai(Entry {
            root: root.clone(),
            election: Election::new_close(
                id,
                root.clone(),
                candidate,
                committee,
                Duration::ZERO,
                Timestamp::new_test_instance(),
            ),
            priority: BlockPriority::default(),
        }));

        let requests = container.rai_active_close_vote_requests(epoch, 16);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, candidate);
        assert_eq!(requests[0].1, root.root);
        assert_eq!(
            requests[0].2,
            crate::consensus::election::RaiElectionId::CloseRecord { epoch, round }
        );

        // A close timeout is terminal for the election, but not for the
        // logical round. Keep soliciting the timeout target until every
        // replica can advance from the retained certificate evidence.
        container
            .roots
            .election_for_rai_id_mut(&requests[0].2)
            .unwrap()
            .rai_votes
            .outcome = crate::consensus::rai::RaiOutcome::TimedOut;
        let requests = container.rai_active_close_vote_requests(epoch, 16);
        assert_eq!(requests.len(), 1);
        assert!(
            container
                .rai_active_close_vote_target_for_root(&root.root)
                .is_some()
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rejects_vote_when_governing_close_is_unavailable() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{
            PrivateKey, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId, RaiVoteMetadata,
            RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let mut container = ActiveElectionsContainer::default();
        let hash = BlockHash::from(1);
        let epoch = RaiEpoch::new(2);
        let election_id = RaiElectionId::Slot(RaiSlotId {
            epoch,
            root: QualifiedRoot::new_test_instance(),
        });
        let received: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &PrivateKey::from(1),
                UnixMillisTimestamp::new(16),
                0,
                hash,
                RaiVoteMetadata {
                    election_id: election_id.clone(),
                    phase: RaiVotePhase::First,
                    epoch,
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();

        let result = container.apply_vote(ApplyVoteArgs {
            vote: &received,
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now: Timestamp::new_test_instance(),
        });

        assert_eq!(result[&hash], Err(VoteError::Invalid));
        assert!(!container.rai_pending_votes.contains_key(&election_id));
    }

    #[cfg(not(feature = "rai_protocol"))]
    fn test_final_vote(rep_key: &PrivateKey, block_hash: BlockHash) -> ReceivedVote {
        let vote = Arc::new(Vote::new_final(rep_key, vec![block_hash]));
        ReceivedVote::new(vote, VoteDelivery::Direct, None)
    }

    fn test_iter(blocks: &[(&SavedBlock, BlockPriority)], expected: &[&SavedBlock]) {
        let mut container = ActiveElectionsContainer::default();

        for (block, prio) in blocks {
            let request = AecInsertRequest::new_priority((**block).clone(), *prio);

            container
                .insert(request, Timestamp::new_test_instance())
                .unwrap();
        }

        let result: Vec<_> = container
            .iter_round_robin()
            .map(|i| i.winner().hash())
            .collect();
        let expected: Vec<_> = expected.iter().map(|i| i.hash()).collect();
        assert_eq!(result, expected);
    }
}
