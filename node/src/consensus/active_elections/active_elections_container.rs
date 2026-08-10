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

#[cfg(feature = "rai_protocol")]
#[derive(Clone)]
struct RaiTerminalSlot {
    outcome: crate::consensus::rai::RaiOutcome,
    account: rsnano_types::Account,
    frontier: Option<rsnano_types::ConfirmationHeightInfo>,
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
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};

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
    rai_visible_obligations: std::collections::BTreeSet<crate::consensus::election::RaiSlotId>,
    #[cfg(feature = "rai_protocol")]
    rai_terminal_slots:
        std::collections::BTreeMap<crate::consensus::election::RaiSlotId, RaiTerminalSlot>,
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
    #[cfg(feature = "rai_protocol")]
    rai_unresolved_references: std::collections::HashSet<(rsnano_types::RaiEpoch, BlockHash)>,
    #[cfg(feature = "rai_protocol")]
    rai_candidate_hashes:
        HashMap<crate::consensus::election::RaiSlotId, std::collections::HashSet<BlockHash>>,
    /// Process-lifetime vote evidence, including votes received before an
    /// election starts and votes for elections evicted from the active set.
    #[cfg(feature = "rai_protocol")]
    rai_pending_votes: HashMap<crate::consensus::election::RaiElectionId, Vec<rsnano_types::Vote>>,
    /// The live ledger is mandatory for publishing a close decision. Direct
    /// container unit tests omit it and use the in-memory state machine only.
    #[cfg(feature = "rai_protocol")]
    rai_ledger: Option<Arc<Ledger>>,
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
    fn commit_rai_close_frontiers(
        ledger: Option<&Ledger>,
        epoch: rsnano_types::RaiEpoch,
        frontiers: &crate::consensus::rai::RaiFrontierMap,
    ) -> bool {
        let Some(ledger) = ledger else {
            debug_assert!(cfg!(test), "live RAI close installation requires a ledger");
            return cfg!(test);
        };
        let stopped = AtomicBool::new(false);
        let mut observer = RaiCloseCementingObserver::default();
        ledger.confirm_batch_rai(
            frontiers
                .values()
                .map(|frontier| (&frontier.frontier, Some(epoch))),
            &stopped,
            usize::MAX,
            &mut observer,
        );
        if observer.failed {
            return false;
        }

        let preceding = ledger.rai_preceding_frontiers(epoch);
        frontiers.iter().all(|(account, frontier)| {
            preceding
                .get(account)
                .is_some_and(|base| frontier.height <= base.height)
                || ledger.rai_finalization_epoch(&frontier.frontier) == Some(epoch)
        })
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
        self.rai_epoch_manager
            .install_certified_close_record_after(
                epoch,
                round,
                hash,
                weights,
                move |epoch, frontiers| {
                    Self::commit_rai_close_frontiers(ledger.as_deref(), epoch, frontiers)
                },
            )
            .cloned()
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_replay_frontier(
        &self,
        hash: BlockHash,
        root: &QualifiedRoot,
        ledger: &rsnano_ledger::Ledger,
    ) -> Option<(rsnano_types::Account, rsnano_types::ConfirmationHeightInfo)> {
        let any = ledger.any();
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
    pub fn rai_progress_close(
        &mut self,
        frontiers: crate::consensus::rai::RaiFrontierMap,
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
        let frontiers = closing
            .epoch
            .number()
            .checked_sub(1)
            .and_then(|epoch| {
                self.rai_epoch_manager
                    .durable_close_state(rsnano_types::RaiEpoch::new(epoch))
            })
            .map(|state| state.frontiers)
            .unwrap_or(frontiers);
        self.rai_epoch_manager
            .initialize_drain_frontiers(closing.epoch, frontiers);
        let obligations = self
            .rai_epoch_manager
            .obligations_to_drain(closing.epoch)
            .cloned()
            .unwrap_or_default();
        for slot in obligations {
            let any = ledger.any();
            if let Some(hash) = any.block_successor_by_qualified_root(&slot.root)
                && ledger.rai_finalization_epoch(&hash) == Some(slot.epoch)
                && let Some(block) = any.get_block(&hash)
                && block.qualified_root() == slot.root
                && self.rai_epoch_manager.record_finalized_drain(
                    closing.epoch,
                    &slot,
                    hash,
                    [(
                        block.account(),
                        rsnano_types::ConfirmationHeightInfo::new(block.height(), hash),
                    )],
                )
            {
                continue;
            }
            if let Some(terminal) = self.rai_terminal_slots.get(&slot).cloned() {
                let terminal_hash = match terminal.outcome {
                    crate::consensus::rai::RaiOutcome::Notarized(hash)
                    | crate::consensus::rai::RaiOutcome::Confirmed(hash) => Some(hash),
                    _ => None,
                };
                // The election may have ended while its winner was still an
                // unsaved publish. Resolve the certified digest against the
                // ledger now; close-local replay must not freeze an empty
                // segment merely because block processing lagged the vote.
                let segment = terminal_hash
                    .and_then(|hash| self.rai_replay_frontier(hash, &slot.root, ledger))
                    .map(|frontier| [frontier])
                    .or_else(|| terminal.frontier.map(|info| [(terminal.account, info)]))
                    .unwrap_or_default();
                let outcome = match terminal.outcome {
                    crate::consensus::rai::RaiOutcome::Notarized(hash) => self
                        .rai_epoch_manager
                        .record_notarized_drain(closing.epoch, &slot, hash, segment),
                    crate::consensus::rai::RaiOutcome::Confirmed(hash) => self
                        .rai_epoch_manager
                        .record_finalized_drain(closing.epoch, &slot, hash, segment)
                        .then_some(crate::consensus::rai::RaiDrainOutcome::Finalized(hash)),
                    crate::consensus::rai::RaiOutcome::Pending
                    | crate::consensus::rai::RaiOutcome::TimedOut => None,
                };
                if outcome.is_some() {
                    if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                        eprintln!(
                            "RAI_MSG pr={pr} event=drain_settled source=terminal slot={slot:?} outcome={outcome:?}"
                        );
                    }
                    continue;
                }
                debug_assert!(
                    false,
                    "pending RAI slot was removed before obtaining terminal certificate evidence"
                );
            }
            let id = crate::consensus::election::RaiElectionId::Slot(slot.clone());
            // Repair replies can be delivered in a phase order different from
            // the original election. Re-evaluate the durable batch in
            // First/Notar/Final order before deriving drain evidence.
            self.apply_pending_rai_votes(&id, now);
            let Some(election) = self.roots.election_for_rai_id(&id) else {
                continue;
            };
            let evidence = election.rai_votes.clone();
            let winner_hash = election.winner().hash();
            let confirmed = self.rai_replay_frontier(winner_hash, &slot.root, ledger);
            let outcome = self
                .rai_epoch_manager
                .happy_path_drain(closing.epoch)
                .and_then(|drain| {
                    let mut probe = drain.clone();
                    probe.record_persistent_evidence(&slot, &evidence)
                });
            if let Some(outcome) = outcome {
                let segment = match outcome {
                    crate::consensus::rai::RaiDrainOutcome::Finalized(hash)
                    | crate::consensus::rai::RaiDrainOutcome::Selected(hash) => confirmed
                        .filter(|(_, info)| info.frontier == hash)
                        .map(|(account, info)| [(account, info)])
                        .unwrap_or_default(),
                    crate::consensus::rai::RaiDrainOutcome::ReleasedTimeout
                    | crate::consensus::rai::RaiDrainOutcome::ReleasedConflict => {
                        Default::default()
                    }
                };
                let recorded = self.rai_epoch_manager.record_drain_evidence(
                    closing.epoch,
                    &slot,
                    &evidence,
                    segment,
                );
                if recorded.is_some()
                    && let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR")
                {
                    eprintln!(
                        "RAI_MSG pr={pr} event=drain_settled source=active slot={slot:?} outcome={recorded:?}"
                    );
                }
            }
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
        let mut visible = self
            .rai_visible_obligations
            .iter()
            .filter(|slot| {
                self.rai_epoch_manager
                    .slot_election_enabled(slot.epoch, &slot.root)
            })
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for entry in self.roots.iter_rai() {
            if entry.election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot {
                let slot = crate::consensus::election::RaiSlotId {
                    epoch: entry.election.rai_epoch(),
                    root: entry.root.clone(),
                };
                if self
                    .rai_epoch_manager
                    .slot_election_enabled(slot.epoch, &slot.root)
                {
                    visible.insert(slot);
                }
            }
        }
        let visible =
            visible
                .into_iter()
                .fold(std::collections::BTreeMap::new(), |mut by_epoch, slot| {
                    by_epoch
                        .entry(slot.epoch)
                        .or_insert_with(std::collections::BTreeSet::new)
                        .insert(slot);
                    by_epoch
                });

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

    #[cfg(feature = "rai_protocol")]
    pub fn rai_pending_slot_requests(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<(BlockHash, rsnano_types::Root)> {
        let mut requests = self
            .roots
            .iter_rai()
            .filter(|entry| {
                entry.election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot
                    && entry.election.rai_epoch() == epoch
                    && !entry.election.state().has_ended()
                    && self.rai_election_vote_enabled(entry.election.rai_id())
            })
            .map(|entry| (entry.election.rai_request_hash(), entry.root.root))
            .collect::<Vec<_>>();
        // A cut can contain a slot whose payload was only visible to another
        // reporter. Ask by qualified root even before the block is local; the
        // responder returns the payload before replaying its certificate.
        if let Some(drain) = self.rai_epoch_manager.happy_path_drain(epoch) {
            requests.extend(
                drain
                    .obligations
                    .iter()
                    .filter(|slot| {
                        !drain.finalized.contains_key(*slot)
                            && !drain.selected.contains_key(*slot)
                            && !drain.released.contains_key(*slot)
                    })
                    .map(|slot| (BlockHash::ZERO, slot.root.root)),
            );
        }
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
        let Some(mut current) = selected else {
            return Vec::new();
        };
        let mut segment = Vec::new();
        while let Some(block) = self.rai_blocks.get(&current).cloned() {
            current = block.previous();
            segment.push(block);
            if current.is_zero() || segment.len() >= 16 * 1024 {
                break;
            }
        }
        segment.reverse();
        segment
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_slot_vote_context_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<rsnano_types::RaiVoteMetadata> {
        if let Some(entry) = self.roots.iter_rai().find(|entry| {
            entry.root.root == *root
                && entry.election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot
                && self.rai_election_vote_enabled(entry.election.rai_id())
        }) {
            return Some(entry.election.rai_vote_metadata());
        }
        self.rai_terminal_slots
            .keys()
            .find(|slot| {
                slot.root.root == *root
                    && self
                        .rai_epoch_manager
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
        if let Some(entry) = self.roots.iter_rai().find(|entry| {
            entry.root.root == *root
                && entry.election.rai_epoch() == epoch
                && entry.election.rai_request_hash().is_zero()
                && !entry.election.state().has_ended()
                && self.rai_election_vote_enabled(entry.election.rai_id())
        }) {
            return Some((BlockHash::ZERO, entry.election.rai_vote_metadata()));
        }
        let (slot, terminal) = self.rai_terminal_slots.iter().find(|(slot, terminal)| {
            slot.root.root == *root
                && slot.epoch == epoch
                && self
                    .rai_epoch_manager
                    .slot_election_enabled(slot.epoch, &slot.root)
                && matches!(
                    terminal.outcome,
                    crate::consensus::rai::RaiOutcome::Notarized(_)
                )
        })?;
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
        let entry = self.roots.iter_rai().find(|entry| {
            entry.root.root == *root
                && entry.election.rai_epoch() == epoch
                && entry.election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot
                && !entry.election.state().has_ended()
                && self.rai_election_vote_enabled(entry.election.rai_id())
        })?;
        let election_id = entry.election.rai_id().clone();
        Some(rsnano_ledger::RaiFinalizedVoteTarget {
            election_id: election_id.clone(),
            hash: entry.election.voting_hash(),
            root: *root,
            metadata: entry.election.rai_vote_metadata(),
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
            .cloned()
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
    pub(crate) fn rai_close_votes_for_epoch(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<rsnano_types::Vote> {
        self.rai_pending_votes
            .iter()
            .filter(|(id, _)| {
                matches!(id,
                    crate::consensus::election::RaiElectionId::CloseCut { epoch: vote_epoch, .. }
                    | crate::consensus::election::RaiElectionId::CloseRecord { epoch: vote_epoch, .. }
                    if *vote_epoch == epoch)
            })
            .flat_map(|(_, votes)| votes.iter().cloned())
            .collect()
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
        let mut votes = self.rai_pending_votes.get(id)?.clone();
        votes.sort_by_key(|vote| match vote.metadata.phase {
            rsnano_types::RaiVotePhase::First => 0,
            rsnano_types::RaiVotePhase::Notar => 1,
            rsnano_types::RaiVotePhase::Final => 2,
        });
        for vote in votes {
            if vote.metadata.election_id != *id || vote.metadata.epoch != epoch {
                continue;
            }
            for hash in vote.hashes {
                let value =
                    if hash.is_zero() && vote.metadata.phase == rsnano_types::RaiVotePhase::Notar {
                        BlockHashOrTimeout::Timeout
                    } else {
                        if vote.metadata.phase != rsnano_types::RaiVotePhase::First
                            && !validated_preimages.contains(&hash)
                        {
                            continue;
                        }
                        BlockHashOrTimeout::Block(hash)
                    };
                let _ = evidence.record_vote(
                    vote.voter,
                    value,
                    vote.metadata.phase,
                    vote.metadata.scope,
                );
            }
        }
        Some(evidence)
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
        // A notarization makes the election's next locally generated vote a
        // Final vote. Keep the current round alive for the same base-latency
        // window used by ordinary elections so the voting scheduler can emit
        // and disseminate that vote before we retire the round and carry its
        // value forward. Advancing immediately here caused healthy close
        // elections to churn through successor rounds until delayed evidence
        // happened to finalize a retired round.
        if matches!(
            evidence.local_result(0),
            Some(crate::consensus::rai::RaiLocalResult::Notarized(_))
        ) {
            let notarized_at = *self.rai_close_notarized_at.entry(id.clone()).or_insert(now);
            if notarized_at.elapsed(now) < self.base_latency {
                return;
            }
        } else {
            self.rai_close_notarized_at.remove(id);
        }
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
        // A carried round intentionally reuses the source hash. Retire the
        // completed source election before insertion so the vote-router's
        // per-hash uniqueness check does not reject the successor. The round
        // tracker already durably records the carry, allowing tick repair to
        // recreate the successor if insertion is interrupted.
        self.rai_close_notarized_at.remove(id);
        if let Some(entry) = self.roots.erase_rai_id(id) {
            self.cleanup_election(entry);
        }
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
        let Some(entry) = self.roots.erase_rai_id(id) else {
            return;
        };
        self.rai_close_notarized_at.remove(id);
        self.record_rai_close_election_duration(id, now);
        match kind {
            crate::consensus::rai::RaiCloseKind::Cut => {
                self.rai_epoch_manager
                    .store_close_cut_evidence(epoch, round, evidence);
                let _ = self.rai_epoch_manager.decide_close_cut(epoch, round, hash);
            }
            crate::consensus::rai::RaiCloseKind::Record => {
                self.rai_epoch_manager
                    .store_close_record_evidence(epoch, round, evidence);
                if self
                    .install_close_record_with_commit(epoch, round, hash, None)
                    .is_ok()
                {
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
        }
        self.cleanup_election(entry);
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
            rai_visible_obligations: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_terminal_slots: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_blocks: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_blocks_by_qualified_root: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_payload_incomplete: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_unresolved_references: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_candidate_hashes: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_pending_votes: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_ledger: None,
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

        let waiting = self
            .rai_payload_incomplete
            .iter()
            .filter(|(slot, hashes)| slot.root == root && hashes.contains(&hash))
            .map(|(slot, _)| slot.clone())
            .collect::<Vec<_>>();
        for slot in waiting {
            if self.admit_candidate(slot.clone(), hash).is_ok() {
                if let Some(hashes) = self.rai_payload_incomplete.get_mut(&slot) {
                    hashes.remove(&hash);
                    if hashes.is_empty() {
                        self.rai_payload_incomplete.remove(&slot);
                    }
                }
            }
        }

        let unresolved_epochs = self
            .rai_unresolved_references
            .iter()
            .filter(|(_, unresolved_hash)| *unresolved_hash == hash)
            .map(|(epoch, _)| *epoch)
            .collect::<Vec<_>>();
        for epoch in unresolved_epochs {
            let slot = crate::consensus::election::RaiSlotId {
                epoch,
                root: root.clone(),
            };
            if self.admit_candidate(slot, hash).is_ok() {
                self.rai_unresolved_references.remove(&(epoch, hash));
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn reference_candidate(&mut self, epoch: rsnano_types::RaiEpoch, candidate: BlockHash) {
        let Some(block) = self.rai_blocks.get(&candidate) else {
            self.rai_unresolved_references.insert((epoch, candidate));
            return;
        };
        let slot = crate::consensus::election::RaiSlotId {
            epoch,
            root: block.qualified_root(),
        };
        let _ = self.admit_candidate(slot, candidate);
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

        let Some(tip) = self.rai_blocks.get(&candidate).cloned() else {
            self.rai_payload_incomplete
                .entry(slot)
                .or_default()
                .insert(candidate);
            return Err(CandidateError::UnknownBlock);
        };
        if tip.qualified_root() != slot.root {
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

        // A block at this qualified root is the one-block segment beginning at
        // the slot's certified base. Longer tips are admitted only after every
        // parent has independently passed block processing and is present.
        let result = entry.try_add_fork(&tip, Amount::ZERO);
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
            if let Some(previous) = self
                .rai_visible_obligations
                .iter()
                .filter(|known| known.root == slot.root && known.epoch < slot.epoch)
                .max_by_key(|known| known.epoch)
                && (!self.retry_released_slots
                    || self.rai_epoch_manager.certified_release(previous).is_none())
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
        if self.roots.get(&spec.root).is_some() || self.roots.vote_router.is_active(&spec.candidate)
        {
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
            .insert(crate::consensus::election::RaiSlotId {
                epoch,
                root: root.clone(),
            });

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
        let Some(mut votes) = self.rai_pending_votes.get(election_id).cloned() else {
            return;
        };
        let Some(election) = self.roots.election_for_rai_id_mut(election_id) else {
            return;
        };
        // A close election can be created after its signed traffic arrives.
        // Replay First evidence before Notar evidence so a timeout vote can be
        // checked against the complete split certificate, independent of
        // network arrival order.
        votes.sort_by_key(|vote| match vote.metadata.phase {
            rsnano_types::RaiVotePhase::First => 0,
            rsnano_types::RaiVotePhase::Notar => 1,
            rsnano_types::RaiVotePhase::Final => 2,
        });
        for vote in votes {
            for hash in &vote.hashes {
                let close_first = election.is_rai_close()
                    && vote.metadata.phase == rsnano_types::RaiVotePhase::First;
                let timeout_vote =
                    hash.is_zero() && vote.metadata.phase != rsnano_types::RaiVotePhase::Final;
                if close_first || timeout_vote || election.contains_candidate(hash) {
                    let _ = election.add_rai_vote(
                        vote.voter,
                        *hash,
                        vote.metadata.clone(),
                        vote.timestamp(),
                        now,
                    );
                }
            }
        }
        // Reconciliation may make an already cached final certificate
        // applicable without another vote arriving to drive ApplyVoteHelper.
        // RAI tallying ignores the legacy weight/quorum arguments and derives
        // its result from the election's frozen committee snapshots.
        election.update_tallies(&Default::default(), Amount::ZERO);
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
    }

    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.roots.get(root).is_some()
    }

    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.roots.vote_router.is_active(block_hash)
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
        if let Some(entry) = self.roots.iter_rai().find(|entry| {
            entry.election.voting_hash() == *block_hash
                && self.rai_election_vote_enabled(entry.election.rai_id())
        }) {
            return Some((
                entry.election.rai_vote_metadata(),
                entry.election.is_rai_close(),
            ));
        }
        if let Some(election) = self.election_for_block(block_hash)
            && self.rai_election_vote_enabled(election.rai_id())
        {
            return Some((election.rai_vote_metadata(), election.is_rai_close()));
        }

        // A peer may request missing close votes after this replica has
        // already installed and removed its close election. Persistent vote
        // evidence retains the exact signed election context needed to replay
        // those votes without reconstructing the election.
        if let Some(vote) = self
            .rai_pending_votes
            .values()
            .flatten()
            .find(|vote| vote.hashes.contains(block_hash))
        {
            let is_close = matches!(
                vote.metadata.election_id,
                crate::consensus::election::RaiElectionId::CloseCut { .. }
                    | crate::consensus::election::RaiElectionId::CloseRecord { .. }
            );
            if is_close {
                return Some((vote.metadata.clone(), true));
            }
        }

        self.rai_terminal_slots
            .iter()
            .find(|(slot, terminal)| {
                self.rai_epoch_manager
                    .slot_election_enabled(slot.epoch, &slot.root)
                    && (terminal
                        .frontier
                        .as_ref()
                        .is_some_and(|info| info.frontier == *block_hash)
                        || matches!(
                            terminal.outcome,
                            crate::consensus::rai::RaiOutcome::Notarized(hash)
                                | crate::consensus::rai::RaiOutcome::Confirmed(hash)
                                if hash == *block_hash
                        ))
            })
            .and_then(|(slot, terminal)| {
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
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_vote_context_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<rsnano_types::RaiVoteMetadata> {
        if let Some(election) = self
            .roots
            .iter_rai()
            .find(|entry| entry.root.root == *root && entry.election.is_rai_close())
        {
            return Some(election.election.rai_vote_metadata());
        }
        self.rai_pending_votes
            .values()
            .flatten()
            .find(|vote| match vote.metadata.election_id {
                crate::consensus::election::RaiElectionId::CloseCut { epoch, round } => {
                    crate::consensus::rai::rai_close_cut_root(epoch, round).root == *root
                }
                crate::consensus::election::RaiElectionId::CloseRecord { epoch, round } => {
                    crate::consensus::rai::rai_close_record_root(epoch, round).root == *root
                }
                crate::consensus::election::RaiElectionId::Slot(_) => false,
            })
            .map(|vote| vote.metadata.clone())
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
        let entry = self.roots.iter_rai().find(|entry| {
            entry.root.root == *root
                && entry.election.is_rai_close()
                && !entry.election.state().has_ended()
        })?;
        let metadata = entry.election.rai_vote_metadata();
        Some(rsnano_ledger::RaiFinalizedVoteTarget {
            election_id: metadata.election_id.clone(),
            hash: entry.election.rai_request_hash(),
            root: *root,
            metadata,
        })
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
            debug_assert!(i.election.rai_is_terminal());
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
        if election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot {
            let confirmed = match election.winner() {
                rsnano_types::MaybeSavedBlock::Saved(block) => Some(
                    rsnano_types::ConfirmationHeightInfo::new(block.height(), block.hash()),
                ),
                rsnano_types::MaybeSavedBlock::Unsaved(_) => None,
            };
            // Only certificate-terminal evidence may leave the active set.
            // Pending slots remain active and solicited until they terminate.
            if matches!(
                election.rai_votes.outcome,
                crate::consensus::rai::RaiOutcome::Notarized(_)
                    | crate::consensus::rai::RaiOutcome::Confirmed(_)
            ) {
                self.rai_terminal_slots.insert(
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
        {
            // Contextless ConfirmReq replies are signed only to prove which
            // representative owns the direct channel used by RepCrawler. They
            // must be rejected before candidate references, persistent vote
            // retention, or election tallies can observe them. VoteApplier
            // still publishes VoteProcessed, allowing RepCrawler to match the
            // authenticated response to its outstanding query.
            if args.vote.metadata.is_discovery() {
                return args
                    .vote
                    .filtered_blocks()
                    .map(|hash| (*hash, Err(VoteError::Invalid)))
                    .collect();
            }
            // The governing close is an implicit, deterministic part of the
            // epoch context. Never retain or apply a vote until that certified
            // state is locally available.
            let election_epoch = match &args.vote.metadata.election_id {
                crate::consensus::election::RaiElectionId::Slot(slot) => slot.epoch,
                crate::consensus::election::RaiElectionId::CloseCut { epoch, .. }
                | crate::consensus::election::RaiElectionId::CloseRecord { epoch, .. } => *epoch,
            };
            if election_epoch != args.vote.metadata.epoch
                || self
                    .rai_epoch_manager
                    .governing_hash(election_epoch)
                    .is_none()
                || !self.rai_election_vote_enabled(&args.vote.metadata.election_id)
            {
                return args
                    .vote
                    .filtered_blocks()
                    .map(|hash| (*hash, Err(VoteError::Invalid)))
                    .collect();
            }
            for hash in args.vote.filtered_blocks() {
                if let crate::consensus::election::RaiElectionId::Slot(slot) =
                    &args.vote.metadata.election_id
                {
                    self.reference_candidate(slot.epoch, *hash);
                }
            }
            // Every signed vote is durable quorum material until its epoch's
            // close record is installed. In particular, a drain replica may
            // have missed a slot's First votes; regenerating only the current
            // Notar phase cannot repair the progression proof for those votes.
            let retained = self
                .rai_pending_votes
                .entry(args.vote.metadata.election_id.clone())
                .or_default();
            if !retained.iter().any(|existing| {
                existing.voter == args.vote.voter && existing.hash() == args.vote.vote.hash()
            }) {
                retained.push((*args.vote.vote).clone());
            }
            if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                eprintln!(
                    "RAI_MSG pr={pr} event=apply_vote id={:?} phase={:?} voter={} vote_hash={} hashes={:?}",
                    args.vote.metadata.election_id,
                    args.vote.metadata.phase,
                    args.vote.voter,
                    args.vote.vote.hash(),
                    args.vote.vote.hashes
                );
            }
        }
        let mut apply_helper = ApplyVoteHelper {
            args: &args,
            recently_confirmed: &mut self.recently_confirmed,
            vote_counter: &mut self.stats.vote_counter,
            observer: &self.observer,
            roots: &mut self.roots,
        };
        let result = apply_helper.apply_vote();
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
                    self.rai_terminal_slots.insert(
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
                                if let Ok(pr) = std::env::var("RSNANO_RAI_TRACE_PR") {
                                    eprintln!(
                                        "RAI_MSG pr={pr} event=cut_decided epoch={epoch:?} round={round} hash={hash} result={decided:?}"
                                    );
                                }
                            }
                            crate::consensus::election::RaiElectionKind::CloseRecord => {
                                if self
                                    .install_close_record_with_commit(
                                        epoch,
                                        round,
                                        hash,
                                        Some(args.rep_weights.clone()),
                                    )
                                    .is_ok()
                                {
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
                            crate::consensus::rai::RaiCloseKind::Record => self
                                .rai_epoch_manager
                                .advance_close_record_round(std::iter::empty()),
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
        self.rai_terminal_slots
            .retain(|slot, _| slot.epoch > closed_epoch);
        // Slot evidence is represented durably by the installed close state.
        // Close vote leaves, however, remain the only authenticated material
        // from which a lagging replica can derive the cut/record certificate;
        // retain them for process-lifetime archival repair.
        self.rai_pending_votes.retain(|id, _| match id {
            crate::consensus::election::RaiElectionId::Slot(slot) => slot.epoch > closed_epoch,
            crate::consensus::election::RaiElectionId::CloseCut { .. }
            | crate::consensus::election::RaiElectionId::CloseRecord { .. } => true,
        });
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
                vec![hash],
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
        assert!(
            container
                .rai_pending_votes
                .contains_key(&crate::consensus::election::RaiElectionId::Slot(slot))
        );
        let (metadata, final_vote) = container.rai_vote_context(&hash).unwrap();
        assert_eq!(metadata.phase, RaiVotePhase::Final);
        assert!(final_vote);
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

        container.rai_progress_close(Default::default(), &rsnano_ledger::Ledger::new_null(), now);

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
    fn pending_slot_cannot_be_erased_and_late_vote_terminates_it_in_place() {
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

        // Legacy lifecycle removal must not disconnect a pending RAI slot.
        // A solicited late vote is applied to the original election.
        assert!(!container.erase(&root));
        assert!(container.roots.election_for_rai_id(&id).is_some());
        assert!(!container.rai_terminal_slots.contains_key(&slot));
        assert!(!container.rai_pending_votes.contains_key(&id));
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                vec![hash],
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

        assert!(container.roots.election_for_rai_id(&id).is_none());
        assert!(container.rai_terminal_slots.contains_key(&slot));
        assert!(container.rai_pending_votes.contains_key(&id));
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
                .rai_pending_slot_requests(RaiEpoch::new(1))
                .is_empty()
        );

        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                vec![hash],
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
            .insert(successor_id.clone(), vec![(*vote.vote).clone()]);
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
                    vec![candidate],
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
                    vec![candidate],
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
        assert!(container.roots.election_for_rai_id(&round_zero).is_none());
        assert!(
            container
                .roots
                .election_for_rai_id(&crate::consensus::election::RaiElectionId::CloseCut {
                    epoch: 0.into(),
                    round: 1,
                })
                .is_some()
        );

        // A fast certificate for the retired source round remains decisive.
        // These two delayed First votes bring round zero from three to five
        // signers, which is the six-member fast threshold.
        for key in &keys[3..5] {
            let vote: FilteredVote = ReceivedVote::new(
                Vote::new_rai(
                    key,
                    UnixMillisTimestamp::new(2),
                    0,
                    vec![candidate],
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
                Err(VoteError::Indeterminate)
            );
        }
        container.rai_tick(
            now + Duration::from_millis(200),
            &keys[0],
            Duration::from_secs(30),
        );
        assert_eq!(
            container.rai_epoch_manager.decided_close_hash(0.into()),
            Some(candidate)
        );
        assert_eq!(
            container.rai_epoch_manager.closing_epoch().unwrap().phase,
            crate::consensus::rai::RaiClosingPhase::Draining
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
                .push(Vote::new_rai(
                    key,
                    UnixMillisTimestamp::new(1),
                    0,
                    vec![remote_hash],
                    RaiVoteMetadata {
                        election_id: crate::consensus::election::RaiElectionId::CloseCut {
                            epoch: 0.into(),
                            round: 0,
                        },
                        phase: RaiVotePhase::Final,
                        epoch: 0.into(),
                        scope: RaiCommitteeScope::All,
                    },
                ));
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
                vec![candidate],
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
                vec![remote_hash],
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
                vec![unknown],
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
                vec![hash],
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
                vec![hash],
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
