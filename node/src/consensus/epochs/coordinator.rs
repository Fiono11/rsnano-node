use super::VoteGate;
use crate::{
    NodeEvent,
    consensus::{AecInsertRequest, AecService},
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_messages::{ConfirmReq, EpochFinalization, EpochReportChunk, EpochStart, Message};
use rsnano_types::{Blake2Hash, Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, SlotRoot};
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
    reclassified_elections: usize,
    finalization_round: u32,
    target_non_cut_count: u64,
    finalization_reports: HashMap<PublicKey, EpochFinalization>,
    future_finalization_reports: HashMap<u32, HashMap<PublicKey, EpochFinalization>>,
    closing_epoch: u64,
    open_epoch: Option<u64>,
    next_start: Option<EpochStart>,
    next_drain_log_ms: u64,
    next_finalization_broadcast_ms: u64,
    next_closure_wait_log_ms: u64,
    next_final_vote_recovery_ms: u64,
    observer: Option<SyncSender<NodeEvent>>,
}

impl EpochCoordinator {
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
            reclassified_elections: 0,
            finalization_round: 0,
            target_non_cut_count: 0,
            finalization_reports: Default::default(),
            future_finalization_reports: Default::default(),
            closing_epoch: 0,
            open_epoch: None,
            next_start: None,
            next_drain_log_ms: 0,
            next_finalization_broadcast_ms: 0,
            next_closure_wait_log_ms: 0,
            next_final_vote_recovery_ms: 0,
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
        if report.round < self.finalization_round {
            return;
        }
        if report.round > self.finalization_round {
            self.future_finalization_reports
                .entry(report.round)
                .or_default()
                .entry(report.reporter)
                .or_insert(report);
            return;
        }
        let replace = self
            .finalization_reports
            .get(&report.reporter)
            .is_none_or(|old| {
                report.round > old.round
                    || (report.round == old.round
                        && (report.non_cut_count > old.non_cut_count
                            || (report.non_cut_count == old.non_cut_count
                                && report.finalized_hash != old.finalized_hash)))
            });
        if replace {
            self.finalization_reports.insert(report.reporter, report);
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
            self.aec.clear_finalized_for_epoch(open_epoch);
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
        let f = self.committee.len().saturating_sub(1) / 3;
        self.cut = support
            .into_iter()
            .filter_map(|(election, reporters)| (reporters.len() >= f + 1).then_some(election))
            .collect();
        // Reports are snapshots taken on different PRs. A slot can be reported for this epoch on
        // one PR just before its duplicate is removed there, while another PR has already assigned
        // it to an earlier finalized epoch. Earlier epoch agreement is authoritative and prevents
        // the later cut from waiting forever for an intentionally removed election.
        let finalized_earlier = self.aec.finalized_before_epoch(epoch);
        self.cut.retain(|slot| !finalized_earlier.contains(slot));
        self.gate
            .install_cut(epoch, self.open_epoch, self.cut.clone());
        self.reclassified_elections += self.aec.install_epoch_cut(epoch, self.cut.clone());
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

    fn complete_cut_dependency_closure(&self) -> Result<CompleteClosure, BlockHash> {
        let cut_values = self
            .aec
            .terminated_cut_values(self.closing_epoch, &self.cut)
            .expect("cut closure is only computed after every cut election terminates");
        let mut cut_winners: Vec<_> = cut_values.values().copied().collect();
        cut_winners.sort_unstable();
        let mut stack = cut_winners.clone();
        let mut visited = HashSet::new();
        // Slot elections may finalize locally in more than one epoch. Epoch-close membership is
        // instead derived only from the replicated cut and its dependency closure, then assigned
        // to the earliest close which contains each slot.
        let mut closure = HashMap::new();
        let any = self.ledger.any();
        while let Some(hash) = stack.pop() {
            if hash.is_zero() || !visited.insert(hash) {
                continue;
            }
            let block = any.get_block(&hash).ok_or(hash)?;
            closure.insert(block.qualified_root().slot(), hash);
            for dependency in any.block_dependencies(&block).iter() {
                if !dependency.is_zero() {
                    stack.push(*dependency);
                }
            }
        }
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
        Ok(CompleteClosure {
            finalized: closure,
            cut_winners,
            cut_winners_hash: cut_hash,
            closure_hash,
        })
    }

    fn send_local_finalization(&mut self) {
        // Cementation only emits newly confirmed blocks. Reconstruct the complete closure here
        // so an already-cemented dependency is attributed identically by every PR.
        let closure = match self.complete_cut_dependency_closure() {
            Ok(closure) => closure,
            Err(missing) => {
                let now = Self::now_ms();
                if now >= self.next_closure_wait_log_ms {
                    warn!(
                        epoch = self.closing_epoch,
                        %missing,
                        "RAI epoch closure waiting for a ledger dependency"
                    );
                    self.next_closure_wait_log_ms = now + 5_000;
                }
                return;
            }
        };
        let closure_count = closure.finalized.len();
        let Some(key) = self.committee_key() else {
            error!("Cannot report RAI finalization: no local committee key");
            return;
        };
        let (hash, non_cut_count, finalized_count) = self.local_finalization(&closure.finalized);
        if self.finalization_round > 0 && non_cut_count < self.target_non_cut_count {
            return;
        }
        let reporter = key.public_key();
        let existing = self
            .finalization_reports
            .get(&reporter)
            .filter(|report| report.round == self.finalization_round)
            .cloned();
        if let Some(report) = existing
            .as_ref()
            .filter(|report| report.non_cut_count == non_cut_count && report.finalized_hash == hash)
        {
            let now = Self::now_ms();
            if now >= self.next_finalization_broadcast_ms {
                self.flooder
                    .lock()
                    .unwrap()
                    .send_to_all_prs_once(&Message::EpochFinalization(report.clone()));
                self.next_finalization_broadcast_ms = now + 250;
            }
            return;
        }
        // Finalization can continue while reports converge. Replace this PR's same-round report
        // when its normalized epoch set grows, otherwise peers can wait forever on a stale hash.
        if existing
            .as_ref()
            .is_some_and(|report| non_cut_count < report.non_cut_count)
        {
            return;
        }
        let report = EpochFinalization::new(
            self.closing_epoch,
            self.finalization_round,
            &key,
            hash,
            non_cut_count,
        );
        self.finalization_reports.insert(reporter, report.clone());
        self.flooder
            .lock()
            .unwrap()
            .send_to_all_prs_once(&Message::EpochFinalization(report));
        self.next_finalization_broadcast_ms = Self::now_ms() + 250;
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

    fn evaluate_finalizations(&mut self) {
        if self.finalization_reports.len() != self.committee.len() {
            return;
        }
        let first = self.finalization_reports.values().next().unwrap();
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
            let closure = self
                .complete_cut_dependency_closure()
                .expect("an agreed local report has a complete ledger closure");
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
            if let Some(start) = self.next_start.take() {
                debug_assert_eq!(start.epoch, self.closing_epoch + 1);
                self.closing_epoch = start.epoch;
                self.open_epoch = None;
                self.reports.clear();
                self.local_snapshot.clear();
                self.cut.clear();
                self.reclassified_elections = 0;
                self.finalization_round = 0;
                self.target_non_cut_count = 0;
                self.finalization_reports.clear();
                self.future_finalization_reports.clear();
                self.next_finalization_broadcast_ms = 0;
                self.next_closure_wait_log_ms = 0;
                self.next_final_vote_recovery_ms = 0;
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
        self.target_non_cut_count = highest;
        self.finalization_round += 1;
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
        let now = Self::now_ms();
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
                if now >= self.next_final_vote_recovery_ms {
                    let targets = self
                        .aec
                        .final_vote_recovery_targets(self.closing_epoch, &self.cut);
                    for chunk in targets.chunks(ConfirmReq::HASHES_MAX) {
                        let request =
                            ConfirmReq::new(chunk.to_vec()).with_epoch(self.closing_epoch);
                        self.flooder
                            .lock()
                            .unwrap()
                            .send_to_all_prs_once(&Message::ConfirmReq(request));
                    }
                    if !targets.is_empty() {
                        info!(
                            epoch = self.closing_epoch,
                            elections = targets.len(),
                            "Requested final-vote recovery for RAI epoch cut"
                        );
                    }
                    self.next_final_vote_recovery_ms = now + 500;
                }
                let any = self.ledger.any();
                for slot in self.aec.missing_for_epoch(self.closing_epoch, &self.cut) {
                    let root = slot.with_epoch(self.closing_epoch);
                    let Some(hash) = any.block_successor_by_qualified_root(&root) else {
                        continue;
                    };
                    let Some(block) = any.get_block(&hash) else {
                        continue;
                    };
                    let priority = any.block_priority(&block);
                    let _ = self.aec.insert_now(AecInsertRequest::new_hinted_for_epoch(
                        block,
                        priority,
                        self.closing_epoch,
                    ));
                }
                self.gate.set_finalized(
                    self.closing_epoch,
                    self.aec.finalized_for_epoch(self.closing_epoch),
                );
                let terminated = self
                    .aec
                    .terminated_cut_values(self.closing_epoch, &self.cut)
                    .is_some();
                if terminated {
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
                        remaining = self.cut.len().saturating_sub(status.terminated),
                        missing = status.missing,
                        no_votes = status.no_votes,
                        awaiting_second_look = status.awaiting_second_look,
                        second_look = status.second_look,
                        quorum = status.quorum,
                        terminated = status.terminated,
                        "RAI epoch cut drain progress"
                    );
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
