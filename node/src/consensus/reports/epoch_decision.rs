use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rsnano_messages::{EpochProp, Message, ReportSelection};
use rsnano_network::{Channel, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{Amount, BlockHash, ConsensusEpoch, PublicKey};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use super::ReportExchange;
use super::checkpoint::{CheckpointDifference, CheckpointTransfer};
use super::close_proof::{VerifiedCheckpoint, verify_close_proof};
use crate::{
    consensus::{
        AecService,
        active_elections::EpochProposalContext,
        election::{
            Committee, EpochLedger, EpochValue, ReportIndex, ReportRef, ReportSource,
            SelectedReport,
        },
    },
    transport::MessageFlooder,
    utils::diagnostic,
    wallets::WalletRepresentatives,
};
use rsnano_messages::{CheckpointReply, CheckpointReq};
use rsnano_messages::{CloseProofReply, CloseProofReq};

/// RAI, "The joint epoch election": the part of the close that reads and
/// writes. The election itself is in the active elections - it is Kudzu over
/// election slots, with the same vote pools and thresholds as an account
/// domain - and what happens here is the value: selecting `N - f` usable
/// reports, deriving `BuildState(S_{e-1}, Q_e)` from them, proposing
/// `X = (h_p, Q_e, d_e)`, and re-deriving that state to check a proposal
/// another leader made.
///
/// A validator never proposes a state of its own and never compares a
/// proposal with one. It checks that the reports named determine the state
/// named, which is a public function of evidence every replica can obtain.
pub struct EpochDecisionService {
    exchange: Arc<Mutex<ReportExchange>>,
    active_elections: Arc<AecService>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    flooder: Mutex<MessageFlooder>,
    clock: Arc<SteadyClock>,
    stats: Arc<Stats>,
    /// The proposals this node made, re-broadcast while their epoch is
    /// still closing: a replica that missed the message holds no value to
    /// vote for, and the round would time out for want of a message rather
    /// than of agreement.
    proposals: Mutex<HashMap<(ConsensusEpoch, u32), EpochProp>>,
    repeated: Mutex<Option<Timestamp>>,
    /// When this node last said why it could not derive a value
    unready_logged: Mutex<HashMap<ConsensusEpoch, Timestamp>>,
    verified_checkpoint: Mutex<Option<VerifiedCheckpoint>>,
    transfer: Mutex<Option<CheckpointTransfer>>,
    page_requested: Mutex<Option<(ConsensusEpoch, u32, Timestamp)>>,
    transferred: Mutex<Option<(ConsensusEpoch, Arc<EpochLedger>)>>,
    differences: Mutex<std::collections::BTreeMap<ConsensusEpoch, CheckpointDifference>>,
}

impl EpochDecisionService {
    /// How often this node repeats the proposals of its current rounds
    const REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
    /// How often a close that can not start says what it is short of
    const UNREADY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
    /// Epochs whose proposals this node keeps repeating: the recent ones, so
    /// that a straggler can still obtain a value while an old epoch's
    /// proposals are not repeated for the rest of the run
    const REPEATED_EPOCHS: usize = 4;

    pub fn new(
        exchange: Arc<Mutex<ReportExchange>>,
        active_elections: Arc<AecService>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            exchange,
            active_elections,
            wallet_reps,
            flooder: Mutex::new(flooder),
            clock,
            stats,
            proposals: Mutex::new(HashMap::new()),
            repeated: Mutex::new(None),
            unready_logged: Mutex::new(HashMap::new()),
            verified_checkpoint: Mutex::new(None),
            transfer: Mutex::new(None),
            page_requested: Mutex::new(None),
            transferred: Mutex::new(None),
            differences: Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    /// RAI: drive the joint election of every epoch still closing. A replica
    /// takes part once it holds the decided predecessor state and enough
    /// usable reports to derive a value; the leader of the round it is in
    /// proposes one.
    pub fn tick(&self) {
        let closes = self.active_elections.epoch_closes();
        for close in &closes {
            // Not "until the epoch closes" but until it is decided here: a
            // replica that learned the certificate before it could derive
            // the value holds no state for the epoch, and it is the
            // derivation, not the certificate, that decides one. It goes on
            // collecting reports until it can.
            if close.value.is_some() {
                continue;
            }
            let ready = self.can_derive(close.epoch);
            if ready != close.ready {
                self.active_elections.set_close_ready(close.epoch, ready);
            }
        }
        for context in self.active_elections.epoch_proposals_due() {
            self.propose(context);
        }
        self.repeat_proposals();
        self.request_checkpoint_page();
    }

    /// RAI: a leader's proposal. It is validated like any other: this node
    /// derives the state the named reports determine and requires its hash
    /// to be the one proposed. A value it can not check is one it does not
    /// vote for, and its round times out.
    pub fn handle_proposal(&self, prop: EpochProp, _channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::EpochProp, Direction::In);
        if prop.reports.is_empty() {
            return;
        }
        let value = EpochValue::from_parts(
            prop.epoch,
            prop.slot,
            prop.parent,
            prop.reports.iter().map(report_ref).collect(),
            prop.state,
        );
        let hash = value.hash();
        if !prop.verify(hash) {
            return;
        }
        // A leader repeats its proposal while its round stands, and
        // deriving the state walks the whole predecessor: check once
        if self.active_elections.holds_epoch_value(prop.epoch, &hash) {
            self.active_elections.retain_close_proposal(prop, hash);
            return;
        }
        let Some((_, state)) = self.validate(&value) else {
            return;
        };
        self.active_elections.accept_epoch_value(value, state);
        self.active_elections
            .retain_close_proposal(prop.clone(), hash);
    }

    pub fn handle_close_proof_request(&self, request: CloseProofReq, channel: &Arc<Channel>) {
        let Some(proof) = self.active_elections.close_proof(request.epoch) else {
            return;
        };
        self.flooder.lock().unwrap().try_send(
            channel,
            &Message::CloseProofReply(proof),
            TrafficType::Generic,
        );
    }

    pub fn handle_close_proof(&self, proof: CloseProofReply) {
        let next = self.active_elections.next_checkpoint();
        let Some(committees) = self.active_elections.close_committees(next) else {
            return;
        };
        let Some(verified) = verify_close_proof(&proof, next, &committees) else {
            return;
        };
        let mut held = self.verified_checkpoint.lock().unwrap();
        if held.as_ref() == Some(&verified) {
            return;
        }
        let Some(previous) = self.active_elections.epoch_previous_state(next) else {
            return;
        };
        *self.transfer.lock().unwrap() = Some(CheckpointTransfer::new(verified.clone(), previous));
        *held = Some(verified);
    }

    fn request_checkpoint_page(&self) {
        let request = self.transfer.lock().unwrap().as_ref().map(|t| t.request());
        if let Some(request) = request {
            let now = self.clock.now();
            let mut last = self.page_requested.lock().unwrap();
            if last.is_some_and(|(epoch, offset, at)| {
                epoch == request.epoch
                    && offset == request.offset
                    && at.elapsed(now) < Self::REPEAT_INTERVAL
            }) {
                return;
            }
            *last = Some((request.epoch, request.offset, now));
            drop(last);
            self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                &Message::CheckpointReq(request),
                TrafficType::Generic,
                1.0,
            );
        }
    }

    pub fn handle_checkpoint_request(&self, request: CheckpointReq, channel: &Arc<Channel>) {
        let reply = {
            let mut differences = self.differences.lock().unwrap();
            if !differences.contains_key(&request.epoch) {
                let Some(source) = self.active_elections.epoch_previous_state(request.epoch) else {
                    return;
                };
                let Some(target) = self.active_elections.epoch_decided_state(request.epoch) else {
                    return;
                };
                if source.state_hash() != request.source || target.state_hash() != request.target {
                    return;
                }
                differences.insert(
                    request.epoch,
                    CheckpointDifference::new(request.epoch, &source, &target),
                );
                // Pages are reproducible from decided states; eight cached deltas
                // bound this acceleration cache independently of archive history.
                while differences.len() > 8 {
                    differences.pop_first();
                }
            }
            differences
                .get(&request.epoch)
                .and_then(|d| d.page(&request))
        };
        if let Some(reply) = reply {
            self.flooder.lock().unwrap().try_send(
                channel,
                &Message::CheckpointReply(reply),
                TrafficType::Generic,
            );
        }
    }

    pub fn handle_checkpoint_reply(&self, reply: CheckpointReply) {
        let mut transfer = self.transfer.lock().unwrap();
        let Some(pending) = transfer.as_mut() else {
            return;
        };
        // Other responders may send the preceding page; ignore it.
        if reply.offset != pending.request().offset {
            return;
        }
        match pending.accept(&reply) {
            Ok(Some(state)) => {
                *self.transferred.lock().unwrap() = Some((reply.difference.epoch, state));
                *transfer = None;
            }
            Ok(None) => {}
            Err(()) => {
                // Discard the partial state on a corrupt page, then retry from
                // the same installed predecessor and verified commitment.
                let request = pending.request();
                if let Some(previous) = self.active_elections.epoch_previous_state(request.epoch) {
                    *transfer = Some(CheckpointTransfer::new(
                        VerifiedCheckpoint {
                            epoch: request.epoch,
                            state: request.target,
                        },
                        previous,
                    ));
                }
            }
        }
    }

    /// Whether this node can derive a value for an epoch's close: it holds
    /// the decided predecessor state and reports carrying `N - f` of the old
    /// committee's weight
    fn can_derive(&self, epoch: ConsensusEpoch) -> bool {
        let Some(committee) = self.active_elections.epoch_committee(epoch) else {
            self.report_unready(epoch, "no committee", 0, Amount::ZERO, Amount::ZERO);
            return false;
        };
        let Some(previous) = self.active_elections.epoch_previous_state(epoch) else {
            self.report_unready(
                epoch,
                "no predecessor state",
                0,
                Amount::ZERO,
                committee.thresholds().report,
            );
            return false;
        };
        let predecessor = previous.state_hash();
        let (usable, weight) = {
            let exchange = self.exchange.lock().unwrap();
            (
                exchange.usable(epoch).len(),
                selected_weight(&exchange, epoch, &committee, predecessor),
            )
        };
        if weight >= committee.thresholds().report {
            return true;
        }
        self.report_unready(
            epoch,
            "reports",
            usable,
            weight,
            committee.thresholds().report,
        );
        false
    }

    /// Why this node can not derive a value yet, at most once a second per
    /// epoch. A close that never starts is the one thing that stops an
    /// epoch for good, so it says what it is short of.
    fn report_unready(
        &self,
        epoch: ConsensusEpoch,
        reason: &str,
        usable: usize,
        weight: Amount,
        required: Amount,
    ) {
        let now = self.clock.now();
        {
            let mut last = self.unready_logged.lock().unwrap();
            if last
                .get(&epoch)
                .is_some_and(|last| last.elapsed(now) < Self::UNREADY_INTERVAL)
            {
                return;
            }
            last.insert(epoch, now);
        }
        let (held, roots) = {
            let exchange = self.exchange.lock().unwrap();
            let reports = exchange.reports(epoch);
            (
                reports.len(),
                reports
                    .iter()
                    .map(|report| {
                        format!(
                            "{}:{}:{}",
                            &report.reporter.to_string()[..8],
                            &report.certified.to_string()[..8],
                            &report.residual.to_string()[..8]
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };
        diagnostic!(
            "EPOCH_CLOSE_UNREADY epoch={} short_of={} usable={} weight={} required={} held={} theirs={:?}",
            epoch,
            reason,
            usable,
            weight.number(),
            required.number(),
            held,
            roots
        );
    }

    /// RAI, "Leader behavior": propose into the round this node leads. With
    /// a joint-complete parent it copies that parent's `(Q_e, d_e)`; with
    /// none it extends election genesis and selects `N - f` usable reports
    /// of its own.
    fn propose(&self, context: EpochProposalContext) {
        let epoch = context.epoch;
        let round = context.round;
        let built = match (context.parent.clone(), context.parent_state.clone()) {
            (Some(parent), Some(state)) => Some((parent.extend(round), state)),
            _ => self.derive_fresh(epoch, round),
        };
        let Some((value, state)) = built else {
            return;
        };
        let hash = value.hash();
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        let Some(key) = keys
            .iter()
            .find(|key| key.public_key() == context.leader)
            .cloned()
        else {
            return;
        };
        diagnostic!(
            "EPOCH_PROPOSED epoch={} round={} value={} state={} reports={}",
            epoch,
            round,
            hash,
            value.state,
            value.reports().len()
        );
        let prop = EpochProp::new(
            &key,
            epoch,
            round,
            value.parent,
            value.state,
            value.reports().iter().map(selection_of).collect(),
            hash,
        );
        self.active_elections.accept_epoch_value(value, state);
        self.active_elections
            .retain_close_proposal(prop.clone(), hash);
        self.active_elections
            .record_epoch_proposal(epoch, round, hash);
        self.proposals
            .lock()
            .unwrap()
            .insert((epoch, round), prop.clone());
        self.broadcast(prop);
    }

    /// RAI: select the usable reports and derive the state they determine.
    /// The selection is the shortest prefix in canonical reporter order that
    /// carries `N - f` of the old committee's weight, so two correct leaders
    /// holding the same reports propose the same value and the first round
    /// decides.
    fn derive_fresh(
        &self,
        epoch: ConsensusEpoch,
        round: u32,
    ) -> Option<(EpochValue, Arc<EpochLedger>)> {
        let previous = self.active_elections.epoch_previous_state(epoch)?;
        let committee = self.active_elections.epoch_committee(epoch)?;
        let predecessor = previous.state_hash();
        let exchange = self.exchange.lock().unwrap();
        // Only reports signed against the same predecessor checkpoint
        let mut usable: Vec<PublicKey> = exchange
            .usable(epoch)
            .iter()
            .filter(|(report, _, _)| report.predecessor == predecessor)
            .map(|(report, _, _)| report.reporter)
            .collect();
        usable.sort();
        let mut selected: Vec<ReportRef> = Vec::new();
        let mut weight = Amount::ZERO;
        for reporter in usable {
            let Some((report, _, _)) = exchange
                .usable(epoch)
                .into_iter()
                .find(|(report, _, _)| report.reporter == reporter)
            else {
                continue;
            };
            selected.push(ReportRef {
                reporter,
                certified: report.certified,
                residual: report.residual,
            });
            weight = weight
                .number()
                .checked_add(committee.weight(&reporter).number())
                .map(Amount::raw)
                .unwrap_or(Amount::MAX);
            if weight >= committee.thresholds().report {
                break;
            }
        }
        if weight < committee.thresholds().report {
            return None;
        }
        let source = UsableReports {
            exchange: &exchange,
            epoch,
            committee: &committee,
            predecessor,
        };
        let resolved: Vec<(ReportRef, SelectedReport)> = selected
            .iter()
            .filter_map(|report| Some((*report, source.report(report)?)))
            .collect();
        if resolved.len() != selected.len() {
            return None;
        }
        let states: Vec<SelectedReport> = resolved.iter().map(|(_, state)| *state).collect();
        let index = ReportIndex::new(&previous, &states);
        let (value, ledger) = EpochValue::propose(
            epoch,
            round,
            BlockHash::ZERO,
            &previous,
            &resolved,
            &index,
            committee.thresholds().many,
        )
        .ok()?;
        Some((value, Arc::new(ledger)))
    }

    /// RAI: derive the state a value's reports determine and check that it
    /// hashes to the `d_e` the value carries
    fn validate(&self, value: &EpochValue) -> Option<(BlockHash, Arc<EpochLedger>)> {
        let previous = self.active_elections.epoch_previous_state(value.epoch)?;
        let committee = self.active_elections.epoch_committee(value.epoch)?;
        let exchange = self.exchange.lock().unwrap();
        let source = UsableReports {
            exchange: &exchange,
            epoch: value.epoch,
            committee: &committee,
            predecessor: previous.state_hash(),
        };
        let states: Vec<SelectedReport> = value
            .reports()
            .iter()
            .filter_map(|report| source.report(report))
            .collect();
        if states.len() != value.reports().len() {
            return None;
        }
        let index = ReportIndex::new(&previous, &states);
        match value.validate(
            &previous,
            &source,
            &index,
            committee.thresholds().report,
            committee.thresholds().many,
        ) {
            Ok(ledger) => Some((value.hash(), Arc::new(ledger))),
            Err(error) => {
                diagnostic!(
                    "EPOCH_VALUE_REFUSED epoch={} slot={} value={} reason={:?}",
                    value.epoch,
                    value.slot,
                    value.hash(),
                    error
                );
                None
            }
        }
    }

    /// Repeats the proposals of the rounds this node leads: a replica that
    /// missed the message holds no value to vote for, and the round would
    /// time out for want of a message rather than of agreement.
    fn repeat_proposals(&self) {
        let now = self.clock.now();
        {
            let mut repeated = self.repeated.lock().unwrap();
            if repeated.is_some_and(|last| last.elapsed(now) < Self::REPEAT_INTERVAL) {
                return;
            }
            *repeated = Some(now);
        }
        // A proposal is repeated while its epoch is still tracked, not only
        // while the election is open: a replica that missed it and learned
        // the certificate instead has no other way to obtain the value the
        // certificate decided, and without it that epoch has no state there.
        let mut tracked: Vec<ConsensusEpoch> = self
            .active_elections
            .epoch_closes()
            .into_iter()
            .map(|close| close.epoch)
            .collect();
        tracked.sort();
        if tracked.len() > Self::REPEATED_EPOCHS {
            tracked.drain(..tracked.len() - Self::REPEATED_EPOCHS);
        }
        let repeat: Vec<EpochProp> = {
            let mut proposals = self.proposals.lock().unwrap();
            proposals.retain(|(epoch, _), _| tracked.contains(epoch));
            proposals.values().cloned().collect()
        };
        for prop in repeat {
            self.broadcast(prop);
        }
    }

    fn broadcast(&self, prop: EpochProp) {
        self.stats
            .inc_dir(StatType::Message, DetailType::EpochProp, Direction::Out);
        self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
            &Message::EpochProp(prop),
            TrafficType::Generic,
            1.0,
        );
    }
}

/// The reports this node has reconstructed, as an epoch derivation sees
/// them: those signed against the predecessor checkpoint the derivation
/// starts from
struct UsableReports<'a> {
    exchange: &'a ReportExchange,
    epoch: ConsensusEpoch,
    committee: &'a Committee,
    predecessor: BlockHash,
}

impl ReportSource for UsableReports<'_> {
    fn report(&self, report: &ReportRef) -> Option<SelectedReport<'_>> {
        let (certified, residual) = self.exchange.usable_report(
            self.epoch,
            &report.reporter,
            report.certified,
            report.residual,
            self.predecessor,
        )?;
        Some(SelectedReport {
            reporter: report.reporter,
            weight: self.committee.weight(&report.reporter),
            certified,
            residual,
        })
    }
}

/// The weight of the usable reports of an epoch in the committee that issued
/// its votes
fn selected_weight(
    exchange: &ReportExchange,
    epoch: ConsensusEpoch,
    committee: &Committee,
    predecessor: BlockHash,
) -> Amount {
    exchange
        .usable(epoch)
        .iter()
        .filter(|(report, _, _)| report.predecessor == predecessor)
        .fold(Amount::ZERO, |sum, (report, _, _)| {
            sum.number()
                .checked_add(committee.weight(&report.reporter).number())
                .map(Amount::raw)
                .unwrap_or(Amount::MAX)
        })
}

fn report_ref(selection: &ReportSelection) -> ReportRef {
    ReportRef {
        reporter: selection.reporter,
        certified: selection.certified,
        residual: selection.residual,
    }
}

fn selection_of(report: &ReportRef) -> ReportSelection {
    ReportSelection {
        reporter: report.reporter,
        certified: report.certified,
        residual: report.residual,
    }
}
