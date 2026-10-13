mod checkpoint_fetch;
mod chunk_window;
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
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

/// RAI: frozen reports and their local reconstructions. A report whose
/// roots match a state held here is usable at once; any other is rebuilt by
/// rateless reconciliation: this node streams the coded symbols of the
/// reporter's inventory from whoever holds it, subtracts its own view and
/// peels the difference, then checks the result against the signed root.
pub(crate) struct ReportExchange {
    epochs: BTreeMap<ConsensusEpoch, EpochReports>,
    max_epochs: usize,
    /// The epochs whose evidence was released: their reports keep arriving
    /// from replicas that still repeat theirs, and must not bring the epoch
    /// back, with the reconciliation and verification that would follow
    released: BTreeSet<ConsensusEpoch>,
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
    reconstructed: Option<Arc<CertifiedState>>,
    residual: Option<Arc<ResidualVotes>>,
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
    /// What the stream asks for: a residual set, a certified state in full,
    /// or a certified state's changes over the inherited base
    set: ReportSet,
    base: B,
    decoder: Decoder,
    /// When symbols were last asked for; None before the first request
    requested_at: Option<Timestamp>,
    /// Symbols asked for in the last request
    batch: u16,
}

impl<B> Stream<B> {
    const FIRST_BATCH: u16 = 16;

    fn new(set: ReportSet, base: B, decoder: Decoder) -> Self {
        Self {
            set,
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
        Some(self.request(epoch, target))
    }

    fn request(&self, epoch: ConsensusEpoch, target: BlockHash) -> ReportSymbolsReq {
        ReportSymbolsReq {
            epoch,
            set: self.set,
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
        // An entry of the report's inherited base is inherited
        for (block, entry) in report.certified.changed_entries() {
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

/// RAI: a reconstructed report due a check against the evidence held here,
/// taken out of the exchange so that the check runs without holding it
pub(crate) struct VerifyJob {
    reporter: PublicKey,
    certified: Arc<CertifiedState>,
    residual: Arc<ResidualVotes>,
    /// The hashes found missing before; None for the first check
    only: Option<Vec<BlockHash>>,
    /// When the report was last checked, to tell a job another overtook
    verified_at: Option<Timestamp>,
}

impl VerifyJob {
    pub fn check(
        &self,
        check: &mut impl FnMut(
            &PublicKey,
            &CertifiedState,
            &ResidualVotes,
            Option<&[BlockHash]>,
        ) -> Option<Verification>,
    ) -> Option<Verification> {
        check(
            &self.reporter,
            &self.certified,
            &self.residual,
            self.only.as_deref(),
        )
    }

    /// The report is still the one checked, and still unchecked since
    fn still_due(&self, their: &TheirReport) -> bool {
        !their.verified
            && !their.malformed
            && their.verified_at == self.verified_at
            && their
                .reconstructed
                .as_ref()
                .is_some_and(|state| Arc::ptr_eq(state, &self.certified))
            && their
                .residual
                .as_ref()
                .is_some_and(|residual| Arc::ptr_eq(residual, &self.residual))
    }
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
///
/// Fix B, "Usable reports": a G_i record names no hash that T_i tags N or F,
/// since such an entry already exposes the vote. It may name one T_i tags R:
/// a reporter that first voted an R-tagged block in the epoch lists that
/// block in both.
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
    // An entry of the inherited base is placed by the predecessor itself
    certified
        .changed_entries()
        .all(|(block, entry)| placed(block, &entry.previous))
        && residual
            .entries()
            .all(|(block, _, parent)| placed(&block, &parent) && !certified.summarizes(&block.hash))
}

/// RAI, "Reports that remain reconstructible": the hashes of a
/// reconstructed report whose evidence this node lacks. A root authenticates
/// the tags of a report, not the quorums behind them: a notarized entry
/// needs a notarization certificate assembled here from signed votes of the
/// epoch, a finalized entry a finalization or fast certificate or the
/// predecessor checkpoint's finality, and a residual record the reporter's
/// own signed vote. `only` restricts the check to hashes found missing
/// before. None while the epoch's committee is not known here.
///
/// RAI, overlap certificates: an F entry for a block that bypasses a lock
/// record of the predecessor checkpoint rests on the overlap exception, and
/// is justified only together with the exclusion witnesses of the records'
/// origins (`witnessed`): with them the entry's certificate and witnesses
/// are the overlap certificate the next closure verifies. A correct
/// reporter holds them, since it accepted the block only with them. N
/// entries and G records need no witness: BuildState leaves those that
/// bypass a surviving record out, so they cannot keep a report unusable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn unjustified(
    epoch: ConsensusEpoch,
    certified: &CertifiedState,
    residual: &ResidualVotes,
    only: Option<&[BlockHash]>,
    certificate_kinds: &dyn Fn(ConsensusEpoch, &[BlockHash]) -> Option<Vec<CertificateKinds>>,
    inherited: &dyn Fn(&CertifiedBlock, Certification) -> bool,
    reporter_votes: &dyn Fn(&[(BlockHash, ResidualKind)]) -> Vec<bool>,
    witnessed: &dyn Fn(&CertifiedBlock, Certification) -> bool,
) -> Option<Vec<BlockHash>> {
    let only: Option<HashSet<&BlockHash>> = only.map(|only| only.iter().collect());
    let wanted = |hash: &BlockHash| only.as_ref().is_none_or(|only| only.contains(hash));
    // What the predecessor checkpoint itself holds is justified by it,
    // whatever its certificates here: they are looked up for the rest only.
    // The entries of the report's inherited base are all of that kind, and
    // no unproven entry is in the finalized prefix of one: that prefix is
    // inherited too.
    let entries: Vec<(&CertifiedBlock, Certification, bool)> = certified
        .changed_entries()
        .filter(|(block, _)| wanted(&block.hash))
        .map(|(block, entry)| (block, *entry, inherited(block, *entry)))
        .collect();
    let hashes: Vec<BlockHash> = entries
        .iter()
        .filter(|(_, _, inherited)| !inherited)
        .map(|(block, _, _)| block.hash)
        .collect();
    let mut kinds = certificate_kinds(epoch, &hashes)?.into_iter();
    // The epoch's committee must be known here even when every entry is
    // inherited: one lookup tells, as the lookup of them all did
    if hashes.is_empty()
        && let Some((block, _, _)) = entries.first()
    {
        certificate_kinds(epoch, &[block.hash])?;
    }
    let mut justified: HashSet<CertifiedBlock> = HashSet::new();
    let mut anchors: Vec<CertifiedBlock> = Vec::new();
    // F entries without a proof of this epoch: finality assembled in a
    // retained earlier epoch counts too, a certificate formed after that
    // epoch's checkpoint was decided
    let mut unproven: Vec<CertifiedBlock> = Vec::new();
    let mut missing = Vec::new();
    for (block, entry, inherited) in &entries {
        // R, inherited protection, is justified by nothing else
        let inherited = *inherited;
        let kinds = if inherited {
            CertificateKinds::default()
        } else {
            kinds
                .next()
                .expect("certificate kinds for every entry not inherited")
        };
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
                if !inherited && !witnessed(block, *entry) {
                    missing.push(block.hash);
                } else if inherited || kinds.finalization || kinds.fast {
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
                .changed_entries()
                .filter(|(block, entry)| {
                    entry.status == CertifiedStatus::Finalized && !wanted(&block.hash)
                })
                .map(|(block, _)| *block),
        );
    }
    if !unproven.is_empty() {
        for anchor in anchors {
            let mut current = anchor;
            while justified.insert(current) {
                let Some(entry) = certified.certification(&current) else {
                    break;
                };
                if current.height <= 1 || entry.previous.is_zero() {
                    break;
                }
                // The parent is the entry the parent hash names, which a
                // well-placed report holds one height below on the account
                let parent =
                    CertifiedBlock::new(current.account, current.height - 1, entry.previous);
                if certified.status(&parent) != Some(CertifiedStatus::Finalized) {
                    break;
                }
                current = parent;
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
    let mut listed: HashSet<BlockHash> = missing.iter().copied().collect();
    for ((hash, _), held) in records.iter().zip(reporter_votes(&records)) {
        if !held && listed.insert(*hash) {
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
    /// Epochs held until released: an epoch's handoff evidence stays until
    /// `N - f` of its successors acknowledged a durable copy of its
    /// checkpoint; this bound only caps what a release that never comes
    /// costs, and hitting it is logged
    pub const MAX_EPOCHS: usize = 16;
    pub const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);
    pub const REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
    /// Released epochs remembered; older ones are long past any repeat
    const MAX_RELEASED: usize = 64;
    pub fn new() -> Self {
        Self {
            epochs: BTreeMap::new(),
            max_epochs: Self::MAX_EPOCHS,
            released: BTreeSet::new(),
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
        held.live.certify_all(&certified);
        held.snapshots.insert(certified_root, certified);
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
        Some((their.reconstructed.as_deref()?, their.residual.as_deref()?))
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
                    their.reconstructed.as_deref()?,
                    their.residual.as_deref()?,
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
        if !report.verify(payload) || self.released.contains(&report.epoch) {
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
        let residual = (report.residual == ResidualVotes::new().root())
            .then(|| Arc::new(ResidualVotes::new()));
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
            their.residual = Some(Arc::new(derived));
            their.residual_stream = None;
        } else if their.residual_stream.is_none() {
            // A running stream keeps its base: it must not move under it
            let decoder = Decoder::new(derived.items());
            their.residual_stream = Some(Stream::new(ReportSet::Residual, derived, decoder));
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
    #[cfg(test)]
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
        let checked = self
            .verification_jobs(epoch, now)
            .into_iter()
            .map(|job| {
                let found = job.check(&mut check);
                (job, found)
            })
            .collect();
        self.apply_verifications(epoch, now, checked)
    }

    /// RAI: the reconstructed reports `verify` would check now. The checks
    /// read the active elections and take long; a caller sharing this
    /// exchange runs them without holding it and records what they found
    /// with `apply_verifications`.
    pub fn verification_jobs(&self, epoch: ConsensusEpoch, now: Timestamp) -> Vec<VerifyJob> {
        let Some(held) = self.epochs.get(&epoch) else {
            return Vec::new();
        };
        held.theirs
            .iter()
            .filter(|(_, their)| {
                !their.verified
                    && !their.malformed
                    && their
                        .verified_at
                        .is_none_or(|at| at.elapsed(now) >= Self::RETRY_INTERVAL)
            })
            .filter_map(|(reporter, their)| {
                Some(VerifyJob {
                    reporter: *reporter,
                    certified: their.reconstructed.clone()?,
                    residual: their.residual.clone()?,
                    only: their.verified_at.is_some().then(|| their.missing.clone()),
                    verified_at: their.verified_at,
                })
            })
            .collect()
    }

    /// RAI: records what the checks of `verification_jobs` found. A job
    /// whose report changed meanwhile - reconstructed again, or checked by
    /// another caller - is dropped; a later round checks it anew.
    pub fn apply_verifications(
        &mut self,
        epoch: ConsensusEpoch,
        now: Timestamp,
        checked: Vec<(VerifyJob, Option<Verification>)>,
    ) -> Vec<(PublicKey, Vec<BlockHash>)> {
        let mut requests = Vec::new();
        let Some(held) = self.epochs.get_mut(&epoch) else {
            return requests;
        };
        for (job, found) in checked {
            let Some(their) = held.theirs.get_mut(&job.reporter) else {
                continue;
            };
            if !job.still_due(their) {
                continue;
            }
            let missing = match found {
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
                requests.push((job.reporter, missing.clone()));
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
                            .due(*epoch, report.certified, now)
                            .map(ReportMessage::Request),
                    );
                }
                if let Some(stream) = &mut their.residual_stream {
                    messages.extend(
                        stream
                            .due(*epoch, report.residual, now)
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
                // Over the base this node's live view of the epoch is laid
                // over: the inherited base, the same at every holder
                ReportSet::CertifiedChanges => {
                    Encoder::signed(held.state(request.target)?.changed_items(held.live.base()?))
                }
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
                ReportSet::Certified | ReportSet::CertifiedChanges => {
                    if their.report.certified != reply.target {
                        continue;
                    }
                    let Some(stream) = &mut their.certified_stream else {
                        continue;
                    };
                    if stream.set != reply.set || !stream.accept(reply, now) {
                        continue;
                    }
                    if stream.decoder.failed() {
                        results.push(dropped(reply, *reporter, stream.decoder.received()));
                        their.certified_stream = None;
                        continue;
                    }
                    if !stream.decoder.is_done() {
                        messages.push(ReportMessage::Request(
                            stream.request(reply.epoch, reply.target),
                        ));
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
                    their.reconstructed = Some(Arc::new(rebuilt));
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
                        messages.push(ReportMessage::Request(
                            stream.request(reply.epoch, reply.target),
                        ));
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
                    their.residual = Some(Arc::new(rebuilt));
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
            crate::utils::diagnostic!(
                "EPOCH_RETENTION_FORCED epoch={} : dropped before its release",
                oldest
            );
        }
    }

    /// RAI, durable signing records: this node's frozen report of an epoch,
    /// as persisted before a restart. It is not signed again.
    pub fn restore_signed(
        &mut self,
        epoch: ConsensusEpoch,
        signed: Vec<Report>,
        certified: CertifiedState,
        residual: ResidualVotes,
    ) {
        let held = self.epochs.entry(epoch).or_default();
        if !held.signed.is_empty() || signed.is_empty() {
            return;
        }
        let certified_root = certified.root();
        let residual_root = residual.root();
        held.live.certify_all(&certified);
        held.snapshots.insert(certified_root, certified);
        held.residuals.insert(residual_root, residual);
        held.signed = signed;
        self.trim();
    }

    /// RAI, "Retained evidence": the handoff evidence of an epoch, its
    /// frozen reports, reconstructions and symbol encoders, is released once
    /// the successors hold the checkpoint it produced
    pub fn release_epoch(&mut self, epoch: ConsensusEpoch) -> bool {
        if self.epochs.remove(&epoch).is_none() {
            return false;
        }
        self.released.insert(epoch);
        while self.released.len() > Self::MAX_RELEASED {
            self.released.pop_first();
        }
        true
    }

    /// RAI: the given entries of a reporter's reconstructed certified
    /// state, for a diagnostic
    pub fn report_entries(
        &self,
        epoch: ConsensusEpoch,
        reporter: &PublicKey,
        hashes: &[BlockHash],
    ) -> Vec<(CertifiedBlock, Certification)> {
        let Some(their) = self
            .epochs
            .get(&epoch)
            .and_then(|held| held.theirs.get(reporter))
        else {
            return Vec::new();
        };
        let Some(certified) = their.reconstructed.as_ref() else {
            return Vec::new();
        };
        certified
            .entries()
            .filter(|(block, _)| hashes.contains(&block.hash))
            .map(|(block, entry)| (*block, *entry))
            .collect()
    }

    pub fn pending_epochs(&self) -> Vec<ConsensusEpoch> {
        self.epochs.keys().copied().collect()
    }
    pub fn refresh_live(&mut self, epoch: ConsensusEpoch, live: CertifiedState) {
        if self.released.contains(&epoch) {
            return;
        }
        let held = self.epochs.entry(epoch).or_default();
        held.live.certify_all(&live);
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
            their.reconstructed = Some(Arc::new(state));
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
            // Laid over the inherited base, the live view asks for the
            // reporter's changes over it and decodes against its own
            let base = held_live.clone();
            let stream = match held_live.base() {
                Some(inherited) => Stream::new(
                    ReportSet::CertifiedChanges,
                    base.clone(),
                    Decoder::signed(base.changed_items(inherited)),
                ),
                None => {
                    let decoder = Decoder::new(base.items());
                    Stream::new(ReportSet::Certified, base, decoder)
                }
            };
            their.certified_stream = Some(stream);
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
            .filter_map(|their| their.residual.as_deref())
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
            .filter_map(|their| their.reconstructed.as_deref())
            .find(|state| state.root() == root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::{BlockIndex, CertifiedStatus};
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

    /// Two nodes whose views are laid over the same inherited base, each its
    /// own copy, reconcile the changes only: the holder encodes what the
    /// report changes over the base, a removed inherited entry included, and
    /// the requester rebuilds the report from its own changes
    #[test]
    fn reports_over_the_inherited_base_reconcile_their_changes_only() {
        let mut inherited = CertifiedState::new();
        for i in 1..=2000 {
            inherited.certify(entry(i), BlockHash::ZERO, CertifiedStatus::Finalized);
        }
        let (inherited, _) = inherited.into_report_base();
        let (base_r, base_q) = (Arc::new(inherited.clone()), Arc::new(inherited));
        let mut state = CertifiedState::over(&base_r);
        for i in 3001..=3030 {
            state.certify(entry(i), BlockHash::ZERO, CertifiedStatus::Notarized);
        }
        state.remove(&entry(5));
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            EPOCH,
            state.clone(),
            ResidualVotes::new(),
            BlockHash::from(3),
            BlockHash::from(4),
            &[PrivateKey::from(1)],
        );
        let report = reporter.own_reports(EPOCH)[0].clone();
        let mut live = CertifiedState::over(&base_q);
        for i in 3010..=3040 {
            live.certify(entry(i), BlockHash::ZERO, CertifiedStatus::Finalized);
        }
        let mut requester = ReportExchange::new();
        requester.refresh_live(EPOCH, live);
        requester.handle_report(report.clone());
        let now = Timestamp::new_test_instance();
        requester.reconcile(EPOCH, report.reporter, now);
        let requests = requester.stream_requests(now);
        assert!(requests.iter().any(|message| matches!(
            message,
            ReportMessage::Request(request) if request.set == ReportSet::CertifiedChanges
        )));
        carry(&mut requester, &mut reporter, requests, now);
        pull(&mut requester, &mut reporter, now);
        let encoded: Vec<usize> = reporter.epochs[&EPOCH]
            .encoders
            .values()
            .map(Encoder::len)
            .collect();
        assert_eq!(encoded, vec![31]);
        trust_all(&mut requester, EPOCH);
        let usable = requester.usable(EPOCH);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].1.root(), report.certified);
        assert_eq!(*usable[0].1, state);
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
            EPOCH,
            &certified,
            &residual,
            None,
            &kinds,
            &nothing,
            &no_votes,
            &|_: &CertifiedBlock, _: Certification| true,
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
            &|_: &CertifiedBlock, _: Certification| true,
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
            &|_: &CertifiedBlock, _: Certification| true,
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
            &|_: &CertifiedBlock, _: Certification| true,
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
                &votes,
                &|_: &CertifiedBlock, _: Certification| true
            ),
            Some(Vec::new())
        );
    }

    /// What the predecessor checkpoint holds needs no certificate here, so
    /// none is looked up for it; the epoch's committee must still be known
    #[test]
    fn inherited_entries_are_not_looked_up() {
        let carried = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(11));
        let fresh = CertifiedBlock::new(Account::from(2), 1, BlockHash::from(12));
        let mut certified = CertifiedState::new();
        certified.certify(carried, BlockHash::ZERO, CertifiedStatus::Finalized);
        certified.certify(fresh, BlockHash::ZERO, CertifiedStatus::Notarized);
        let looked_up = std::cell::RefCell::new(Vec::new());
        let kinds = |_: ConsensusEpoch, hashes: &[BlockHash]| {
            looked_up.borrow_mut().extend_from_slice(hashes);
            Some(vec![CertificateKinds::default(); hashes.len()])
        };
        let previously = |block: &CertifiedBlock, _: Certification| *block == carried;
        let no_votes = |votes: &[(BlockHash, ResidualKind)]| vec![false; votes.len()];
        let missing = unjustified(
            EPOCH,
            &certified,
            &ResidualVotes::new(),
            None,
            &kinds,
            &previously,
            &no_votes,
            &|_: &CertifiedBlock, _: Certification| true,
        );
        assert_eq!(missing, Some(vec![fresh.hash]));
        assert_eq!(*looked_up.borrow(), vec![fresh.hash]);

        // Every entry inherited and no committee here: not checkable yet
        let mut carried_only = CertifiedState::new();
        carried_only.certify(carried, BlockHash::ZERO, CertifiedStatus::Finalized);
        let unknown = |_: ConsensusEpoch, hashes: &[BlockHash]| (hashes.is_empty()).then(Vec::new);
        assert_eq!(
            unjustified(
                EPOCH,
                &carried_only,
                &ResidualVotes::new(),
                None,
                &unknown,
                &previously,
                &no_votes,
                &|_: &CertifiedBlock, _: Certification| true
            ),
            None
        );
    }

    /// The checks run outside the exchange: a result for a report checked
    /// or reconstructed again meanwhile is dropped, not recorded
    #[test]
    fn a_verification_overtaken_meanwhile_is_dropped() {
        let (mut reporter, report, mut requester, now) = diverged(3, 40);
        requester.reconcile(report.epoch, report.reporter, now);
        pull(&mut requester, &mut reporter, now);
        let first = requester.verification_jobs(report.epoch, now);
        let second = requester.verification_jobs(report.epoch, now);
        assert_eq!(first.len(), 1);
        let missing = |job| {
            (
                job,
                Some(Verification::Missing(vec![BlockHash::from(1001)])),
            )
        };
        let justified = |job| (job, Some(Verification::Missing(Vec::new())));
        let requests = requester.apply_verifications(
            report.epoch,
            now,
            first.into_iter().map(missing).collect(),
        );
        assert_eq!(requests.len(), 1);
        // The second job was taken before the first was recorded
        requester.apply_verifications(
            report.epoch,
            now,
            second.into_iter().map(justified).collect(),
        );
        assert!(requester.usable(report.epoch).is_empty());
        assert_eq!(requester.unverified_count(report.epoch), 1);
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

    /// Replicas that have not released the epoch yet keep repeating their
    /// reports; one arriving after the release does not bring the epoch
    /// back, nor does a refresh of its live state
    #[test]
    fn a_released_epoch_is_not_brought_back() {
        let (mut reporter, report, mut requester, now) = diverged(3, 40);
        requester.reconcile(report.epoch, report.reporter, now);
        pull(&mut requester, &mut reporter, now);
        assert!(requester.release_epoch(report.epoch));
        let repeated = reporter.repeat_reports(now + ReportExchange::REPEAT_INTERVAL);
        let ReportMessage::Broadcast(repeated) = repeated[0].clone() else {
            panic!("a repeated report");
        };
        assert!(!requester.handle_report(repeated));
        requester.refresh_live(report.epoch, CertifiedState::new());
        assert!(requester.pending_epochs().is_empty());
        assert!(requester.reports(report.epoch).is_empty());
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

    /// RAI, overlap certificates: an F entry for a block that bypasses a
    /// recovery record of the predecessor checkpoint is justified only with
    /// the exclusion witness of the record's origin, whatever certificate
    /// it has; an N entry for it needs none (BuildState leaves it out)
    #[test]
    fn an_f_entry_bypassing_a_record_needs_the_matching_origin_witness() {
        let account = Account::from(1);
        let locked = CertifiedBlock::new(account, 1, BlockHash::from(10));
        let rival = CertifiedBlock::new(account, 1, BlockHash::from(20));
        let mut previous = EpochLedger::new();
        previous.retain_recovery_for_test(
            AccountSlot::new(account, 1),
            locked.hash,
            BlockHash::ZERO,
        );
        let finalized = |_: ConsensusEpoch, hashes: &[BlockHash]| {
            Some(vec![
                CertificateKinds {
                    notarization: true,
                    finalization: true,
                    fast: false,
                };
                hashes.len()
            ])
        };
        let nothing = |_: &CertifiedBlock, _: Certification| false;
        let no_votes = |votes: &[(BlockHash, ResidualKind)]| vec![false; votes.len()];
        let residual = ResidualVotes::new();
        let check = |status: CertifiedStatus, held: bool| {
            let mut certified = CertifiedState::new();
            certified.certify(rival, BlockHash::ZERO, status);
            let witness = |origin: ConsensusEpoch, hash: &BlockHash| {
                held && origin == ConsensusEpoch::ZERO && *hash == rival.hash
            };
            unjustified(
                EPOCH,
                &certified,
                &residual,
                None,
                &finalized,
                &nothing,
                &no_votes,
                &|block, entry| {
                    previous.admits(
                        AccountSlot::new(block.account, block.height),
                        block.hash,
                        entry.previous,
                        &witness,
                    )
                },
            )
        };
        assert_eq!(
            check(CertifiedStatus::Finalized, false),
            Some(vec![rival.hash])
        );
        assert_eq!(check(CertifiedStatus::Finalized, true), Some(vec![]));
        assert_eq!(check(CertifiedStatus::Notarized, false), Some(vec![]));
    }

    /// Fix B: a G record for a hash T tags N or F is malformed; one for a
    /// hash T tags R is the re-vote of an inherited recovery lock, and the
    /// report stays usable
    #[test]
    fn a_g_record_may_name_an_r_tag_but_not_an_n_or_f_tag() {
        let block = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(5));
        let mut residual = ResidualVotes::new();
        residual.record(block, BlockHash::ZERO, ResidualKind::First);
        for (status, usable) in [
            (CertifiedStatus::Recovery, true),
            (CertifiedStatus::Notarized, false),
            (CertifiedStatus::Finalized, false),
        ] {
            let mut certified = CertifiedState::new();
            certified.certify(block, BlockHash::ZERO, status);
            assert_eq!(
                well_formed(&certified, &residual, None),
                usable,
                "{status:?}"
            );
        }
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

    /// Reports laid over the inherited base that projects the predecessor
    /// checkpoint give every check and the derivation exactly what the same
    /// reports held flat give: they only skip what the base holds. Random
    /// forked trees, two closures: the first builds the predecessor, the
    /// second compares.
    #[test]
    fn reports_over_the_inherited_base_derive_as_flat_reports() {
        use crate::consensus::election::{
            BuildRules, ReportIndex, SelectedReport, build_state, fresh_finalized, witness_claims,
        };
        let (mut compared, mut locked) = (0, 0);
        for seed in 1..400u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let tree = random_tree(&mut rng);
            let chains = true_chains(&mut rng, &tree);
            let epoch1 = ConsensusEpoch::new(1);
            let epoch2 = ConsensusEpoch::new(2);
            let witness = |epoch: ConsensusEpoch, hash: &BlockHash| {
                (hash.as_bytes()[31] as u64 + epoch.as_u64()) % 3 == 0
            };

            let first: Vec<(CertifiedState, ResidualVotes)> = (0..4)
                .map(|_| random_report(&mut rng, &tree, &chains, 0, CertifiedState::new()))
                .collect();
            let selection = selected(&first);
            let index = ReportIndex::new(&EpochLedger::new(), &selection);
            let Ok(previous) = build_state(
                &EpochLedger::new(),
                &selection,
                &index,
                BuildRules { epoch: epoch1 },
                &witness,
            ) else {
                continue;
            };

            locked += previous
                .report_ledger()
                .entries()
                .any(|(_, entry)| entry.status == CertifiedStatus::Notarized)
                as usize;
            let (base, _) = previous.report_ledger().into_report_base();
            let base = Arc::new(base);
            let over: Vec<(CertifiedState, ResidualVotes)> = (0..4)
                .map(|_| random_report(&mut rng, &tree, &chains, 1, CertifiedState::over(&base)))
                .collect();
            let flat: Vec<(CertifiedState, ResidualVotes)> = over
                .iter()
                .map(|(certified, residual)| (certified.flattened(), residual.clone()))
                .collect();
            let (over_selection, flat_selection) = (selected(&over), selected(&flat));
            let over_index = ReportIndex::new(&previous, &over_selection);
            let flat_index = ReportIndex::new(&previous, &flat_selection);
            for hash in tree.keys() {
                assert_eq!(over_index.placement(hash), flat_index.placement(hash));
            }
            let derive = |selection: &[SelectedReport], index: &ReportIndex| {
                build_state(
                    &previous,
                    selection,
                    index,
                    BuildRules { epoch: epoch2 },
                    &witness,
                )
            };
            let derived = derive(&over_selection, &over_index);
            assert_eq!(derived, derive(&flat_selection, &flat_index), "seed {seed}");
            compared += derived.is_ok() as usize;
            assert_eq!(
                witness_claims(&previous, &over_selection, &over_index),
                witness_claims(&previous, &flat_selection, &flat_index)
            );
            assert_eq!(
                fresh_finalized(&over_selection, &previous),
                fresh_finalized(&flat_selection, &previous)
            );
            let inherited = |block: &CertifiedBlock, entry| {
                report_service::inherited_from(&previous, block, entry)
            };
            assert_eq!(
                manifest_claims(epoch2, &over_selection, &inherited),
                manifest_claims(epoch2, &flat_selection, &inherited)
            );
            for ((over, residual), (flat, _)) in over.iter().zip(&flat) {
                assert_eq!(
                    well_formed(over, residual, Some(&previous)),
                    well_formed(flat, residual, Some(&previous))
                );
                let check = |certified: &CertifiedState, only: Option<&[BlockHash]>| {
                    unjustified(
                        epoch2,
                        certified,
                        residual,
                        only,
                        &|epoch, hashes| {
                            Some(
                                hashes
                                    .iter()
                                    .map(|hash| {
                                        let byte = hash.as_bytes()[31] as u64 + epoch.as_u64();
                                        CertificateKinds {
                                            notarization: byte % 2 == 0,
                                            finalization: byte % 5 == 0,
                                            fast: byte % 7 == 0,
                                        }
                                    })
                                    .collect(),
                            )
                        },
                        &inherited,
                        &|votes| {
                            votes
                                .iter()
                                .map(|(hash, _)| hash.as_bytes()[30] % 2 == 0)
                                .collect()
                        },
                        &|block, _| block.hash.as_bytes()[31] % 4 != 0,
                    )
                    .map(|mut missing| {
                        missing.sort();
                        missing
                    })
                };
                assert_eq!(check(over, None), check(flat, None), "seed {seed}");
                let only: Vec<BlockHash> = tree.keys().copied().step_by(2).collect();
                assert_eq!(
                    check(over, Some(&only)),
                    check(flat, Some(&only)),
                    "seed {seed}"
                );
            }
        }
        assert!(compared > 50, "only {compared} derivations compared");
        assert!(
            locked > 20,
            "only {locked} predecessors with notarization locks"
        );
    }

    /// The cost of an epoch's report steps against the history the
    /// inherited base holds, flat and laid over the base, 15,000 changes an
    /// epoch. Not a check: run with
    /// `cargo test --release --lib --features rai_protocol -p rsnano_node -- --ignored report_cost --nocapture`
    #[test]
    #[ignore]
    fn report_cost_against_history() {
        use std::time::Instant;
        let changes = 15_000u64;
        println!("history   step                     flat ms   over ms");
        for history in [45_000u64, 450_000, 2_000_000] {
            let mut inherited = CertifiedState::new();
            for i in 1..=history {
                inherited.certify(entry(i), BlockHash::ZERO, CertifiedStatus::Finalized);
            }
            let (inherited, _) = inherited.into_report_base();
            let base = Arc::new(inherited);
            let epoch_entries = |state: &mut CertifiedState| {
                for i in history + 1..=history + changes {
                    state.certify(entry(i), BlockHash::ZERO, CertifiedStatus::Finalized);
                }
                state.project_final_prefixes();
            };
            let started = Instant::now();
            let mut flat = (*base).clone();
            epoch_entries(&mut flat);
            let flat_build = started.elapsed();
            let started = Instant::now();
            let mut over = CertifiedState::over(&base);
            epoch_entries(&mut over);
            let over_build = started.elapsed();
            assert_eq!(flat.root(), over.root());

            let started = Instant::now();
            let mut live = (*base).clone();
            live.certify_all(&flat);
            let flat_merge = started.elapsed();
            let started = Instant::now();
            let mut live = CertifiedState::over(&base);
            live.certify_all(&over);
            let over_merge = started.elapsed();

            let started = Instant::now();
            let mut encoder = Encoder::new(flat.items());
            encoder.symbols(0, 64);
            let flat_encode = started.elapsed();
            let started = Instant::now();
            let mut encoder = Encoder::signed(over.changed_items(&base));
            encoder.symbols(0, 64);
            let over_encode = started.elapsed();

            let started = Instant::now();
            let flat_checked = flat.changed_entries().count();
            let flat_scan = started.elapsed();
            let started = Instant::now();
            let over_checked = over.changed_entries().count();
            let over_scan = started.elapsed();

            let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
            for (step, flat, over) in [
                ("build the report", ms(flat_build), ms(over_build)),
                ("merge into the live view", ms(flat_merge), ms(over_merge)),
                ("encode for a stream", ms(flat_encode), ms(over_encode)),
                ("entries a check reads", ms(flat_scan), ms(over_scan)),
            ] {
                println!("{history:>9} {step:<24} {flat:>9.1} {over:>9.1}");
            }
            println!(
                "{history:>9} {:<24} {:>9} {:>9}",
                "entries checked / kB", flat_checked, over_checked
            );
            println!(
                "{history:>9} {:<24} {:>9} {:>9}",
                "persisted kB",
                flat.len() * 105 / 1000,
                over.changed_items(&base).len() * 106 / 1000
            );
        }
    }

    /// Every entry counts as justified: the evidence check is tested apart
    fn trust_all(exchange: &mut ReportExchange, epoch: ConsensusEpoch) {
        exchange.verify(epoch, Timestamp::new_test_instance(), |_, _, _, _| {
            Some(Verification::Missing(Vec::new()))
        });
    }

    /* Test helpers */

    const EPOCH: ConsensusEpoch = ConsensusEpoch::new(1);

    /// A deterministic xorshift generator
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, bound: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % bound
        }
    }

    /// Four accounts, four heights, one or two blocks at each position, each
    /// continuing one of the blocks below it: forks with descendants
    fn random_tree(rng: &mut Rng) -> BTreeMap<BlockHash, (CertifiedBlock, BlockHash)> {
        let mut tree = BTreeMap::new();
        let mut next = 1u64;
        for account in 1..=4u64 {
            let mut below: Vec<BlockHash> = vec![BlockHash::ZERO];
            for height in 1..=4u64 {
                let mut here = Vec::new();
                for _ in 0..1 + rng.below(2) {
                    let previous = below[rng.below(below.len() as u64) as usize];
                    let hash = BlockHash::from(next * 1_000_003);
                    next += 1;
                    tree.insert(
                        hash,
                        (
                            CertifiedBlock::new(Account::from(account), height, hash),
                            previous,
                        ),
                    );
                    here.push(hash);
                }
                below = here;
            }
        }
        tree
    }

    /// The branch every account's finality follows: from a block at the top
    /// height down its parents
    fn true_chains(
        rng: &mut Rng,
        tree: &BTreeMap<BlockHash, (CertifiedBlock, BlockHash)>,
    ) -> Vec<Vec<(CertifiedBlock, BlockHash)>> {
        (1..=4u64)
            .map(|account| {
                let tops: Vec<_> = tree
                    .values()
                    .filter(|(block, _)| {
                        block.account == Account::from(account) && block.height == 4
                    })
                    .collect();
                let mut chain = Vec::new();
                let mut current = *tops[rng.below(tops.len() as u64) as usize];
                loop {
                    chain.push(current);
                    let Some(parent) = tree.get(&current.1) else {
                        break;
                    };
                    current = *parent;
                }
                chain.reverse();
                chain
            })
            .collect()
    }

    /// A correct reporter's certified state, starting from `start`: the true
    /// chain of every account finalized up to a height at least `floor`, the
    /// block above notarized or not; and its first votes, on any branch,
    /// for what that state does not summarize
    fn random_report(
        rng: &mut Rng,
        tree: &BTreeMap<BlockHash, (CertifiedBlock, BlockHash)>,
        chains: &[Vec<(CertifiedBlock, BlockHash)>],
        floor: u64,
        start: CertifiedState,
    ) -> (CertifiedState, ResidualVotes) {
        let mut certified = start;
        for chain in chains {
            let finalized = floor + rng.below(5 - floor);
            for (block, previous) in chain.iter().take(finalized as usize) {
                certified.certify(*block, *previous, CertifiedStatus::Finalized);
            }
            // Notarized: the block above the finalized tip, or one with a gap
            // below it, which no promotion turns into finality
            let above = finalized + rng.below(2);
            if let Some((block, previous)) = chain.get(above as usize)
                && rng.below(3) != 0
            {
                certified.certify(*block, *previous, CertifiedStatus::Notarized);
            }
            // Rarely, a rival notarized at a position of the chain
            if rng.below(6) == 0 {
                let height = 1 + rng.below(4);
                let rivals: Vec<_> = tree
                    .values()
                    .filter(|(block, _)| {
                        block.account == chain[0].0.account
                            && block.height == height
                            && block.hash != chain[height as usize - 1].0.hash
                    })
                    .collect();
                if let Some((block, previous)) = rivals.first() {
                    certified.certify(*block, *previous, CertifiedStatus::Notarized);
                }
            }
        }
        // Rarely, a reconstruction that lost an entry of the base
        if rng.below(8) == 0
            && let Some((block, _)) = certified.entries().nth(rng.below(4) as usize)
        {
            let block = *block;
            certified.remove(&block);
        }
        certified.project_final_prefixes();
        let mut residual = ResidualVotes::new();
        for (block, previous) in tree.values() {
            if !certified.contains_hash(&block.hash) && rng.below(3) == 0 {
                residual.record(*block, *previous, ResidualKind::First);
            }
        }
        (certified, residual)
    }

    fn selected(reports: &[(CertifiedState, ResidualVotes)]) -> Vec<SelectedReport<'_>> {
        reports
            .iter()
            .enumerate()
            .map(|(i, (certified, residual))| SelectedReport {
                reporter: PrivateKey::from(i as u64 + 1).public_key(),
                weight: rsnano_types::Amount::raw(10),
                certified,
                residual,
            })
            .collect()
    }
}
