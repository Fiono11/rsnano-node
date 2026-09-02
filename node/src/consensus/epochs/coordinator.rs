use super::VoteGate;
use crate::{
    NodeEvent,
    cementation::{ConfirmingSet, EpochCementationTracker},
    consensus::AecService,
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_messages::{EpochFinalization, EpochReportChunk, EpochStart, Message};
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

pub struct EpochCoordinator {
    phase: Phase,
    aec: Arc<AecService>,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    epoch_cementation_tracker: Arc<EpochCementationTracker>,
    gate: Arc<VoteGate>,
    flooder: Arc<Mutex<MessageFlooder>>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    committee: HashSet<PublicKey>,
    reports: HashMap<PublicKey, PartialReport>,
    local_snapshot: HashMap<SlotRoot, BlockHash>,
    cut: HashSet<SlotRoot>,
    finalization_round: u32,
    target_non_cut_count: u64,
    round_delayed_reporter: Option<PublicKey>,
    finalization_reports: HashMap<PublicKey, EpochFinalization>,
    future_finalization_reports: HashMap<u32, HashMap<PublicKey, EpochFinalization>>,
    closing_epoch: u64,
    open_epoch: Option<u64>,
    next_start: Option<EpochStart>,
    next_drain_log_ms: u64,
    observer: Option<SyncSender<NodeEvent>>,
}

impl EpochCoordinator {
    pub fn new(
        aec: Arc<AecService>,
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        epoch_cementation_tracker: Arc<EpochCementationTracker>,
        gate: Arc<VoteGate>,
        flooder: Arc<Mutex<MessageFlooder>>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        observer: Option<SyncSender<NodeEvent>>,
    ) -> Self {
        let committee = std::env::var("NANO_RAI_FIXED_COMMITTEE")
            .unwrap_or_default()
            .split(',')
            .filter_map(|key| PublicKey::decode_hex(key.trim()))
            .collect();
        Self {
            phase: Phase::Waiting,
            aec,
            ledger,
            confirming_set,
            epoch_cementation_tracker,
            gate,
            flooder,
            wallet_reps,
            committee,
            reports: Default::default(),
            local_snapshot: Default::default(),
            cut: Default::default(),
            finalization_round: 0,
            target_non_cut_count: 0,
            round_delayed_reporter: None,
            finalization_reports: Default::default(),
            future_finalization_reports: Default::default(),
            closing_epoch: 0,
            open_epoch: None,
            next_start: None,
            next_drain_log_ms: 0,
            observer,
        }
    }

    pub fn schedule(&mut self, start: EpochStart) {
        if !matches!(self.phase, Phase::Waiting) {
            if start.epoch == self.closing_epoch + 1 && self.next_start.is_none() {
                info!(epoch = start.epoch, "RAI next epoch scheduled");
                self.next_start = Some(start);
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
    }

    pub fn receive_finalization(&mut self, report: EpochFinalization) {
        if !matches!(self.phase, Phase::Cut | Phase::Converging)
            || report.epoch != self.closing_epoch
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
            .is_none_or(|old| report.round > old.round);
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
        // Later epochs may reach consensus concurrently, but their ledger cementation must not
        // overtake the closing epoch and claim its dependency closure.
        self.confirming_set.set_max_consensus_epoch(epoch);
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
        self.gate
            .install_cut(epoch, self.open_epoch, self.cut.clone());
        self.aec.install_epoch_cut(epoch, self.cut.clone());
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
        if let Some(observer) = &self.observer {
            let _ = observer.send(NodeEvent::EpochCut {
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

    fn complete_cut_dependency_closure(
        &self,
    ) -> (HashMap<SlotRoot, BlockHash>, Blake2Hash, Blake2Hash) {
        let finalized = self.aec.finalized_for_epoch(self.closing_epoch);
        let mut cut_winners: Vec<_> = self
            .cut
            .iter()
            .filter_map(|slot| finalized.get(slot).copied())
            .collect();
        cut_winners.sort_unstable();
        let mut stack: Vec<_> = self
            .cut
            .iter()
            .filter_map(|slot| finalized.get(slot).copied())
            .collect();
        let mut visited = HashSet::new();
        let mut closure = HashMap::new();
        let any = self.ledger.any();
        while let Some(hash) = stack.pop() {
            if hash.is_zero() || !visited.insert(hash) {
                continue;
            }
            let Some(block) = any.get_block(&hash) else {
                continue;
            };
            closure.insert(block.qualified_root().slot(), hash);
            for dependency in any.block_dependencies(&block).iter() {
                if !dependency.is_zero() {
                    stack.push(*dependency);
                }
            }
        }
        let mut closure_hashes: Vec<_> = closure.values().copied().collect();
        closure_hashes.sort_unstable();
        let fingerprint = |domain: &[u8], hashes: &[BlockHash]| {
            let mut builder = Blake2HashBuilder::default().update(domain);
            for hash in hashes {
                builder = builder.update(hash.as_bytes());
            }
            builder.build()
        };
        let cut_hash = fingerprint(b"RAI/CUT_WINNERS/v1", &cut_winners);
        let closure_hash = fingerprint(b"RAI/CUT_CLOSURE/v1", &closure_hashes);
        // Cementation events are local observations and cannot define a replicated epoch set.
        // The agreed cut winners and their dependency closure are the deterministic finalized
        // set every PR can derive from the protocol transcript.
        self.aec
            .replace_finalized_for_epoch(self.closing_epoch, closure.clone());
        (closure, cut_hash, closure_hash)
    }

    fn send_local_finalization(&mut self) {
        // Cementation only emits newly confirmed blocks. Reconstruct the complete closure here
        // so an already-cemented dependency is attributed identically by every PR.
        let (closure, cut_winners_hash, closure_hash) = self.complete_cut_dependency_closure();
        let closure_count = closure.len();
        let Some(key) = self.committee_key() else {
            error!("Cannot report RAI finalization: no local committee key");
            return;
        };
        let (hash, non_cut_count, finalized_count) = self.local_finalization(&closure);
        if self.finalization_round > 0 && non_cut_count < self.target_non_cut_count {
            return;
        }
        let reporter = key.public_key();
        if self.finalization_round > 0
            && self.round_delayed_reporter == Some(reporter)
            && self.finalization_reports.is_empty()
        {
            return;
        }
        if self
            .finalization_reports
            .get(&reporter)
            .is_some_and(|report| report.round == self.finalization_round)
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
        info!(
            round = self.finalization_round,
            epoch = self.closing_epoch,
            non_cut_count,
            finalized_count,
            finalized_hash = %hash,
            closure_count,
            cut_winners_hash = %cut_winners_hash,
            closure_hash = %closure_hash,
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
            if let Some(observer) = &self.observer {
                let _ = observer.send(NodeEvent::EpochComplete {
                    epoch: self.closing_epoch,
                    non_cut_count: first.non_cut_count,
                    finalized_hash: first.finalized_hash,
                });
            }
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
                self.finalization_round = 0;
                self.target_non_cut_count = 0;
                self.round_delayed_reporter = None;
                self.finalization_reports.clear();
                self.future_finalization_reports.clear();
                self.gate.open();
                self.confirming_set.set_max_consensus_epoch(start.epoch);
                info!(
                    epoch = start.epoch,
                    "RAI next epoch remains active after prior close"
                );
                self.phase = Phase::Open(start);
            } else {
                self.confirming_set.set_max_consensus_epoch(u64::MAX);
                self.phase = Phase::Complete;
            }
            return;
        }

        let highest_report = self
            .finalization_reports
            .values()
            .max_by_key(|report| (report.non_cut_count, report.reporter))
            .unwrap();
        let highest = highest_report.non_cut_count;
        self.round_delayed_reporter = Some(highest_report.reporter);
        self.target_non_cut_count = highest;
        self.finalization_round += 1;
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
                self.gate.set_finalized(
                    self.closing_epoch,
                    self.aec.finalized_for_epoch(self.closing_epoch),
                );
                let finalized = self.aec.finalized_for_epoch(self.closing_epoch);
                let remaining = self
                    .cut
                    .iter()
                    .filter(|slot| !finalized.contains_key(slot))
                    .count();
                if remaining == 0 {
                    info!(cut = self.cut.len(), "RAI epoch cut finalized");
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
                        remaining,
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
