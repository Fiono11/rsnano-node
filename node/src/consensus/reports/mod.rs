mod epoch_decision;
mod report_plugin;
mod report_service;
use crate::consensus::election::{
    AccountSlot, CertificateKinds, Certification, CertifiedBlock, CertifiedState, CertifiedStatus,
    CodedSymbol, Decoder, Encoder, EpochLedger, ReportCommitment, ResidualKind, ResidualVotes,
    SelectedReport,
};
pub use epoch_decision::EpochDecisionService;
pub(crate) use report_plugin::{ReportPlugin, ReportTicker};
pub use report_service::ReportService;
use rsnano_messages::{Report, ReportSet, ReportSymbolsReply, ReportSymbolsReq};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey, PublicKey};
use std::collections::{BTreeMap, HashMap, HashSet};

/// RAI: frozen reports and their local reconstructions. A report whose
/// roots match a state held here is usable at once; any other is rebuilt by
/// rateless reconciliation: this node streams the coded symbols of the
/// reporter's inventory from whoever holds it, subtracts its own view and
/// peels the difference, then checks the result against the signed root.
pub(crate) struct ReportExchange {
    epochs: BTreeMap<ConsensusEpoch, EpochReports>,
    max_epochs: usize,
}
#[derive(Default)]
struct EpochReports {
    live: CertifiedState,
    snapshots: BTreeMap<BlockHash, CertifiedState>,
    residuals: HashMap<BlockHash, ResidualVotes>,
    signed: Vec<Report>,
    theirs: HashMap<PublicKey, TheirReport>,
    repeated: Option<Timestamp>,
    /// The symbol streams this node serves, by inventory and root
    encoders: HashMap<(ReportSet, BlockHash), Encoder>,
}
struct TheirReport {
    report: Report,
    reconstructed: Option<CertifiedState>,
    residual: Option<ResidualVotes>,
    derived: Option<Timestamp>,
    certified_stream: Option<Stream<CertifiedState>>,
    residual_stream: Option<Stream<ResidualVotes>>,
    /// Every certified entry and residual record is justified by signed
    /// votes held here: only then is the report usable
    verified: bool,
    /// The hashes still lacking evidence, rechecked alone
    missing: Vec<BlockHash>,
    verified_at: Option<Timestamp>,
    /// The report names a block by a placement no derivation can use; no
    /// evidence makes it usable, and it is not asked for
    malformed: bool,
}

/// RAI: one running reconciliation of an inventory against a fixed base,
/// the local view as it stood when the stream started
struct Stream<B> {
    base: B,
    decoder: Decoder,
    /// When symbols were last asked for; None before the first request
    requested_at: Option<Timestamp>,
    /// Symbols asked for in the last request
    batch: u16,
}

impl<B> Stream<B> {
    const FIRST_BATCH: u16 = 16;

    fn new(base: B, decoder: Decoder) -> Self {
        Self {
            base,
            decoder,
            requested_at: None,
            batch: Self::FIRST_BATCH,
        }
    }

    /// The request to send now, if one is due: the first one, or a repeat
    /// of one unanswered for `ReportExchange::RETRY_INTERVAL`
    fn due(
        &mut self,
        epoch: ConsensusEpoch,
        set: ReportSet,
        target: BlockHash,
        now: Timestamp,
    ) -> Option<ReportSymbolsReq> {
        if self
            .requested_at
            .is_some_and(|at| at.elapsed(now) < ReportExchange::RETRY_INTERVAL)
        {
            return None;
        }
        self.requested_at = Some(now);
        Some(self.request(epoch, set, target))
    }

    fn request(
        &self,
        epoch: ConsensusEpoch,
        set: ReportSet,
        target: BlockHash,
    ) -> ReportSymbolsReq {
        ReportSymbolsReq {
            epoch,
            set,
            target,
            from: self.decoder.received() as u32,
            count: self.batch,
        }
    }

    /// Feeds a reply that continues the stream; replies for another offset
    /// (a duplicate from a second holder, a late answer) are ignored
    fn accept(&mut self, reply: &ReportSymbolsReply, now: Timestamp) -> bool {
        if reply.from as usize != self.decoder.received() || reply.symbol_count() == 0 {
            return false;
        }
        let symbols: Vec<CodedSymbol> = reply
            .symbols()
            .filter_map(CodedSymbol::deserialize)
            .collect();
        self.decoder.add_symbols(&symbols);
        // The next request goes out with this reply's handling
        self.requested_at = Some(now);
        // Half of what came so far: the overshoot past the symbol that
        // completes the decode stays under half of the symbols needed
        self.batch = (self.decoder.received() / 2)
            .clamp(Self::FIRST_BATCH as usize, ReportSymbolsReply::MAX_SYMBOLS)
            as u16;
        true
    }
}
impl TheirReport {
    fn is_complete(&self) -> bool {
        self.reconstructed.is_some() && self.residual.is_some()
    }

    fn is_usable(&self) -> bool {
        self.is_complete() && self.verified
    }
}

/// RAI, "Immutable candidate inputs": the blocks, by epoch, whose signed
/// votes a candidate's evidence manifest must name for the selected reports:
/// every certified entry the predecessor checkpoint does not justify, in the
/// report epoch and, for a finalized entry, in the earlier epochs whose
/// finality may justify it; and every residual record, in the report epoch.
pub(crate) fn manifest_claims(
    epoch: ConsensusEpoch,
    selection: &[SelectedReport],
    inherited: &dyn Fn(&CertifiedBlock, Certification) -> bool,
) -> Vec<(ConsensusEpoch, BlockHash)> {
    let earlier: Vec<ConsensusEpoch> = (1..=EARLIER_FINALITY_EPOCHS)
        .filter_map(|back| epoch.as_u64().checked_sub(back).map(ConsensusEpoch::new))
        .collect();
    let mut claims = std::collections::BTreeSet::new();
    for report in selection {
        for (block, entry) in report.certified.entries() {
            if inherited(block, *entry) {
                continue;
            }
            claims.insert((epoch, block.hash));
            if entry.status == CertifiedStatus::Finalized {
                for before in &earlier {
                    claims.insert((*before, block.hash));
                }
            }
        }
        for (block, _, _) in report.residual.entries() {
            claims.insert((epoch, block.hash));
        }
    }
    claims.into_iter().collect()
}

/// What checking a reconstructed report against the evidence held here found
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verification {
    /// The hashes whose evidence is still lacking; none means justified
    Missing(Vec<BlockHash>),
    /// The report is not well formed (see `well_formed`); it never becomes usable
    Malformed,
}

/// RAI: whether every block a report names is placed by a parent a
/// derivation can follow. A certified entry or residual record above the
/// first height names its parent; a zero parent there is accepted only for
/// a block the predecessor checkpoint finalized at that very position, whose
/// path a derivation never walks. A report violating this would make
/// `BuildState` fail for every validator that selects it, so it is left out
/// of every selection instead of stalling the close.
pub(crate) fn well_formed(
    certified: &CertifiedState,
    residual: &ResidualVotes,
    previous: Option<&EpochLedger>,
) -> bool {
    let placed = |block: &CertifiedBlock, parent: &BlockHash| {
        block.height <= 1
            || !parent.is_zero()
            || previous.is_some_and(|previous| {
                previous.is_finalized(&AccountSlot::new(block.account, block.height), &block.hash)
            })
    };
    certified
        .entries()
        .all(|(block, entry)| placed(block, &entry.previous))
        && residual
            .entries()
            .all(|(block, _, parent)| placed(&block, &parent))
}

/// RAI, "Reports that remain reconstructible": the hashes of a
/// reconstructed report whose evidence this node lacks. A root authenticates
/// the tags of a report, not the quorums behind them: a notarized entry
/// needs a notarization certificate assembled here from signed votes of the
/// epoch, a finalized entry a finalization or fast certificate or the
/// predecessor checkpoint's finality, and a residual record the reporter's
/// own signed vote. `only` restricts the check to hashes found missing
/// before. None while the epoch's committee is not known here.
pub(crate) fn unjustified(
    epoch: ConsensusEpoch,
    certified: &CertifiedState,
    residual: &ResidualVotes,
    only: Option<&[BlockHash]>,
    certificate_kinds: &dyn Fn(ConsensusEpoch, &[BlockHash]) -> Option<Vec<CertificateKinds>>,
    inherited: &dyn Fn(&CertifiedBlock, Certification) -> bool,
    reporter_votes: &dyn Fn(&[(BlockHash, ResidualKind)]) -> Vec<bool>,
) -> Option<Vec<BlockHash>> {
    let wanted = |hash: &BlockHash| only.is_none_or(|only| only.contains(hash));
    let entries: Vec<(&CertifiedBlock, Certification)> = certified
        .entries()
        .filter(|(block, _)| wanted(&block.hash))
        .map(|(block, entry)| (block, *entry))
        .collect();
    let hashes: Vec<BlockHash> = entries.iter().map(|(block, _)| block.hash).collect();
    let kinds = certificate_kinds(epoch, &hashes)?;
    let mut justified: HashSet<CertifiedBlock> = HashSet::new();
    let mut anchors: Vec<CertifiedBlock> = Vec::new();
    // F entries without a proof of this epoch: finality assembled in a
    // retained earlier epoch counts too, a certificate formed after that
    // epoch's checkpoint was decided
    let mut unproven: Vec<CertifiedBlock> = Vec::new();
    let mut missing = Vec::new();
    for ((block, entry), kinds) in entries.iter().zip(kinds) {
        // What the predecessor checkpoint itself holds is justified by it;
        // R, inherited protection, is justified by nothing else
        let inherited = inherited(block, *entry);
        match entry.status {
            CertifiedStatus::Recovery | CertifiedStatus::Notarized => {
                let fresh = entry.status == CertifiedStatus::Notarized && kinds.notarization;
                if inherited || fresh {
                    justified.insert(**block);
                } else {
                    missing.push(block.hash);
                }
            }
            CertifiedStatus::Finalized => {
                if inherited || kinds.finalization || kinds.fast {
                    anchors.push(**block);
                } else {
                    unproven.push(**block);
                }
            }
        }
    }
    let earlier: Vec<ConsensusEpoch> = (1..=EARLIER_FINALITY_EPOCHS)
        .filter_map(|back| epoch.as_u64().checked_sub(back).map(ConsensusEpoch::new))
        .collect();
    for before in earlier {
        if unproven.is_empty() {
            break;
        }
        let hashes: Vec<BlockHash> = unproven.iter().map(|block| block.hash).collect();
        let Some(kinds) = certificate_kinds(before, &hashes) else {
            continue;
        };
        let mut still = Vec::new();
        for (block, kinds) in unproven.into_iter().zip(kinds) {
            if kinds.finalization || kinds.fast {
                anchors.push(block);
            } else {
                still.push(block);
            }
        }
        unproven = still;
    }
    // "An entry tagged F must have an explicit valid finality proof for the
    // block or a selected descendant": the F prefix below a justified F
    // entry is justified by it. On a recheck, an F entry outside the
    // rechecked set was justified before and anchors its prefix as well.
    if only.is_some() {
        anchors.extend(
            certified
                .entries()
                .filter(|(block, entry)| {
                    entry.status == CertifiedStatus::Finalized && !wanted(&block.hash)
                })
                .map(|(block, _)| *block),
        );
    }
    if !unproven.is_empty() {
        let by_hash: HashMap<BlockHash, CertifiedBlock> = certified
            .entries()
            .map(|(block, _)| (block.hash, *block))
            .collect();
        for anchor in anchors {
            let mut current = anchor;
            while justified.insert(current) {
                let Some(entry) = certified.certification(&current) else {
                    break;
                };
                if current.height <= 1 || entry.previous.is_zero() {
                    break;
                }
                let Some(parent) = by_hash.get(&entry.previous) else {
                    break;
                };
                if parent.height + 1 != current.height
                    || parent.account != current.account
                    || certified.status(parent) != Some(CertifiedStatus::Finalized)
                {
                    break;
                }
                current = *parent;
            }
        }
        missing.extend(
            unproven
                .iter()
                .filter(|block| !justified.contains(block))
                .map(|block| block.hash),
        );
    }
    let records: Vec<(BlockHash, ResidualKind)> = residual
        .entries()
        .filter(|(block, _, _)| wanted(&block.hash))
        .map(|(block, kind, _)| (block.hash, kind))
        .collect();
    for ((hash, _), held) in records.iter().zip(reporter_votes(&records)) {
        if !held && !missing.contains(hash) {
            missing.push(*hash);
        }
    }
    Some(missing)
}

/// RAI: how many epochs before a report's own an F entry's finality proof
/// may come from: the vote records kept here
const EARLIER_FINALITY_EPOCHS: u64 = 3;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReportMessage {
    Broadcast(Report),
    /// Coded symbols asked of every node that may hold the inventory
    Request(ReportSymbolsReq),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReconcileResult {
    pub epoch: ConsensusEpoch,
    pub reporter: PublicKey,
    pub complete: bool,
    pub entries: usize,
    pub total: usize,
    /// Coded symbols the stream needed; 0 when no stream ran
    pub symbols: usize,
    /// The stream did not rebuild the signed root and was dropped
    pub dropped: bool,
}
impl ReportExchange {
    pub const MAX_EPOCHS: usize = 4;
    pub const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);
    pub const REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
    pub fn new() -> Self {
        Self {
            epochs: BTreeMap::new(),
            max_epochs: Self::MAX_EPOCHS,
        }
    }
    pub fn live_root(&self, epoch: ConsensusEpoch) -> Option<BlockHash> {
        self.epochs.get(&epoch).map(|held| held.live.root())
    }
    pub fn report_epoch(
        &mut self,
        epoch: ConsensusEpoch,
        certified: CertifiedState,
        residual: ResidualVotes,
        committee: BlockHash,
        predecessor: BlockHash,
        keys: &[PrivateKey],
    ) -> Vec<ReportMessage> {
        let held = self.epochs.entry(epoch).or_default();
        if !held.signed.is_empty() {
            return Vec::new();
        }
        let certified_root = certified.root();
        let residual_root = residual.root();
        held.signed = keys
            .iter()
            .map(|key| {
                let payload = ReportCommitment {
                    epoch,
                    committee,
                    predecessor,
                    certified: certified_root,
                    residual: residual_root,
                    reporter: key.public_key(),
                }
                .payload();
                Report::new(
                    key,
                    epoch,
                    committee,
                    predecessor,
                    certified_root,
                    residual_root,
                    payload,
                )
            })
            .collect();
        held.snapshots.insert(certified_root, certified.clone());
        for (block, entry) in certified.entries() {
            held.live.certify(*block, entry.previous, entry.status);
        }
        held.residuals.insert(residual_root, residual);
        let messages = held
            .signed
            .iter()
            .cloned()
            .map(ReportMessage::Broadcast)
            .collect();
        self.trim();
        messages
    }
    pub fn has_reported(&self, epoch: ConsensusEpoch) -> bool {
        self.epochs
            .get(&epoch)
            .is_some_and(|held| !held.signed.is_empty())
    }
    pub fn own_reports(&self, epoch: ConsensusEpoch) -> &[Report] {
        self.epochs
            .get(&epoch)
            .map(|held| held.signed.as_slice())
            .unwrap_or_default()
    }
    pub fn repeat_reports(&mut self, now: Timestamp) -> Vec<ReportMessage> {
        let mut messages = Vec::new();
        for held in self.epochs.values_mut() {
            if held.signed.is_empty()
                || held
                    .repeated
                    .is_some_and(|last| last.elapsed(now) < Self::REPEAT_INTERVAL)
            {
                continue;
            }
            held.repeated = Some(now);
            messages.extend(held.signed.iter().cloned().map(ReportMessage::Broadcast));
        }
        messages
    }
    pub fn reports(&self, epoch: ConsensusEpoch) -> Vec<&Report> {
        self.epochs
            .get(&epoch)
            .map(|held| held.theirs.values().map(|their| &their.report).collect())
            .unwrap_or_default()
    }
    pub fn usable_report(
        &self,
        epoch: ConsensusEpoch,
        reporter: &PublicKey,
        certified: BlockHash,
        residual: BlockHash,
        predecessor: BlockHash,
    ) -> Option<(&CertifiedState, &ResidualVotes)> {
        let held = self.epochs.get(&epoch)?;
        // This node's own reports count like any other: a selection is
        // `N - f` reports from distinct old-committee validators, and the
        // reporter is one of them. Leaving its own out would cost a
        // validator its own weight, which the largest member of a committee
        // can not afford.
        if let Some(own) = held
            .signed
            .iter()
            .find(|report| &report.reporter == reporter)
        {
            if own.certified != certified
                || own.residual != residual
                || own.predecessor != predecessor
            {
                return None;
            }
            return Some((held.state(certified)?, held.residuals.get(&residual)?));
        }
        let their = held.theirs.get(reporter)?;
        if their.report.certified != certified
            || their.report.residual != residual
            || their.report.predecessor != predecessor
            || !their.verified
        {
            return None;
        }
        Some((their.reconstructed.as_ref()?, their.residual.as_ref()?))
    }
    pub fn usable(&self, epoch: ConsensusEpoch) -> Vec<(&Report, &CertifiedState, &ResidualVotes)> {
        let Some(held) = self.epochs.get(&epoch) else {
            return Vec::new();
        };
        let own = held.signed.iter().filter_map(|report| {
            Some((
                report,
                held.state(report.certified)?,
                held.residuals.get(&report.residual)?,
            ))
        });
        let theirs = held
            .theirs
            .values()
            .filter(|their| their.verified)
            .filter_map(|their| {
                Some((
                    &their.report,
                    their.reconstructed.as_ref()?,
                    their.residual.as_ref()?,
                ))
            });
        own.chain(theirs).collect()
    }
    pub fn handle_report(&mut self, report: Report) -> bool {
        let payload = ReportCommitment {
            epoch: report.epoch,
            committee: report.committee,
            predecessor: report.predecessor,
            certified: report.certified,
            residual: report.residual,
            reporter: report.reporter,
        }
        .payload();
        if !report.verify(payload) {
            return false;
        }
        let held = self.epochs.entry(report.epoch).or_default();
        // A reporter signs one report per epoch; a second one changes nothing
        if held.theirs.contains_key(&report.reporter)
            || held
                .signed
                .iter()
                .any(|own| own.reporter == report.reporter)
        {
            return false;
        }
        // An empty residual object is settled by its signed root alone:
        // every replica knows the root of the empty object
        let residual = (report.residual == ResidualVotes::new().root()).then(ResidualVotes::new);
        held.theirs.insert(
            report.reporter,
            TheirReport {
                report,
                reconstructed: None,
                residual,
                derived: None,
                certified_stream: None,
                residual_stream: None,
                verified: false,
                missing: Vec::new(),
                verified_at: None,
                malformed: false,
            },
        );
        true
    }
    pub fn needs_residual(
        &self,
        epoch: ConsensusEpoch,
        reporter: &PublicKey,
        now: Timestamp,
    ) -> bool {
        self.epochs
            .get(&epoch)
            .and_then(|held| held.theirs.get(reporter))
            .is_some_and(|their| {
                their.reconstructed.is_some()
                    && their.residual.is_none()
                    && their
                        .derived
                        .is_none_or(|last| last.elapsed(now) >= Self::RETRY_INTERVAL)
            })
    }
    pub fn derive_residual(
        &mut self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
        votes: impl IntoIterator<Item = (CertifiedBlock, ResidualKind, BlockHash)>,
        now: Timestamp,
    ) -> Option<ReconcileResult> {
        let held = self.epochs.get_mut(&epoch)?;
        let their = held.theirs.get_mut(&reporter)?;
        let certified = their.reconstructed.as_ref()?;
        if their.residual.is_some() {
            return None;
        }
        their.derived = Some(now);
        let derived = ResidualVotes::derive(certified, votes);
        let total = derived.len();
        let complete = derived.root() == their.report.residual;
        if complete {
            // Votes that arrived since make a running stream unnecessary
            their.residual = Some(derived);
            their.residual_stream = None;
        } else if their.residual_stream.is_none() {
            // A running stream keeps its base: it must not move under it
            let decoder = Decoder::new(derived.items());
            their.residual_stream = Some(Stream::new(derived, decoder));
        }
        Some(ReconcileResult {
            epoch,
            reporter,
            complete,
            entries: total,
            total,
            symbols: 0,
            dropped: false,
        })
    }

    /// RAI: checks the reconstructed reports not yet verified, at most once
    /// per `RETRY_INTERVAL` each: the first time every entry, later only the
    /// hashes found missing. Returns, per reporter, the hashes still lacking
    /// evidence, which are asked for.
    pub fn verify(
        &mut self,
        epoch: ConsensusEpoch,
        now: Timestamp,
        mut check: impl FnMut(
            &PublicKey,
            &CertifiedState,
            &ResidualVotes,
            Option<&[BlockHash]>,
        ) -> Option<Verification>,
    ) -> Vec<(PublicKey, Vec<BlockHash>)> {
        let mut requests = Vec::new();
        let Some(held) = self.epochs.get_mut(&epoch) else {
            return requests;
        };
        for (reporter, their) in &mut held.theirs {
            if their.verified
                || their.malformed
                || their
                    .verified_at
                    .is_some_and(|at| at.elapsed(now) < Self::RETRY_INTERVAL)
            {
                continue;
            }
            let (Some(certified), Some(residual)) = (&their.reconstructed, &their.residual) else {
                continue;
            };
            let only = their
                .verified_at
                .is_some()
                .then_some(their.missing.as_slice());
            let missing = match check(reporter, certified, residual, only) {
                None => continue,
                Some(Verification::Malformed) => {
                    their.malformed = true;
                    continue;
                }
                Some(Verification::Missing(missing)) => missing,
            };
            their.verified_at = Some(now);
            their.verified = missing.is_empty();
            if !missing.is_empty() {
                requests.push((*reporter, missing.clone()));
            }
            their.missing = missing;
        }
        requests
    }

    /// RAI: reconstructed reports still lacking evidence, for the record
    pub fn unverified_count(&self, epoch: ConsensusEpoch) -> usize {
        self.epochs.get(&epoch).map_or(0, |held| {
            held.theirs
                .values()
                .filter(|their| their.is_complete() && !their.verified)
                .count()
        })
    }

    /// RAI: the symbol requests due now: the first of every stream started
    /// and a repeat of every one unanswered for `RETRY_INTERVAL`
    pub fn stream_requests(&mut self, now: Timestamp) -> Vec<ReportMessage> {
        let mut messages = Vec::new();
        for (epoch, held) in &mut self.epochs {
            for their in held.theirs.values_mut() {
                let report = &their.report;
                if let Some(stream) = &mut their.certified_stream {
                    messages.extend(
                        stream
                            .due(*epoch, ReportSet::Certified, report.certified, now)
                            .map(ReportMessage::Request),
                    );
                }
                if let Some(stream) = &mut their.residual_stream {
                    messages.extend(
                        stream
                            .due(*epoch, ReportSet::Residual, report.residual, now)
                            .map(ReportMessage::Request),
                    );
                }
            }
        }
        messages
    }

    /// RAI: symbols of an inventory this node holds, for a node rebuilding
    /// it. Any holder answers: a reporter's frozen snapshot, the live state
    /// while its root matches, and every report reconstructed here.
    pub fn symbols_for(&mut self, request: &ReportSymbolsReq) -> Option<ReportSymbolsReply> {
        let held = self.epochs.get_mut(&request.epoch)?;
        let key = (request.set, request.target);
        if !held.encoders.contains_key(&key) {
            let encoder = match request.set {
                ReportSet::Certified => Encoder::new(held.state(request.target)?.items()),
                ReportSet::Residual => Encoder::new(held.residual(request.target)?.items()),
            };
            while held.encoders.len() >= EpochReports::MAX_ENCODERS {
                let Some(evicted) = held.encoders.keys().next().copied() else {
                    break;
                };
                held.encoders.remove(&evicted);
            }
            held.encoders.insert(key, encoder);
        }
        let encoder = held.encoders.get_mut(&key)?;
        let count = (request.count as usize).min(ReportSymbolsReply::MAX_SYMBOLS);
        let mut symbols = Vec::with_capacity(count * ReportSymbolsReply::SYMBOL_SIZE);
        for symbol in encoder.symbols(request.from as usize, count) {
            symbol.serialize(&mut symbols);
        }
        if symbols.is_empty() {
            return None;
        }
        Some(ReportSymbolsReply {
            epoch: request.epoch,
            set: request.set,
            target: request.target,
            from: request.from,
            symbols,
        })
    }

    /// RAI: symbols of an inventory this node is rebuilding. Once the
    /// difference decodes, the base with the difference applied must hash to
    /// the signed root; anything else drops the stream, and the next tick
    /// starts another from the live view as it stands then.
    pub fn handle_symbols(
        &mut self,
        reply: &ReportSymbolsReply,
        now: Timestamp,
    ) -> (Vec<ReportMessage>, Vec<ReconcileResult>) {
        let mut messages = Vec::new();
        let mut results = Vec::new();
        let Some(held) = self.epochs.get_mut(&reply.epoch) else {
            return (messages, results);
        };
        for (reporter, their) in &mut held.theirs {
            match reply.set {
                ReportSet::Certified => {
                    if their.report.certified != reply.target {
                        continue;
                    }
                    let Some(stream) = &mut their.certified_stream else {
                        continue;
                    };
                    if !stream.accept(reply, now) {
                        continue;
                    }
                    if stream.decoder.failed() {
                        results.push(dropped(reply, *reporter, stream.decoder.received()));
                        their.certified_stream = None;
                        continue;
                    }
                    if !stream.decoder.is_done() {
                        messages.push(ReportMessage::Request(stream.request(
                            reply.epoch,
                            ReportSet::Certified,
                            reply.target,
                        )));
                        continue;
                    }
                    let entries = stream.decoder.recovered().count();
                    let symbols = stream.decoder.received();
                    let rebuilt = stream.base.with_difference(stream.decoder.recovered());
                    their.certified_stream = None;
                    let Some(rebuilt) = rebuilt.filter(|state| state.root() == reply.target) else {
                        results.push(dropped(reply, *reporter, symbols));
                        continue;
                    };
                    let total = rebuilt.len();
                    their.reconstructed = Some(rebuilt);
                    results.push(ReconcileResult {
                        epoch: reply.epoch,
                        reporter: *reporter,
                        complete: their.is_complete(),
                        entries,
                        total,
                        symbols,
                        dropped: false,
                    });
                }
                ReportSet::Residual => {
                    if their.report.residual != reply.target {
                        continue;
                    }
                    let Some(stream) = &mut their.residual_stream else {
                        continue;
                    };
                    if !stream.accept(reply, now) {
                        continue;
                    }
                    if stream.decoder.failed() {
                        results.push(dropped(reply, *reporter, stream.decoder.received()));
                        their.residual_stream = None;
                        continue;
                    }
                    if !stream.decoder.is_done() {
                        messages.push(ReportMessage::Request(stream.request(
                            reply.epoch,
                            ReportSet::Residual,
                            reply.target,
                        )));
                        continue;
                    }
                    let entries = stream.decoder.recovered().count();
                    let symbols = stream.decoder.received();
                    let rebuilt = stream.base.with_difference(stream.decoder.recovered());
                    their.residual_stream = None;
                    let Some(rebuilt) = rebuilt.filter(|votes| votes.root() == reply.target) else {
                        results.push(dropped(reply, *reporter, symbols));
                        continue;
                    };
                    let total = rebuilt.len();
                    their.residual = Some(rebuilt);
                    results.push(ReconcileResult {
                        epoch: reply.epoch,
                        reporter: *reporter,
                        complete: their.is_complete(),
                        entries,
                        total,
                        symbols,
                        dropped: false,
                    });
                }
            }
        }
        (messages, results)
    }
    fn trim(&mut self) {
        while self.epochs.len() > self.max_epochs {
            let Some(oldest) = self.epochs.keys().next().copied() else {
                break;
            };
            self.epochs.remove(&oldest);
        }
    }

    /// RAI, "Retained evidence": the handoff evidence of an epoch, its
    /// frozen reports, reconstructions and symbol encoders, is released once
    /// the successors hold the checkpoint it produced
    pub fn release_epoch(&mut self, epoch: ConsensusEpoch) -> bool {
        self.epochs.remove(&epoch).is_some()
    }

    pub fn pending_epochs(&self) -> Vec<ConsensusEpoch> {
        self.epochs.keys().copied().collect()
    }
    pub fn refresh_live(&mut self, epoch: ConsensusEpoch, live: CertifiedState) {
        let held = self.epochs.entry(epoch).or_default();
        for (block, entry) in live.entries() {
            held.live.certify(*block, entry.previous, entry.status);
        }
    }
    pub fn reconcile(
        &mut self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
        now: Timestamp,
    ) -> Option<(Vec<ReportMessage>, Option<ReconcileResult>)> {
        let held = self.epochs.get_mut(&epoch)?;
        let their = held.theirs.get(&reporter)?;
        if their.is_complete() || their.reconstructed.is_some() {
            return None;
        }
        let state = held.state(their.report.certified).cloned();
        let held_live = &held.live;
        let their = held.theirs.get_mut(&reporter)?;
        if let Some(state) = state {
            let total = state.len();
            their.reconstructed = Some(state);
            their.certified_stream = None;
            return Some((
                Vec::new(),
                Some(ReconcileResult {
                    epoch,
                    reporter,
                    complete: their.is_complete(),
                    entries: 0,
                    total,
                    symbols: 0,
                    dropped: false,
                }),
            ));
        }
        if their.certified_stream.is_none() {
            let base = held_live.clone();
            let decoder = Decoder::new(base.items());
            their.certified_stream = Some(Stream::new(base, decoder));
        }
        Some((Vec::new(), None))
    }
}
/// A stream that did not rebuild the signed root
fn dropped(reply: &ReportSymbolsReply, reporter: PublicKey, symbols: usize) -> ReconcileResult {
    ReconcileResult {
        epoch: reply.epoch,
        reporter,
        complete: false,
        entries: 0,
        total: 0,
        symbols,
        dropped: true,
    }
}

impl EpochReports {
    /// Symbol streams served per epoch; a request for another root rebuilds
    /// its encoder
    const MAX_ENCODERS: usize = 16;

    fn residual(&self, root: BlockHash) -> Option<&ResidualVotes> {
        if let Some(residual) = self.residuals.get(&root) {
            return Some(residual);
        }
        self.theirs
            .values()
            .filter_map(|their| their.residual.as_ref())
            .find(|residual| residual.root() == root)
    }

    fn state(&self, root: BlockHash) -> Option<&CertifiedState> {
        if self.live.root() == root {
            return Some(&self.live);
        }
        if let Some(state) = self.snapshots.get(&root) {
            return Some(state);
        }
        self.theirs
            .values()
            .filter_map(|their| their.reconstructed.as_ref())
            .find(|state| state.root() == root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::CertifiedStatus;
    use rsnano_types::Account;

    fn block() -> CertifiedBlock {
        CertifiedBlock::new(Account::from(1), 1, BlockHash::from(2))
    }

    fn report(state: CertifiedState, residual: ResidualVotes) -> Report {
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            ConsensusEpoch::ZERO,
            state,
            residual,
            BlockHash::from(3),
            BlockHash::from(4),
            &[PrivateKey::from(1)],
        );
        reporter.own_reports(ConsensusEpoch::ZERO)[0].clone()
    }

    #[test]
    fn identical_root_is_usable_without_messages() {
        let mut state = CertifiedState::new();
        state.certify(block(), BlockHash::ZERO, CertifiedStatus::Notarized);
        let report = report(state.clone(), ResidualVotes::new());
        let mut exchange = ReportExchange::new();
        exchange.refresh_live(report.epoch, state);
        assert!(exchange.handle_report(report.clone()));
        let (messages, result) = exchange
            .reconcile(
                report.epoch,
                report.reporter,
                Timestamp::new_test_instance(),
            )
            .unwrap();
        assert!(messages.is_empty());
        assert!(result.unwrap().complete);
        trust_all(&mut exchange, report.epoch);
        assert_eq!(exchange.usable(report.epoch).len(), 1);
    }

    #[test]
    fn different_root_stays_unusable_until_local_evidence_matches() {
        let mut state = CertifiedState::new();
        state.certify(block(), BlockHash::ZERO, CertifiedStatus::Finalized);
        let report = report(state.clone(), ResidualVotes::new());
        let mut exchange = ReportExchange::new();
        exchange.handle_report(report.clone());
        let (messages, result) = exchange
            .reconcile(
                report.epoch,
                report.reporter,
                Timestamp::new_test_instance(),
            )
            .unwrap();
        assert!(messages.is_empty());
        assert!(result.is_none());
        assert!(exchange.usable(report.epoch).is_empty());
        exchange.refresh_live(report.epoch, state);
        exchange.reconcile(
            report.epoch,
            report.reporter,
            Timestamp::new_test_instance(),
        );
        trust_all(&mut exchange, report.epoch);
        assert_eq!(exchange.usable(report.epoch).len(), 1);
    }

    #[test]
    fn both_roots_must_match() {
        let mut votes = ResidualVotes::new();
        votes.record(block(), BlockHash::ZERO, ResidualKind::First);
        let report = report(CertifiedState::new(), votes);
        let mut exchange = ReportExchange::new();
        exchange.handle_report(report.clone());
        let now = Timestamp::new_test_instance();
        exchange.reconcile(report.epoch, report.reporter, now);
        assert!(exchange.usable(report.epoch).is_empty());
        exchange.derive_residual(report.epoch, report.reporter, [], now);
        assert!(exchange.usable(report.epoch).is_empty());
        exchange.derive_residual(
            report.epoch,
            report.reporter,
            [(block(), ResidualKind::First, BlockHash::ZERO)],
            now,
        );
        trust_all(&mut exchange, report.epoch);
        assert_eq!(exchange.usable(report.epoch).len(), 1);
    }

    #[test]
    fn report_signature_and_identity_are_checked() {
        let report = report(CertifiedState::new(), ResidualVotes::new());
        let mut exchange = ReportExchange::new();
        let mut altered = report.clone();
        altered.certified = BlockHash::from(99);
        assert!(!exchange.handle_report(altered));
        assert!(exchange.handle_report(report.clone()));
        assert!(!exchange.handle_report(report));
    }

    #[test]
    fn own_snapshot_is_frozen_and_not_counted_twice() {
        let mut exchange = ReportExchange::new();
        let epoch = ConsensusEpoch::ZERO;
        exchange.report_epoch(
            epoch,
            CertifiedState::new(),
            ResidualVotes::new(),
            BlockHash::from(3),
            BlockHash::from(4),
            &[PrivateKey::from(1)],
        );
        let own = exchange.own_reports(epoch)[0].clone();
        assert!(!exchange.handle_report(own.clone()));
        let mut live = CertifiedState::new();
        live.certify(block(), BlockHash::ZERO, CertifiedStatus::Finalized);
        exchange.refresh_live(epoch, live);
        assert_ne!(exchange.live_root(epoch), Some(own.certified));
        trust_all(&mut exchange, epoch);
        assert_eq!(exchange.usable(epoch).len(), 1);
        assert_eq!(exchange.usable(epoch)[0].1.root(), own.certified);
    }

    /// RAI: a report whose root no local state shares is rebuilt from the
    /// reporter's coded symbols: no shared root, no retry on a failed decode
    #[test]
    fn a_report_without_a_shared_root_is_rebuilt_from_symbols() {
        let (reporter, report, mut requester, now) = diverged(3, 40);
        let mut reporter = reporter;
        assert!(requester.usable(report.epoch).is_empty());
        let (messages, result) = requester
            .reconcile(report.epoch, report.reporter, now)
            .unwrap();
        assert!(messages.is_empty() && result.is_none());
        let rounds = pull(&mut requester, &mut reporter, now);
        assert!(rounds >= 1);
        trust_all(&mut requester, report.epoch);
        let usable = requester.usable(report.epoch);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].1.root(), report.certified);
    }

    /// A node thousands of entries behind finishes in one pull loop
    #[test]
    fn a_node_thousands_of_entries_short_catches_up() {
        let (mut reporter, report, mut requester, now) = diverged(3000, 3010);
        requester.reconcile(report.epoch, report.reporter, now);
        pull(&mut requester, &mut reporter, now);
        trust_all(&mut requester, report.epoch);
        assert_eq!(requester.usable(report.epoch).len(), 1);
    }

    /// Symbols for another root, or for an offset the stream is not at,
    /// change nothing
    #[test]
    fn a_reply_for_another_target_or_offset_is_ignored() {
        let (mut reporter, report, mut requester, now) = diverged(3, 40);
        requester.reconcile(report.epoch, report.reporter, now);
        let request = match requester.stream_requests(now).remove(0) {
            ReportMessage::Request(request) => request,
            other => panic!("{other:?}"),
        };
        let mut reply = reporter.symbols_for(&request).unwrap();
        reply.target = BlockHash::from(12345);
        let (messages, results) = requester.handle_symbols(&reply, now);
        assert!(messages.is_empty() && results.is_empty());
        let mut reply = reporter.symbols_for(&request).unwrap();
        reply.from = 7;
        let (messages, results) = requester.handle_symbols(&reply, now);
        assert!(messages.is_empty() && results.is_empty());
        // The right reply continues the stream
        let reply = reporter.symbols_for(&request).unwrap();
        let (messages, results) = requester.handle_symbols(&reply, now);
        assert!(!messages.is_empty() || !results.is_empty());
    }

    /// A lost reply is asked for again after the retry interval, from the
    /// same offset
    #[test]
    fn a_stream_resumes_after_a_lost_reply() {
        let (mut reporter, report, mut requester, now) = diverged(3, 40);
        requester.reconcile(report.epoch, report.reporter, now);
        let first = requester.stream_requests(now);
        assert_eq!(first.len(), 1);
        // Lost. Nothing is repeated before the retry interval...
        assert!(requester.stream_requests(now).is_empty());
        // ...and the same request goes out after it
        let later = now + ReportExchange::RETRY_INTERVAL;
        let again = requester.stream_requests(later);
        assert_eq!(again, first);
        carry(&mut requester, &mut reporter, again, later);
        requester.derive_residual(report.epoch, report.reporter, [], later);
        trust_all(&mut requester, report.epoch);
        assert_eq!(requester.usable(report.epoch).len(), 1);
    }

    /// The residual object of a reporter whose vote this node never
    /// received is rebuilt the same way, against the derived records
    #[test]
    fn a_lost_vote_is_recovered_from_the_residual_stream() {
        let mut votes = ResidualVotes::new();
        votes.record(block(), BlockHash::ZERO, ResidualKind::First);
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            ConsensusEpoch::ZERO,
            CertifiedState::new(),
            votes,
            BlockHash::from(3),
            BlockHash::from(4),
            &[PrivateKey::from(1)],
        );
        let report = reporter.own_reports(ConsensusEpoch::ZERO)[0].clone();
        let mut requester = ReportExchange::new();
        let now = Timestamp::new_test_instance();
        requester.handle_report(report.clone());
        requester.reconcile(report.epoch, report.reporter, now);
        // This node holds none of the reporter's votes
        requester.derive_residual(report.epoch, report.reporter, [], now);
        assert!(requester.usable(report.epoch).is_empty());
        pull(&mut requester, &mut reporter, now);
        trust_all(&mut requester, report.epoch);
        let usable = requester.usable(report.epoch);
        assert_eq!(usable.len(), 1);
        assert!(usable[0].2.contains(&block(), ResidualKind::First));
    }

    /// A node that rebuilt a report serves it like the reporter would
    #[test]
    fn a_rebuilt_report_is_served_by_its_holder() {
        let (mut reporter, report, mut first, now) = diverged(3, 40);
        first.reconcile(report.epoch, report.reporter, now);
        pull(&mut first, &mut reporter, now);
        let (_, _, mut second, _) = diverged(3, 25);
        second.handle_report(report.clone());
        second.reconcile(report.epoch, report.reporter, now);
        pull(&mut second, &mut first, now);
        trust_all(&mut second, report.epoch);
        assert_eq!(second.usable(report.epoch).len(), 1);
    }

    /// RAI: "an entry tagged F must have an explicit valid finality proof
    /// for the block or a selected descendant". A proven F entry justifies
    /// the F entries below it on its chain; a finalization assembled in an
    /// earlier epoch proves one too.
    #[test]
    fn a_finalized_descendant_or_an_earlier_epoch_proves_an_f_entry() {
        let account = Account::from(1);
        let parent = CertifiedBlock::new(account, 1, BlockHash::from(11));
        let child = CertifiedBlock::new(account, 2, BlockHash::from(12));
        let late = CertifiedBlock::new(Account::from(2), 1, BlockHash::from(21));
        let unproven = CertifiedBlock::new(Account::from(3), 1, BlockHash::from(31));
        let mut certified = CertifiedState::new();
        certified.certify(parent, BlockHash::ZERO, CertifiedStatus::Finalized);
        certified.certify(child, parent.hash, CertifiedStatus::Finalized);
        certified.certify(late, BlockHash::ZERO, CertifiedStatus::Finalized);
        certified.certify(unproven, BlockHash::ZERO, CertifiedStatus::Finalized);
        let kinds = |epoch: ConsensusEpoch, hashes: &[BlockHash]| {
            Some(
                hashes
                    .iter()
                    .map(|hash| CertificateKinds {
                        // The child this epoch, the late block the epoch before
                        finalization: (epoch == EPOCH && *hash == child.hash)
                            || (epoch < EPOCH && *hash == late.hash),
                        ..Default::default()
                    })
                    .collect(),
            )
        };
        let nothing = |_: &CertifiedBlock, _: Certification| false;
        let no_votes = |votes: &[(BlockHash, ResidualKind)]| vec![false; votes.len()];
        let residual = ResidualVotes::new();
        let missing = unjustified(
            EPOCH, &certified, &residual, None, &kinds, &nothing, &no_votes,
        );
        assert_eq!(missing, Some(vec![unproven.hash]));
        // Rechecking the parent alone: its proven child still anchors it
        let only = [parent.hash];
        let none = |_: ConsensusEpoch, hashes: &[BlockHash]| {
            Some(vec![CertificateKinds::default(); hashes.len()])
        };
        let missing = unjustified(
            EPOCH,
            &certified,
            &residual,
            Some(&only),
            &none,
            &nothing,
            &no_votes,
        );
        assert_eq!(missing, Some(vec![]));
    }

    /// RAI: an R entry is inherited protection. Only the predecessor
    /// checkpoint justifies it; certificates held here do not.
    #[test]
    fn a_recovery_entry_is_justified_only_by_the_predecessor() {
        let carried = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(11));
        let invented = CertifiedBlock::new(Account::from(2), 1, BlockHash::from(12));
        let mut certified = CertifiedState::new();
        certified.certify(carried, BlockHash::ZERO, CertifiedStatus::Recovery);
        certified.certify(invented, BlockHash::ZERO, CertifiedStatus::Recovery);
        let everything = |_: ConsensusEpoch, hashes: &[BlockHash]| {
            Some(
                hashes
                    .iter()
                    .map(|_| CertificateKinds {
                        notarization: true,
                        finalization: true,
                        fast: true,
                    })
                    .collect(),
            )
        };
        let previously = |block: &CertifiedBlock, entry: Certification| {
            *block == carried && entry.status == CertifiedStatus::Recovery
        };
        let no_votes = |votes: &[(BlockHash, ResidualKind)]| vec![false; votes.len()];
        let missing = unjustified(
            EPOCH,
            &certified,
            &ResidualVotes::new(),
            None,
            &everything,
            &previously,
            &no_votes,
        );
        assert_eq!(missing, Some(vec![invented.hash]));
    }

    /// RAI: a root authenticates a report's tags, not the quorums behind
    /// them. A notarized entry needs a notarization certificate held here,
    /// a finalized one a finalization or fast certificate or the
    /// predecessor's finality, a residual record the reporter's own vote.
    #[test]
    fn a_report_entry_without_evidence_here_is_unjustified() {
        let notarized = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(11));
        let finalized = CertifiedBlock::new(Account::from(2), 1, BlockHash::from(12));
        let inherited = CertifiedBlock::new(Account::from(3), 1, BlockHash::from(13));
        let voted = CertifiedBlock::new(Account::from(4), 1, BlockHash::from(14));
        let mut certified = CertifiedState::new();
        certified.certify(notarized, BlockHash::ZERO, CertifiedStatus::Notarized);
        certified.certify(finalized, BlockHash::ZERO, CertifiedStatus::Finalized);
        certified.certify(inherited, BlockHash::ZERO, CertifiedStatus::Finalized);
        let mut residual = ResidualVotes::new();
        residual.record(voted, BlockHash::ZERO, ResidualKind::First);
        let kinds = |_: ConsensusEpoch, hashes: &[BlockHash]| {
            Some(
                hashes
                    .iter()
                    .map(|hash| CertificateKinds {
                        // A notarization certificate for both, finality for none
                        notarization: *hash == notarized.hash || *hash == finalized.hash,
                        ..Default::default()
                    })
                    .collect(),
            )
        };
        let previously = |block: &CertifiedBlock, _: Certification| *block == inherited;
        let no_votes = |votes: &[(BlockHash, ResidualKind)]| vec![false; votes.len()];
        let missing = unjustified(
            EPOCH,
            &certified,
            &residual,
            None,
            &kinds,
            &previously,
            &no_votes,
        );
        let mut missing = missing.unwrap();
        missing.sort();
        assert_eq!(missing, vec![finalized.hash, voted.hash]);
        // Rechecking only what was missing, once the votes arrived
        let all = |_: ConsensusEpoch, hashes: &[BlockHash]| {
            Some(
                hashes
                    .iter()
                    .map(|_| CertificateKinds {
                        notarization: true,
                        finalization: true,
                        fast: false,
                    })
                    .collect(),
            )
        };
        let votes = |votes: &[(BlockHash, ResidualKind)]| vec![true; votes.len()];
        assert_eq!(
            unjustified(
                EPOCH,
                &certified,
                &residual,
                Some(&missing),
                &all,
                &previously,
                &votes
            ),
            Some(Vec::new())
        );
    }

    /// A reconstructed report is usable only once verified, and a report
    /// whose evidence is missing is rechecked after the retry interval on
    /// the missing hashes alone
    #[test]
    fn an_unverified_report_is_not_usable_until_its_evidence_arrives() {
        let (mut reporter, report, mut requester, now) = diverged(3, 40);
        requester.reconcile(report.epoch, report.reporter, now);
        pull(&mut requester, &mut reporter, now);
        assert!(requester.usable(report.epoch).is_empty());
        let lacking = BlockHash::from(1001);
        let requests = requester.verify(report.epoch, now, |_, _, _, only| {
            assert!(only.is_none());
            Some(Verification::Missing(vec![lacking]))
        });
        assert_eq!(requests, vec![(report.reporter, vec![lacking])]);
        assert!(requester.usable(report.epoch).is_empty());
        assert_eq!(requester.unverified_count(report.epoch), 1);
        // Not rechecked before the retry interval
        assert!(
            requester
                .verify(report.epoch, now, |_, _, _, _| unreachable!())
                .is_empty()
        );
        let later = now + ReportExchange::RETRY_INTERVAL;
        requester.verify(report.epoch, later, |_, _, _, only| {
            assert_eq!(only, Some(&[lacking][..]));
            Some(Verification::Missing(Vec::new()))
        });
        assert_eq!(requester.usable(report.epoch).len(), 1);
    }

    /// RAI, "Retained evidence": releasing an epoch drops its reports and
    /// reconstructions; nothing of the epoch is usable or served afterwards
    #[test]
    fn a_released_epoch_holds_no_reports() {
        let (mut reporter, report, mut requester, now) = diverged(3, 40);
        requester.reconcile(report.epoch, report.reporter, now);
        pull(&mut requester, &mut reporter, now);
        trust_all(&mut requester, report.epoch);
        assert_eq!(requester.usable(report.epoch).len(), 1);
        assert!(requester.release_epoch(report.epoch));
        assert!(requester.usable(report.epoch).is_empty());
        assert!(requester.reports(report.epoch).is_empty());
        assert!(!requester.release_epoch(report.epoch));
    }

    /// RAI: a report no derivation can use is never usable, and no evidence
    /// is asked for it again
    #[test]
    fn a_malformed_report_is_never_usable_nor_rechecked() {
        let (mut reporter, report, mut requester, now) = diverged(3, 40);
        requester.reconcile(report.epoch, report.reporter, now);
        pull(&mut requester, &mut reporter, now);
        let requests = requester.verify(report.epoch, now, |_, _, _, _| {
            Some(Verification::Malformed)
        });
        assert!(requests.is_empty());
        assert!(requester.usable(report.epoch).is_empty());
        let later = now + ReportExchange::RETRY_INTERVAL;
        requester.verify(report.epoch, later, |_, _, _, _| unreachable!());
        assert!(requester.usable(report.epoch).is_empty());
    }

    /// RAI: a block above the first height names its parent, unless the
    /// predecessor finalized it there (genesis seeds such entries)
    #[test]
    fn a_report_naming_a_block_without_its_parent_is_malformed() {
        let account = Account::from(1);
        let parentless = CertifiedBlock::new(account, 2, BlockHash::from(2));
        let mut certified = CertifiedState::new();
        certified.certify(parentless, BlockHash::ZERO, CertifiedStatus::Notarized);
        assert!(!well_formed(&certified, &ResidualVotes::new(), None));

        let mut previous = EpochLedger::new();
        previous.finalize_genesis(AccountSlot::new(account, 2), BlockHash::from(2));
        assert!(well_formed(
            &certified,
            &ResidualVotes::new(),
            Some(&previous)
        ));

        let mut placed = CertifiedState::new();
        placed.certify(parentless, BlockHash::from(1), CertifiedStatus::Notarized);
        assert!(well_formed(&placed, &ResidualVotes::new(), None));

        let mut residual = ResidualVotes::new();
        residual.record(
            CertifiedBlock::new(account, 3, BlockHash::from(3)),
            BlockHash::ZERO,
            ResidualKind::First,
        );
        assert!(!well_formed(&placed, &residual, None));
        let mut opened = ResidualVotes::new();
        opened.record(
            CertifiedBlock::new(account, 1, BlockHash::from(9)),
            BlockHash::ZERO,
            ResidualKind::First,
        );
        assert!(well_formed(&placed, &opened, None));
    }

    /*
     * Test helpers
     */

    fn entry(i: u64) -> CertifiedBlock {
        CertifiedBlock::new(Account::from(i), 1, BlockHash::from(i + 1000))
    }

    /// A reporter holding entries 1..=reported, the report it signed, and a
    /// requester that received it and holds entries `shared..=held` live
    fn diverged(
        shared_from: u64,
        held: u64,
    ) -> (ReportExchange, Report, ReportExchange, Timestamp) {
        let reported = 30;
        let mut state = CertifiedState::new();
        for i in 1..=reported {
            state.certify(entry(i), BlockHash::ZERO, CertifiedStatus::Notarized);
        }
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            ConsensusEpoch::ZERO,
            state,
            ResidualVotes::new(),
            BlockHash::from(3),
            BlockHash::from(4),
            &[PrivateKey::from(1)],
        );
        let report = reporter.own_reports(ConsensusEpoch::ZERO)[0].clone();
        let mut live = CertifiedState::new();
        for i in shared_from..=held {
            live.certify(entry(i), BlockHash::ZERO, CertifiedStatus::Finalized);
        }
        let mut requester = ReportExchange::new();
        requester.refresh_live(report.epoch, live);
        requester.handle_report(report.clone());
        (reporter, report, requester, Timestamp::new_test_instance())
    }

    /// Carries requests to the holder and its replies back, and the
    /// requests those replies lead to, until none is left; returns the
    /// replies carried
    fn carry(
        requester: &mut ReportExchange,
        holder: &mut ReportExchange,
        messages: Vec<ReportMessage>,
        now: Timestamp,
    ) -> usize {
        let mut pending = messages;
        let mut replies = 0;
        while let Some(message) = pending.pop() {
            let ReportMessage::Request(request) = message else {
                continue;
            };
            let Some(reply) = holder.symbols_for(&request) else {
                continue;
            };
            replies += 1;
            assert!(replies < 1000, "the stream does not end");
            let (more, _) = requester.handle_symbols(&reply, now);
            pending.extend(more);
        }
        replies
    }

    /// One reconciliation round: the certified streams due, the residual
    /// derivation they enable, then the residual streams due
    fn pull(requester: &mut ReportExchange, holder: &mut ReportExchange, now: Timestamp) -> usize {
        let requests = requester.stream_requests(now);
        let mut replies = carry(requester, holder, requests, now);
        for epoch in requester.pending_epochs() {
            let reporters: Vec<PublicKey> = requester
                .reports(epoch)
                .iter()
                .map(|r| r.reporter)
                .collect();
            for reporter in reporters {
                requester.derive_residual(epoch, reporter, [], now);
            }
        }
        let requests = requester.stream_requests(now);
        replies += carry(requester, holder, requests, now);
        replies
    }

    /// Every entry counts as justified: the evidence check is tested apart
    fn trust_all(exchange: &mut ReportExchange, epoch: ConsensusEpoch) {
        exchange.verify(epoch, Timestamp::new_test_instance(), |_, _, _, _| {
            Some(Verification::Missing(Vec::new()))
        });
    }

    /* Test helpers */

    const EPOCH: ConsensusEpoch = ConsensusEpoch::new(1);
}
