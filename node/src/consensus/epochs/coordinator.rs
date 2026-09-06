use super::VoteGate;
use crate::{
    NodeEvent,
    consensus::{AecInsertRequest, AecService},
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};
use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_messages::{ConfirmReq, EpochFinalization, EpochReportChunk, EpochStart, Message};
use rsnano_types::{
    Blake2Hash, Blake2HashBuilder, Block, BlockHash, PrivateKey, PublicKey, SlotRoot,
};
use rsnano_utils::{CancellationToken, ticker::Tickable};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, mpsc::SyncSender},
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{error, info, warn};

enum Phase {
    Waiting,
    Scheduled(EpochStart),
    Open(EpochStart),
    Collecting,
    Cut,
    Converging,
    Complete,
}

#[derive(Default)]
struct PartialReport {
    chunk_count: u16,
    chunks: HashMap<u16, Vec<SlotRoot>>,
}

#[derive(Clone)]
struct CompleteClosure {
    finalized: HashMap<SlotRoot, BlockHash>,
    cut_winners: Vec<BlockHash>,
    cut_winners_hash: Blake2Hash,
    closure_hash: Blake2Hash,
}

pub struct EpochCoordinator {
    phase: Phase,
    aec: Arc<AecService>,
    ledger: Arc<Ledger>,
    gate: Arc<VoteGate>,
    flooder: Arc<Mutex<MessageFlooder>>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    committee: HashSet<PublicKey>,
    reports: HashMap<PublicKey, PartialReport>,
    local_snapshot: HashMap<SlotRoot, BlockHash>,
    cut: HashSet<SlotRoot>,
    recovery_cut: HashSet<SlotRoot>,
    reclassified_elections: usize,
    finalization_round: u32,
    target_non_cut_count: u64,
    finalization_reports: HashMap<PublicKey, EpochFinalization>,
    future_finalization_reports: HashMap<u32, HashMap<PublicKey, EpochFinalization>>,
    local_round_reports: HashMap<(u64, u32), EpochFinalization>,
    local_round_snapshot: Option<CompleteClosure>,
    local_round_cut_hashes: HashMap<(u64, u32), Blake2Hash>,
    live_epoch_hash: Option<Blake2Hash>,
    wait_for_round_peer: bool,
    closing_epoch: u64,
    open_epoch: Option<u64>,
    next_start: Option<EpochStart>,
    next_drain_log_ms: u64,
    next_finalization_broadcast_ms: u64,
    next_closure_wait_log_ms: u64,
    next_final_vote_recovery_ms: u64,
    recovery_request_cursor: usize,
    recovery_pass_complete: bool,
    closing_candidate_blocks: Vec<rsnano_types::Block>,
    recovery_blocks: HashMap<BlockHash, rsnano_types::Block>,
    missing_vote_blocks: HashSet<(u64, BlockHash)>,
    next_block_recovery_ms: u64,
    expanded_finalizations: HashSet<(u64, BlockHash)>,
    observer: Option<SyncSender<NodeEvent>>,
}

impl EpochCoordinator {
    pub fn observe_block_receipt(&self, hash: BlockHash) {
        self.aec.observe_block_receipt(hash);
    }

    pub fn observe_vote_blocks(&mut self, vote: &rsnano_types::Vote) {
        if !self.committee.contains(&vote.voter) || vote.validate().is_err() {
            return;
        }
        let any = self.ledger.any();
        for hash in &vote.hashes {
            let known = self.aec.candidate_block(vote.epoch(), hash)
                .map(Block::from).or_else(|| any.get_block(hash).map(Block::from));
            if let Some(block) = known {
                if !self.aec.is_active_hash_in_epoch(vote.epoch(), hash) {
                    // A retained block body is not necessarily attached to the active election.
                    // Attach it locally before routing the vote; no block download is needed.
                    self.aec.insert_vote_recovery(block, vote.epoch());
                }
            } else if !hash.is_zero() {
                self.missing_vote_blocks.insert((vote.epoch(), *hash));
            }
        }
    }
    pub fn receive_recovery_block(&mut self, block: Block) {
        // Receiving a body cannot amend the report-agreed cut or its voting policy.
        let epochs: Vec<_> = self.missing_vote_blocks.iter()
            .filter_map(|(epoch, hash)| (*hash == block.hash()).then_some(*epoch))
            .collect();
        if epochs.is_empty() {
            self.aec.insert_cut_recovery(block.clone());
        } else {
            for epoch in epochs {
                self.aec.insert_vote_recovery(block.clone(), epoch);
            }
        }
        self.recovery_blocks.entry(block.hash()).or_insert(block);
    }

    pub fn new(
        aec: Arc<AecService>,
        ledger: Arc<Ledger>,
        gate: Arc<VoteGate>,
        flooder: Arc<Mutex<MessageFlooder>>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        observer: Option<SyncSender<NodeEvent>>,
    ) -> Self {
        let committee = std::env::var("NANO_RAI_EPOCH_COMMITTEE")
            .or_else(|_| std::env::var("NANO_RAI_FIXED_COMMITTEE"))
            .unwrap_or_default()
            .split(',')
            .filter_map(|key| PublicKey::decode_hex(key.trim()))
            .collect();
        Self {
            phase: Phase::Waiting,
            aec,
            ledger,
            gate,
            flooder,
            wallet_reps,
            committee,
            reports: Default::default(),
            local_snapshot: Default::default(),
            cut: Default::default(),
            recovery_cut: Default::default(),
            reclassified_elections: 0,
            finalization_round: 0,
            target_non_cut_count: 0,
            finalization_reports: Default::default(),
            future_finalization_reports: Default::default(),
            local_round_reports: Default::default(),
            local_round_snapshot: None,
            local_round_cut_hashes: Default::default(),
            live_epoch_hash: None,
            wait_for_round_peer: false,
            closing_epoch: 0,
            open_epoch: None,
            next_start: None,
            next_drain_log_ms: 0,
            next_finalization_broadcast_ms: 0,
            next_closure_wait_log_ms: 0,
            next_final_vote_recovery_ms: 0,
            recovery_request_cursor: 0,
            recovery_pass_complete: false,
            closing_candidate_blocks: Default::default(),
            recovery_blocks: Default::default(),
            missing_vote_blocks: Default::default(),
            next_block_recovery_ms: 0,
            expanded_finalizations: Default::default(),
            observer,
        }
    }

    pub fn schedule(&mut self, start: EpochStart) {
        if let Phase::Scheduled(current) = &mut self.phase
            && start.epoch == current.epoch
        {
            current.closes_at_unix_ms = current.closes_at_unix_ms.min(start.closes_at_unix_ms);
            return;
        }
        if let Phase::Open(current) = &mut self.phase
            && start.epoch == current.epoch
        {
            current.closes_at_unix_ms = current.closes_at_unix_ms.min(start.closes_at_unix_ms);
            return;
        }
        if !matches!(self.phase, Phase::Waiting) {
            if start.epoch == self.closing_epoch + 1 {
                if let Some(next) = &mut self.next_start {
                    next.closes_at_unix_ms = next.closes_at_unix_ms.min(start.closes_at_unix_ms);
                } else {
                    info!(epoch = start.epoch, "RAI next epoch scheduled");
                    self.next_start = Some(start);
                }
            }
            return;
        }
        // Setup voting and equal stake distribution are complete before this message. Freeze
        // production until the common absolute start boundary reaches every PR.
        self.gate.pause();
        self.aec.clear_finalized_for_epoch(start.epoch);
        self.gate.clear_finalized(start.epoch);
        self.closing_epoch = start.epoch;
        info!(
            epoch = start.epoch,
            starts_at = start.starts_at_unix_ms,
            closes_at = start.closes_at_unix_ms,
            "RAI epoch scheduled"
        );
        self.phase = Phase::Scheduled(start);
    }

    pub fn receive_report(&mut self, chunk: EpochReportChunk) {
        if !matches!(self.phase, Phase::Open(_) | Phase::Collecting | Phase::Cut)
            || !self.committee.contains(&chunk.reporter)
            || !chunk.validate()
            || chunk.epoch != self.closing_epoch
        {
            return;
        }
        let report = self.reports.entry(chunk.reporter).or_default();
        if report.chunk_count == 0 {
            report.chunk_count = chunk.chunk_count;
        }
        if report.chunk_count != chunk.chunk_count {
            warn!(reporter = %chunk.reporter, "Conflicting epoch report chunk count");
            return;
        }
        report
            .chunks
            .entry(chunk.chunk_index)
            .or_insert(chunk.elections);
        let provisional_cut: HashSet<_> = self
            .reports
            .values()
            .flat_map(|report| report.chunks.values().flatten().copied())
            .collect();
        self.reclassified_elections += self
            .aec
            .install_epoch_cut(self.closing_epoch, provisional_cut);
    }

    pub fn receive_finalization(&mut self, report: EpochFinalization) {
        if !matches!(
            self.phase,
            Phase::Collecting | Phase::Cut | Phase::Converging
        ) || report.epoch != self.closing_epoch
            || !self.committee.contains(&report.reporter)
            || !report.validate()
        {
            return;
        }
        if report.round < self.finalization_round { return; }
        let round = report.round;
        let reporter = report.reporter;
        if round > self.finalization_round {
            self.future_finalization_reports.entry(round).or_default()
                .entry(reporter).or_insert(report);
        } else {
            self.finalization_reports.entry(reporter).or_insert(report);
        }
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    fn close(&mut self, epoch: u64) {
        self.gate.pause();
        self.closing_candidate_blocks = self.aec.candidate_blocks_for_epoch(epoch);
        let pending = self.aec.pending_for_epoch(epoch);
        self.local_snapshot = pending.iter().copied().collect();
        let elections: Vec<_> = pending.into_iter().map(|(slot, _)| slot).collect();
        self.open_epoch = self
            .next_start
            .as_ref()
            .filter(|start| start.epoch == epoch + 1)
            .map(|start| start.epoch);
        if let Some(open_epoch) = self.open_epoch {
            let advanced = self.aec.advance_epoch();
            debug_assert_eq!(advanced, open_epoch);
            // Votes may arrive before this PR opens the epoch. Preserve any
            // finalizations they already established.
            self.gate.clear_finalized(open_epoch);
            info!(
                closing_epoch = epoch,
                open_epoch, "RAI next epoch opened at closing boundary"
            );
        }
        self.gate.start_collecting(epoch, self.open_epoch);
        let keys = {
            let mut keys = Vec::new();
            self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
            keys
        };
        let Some(key) = keys
            .into_iter()
            .find(|key| self.committee.contains(&key.public_key()))
        else {
            error!("Cannot close RAI epoch: no local committee key");
            return;
        };
        let chunks: Vec<_> = if elections.is_empty() {
            vec![Vec::new()]
        } else {
            elections
                .chunks(EpochReportChunk::MAX_ELECTIONS)
                .map(<[_]>::to_vec)
                .collect()
        };
        let chunk_count = chunks.len() as u16;
        self.phase = Phase::Collecting;
        for (index, elections) in chunks.into_iter().enumerate() {
            let chunk = EpochReportChunk::new(epoch, &key, index as u16, chunk_count, elections);
            self.receive_report(chunk.clone());
            let sent = self
                .flooder
                .lock()
                .unwrap()
                .send_to_all_prs_once(&Message::EpochReportChunk(chunk));
            if sent.principal_reps + 1 < self.committee.len() {
                error!(
                    sent = sent.principal_reps,
                    expected = self.committee.len() - 1,
                    "Epoch report was not queued to every remote PR"
                );
            }
        }
        info!(
            epoch,
            elections = self.local_snapshot.len(),
            chunks = chunk_count,
            "RAI epoch report sent"
        );
    }

    fn all_reports_complete(&self) -> bool {
        self.reports.len() == self.committee.len()
            && self.reports.values().all(|report| {
                report.chunk_count > 0 && report.chunks.len() == report.chunk_count as usize
            })
    }

    fn install_cut(&mut self, epoch: u64) {
        let mut support: HashMap<SlotRoot, HashSet<PublicKey>> = HashMap::new();
        for (reporter, report) in &self.reports {
            let unique: HashSet<_> = report.chunks.values().flatten().copied().collect();
            for election in unique {
                support.entry(election).or_default().insert(*reporter);
            }
        }
        let reported_slots: HashSet<_> = support.keys().copied().collect();
        let f = self.committee.len().saturating_sub(1) / 3;
        self.cut = support
            .iter()
            .filter_map(|(election, reporters)| (reporters.len() >= f + 1).then_some(*election))
            .collect();
        // Reports are snapshots taken on different PRs. A slot can be reported for this epoch on
        // one PR just before its duplicate is removed there, while another PR has already assigned
        // it to an earlier finalized epoch. Earlier epoch agreement is authoritative and prevents
        // the later cut from waiting forever for an intentionally removed election.
        let finalized_earlier = self.aec.finalized_before_epoch(epoch);
        self.cut.retain(|slot| !finalized_earlier.contains(slot));
        // The agreed cut remains the report-derived set above. Locally, voting must also remain
        // open for its causal predecessors; otherwise a cut election whose low-support parent was
        // omitted by the f+1 report threshold can never satisfy dependency confirmation.
        // The cut is the f+1-supported subset. Every other reported (non-cut) election must
        // nevertheless run until it either finalizes or terminates by protocol timeout before
        // the unfinished remainder can be discarded.
        let mut drain_cut = reported_slots.clone();
        let mut blocks: HashMap<_, _> = self
            .closing_candidate_blocks
            .iter()
            .filter(|block| self.cut.contains(&block.qualified_root().slot()))
            .cloned()
            .map(|block| (block.hash(), block))
            .collect();
        blocks.extend(
            self.aec
                .cut_repair_blocks(epoch, &self.cut)
                .into_iter()
                .map(|block| (block.hash(), block)),
        );
        let any = self.ledger.any();
        let mut dependencies: Vec<_> = blocks
            .values()
            .flat_map(|block| [block.previous(), block.source_or_link()])
            .filter(|hash| !hash.is_zero())
            .collect();
        let mut seen = HashSet::new();
        while let Some(hash) = dependencies.pop() {
            if !seen.insert(hash) {
                continue;
            }
            let Some(block) = any.get_block(&hash) else {
                continue;
            };
            drain_cut.insert(block.qualified_root().slot());
            dependencies.extend(
                any.block_dependencies(&block)
                    .iter()
                    .filter(|hash| !hash.is_zero()),
            );
        }
        self.gate
            .install_cut(epoch, self.open_epoch, drain_cut.clone());
        self.reclassified_elections += self.aec.install_epoch_cut(epoch, drain_cut.clone());
        // Provisional reports also install cuts. Only the complete report set ends
        // admission based on a block's original receipt epoch.
        self.aec.mark_epoch_cut_decided(epoch);
        self.recovery_cut = self.cut.clone();
        // Alternate fork candidates are delivered as recovery publishes so the normal ledger
        // fork result cannot discard them before the cut exists. Now that the cut is installed,
        // attach the already-received candidates directly to their cut elections.
        for block in self.recovery_blocks.values().cloned() {
            if self.recovery_cut.contains(&block.qualified_root().slot()) {
                self.aec.insert_cut_recovery(block);
            }
        }
        self.gate
            .set_finalized(epoch, self.aec.finalized_for_epoch(epoch));
        let mut cut_hashes = Vec::new();
        let mut non_cut_hashes = Vec::new();
        for (slot, hash) in &self.local_snapshot {
            if self.cut.contains(slot) {
                cut_hashes.push(*hash);
            } else {
                non_cut_hashes.push(*hash);
            }
        }
        let mut cut_slots: Vec<_> = self.cut.iter().copied().collect();
        cut_slots.sort_unstable();
        let mut cut_slots_builder = Blake2HashBuilder::default().update(b"RAI/CUT_SLOTS/v1");
        for slot in cut_slots {
            cut_slots_builder = cut_slots_builder
                .update(slot.root.as_bytes())
                .update(slot.previous.as_bytes());
        }
        if let Some(observer) = &self.observer {
            let _ = observer.send(NodeEvent::EpochCut {
                epoch,
                cut_hash: cut_slots_builder.build(),
                reclassified_elections: self.reclassified_elections,
                cut: cut_hashes,
                non_cut: non_cut_hashes,
            });
        }
        info!(
            epoch,
            open_epoch = ?self.open_epoch,
            cut = self.cut.len(),
            non_cut = self.local_snapshot.len().saturating_sub(self.cut.len()),
            "RAI epoch cut installed"
        );
        self.phase = Phase::Cut;
        self.next_final_vote_recovery_ms = 0;
        self.recovery_request_cursor = 0;
        self.recovery_pass_complete = false;
    }

    fn request_election_votes(&self, slots: &[SlotRoot]) {
        let mut flooder = self.flooder.lock().unwrap();
        for voter in &self.committee {
            for chunk in slots.chunks(ConfirmReq::HASHES_MAX) {
                flooder.try_send_to_rep_once(voter, &Message::ConfirmReq(
                    ConfirmReq::new_elections(chunk.to_vec()),
                ));
            }
        }
    }

    fn vote_recovery_slots(&self) -> Vec<SlotRoot> {
        let mut slots = self.aec.unfinalized_election_slots();
        if matches!(self.phase, Phase::Cut | Phase::Converging) {
            slots.extend(self.aec.missing_for_epoch(self.closing_epoch, &self.cut));
        }
        slots.sort_unstable();
        slots.dedup();
        slots
    }

    fn committee_key(&self) -> Option<PrivateKey> {
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        keys.into_iter()
            .find(|key| self.committee.contains(&key.public_key()))
    }

    fn local_finalization(
        &self,
        finalized: &HashMap<SlotRoot, BlockHash>,
    ) -> (Blake2Hash, u64, usize) {
        let epoch = self.closing_epoch;
        let mut hashes = Vec::new();
        let mut non_cut_count = 0;
        for (slot, hash) in finalized {
            hashes.push(*hash);
            if !self.cut.contains(&slot) {
                non_cut_count += 1;
            }
        }
        hashes.sort_unstable();
        hashes.dedup();
        let mut builder = Blake2HashBuilder::default()
            .update(b"RAI/FINALIZED_BLOCKS/v1")
            .update(epoch.to_be_bytes())
            .update((hashes.len() as u64).to_be_bytes());
        for hash in &hashes {
            builder = builder.update(hash.as_bytes());
        }
        (builder.build(), non_cut_count, hashes.len())
    }

    fn dependency_closure(
        &mut self,
        epoch: u64,
        roots: HashMap<SlotRoot, BlockHash>,
    ) -> Option<HashMap<SlotRoot, BlockHash>> {
        let any = self.ledger.any();
        let mut pending: Vec<_> = roots.values().copied().collect();
        let mut seen = HashSet::new();
        let mut closure = HashMap::new();
        let mut complete = true;
        while let Some(hash) = pending.pop() {
            if hash.is_zero() || hash == self.ledger.constants.genesis_block.hash() || !seen.insert(hash) {
                continue;
            }
            let block = any.get_block(&hash).map(rsnano_types::MaybeSavedBlock::Saved)
                .or_else(|| self.aec.candidate_block(epoch, &hash));
            let Some(block) = block else {
                self.missing_vote_blocks.insert((epoch, hash));
                complete = false;
                continue;
            };
            let slot = block.qualified_root().slot();
            // An earlier finalized dependency keeps its epoch; a sealed assignment
            // cannot move even when an older certificate arrives later.
            if self.aec.finalization_epoch(slot, epoch) != epoch {
                continue;
            }
            if closure.insert(slot, hash).is_some_and(|other| other != hash) {
                return None;
            }
            let dependencies = match &block {
                rsnano_types::MaybeSavedBlock::Saved(block) => any.block_dependencies(block),
                rsnano_types::MaybeSavedBlock::Unsaved(block) => any.block_dependencies_for_unsaved(block),
            };
            pending.extend(dependencies.iter());
        }
        complete.then_some(closure)
    }

    fn complete_cut_dependency_closure(&mut self) -> Option<CompleteClosure> {
        self.epoch_dependency_closure(true)
    }

    fn epoch_dependency_closure(&mut self, require_complete: bool) -> Option<CompleteClosure> {
        let cut_values = if require_complete {
            self.aec.terminated_cut_values(self.closing_epoch, &self.cut)?
        } else {
            self.aec.observed_cut_values(self.closing_epoch, &self.cut)
        };
        let mut cut_winners: Vec<_> = cut_values.values().copied().collect();
        cut_winners.sort_unstable();
        // Finalized winners bring their dependencies into this epoch, including
        // dependencies already cemented before this winner's certificate arrived.
        let mut closure = self.aec.finalized_for_epoch(self.closing_epoch);
        closure = self.dependency_closure(self.closing_epoch, closure)?;
        self.aec.merge_finalized_for_epoch(self.closing_epoch, closure.clone());
        for (slot, hash) in cut_values {
            closure.insert(slot, hash);
        }
        // Agreement finalizes the selected notarized cut values as well, so their
        // dependencies must be committed in the same hash before the epoch is sealed.
        let closure = self.dependency_closure(self.closing_epoch, closure)?;
        let fingerprint = |domain: &[u8], hashes: &[BlockHash]| {
            let mut builder = Blake2HashBuilder::default().update(domain);
            for hash in hashes {
                builder = builder.update(hash.as_bytes());
            }
            builder.build()
        };
        let cut_hash = fingerprint(b"RAI/CUT_WINNERS/v1", &cut_winners);
        let mut closure_hashes: Vec<_> = closure.values().copied().collect();
        closure_hashes.sort_unstable();
        let closure_hash = fingerprint(b"RAI/CUT_CLOSURE/v1", &closure_hashes);
        Some(CompleteClosure {
            finalized: closure,
            cut_winners,
            cut_winners_hash: cut_hash,
            closure_hash,
        })
    }

    fn send_local_finalization(&mut self) {
        // The live set continues evolving after a round's signed snapshot is
        // committed. The reported maximum count never caps this set.
        let live_closure = self.epoch_dependency_closure(false);
        self.live_epoch_hash = live_closure.as_ref()
            .map(|closure| self.local_finalization(&closure.finalized).0);
        let Some(key) = self.committee_key() else {
            error!("Cannot report RAI finalization: no local committee key");
            return;
        };
        let reporter = key.public_key();
        if self.local_round_reports.contains_key(&(self.closing_epoch, self.finalization_round)) {
            return;
        }
        // Cementation only emits newly confirmed blocks. Reconstruct the complete closure here
        // so an already-cemented dependency is attributed identically by every PR.
        let closure = match live_closure.filter(|_| {
            self.aec.terminated_cut_values(self.closing_epoch, &self.cut).is_some()
        }) {
            Some(closure) => closure,
            None => {
                let now = Self::now_ms();
                self.phase = Phase::Cut;
                self.recovery_request_cursor = 0;
                self.recovery_pass_complete = false;
                self.next_final_vote_recovery_ms = 0;
                if now >= self.next_closure_wait_log_ms {
                    warn!(
                        epoch = self.closing_epoch,
                        "RAI epoch closure waiting for cut termination"
                    );
                    self.next_closure_wait_log_ms = now + 5_000;
                }
                return;
            }
        };
        let closure_count = closure.finalized.len();
        if std::env::var_os("NANO_RAI_RECOVERY_DIAGNOSTICS").is_some()
            && Self::now_ms() >= self.next_closure_wait_log_ms {
            for (slot, hash) in &closure.finalized {
                info!(root = %slot.root, %hash, cut = self.cut.contains(slot), "RAI diagnostic epoch member");
            }
            for detail in self.aec.epoch_recovery_diagnostics(self.closing_epoch) {
                info!(%detail, "RAI diagnostic election");
            }
            let targets = self.aec.missing_final_votes(self.closing_epoch, &self.committee);
            info!(?targets, "RAI diagnostic final recovery targets");
            self.next_closure_wait_log_ms = Self::now_ms() + 5_000;
        }
        let (hash, non_cut_count, finalized_count) = self.local_finalization(&closure.finalized);
        if self.finalization_round > 0 && non_cut_count < self.target_non_cut_count {
            return;
        }
        let cut_updated = self.finalization_round.checked_sub(1)
            .and_then(|round| self.local_round_cut_hashes.get(&(self.closing_epoch, round)))
            .is_some_and(|previous| *previous != closure.cut_winners_hash);
        // At the target count, new cut outcomes are sufficient progress to
        // initiate a round without waiting for another PR to broadcast first.
        if self.wait_for_round_peer && non_cut_count == self.target_non_cut_count && !cut_updated
            && !self.finalization_reports.keys().any(|peer| *peer != reporter)
        {
            return;
        }
        let report = EpochFinalization::new(
            self.closing_epoch, self.finalization_round, &key, hash, non_cut_count,
        );
        self.local_round_cut_hashes.insert((self.closing_epoch, self.finalization_round), closure.cut_winners_hash);
        self.local_round_snapshot = Some(closure.clone());
        self.local_round_reports.insert((self.closing_epoch, self.finalization_round), report.clone());
        self.finalization_reports.insert(reporter, report);
        self.next_finalization_broadcast_ms = 0;
        info!(
            round = self.finalization_round,
            epoch = self.closing_epoch,
            non_cut_count,
            finalized_count,
            finalized_hash = %hash,
            closure_count,
            cut_winners_hash = %closure.cut_winners_hash,
            closure_hash = %closure.closure_hash,
            "RAI epoch finalization broadcast"
        );
    }

    fn rebroadcast_finalizations(&mut self) {
        let now = Self::now_ms();
        if now < self.next_finalization_broadcast_ms { return; }
        let mut flooder = self.flooder.lock().unwrap();
        for report in self.local_round_reports.values() {
            flooder.send_to_all_prs_once(&Message::EpochFinalization(report.clone()));
        }
        self.next_finalization_broadcast_ms = now + 250;
    }

    fn evaluate_finalizations(&mut self) {
        if self.finalization_reports.len() != self.committee.len() {
            return;
        }
        let first = self.finalization_reports.values().next().unwrap().clone();
        if self.finalization_reports.values().all(|report| {
            report.finalized_hash == first.finalized_hash
                && report.non_cut_count == first.non_cut_count
        }) {
            info!(
                round = self.finalization_round,
                epoch = self.closing_epoch,
                non_cut_count = first.non_cut_count,
                finalized_hash = %first.finalized_hash,
                "All PRs ready to terminate RAI run"
            );
            let Some(closure) = self.local_round_snapshot.clone() else { return; };
            let (snapshot_hash, snapshot_non_cut, _) = self.local_finalization(&closure.finalized);
            if snapshot_hash != first.finalized_hash || snapshot_non_cut != first.non_cut_count { return; }
            if let Some(observer) = &self.observer {
                let _ = observer.send(NodeEvent::EpochComplete {
                    epoch: self.closing_epoch,
                    round: self.finalization_round,
                    non_cut_count: first.non_cut_count,
                    finalized_hash: first.finalized_hash,
                    included_cut: closure.cut_winners.clone(),
                });
            }
            // Epoch agreement turns included notarized values (and their dependency closure)
            // into finalizations. Timeout-only cut elections are discarded below.
            self.aec
                .replace_finalized_for_epoch(self.closing_epoch, closure.finalized);
            // Agreement fixes the closed epoch's complete finalized set. Elections still active
            // in this epoch are not part of that set and may only be discarded now.
            self.aec.seal_finalized_epoch(self.closing_epoch);
            self.aec.remove_epoch_elections(self.closing_epoch);
            self.aec.clear_epoch_cut(self.closing_epoch);
            self.local_round_reports.retain(|(epoch, round), _| {
                *epoch != self.closing_epoch || *round == self.finalization_round
            });
            if let Some(start) = self.next_start.take() {
                debug_assert_eq!(start.epoch, self.closing_epoch + 1);
                self.closing_epoch = start.epoch;
                self.open_epoch = None;
                self.reports.clear();
                self.local_snapshot.clear();
                self.cut.clear();
                self.recovery_cut.clear();
                self.recovery_blocks.clear();
                self.reclassified_elections = 0;
                self.finalization_round = 0;
                self.target_non_cut_count = 0;
                self.finalization_reports.clear();
                self.future_finalization_reports.clear();
                self.local_round_snapshot = None;
                self.live_epoch_hash = None;
                self.wait_for_round_peer = false;
                self.next_finalization_broadcast_ms = 0;
                self.next_closure_wait_log_ms = 0;
                self.next_final_vote_recovery_ms = 0;
                self.recovery_request_cursor = 0;
                self.recovery_pass_complete = false;
                self.gate.open();
                info!(
                    epoch = start.epoch,
                    "RAI next epoch remains active after prior close"
                );
                self.phase = Phase::Open(start);
            } else {
                self.phase = Phase::Complete;
            }
            return;
        }

        let highest = self
            .finalization_reports
            .values()
            .map(|report| report.non_cut_count)
            .max()
            .unwrap();
        let local_key = self.committee_key().map(|key| key.public_key());
        let all_tied = self.finalization_reports.values().all(|report| report.non_cut_count == highest);
        // A lagging PR starts the next round once caught up. Highest-count PRs
        // wait for it. If everyone ties, one deterministic PR breaks the wait.
        self.wait_for_round_peer = local_key.is_some_and(|key| {
            self.finalization_reports.get(&key).is_some_and(|report| report.non_cut_count == highest)
                && (!all_tied || self.committee.iter().min().copied() != Some(key))
        });
        self.local_round_snapshot = None;
        self.target_non_cut_count = highest;
        self.finalization_round += 1;
        // Every peer has reported in the completed round, so none can still
        // require our reports from an earlier round to advance to it.
        let oldest_needed = self.finalization_round.saturating_sub(1);
        self.local_round_reports.retain(|(epoch, round), _| {
            *epoch != self.closing_epoch || *round >= oldest_needed
        });
        self.local_round_cut_hashes.retain(|key, _| self.local_round_reports.contains_key(key));
        self.next_finalization_broadcast_ms = 0;
        self.finalization_reports = self
            .future_finalization_reports
            .remove(&self.finalization_round)
            .unwrap_or_default();
        warn!(
            round = self.finalization_round,
            target_non_cut_count = highest,
            "PR finalization reports differ; waiting for convergence"
        );
    }
}

impl Tickable for EpochCoordinator {
    fn tick(&mut self, _: &CancellationToken) {
        self.rebroadcast_finalizations();
        let now = Self::now_ms();
        if now >= self.next_final_vote_recovery_ms {
            let slots = self.vote_recovery_slots();
            self.request_election_votes(&slots);
            self.next_final_vote_recovery_ms = now + 1_000;
        }
        for epoch in [Some(self.closing_epoch), self.open_epoch].into_iter().flatten() {
            let roots: HashMap<_, _> = self.aec.finalized_for_epoch(epoch).into_iter()
                .filter(|(_, hash)| !self.expanded_finalizations.contains(&(epoch, *hash)))
                .collect();
            if !roots.is_empty()
                && let Some(closure) = self.dependency_closure(epoch, roots.clone())
            {
                self.aec.merge_finalized_for_epoch(epoch, closure);
                self.expanded_finalizations.extend(roots.values().map(|hash| (epoch, *hash)));
            }
        }
        if now >= self.next_block_recovery_ms && !self.missing_vote_blocks.is_empty() {
            let any = self.ledger.any();
            // A body may arrive through ordinary block processing rather than a
            // recovery publish. Attach it before removing its recovery request.
            for (epoch, hash) in &self.missing_vote_blocks {
                if !self.aec.is_active_hash_in_epoch(*epoch, hash)
                    && let Some(block) = any.get_block(hash)
                {
                    self.aec.insert_vote_recovery(block.into(), *epoch);
                }
            }
            self.missing_vote_blocks.retain(|(epoch, hash)| {
                any.get_block(hash).is_none() && self.aec.candidate_block(*epoch, hash).is_none()
            });
            let mut batches: HashMap<u64, Vec<_>> = HashMap::new();
            for (epoch, hash) in &self.missing_vote_blocks {
                batches.entry(*epoch).or_default().push((*hash, rsnano_types::Root::default()));
            }
            let mut flooder = self.flooder.lock().unwrap();
            for (epoch, hashes) in batches {
                for chunk in hashes.chunks(ConfirmReq::HASHES_MAX) {
                    flooder.try_send_to_random_pr_once(&Message::ConfirmReq(
                        ConfirmReq::new(chunk.to_vec()).with_epoch(epoch),
                    ));
                }
            }
            self.next_block_recovery_ms = now + 1_000;
        }
        match &self.phase {
            Phase::Scheduled(start) if now >= start.starts_at_unix_ms => {
                let start = start.clone();
                self.gate.open();
                info!(epoch = start.epoch, "RAI epoch started");
                self.phase = Phase::Open(start);
            }
            Phase::Open(start) if now >= start.closes_at_unix_ms => {
                let epoch = start.epoch;
                self.close(epoch);
            }
            Phase::Collecting if self.all_reports_complete() => {
                self.install_cut(self.closing_epoch)
            }
            Phase::Cut => {
                self.recovery_pass_complete = self.aec.cut_recovery_targets(
                    self.closing_epoch, &self.recovery_cut, &self.committee, false,
                ).is_empty();
                let live = self.epoch_dependency_closure(false);
                self.live_epoch_hash = live.as_ref().map(|closure| self.local_finalization(&closure.finalized).0);
                let any = self.ledger.any();
                let mut recovered_finalized = HashMap::new();
                for slot in self
                    .aec
                    .missing_for_epoch(self.closing_epoch, &self.cut)
                {
                    let root = slot.with_epoch(self.closing_epoch);
                    let Some(hash) = any.block_successor_by_qualified_root(&root) else {
                        continue;
                    };
                    let Some(block) = any.get_block(&hash) else {
                        continue;
                    };
                    if any.confirmed().block_exists(&hash) {
                        recovered_finalized.insert(slot, hash);
                        continue;
                    }
                    let priority = any.block_priority(&block);
                    let _ = self.aec.insert_now(AecInsertRequest::new_hinted_for_epoch(
                        block,
                        priority,
                        self.closing_epoch,
                    ));
                }
                if !recovered_finalized.is_empty() {
                    self.aec
                        .merge_finalized_for_epoch(self.closing_epoch, recovered_finalized);
                }
                self.gate.set_finalized(
                    self.closing_epoch,
                    self.aec.finalized_for_epoch(self.closing_epoch),
                );
                let terminated = self
                    .aec
                    .terminated_cut_values(self.closing_epoch, &self.cut)
                    .is_some();
                if terminated && self.recovery_pass_complete {
                    info!(cut = self.cut.len(), "RAI epoch cut terminated");
                    // Keep the draining policy installed during convergence. It blocks only the
                    // creation of new votes for non-cut epoch-e elections; votes created before
                    // the cut are still routed by their encoded epoch and may finalize them.
                    // Epoch e+1 vote generation remains open in parallel.
                    self.phase = Phase::Converging;
                    self.send_local_finalization();
                } else if now >= self.next_drain_log_ms {
                    let status = self.aec.epoch_drain_status(self.closing_epoch, &self.cut);
                    info!(
                        epoch = self.closing_epoch,
                        remaining = self
                            .cut
                            .len()
                            .saturating_sub(status.terminated + status.finalized),
                        finalized = status.finalized,
                        missing = status.missing,
                        no_votes = status.no_votes,
                        awaiting_second_look = status.awaiting_second_look,
                        second_look = status.second_look,
                        quorum = status.quorum,
                        terminated = status.terminated,
                        "RAI epoch cut drain progress"
                    );
                    for detail in self.aec.stalled_cut_details(self.closing_epoch, &self.cut) {
                        info!(epoch = self.closing_epoch, %detail, "RAI stalled cut election");
                    }
                    self.next_drain_log_ms = now + 5_000;
                }
            }
            Phase::Converging => {
                self.gate.set_finalized(
                    self.closing_epoch,
                    self.aec.finalized_for_epoch(self.closing_epoch),
                );
                self.send_local_finalization();
                self.evaluate_finalizations();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{TestBlockBuilder, Vote, VoteType};

    fn coordinator() -> EpochCoordinator {
        EpochCoordinator::new(
            Arc::new(AecService::new_null()), Arc::new(Ledger::new_null()),
            Arc::new(VoteGate::default()), Arc::new(Mutex::new(MessageFlooder::new_null())),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())), None,
        )
    }

    fn round_coordinator(key_id: u64) -> EpochCoordinator {
        use crate::representatives::RepresentativeTracker;
        use rsnano_types::{Amount, WalletId};
        let mut coordinator = coordinator();
        let wallets = Arc::new(rsnano_wallet::Wallets::new_null());
        let wallet = WalletId::from(1);
        wallets.create(wallet);
        let key = PrivateKey::from(key_id);
        wallets.insert_adhoc2(&wallet, &key.raw_key(), false).unwrap();
        let mut reps = WalletRepresentatives::new(true, Amount::ZERO,
            Arc::new(rsnano_ledger::RepWeightCache::default()), wallets,
            Arc::new(RepresentativeTracker::new_null()));
        reps.compute_reps();
        coordinator.wallet_reps = Arc::new(Mutex::new(reps));
        coordinator.committee = [PrivateKey::from(1).public_key(), PrivateKey::from(2).public_key()].into();
        coordinator.closing_epoch = 1;
        coordinator.phase = Phase::Converging;
        coordinator
    }

    fn deliver_round(source: &EpochCoordinator, target: &mut EpochCoordinator, round: u32) {
        target.receive_finalization(source.local_round_reports[&(1, round)].clone());
    }

    fn finalize_test_block(coordinator: &mut EpochCoordinator) -> Block {
        let block = TestBlockBuilder::legacy_change()
            .previous(coordinator.ledger.constants.genesis_block.hash()).build();
        coordinator.aec.insert_vote_recovery(block.clone(), 1);
        coordinator.aec.merge_finalized_for_epoch(1,
            [(block.qualified_root().slot(), block.hash())].into());
        block
    }

    #[test]
    fn local_round_commitment_is_immutable_and_unanimity_seals_its_snapshot() {
        let mut a = round_coordinator(1);
        let mut b = round_coordinator(2);
        a.send_local_finalization();
        b.send_local_finalization();
        let committed = a.local_round_reports[&(1, 0)].clone();
        finalize_test_block(&mut a);
        a.send_local_finalization();
        assert_eq!(a.local_round_reports[&(1, 0)], committed);
        assert_ne!(a.live_epoch_hash, Some(committed.finalized_hash));
        assert_eq!(a.aec.finalized_for_epoch(1).len(), 1);
        deliver_round(&a, &mut b, 0);
        deliver_round(&b, &mut a, 0);
        a.evaluate_finalizations();
        b.evaluate_finalizations();
        assert!(matches!(a.phase, Phase::Complete));
        assert!(matches!(b.phase, Phase::Complete));
        assert!(a.aec.finalized_for_epoch(1).is_empty());
        assert_eq!(a.aec.finalized_for_epoch(1), b.aec.finalized_for_epoch(1));
        assert_eq!(a.local_round_reports[&(1, 0)], committed);
    }

    #[test]
    fn lagging_pr_reaches_count_then_highest_pr_follows_its_next_round_report() {
        let mut a = round_coordinator(1);
        let mut b = round_coordinator(2);
        finalize_test_block(&mut a);
        a.send_local_finalization();
        b.send_local_finalization();
        deliver_round(&a, &mut b, 0);
        deliver_round(&b, &mut a, 0);
        a.evaluate_finalizations();
        b.evaluate_finalizations();
        assert_eq!(a.finalization_round, 1);
        assert_eq!(b.target_non_cut_count, 1);
        a.send_local_finalization();
        b.send_local_finalization();
        assert!(!a.local_round_reports.contains_key(&(1, 1)));
        assert!(!b.local_round_reports.contains_key(&(1, 1)));
        finalize_test_block(&mut b);
        b.send_local_finalization();
        assert!(b.local_round_reports.contains_key(&(1, 1)));
        a.send_local_finalization();
        assert!(!a.local_round_reports.contains_key(&(1, 1)));
        deliver_round(&b, &mut a, 1);
        a.send_local_finalization();
        deliver_round(&a, &mut b, 1);
        a.evaluate_finalizations();
        b.evaluate_finalizations();
        assert!(matches!(a.phase, Phase::Complete));
        assert!(matches!(b.phase, Phase::Complete));
        assert_eq!(a.aec.finalized_for_epoch(1), b.aec.finalized_for_epoch(1));
    }

    #[test]
    fn previous_highest_count_does_not_cap_next_round_report() {
        let mut coordinator = round_coordinator(1);
        coordinator.finalization_round = 1;
        coordinator.target_non_cut_count = 1;
        coordinator.wait_for_round_peer = true;
        let parent = finalize_test_block(&mut coordinator);
        let child = TestBlockBuilder::legacy_change().previous(parent.hash()).build();
        coordinator.aec.insert_vote_recovery(child.clone(), 1);
        coordinator.aec.merge_finalized_for_epoch(1, [(child.qualified_root().slot(), child.hash())].into());
        coordinator.send_local_finalization();
        assert_eq!(coordinator.local_round_reports[&(1, 1)].non_cut_count, 2);
        assert_eq!(coordinator.local_round_snapshot.as_ref().unwrap().finalized.len(), 2);
        assert_eq!(coordinator.live_epoch_hash, Some(coordinator.local_round_reports[&(1, 1)].finalized_hash));
    }

    #[test]
    fn recovery_keeps_cut_non_cut_and_next_epoch_elections_while_waiting_for_round() {
        use crate::consensus::{ApplyVoteArgs, FilteredVote, ReceivedVote};
        use crate::representatives::QuorumSnapshot;
        use rsnano_ledger::RepWeights;
        use rsnano_types::VoteDelivery;
        let mut coordinator = round_coordinator(1);
        let blocks: Vec<_> = (1..=3).map(|previous| {
            TestBlockBuilder::legacy_change().previous(previous.into()).build()
        }).collect();
        for (index, block) in blocks.iter().enumerate() {
            coordinator.aec.insert_vote_recovery(block.clone(), if index == 2 { 2 } else { 1 });
        }
        coordinator.cut.insert(blocks[0].qualified_root().slot());
        let quorum = QuorumSnapshot::new_test_instance();
        let key = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(key.public_key(), quorum.total_weight);
        let vote: FilteredVote = ReceivedVote::new(Arc::new(Vote::new_rai(&key, 1,
            VoteType::Timeout, vec![blocks[0].hash()])), VoteDelivery::Direct, None).into();
        coordinator.aec.apply_vote(ApplyVoteArgs {
            vote: &vote, rep_weights: &weights, quorum_snapshot: &quorum,
            now: rsnano_nullable_clock::Timestamp::new_test_instance(),
        });
        let election = coordinator.aec.election_for_root(&blocks[0].qualified_root().with_epoch(1)).unwrap();
        assert!(election.is_terminated());
        assert!(!election.is_confirmed());
        coordinator.wait_for_round_peer = true;
        coordinator.target_non_cut_count = 100;
        for phase in [Phase::Collecting, Phase::Cut, Phase::Converging] {
            coordinator.phase = phase;
            let slots = coordinator.vote_recovery_slots();
            assert_eq!(slots.len(), 3);
            for block in &blocks { assert!(slots.contains(&block.qualified_root().slot())); }
        }
        coordinator.aec.merge_finalized_for_epoch(1,
            [(blocks[1].qualified_root().slot(), blocks[1].hash())].into());
        assert_eq!(coordinator.vote_recovery_slots().len(), 2);
    }

    #[test]
    fn conflicting_current_and_future_round_reports_never_replace_first_commitment() {
        let mut coordinator = round_coordinator(1);
        let peer = PrivateKey::from(2);
        for round in [0, 2] {
            for hash in [7, 8] {
                coordinator.receive_finalization(EpochFinalization::new(1, round, &peer,
                    Blake2Hash::from(hash), hash));
            }
        }
        assert_eq!(coordinator.finalization_reports[&peer.public_key()].finalized_hash, Blake2Hash::from(7));
        assert_eq!(coordinator.future_finalization_reports[&2][&peer.public_key()].non_cut_count, 7);
    }

    #[test]
    fn tied_highest_counts_have_one_next_round_initiator() {
        let mut a = round_coordinator(1);
        let mut b = round_coordinator(2);
        for coordinator in [&mut a, &mut b] {
            for id in [1, 2] {
                coordinator.receive_finalization(EpochFinalization::new(1, 0,
                    &PrivateKey::from(id), Blake2Hash::from(id), 0));
            }
            coordinator.evaluate_finalizations();
        }
        assert_ne!(a.wait_for_round_peer, b.wait_for_round_peer);
        let initiator = a.committee.iter().min().copied().unwrap();
        assert_eq!(!a.wait_for_round_peer, a.committee_key().unwrap().public_key() == initiator);
    }

    #[test]
    fn sealed_pr_retransmits_commitment_until_delayed_peer_can_seal() {
        let mut a = round_coordinator(1);
        let mut b = round_coordinator(2);
        a.send_local_finalization();
        b.send_local_finalization();
        // A's first report is lost, but B's report reaches A.
        deliver_round(&b, &mut a, 0);
        a.evaluate_finalizations();
        assert!(matches!(a.phase, Phase::Complete));
        assert!(!matches!(b.phase, Phase::Complete));
        let output = a.flooder.lock().unwrap().track();
        a.next_finalization_broadcast_ms = 0;
        a.rebroadcast_finalizations();
        let retransmissions = output.output();
        assert!(!retransmissions.is_empty());
        for event in retransmissions {
            if let Message::EpochFinalization(report) = event.message {
                b.receive_finalization(report);
            }
        }
        b.evaluate_finalizations();
        assert!(matches!(b.phase, Phase::Complete));
    }

    #[test]
    fn updated_cut_at_highest_count_can_initiate_next_round_without_peer() {
        let mut coordinator = round_coordinator(1);
        let block = finalize_test_block(&mut coordinator);
        coordinator.aec.remove_epoch_elections(1);
        let slot = block.qualified_root().slot();
        coordinator.cut.insert(slot);
        // The previous round committed a timeout for this cut slot. A late
        // certificate now supplies its value without changing the non-cut count.
        let previous = EpochFinalization::new(1, 0, &PrivateKey::from(1), Blake2Hash::from(9), 0);
        coordinator.local_round_cut_hashes.insert((1, 0), Blake2Hash::from(99));
        coordinator.local_round_reports.insert((1, 0), previous.clone());
        coordinator.finalization_round = 1;
        coordinator.target_non_cut_count = 0;
        coordinator.wait_for_round_peer = true;
        coordinator.send_local_finalization();
        let report = &coordinator.local_round_reports[&(1, 1)];
        assert_eq!(report.non_cut_count, 0);
        assert_ne!(report.finalized_hash, previous.finalized_hash);
        assert_eq!(coordinator.local_round_reports[&(1, 0)], previous);
        assert_eq!(coordinator.finalization_reports.len(), 1);
    }

    #[test]
    fn vote_for_missing_body_starts_its_epoch_when_body_arrives() {
        let mut coordinator = coordinator();
        let key = PrivateKey::from(1);
        coordinator.committee.insert(key.public_key());
        let block = TestBlockBuilder::legacy_change().build();
        let hash = block.hash();
        coordinator.observe_vote_blocks(&Vote::new_rai(&key, 2, VoteType::First, vec![hash]));
        assert!(coordinator.missing_vote_blocks.contains(&(2, hash)));
        coordinator.receive_recovery_block(block);
        assert!(coordinator.aec.is_active_hash_in_epoch(2, &hash));
        assert!(!coordinator.aec.is_active_hash_in_epoch(1, &hash));
    }

    #[test]
    fn retained_recovery_body_still_starts_an_absent_election() {
        let mut coordinator = coordinator();
        let key = PrivateKey::from(1);
        coordinator.committee.insert(key.public_key());
        let block = TestBlockBuilder::legacy_change().build();
        let hash = block.hash();
        // No cut exists, so this stores the body without creating an election.
        assert!(!coordinator.aec.insert_cut_recovery(block));
        assert!(!coordinator.aec.is_active_hash_in_epoch(1, &hash));
        coordinator.observe_vote_blocks(&Vote::new_rai(&key, 1, VoteType::First, vec![hash]));
        assert!(coordinator.aec.is_active_hash_in_epoch(1, &hash));
    }

    #[test]
    fn finalized_dependency_closure_moves_unsealed_ancestors_to_earlier_epoch() {
        let mut coordinator = coordinator();
        coordinator.closing_epoch = 1;
        let parent = TestBlockBuilder::legacy_change()
            .previous(coordinator.ledger.constants.genesis_block.hash()).build();
        let child = TestBlockBuilder::legacy_change().previous(parent.hash()).build();
        let parent_slot = parent.qualified_root().slot();
        let child_slot = child.qualified_root().slot();
        coordinator.aec.insert_vote_recovery(parent.clone(), 1);
        coordinator.aec.insert_vote_recovery(child.clone(), 1);
        coordinator.aec.merge_finalized_for_epoch(2, [(parent_slot, parent.hash())].into());
        coordinator.aec.merge_finalized_for_epoch(1, [(child_slot, child.hash())].into());
        let closure = coordinator.complete_cut_dependency_closure().unwrap();
        assert_eq!(closure.finalized, [(parent_slot, parent.hash()), (child_slot, child.hash())].into());
        assert!(coordinator.aec.finalized_for_epoch(2).is_empty());
        assert_eq!(coordinator.aec.finalized_for_epoch(1), closure.finalized);
    }

    #[test]
    fn finalized_dependency_closure_preserves_sealed_ancestors() {
        let mut coordinator = coordinator();
        coordinator.closing_epoch = 2;
        let parent = TestBlockBuilder::legacy_change()
            .previous(coordinator.ledger.constants.genesis_block.hash()).build();
        let child = TestBlockBuilder::legacy_change().previous(parent.hash()).build();
        let parent_slot = parent.qualified_root().slot();
        let child_slot = child.qualified_root().slot();
        coordinator.aec.insert_vote_recovery(parent.clone(), 2);
        coordinator.aec.insert_vote_recovery(child.clone(), 2);
        coordinator.aec.merge_finalized_for_epoch(1, [(parent_slot, parent.hash())].into());
        coordinator.aec.seal_finalized_epoch(1);
        coordinator.aec.merge_finalized_for_epoch(2, [(child_slot, child.hash())].into());
        let closure = coordinator.complete_cut_dependency_closure().unwrap();
        assert_eq!(closure.finalized, [(child_slot, child.hash())].into());
        assert_eq!(coordinator.aec.finalized_for_epoch(1), [(parent_slot, parent.hash())].into());
    }

    #[test]
    fn already_cemented_send_and_its_parent_inherit_receivers_finalization_epoch() {
        use rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder;
        let mut coordinator = coordinator();
        coordinator.closing_epoch = 1;
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let receiver = PrivateKey::from(2);
        let parent = lattice.genesis().send(100, 1);
        let send = lattice.genesis().send(receiver.public_key(), 2);
        let receive = lattice.account(&receiver).receive(&send);
        for block in [&parent, &send, &receive] {
            coordinator.ledger.process_one(block).unwrap();
        }
        coordinator.ledger.confirm(send.hash());
        coordinator.aec.merge_finalized_for_epoch(2,
            [(parent.qualified_root().slot(), parent.hash()), (send.qualified_root().slot(), send.hash())].into());
        coordinator.aec.merge_finalized_for_epoch(1,
            [(receive.qualified_root().slot(), receive.hash())].into());
        let closure = coordinator.complete_cut_dependency_closure().unwrap();
        assert_eq!(closure.finalized.len(), 3);
        for block in [&parent, &send, &receive] {
            assert_eq!(closure.finalized.get(&block.qualified_root().slot()), Some(&block.hash()));
        }
        assert!(coordinator.aec.finalized_for_epoch(2).is_empty());
    }
}
