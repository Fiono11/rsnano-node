use std::collections::{BTreeMap, BTreeSet};

use rsnano_types::{Account, Amount, Blake2HashBuilder, BlockHash, PublicKey};

use super::{CertifiedState, ResidualVotes};

/// A position in an account forest. The slot follows from the parent: a block
/// whose parent sits at slot v-1 is at slot v.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountSlot {
    pub account: Account,
    pub height: u64,
}

impl AccountSlot {
    pub fn new(account: Account, height: u64) -> Self {
        Self { account, height }
    }
}

/// Where a block sits and which block it follows
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockPlacement {
    pub slot: AccountSlot,
    /// The parent account block; zero for the first block of an account
    pub previous: BlockHash,
}

/// A block at a slot with the branch it continues. Two blocks at one account
/// slot conflict, and the parent each names is what says which branch it
/// belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlacedBlock {
    pub hash: BlockHash,
    pub previous: BlockHash,
}

impl PlacedBlock {
    pub fn new(hash: BlockHash, previous: BlockHash) -> Self {
        Self { hash, previous }
    }
}

/// What the derivation needs to know about a block body. Rule 2 of the
/// derivation asks for a full body with valid owner authentication, a valid
/// parent chain and valid external dependencies; a block this can not place
/// is one the deriving validator does not hold, and it is not included.
pub trait BlockIndex {
    fn placement(&self, hash: &BlockHash) -> Option<BlockPlacement>;
}

/// RAI: one of the N-f reports an epoch proposal selects, as the deriving
/// validator reconstructed it
#[derive(Clone, Copy, Debug)]
pub struct SelectedReport<'a> {
    /// The reporting validator. `M_Q` and `FirstCount_Q` count a reporter
    /// once however many votes it recorded, so the identity is needed to
    /// tell two reports apart.
    pub reporter: PublicKey,
    /// Its weight in the committee the reports are counted in. The thresholds
    /// of this implementation are weights rather than validator counts, so
    /// `f + p + 1` is a weight here too.
    pub weight: Amount,
    pub certified: &'a CertifiedState,
    pub residual: &'a ResidualVotes,
}

/// RAI: the decided state of an epoch, `S_e = (L_e, Sigma_e)`. `L_e` is the
/// checkpoint ledger: the finalized block at each account slot, and the
/// conflicting blocks a slot kept without application effect. `Sigma_e` is
/// the replay of the finalized blocks alone, which here is the finalized
/// frontier of every account: what the committee of the epoch is derived
/// from, and what never rolls back.
/// Checkpoint retention is not a report N/F tag and never implies finality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RetainedKind {
    Ancestor = 0,
    Notarized = 2,
    Recovery = 3,
}

/// RAI: how a checkpoint position came to be final. Certificate-backed
/// finality is the paper's; derived finality exists only under the
/// experimental unique-branch variant and is never equivalent to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalityOrigin {
    /// Inherited or exposed by an explicit FC or FF
    Certificate,
    /// Checkpoint-finalized as the unique preserved branch (variant)
    Derived,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpochLedger {
    /// The finalized block at a slot, by account finalization or by the
    /// epoch decision
    finalized: BTreeMap<AccountSlot, PlacedBlock>,
    /// The finalized positions the unique-branch variant decided without a
    /// certificate: part of the versioned commitment, so that a state built
    /// under the variant never hashes like one built under the paper's rule
    derived: BTreeSet<AccountSlot>,
    /// The conflicting blocks a slot kept: checkpoint-notarized, provisional,
    /// with no application effect
    notarized: BTreeMap<AccountSlot, BTreeSet<PlacedBlock>>,
    locks: BTreeMap<BlockHash, RetainedKind>,
}

impl EpochLedger {
    /// Canonical inherited report projection. Uncertified selected ancestors
    /// retain their predecessor protection; they never acquire an NC here.
    pub fn report_ledger(&self) -> CertifiedState {
        use super::{CertifiedBlock, CertifiedStatus};
        let mut report = CertifiedState::new();
        for (slot, block) in &self.finalized {
            report.certify(
                CertifiedBlock::new(slot.account, slot.height, block.hash),
                block.previous,
                CertifiedStatus::Finalized,
            );
        }
        for (slot, blocks) in &self.notarized {
            for block in blocks {
                let status = match self.retained_kind(&block.hash) {
                    RetainedKind::Notarized => CertifiedStatus::Notarized,
                    RetainedKind::Recovery | RetainedKind::Ancestor => CertifiedStatus::Recovery,
                };
                report.certify(
                    CertifiedBlock::new(slot.account, slot.height, block.hash),
                    block.previous,
                    status,
                );
            }
        }
        report
    }

    pub fn retains(&self, slot: &AccountSlot) -> bool {
        self.notarized.contains_key(slot)
    }

    pub fn is_locked(&self, slot: &AccountSlot, hash: &BlockHash) -> bool {
        self.notarized
            .get(slot)
            .is_some_and(|blocks| blocks.iter().any(|b| b.hash == *hash))
    }

    pub fn valid_recovery_entry(
        &self,
        slot: &AccountSlot,
        hash: BlockHash,
        previous: BlockHash,
    ) -> bool {
        self.retained_kind(&hash) != RetainedKind::Notarized
            && self
                .notarized
                .get(slot)
                .is_some_and(|blocks| blocks.contains(&PlacedBlock::new(hash, previous)))
    }

    pub fn retained_depth(&self, account: Account) -> Option<u64> {
        self.notarized
            .range(AccountSlot::new(account, 0)..=AccountSlot::new(account, u64::MAX))
            .next_back()
            .map(|(s, _)| s.height)
    }

    /// Only maximum-depth retained tips may receive a fresh resolving child.
    pub fn locks(&self) -> impl Iterator<Item = (&AccountSlot, BlockHash)> {
        self.notarized
            .iter()
            .filter(|(slot, _)| self.retained_depth(slot.account) == Some(slot.height))
            .flat_map(|(slot, blocks)| blocks.iter().map(move |b| (slot, b.hash)))
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Canonical checkpoint records with their wire status: 1 finalized by
    /// certificate or inherited, 4 finalized by the unique-branch variant,
    /// 2 a represented notarization lock, 3 a recovery lock, 0 retained
    /// ancestry of a lock. Unlike reports, these include inherited history.
    pub fn checkpoint_records(&self) -> Vec<(AccountSlot, PlacedBlock, u8)> {
        self.finalized
            .iter()
            .map(|(s, b)| (*s, *b, if self.derived.contains(s) { 4 } else { 1 }))
            .chain(self.notarized.iter().flat_map(|(s, bs)| {
                bs.iter()
                    .map(move |b| (*s, *b, self.retained_kind(&b.hash) as u8))
            }))
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn checkpoint_entries(&self) -> Vec<rsnano_messages::CertifiedEntry> {
        self.checkpoint_records()
            .into_iter()
            .map(|(slot, block, status)| rsnano_messages::CertifiedEntry {
                account: slot.account,
                height: slot.height,
                hash: block.hash,
                previous: block.previous,
                status,
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn from_checkpoint_entries(
        entries: &[rsnano_messages::CertifiedEntry],
    ) -> Option<Self> {
        let mut state = Self::new();
        for e in entries {
            if e.height == 0 || e.hash.is_zero() || e.status > 4 {
                return None;
            }
            let slot = AccountSlot::new(e.account, e.height);
            let block = PlacedBlock::new(e.hash, e.previous);
            if e.status == 1 || e.status == 4 {
                if state.finalized.insert(slot, block).is_some()
                    || state.notarized.contains_key(&slot)
                {
                    return None;
                }
                if e.status == 4 {
                    state.derived.insert(slot);
                }
            } else {
                if state.finalized.contains_key(&slot)
                    || !state.notarized.entry(slot).or_default().insert(block)
                {
                    return None;
                }
                let kind = match e.status {
                    0 => RetainedKind::Ancestor,
                    2 => RetainedKind::Notarized,
                    3 => RetainedKind::Recovery,
                    _ => return None,
                };
                if kind != RetainedKind::Ancestor {
                    state.locks.insert(e.hash, kind);
                }
            }
        }
        Some(state)
    }

    pub fn retained_kind(&self, hash: &BlockHash) -> RetainedKind {
        self.locks
            .get(hash)
            .copied()
            .unwrap_or(RetainedKind::Ancestor)
    }

    pub fn finalized(&self, slot: &AccountSlot) -> Option<BlockHash> {
        self.finalized.get(slot).map(|block| block.hash)
    }

    pub fn is_finalized(&self, slot: &AccountSlot, hash: &BlockHash) -> bool {
        self.finalized(slot) == Some(*hash)
    }

    /// The blocks a slot kept without deciding between them
    pub fn notarized(&self, slot: &AccountSlot) -> Vec<BlockHash> {
        self.notarized
            .get(slot)
            .map(|blocks| blocks.iter().map(|block| block.hash).collect())
            .unwrap_or_default()
    }

    pub fn finalized_slots(&self) -> impl Iterator<Item = (&AccountSlot, &PlacedBlock)> {
        self.finalized.iter()
    }

    /// Where every block this state holds sits: what places an inherited
    /// provisional block, which the current reports need not mention at all
    pub fn placements(&self) -> impl Iterator<Item = (BlockHash, BlockPlacement)> + '_ {
        let finalized = self.finalized.iter().map(|(slot, block)| (slot, block));
        let notarized = self
            .notarized
            .iter()
            .flat_map(|(slot, blocks)| blocks.iter().map(move |block| (slot, block)));
        finalized.chain(notarized).map(|(slot, block)| {
            (
                block.hash,
                BlockPlacement {
                    slot: *slot,
                    previous: block.previous,
                },
            )
        })
    }

    pub fn finalized_count(&self) -> usize {
        self.finalized.len()
    }

    /// Every retained (unresolved) block with its position, parents before
    /// children: the branches a ledger must hold to follow this checkpoint
    pub fn retained_blocks(&self) -> Vec<(AccountSlot, PlacedBlock)> {
        self.notarized
            .iter()
            .flat_map(|(slot, blocks)| blocks.iter().map(move |block| (*slot, *block)))
            .collect()
    }

    /// Whether the checkpoint retains this block on an unresolved branch
    pub fn retains_block(&self, hash: &BlockHash) -> bool {
        self.notarized
            .values()
            .any(|blocks| blocks.iter().any(|block| block.hash == *hash))
    }

    /// The finalized positions the unique-branch variant decided
    pub fn derived_count(&self) -> usize {
        self.derived.len()
    }

    /// How a finalized position came to be final, if it is
    pub fn finality_origin(&self, slot: &AccountSlot) -> Option<FinalityOrigin> {
        self.finalized.get(slot)?;
        Some(if self.derived.contains(slot) {
            FinalityOrigin::Derived
        } else {
            FinalityOrigin::Certificate
        })
    }

    pub fn notarized_count(&self) -> usize {
        self.notarized.values().map(|hashes| hashes.len()).sum()
    }

    /// Sigma_e: the finalized frontier of every account, which is the replay
    /// of its finalized blocks in parent order.
    ///
    /// The frontier is the top of the account's *contiguous* finalized chain,
    /// so it stops below the first slot the epoch did not decide. Taking the
    /// highest finalized slot instead would skip over an undecided one and
    /// count a balance the account reached through blocks the state does not
    /// hold: the account's sends below the gap would not have reduced it
    /// while the receivers of those sends counted the coins, which is the
    /// same money counted twice. Weights are derived from this, and they sum
    /// to the whole supply when every account is counted once, so there is no
    /// headroom for counting anything twice.
    pub fn frontiers(&self) -> BTreeMap<Account, (u64, BlockHash)> {
        let mut frontiers: BTreeMap<Account, (u64, BlockHash)> = BTreeMap::new();
        for (slot, block) in &self.finalized {
            let entry = frontiers
                .entry(slot.account)
                .or_insert((0, BlockHash::ZERO));
            // The finalized slots of an account are walked in height order,
            // so the frontier advances only while the chain has no gap
            if slot.height == entry.0 + 1 {
                *entry = (slot.height, block.hash);
            }
        }
        frontiers.retain(|_, (height, _)| *height > 0);
        frontiers
    }

    /// `d_e`: the hash an epoch proposal carries. Two validators that derived
    /// the same state from the same reports obtain the same hash.
    pub fn state_hash(&self) -> BlockHash {
        let mut builder = Blake2HashBuilder::new().update(b"RAI epoch state v2 locks");
        for (slot, block) in &self.finalized {
            builder = builder
                .update(b"f")
                .update(slot.account.as_bytes())
                .update(slot.height.to_le_bytes())
                .update(block.hash.as_bytes())
                .update(block.previous.as_bytes());
        }
        for (slot, blocks) in &self.notarized {
            for block in blocks {
                builder = builder
                    .update([self.retained_kind(&block.hash) as u8])
                    .update(slot.account.as_bytes())
                    .update(slot.height.to_le_bytes())
                    .update(block.hash.as_bytes())
                    .update(block.previous.as_bytes());
            }
        }
        for slot in &self.derived {
            builder = builder
                .update(b"d")
                .update(slot.account.as_bytes())
                .update(slot.height.to_le_bytes());
        }
        builder.build()
    }

    /// RAI: seeds the closed genesis state `S_G`: every account at the
    /// block it stood at when the epochs started. Those positions are
    /// finalized, and rule 1 never rolls them back.
    pub fn finalize_genesis(&mut self, slot: AccountSlot, hash: BlockHash) {
        self.finalize(slot, PlacedBlock::new(hash, BlockHash::ZERO));
    }

    /// Trusted genesis contains the complete finalized prefix, not synthetic
    /// frontier records with missing parents. Closure may encounter its blocks
    /// again in reports collected during benchmark setup.
    pub fn finalize_genesis_block(&mut self, slot: AccountSlot, block: PlacedBlock) {
        self.finalize(slot, block);
    }

    fn finalize(&mut self, slot: AccountSlot, block: PlacedBlock) {
        self.finalized.insert(slot, block);
        self.derived.remove(&slot);
        // A finalized position keeps no conflicting survivor
        if let Some(blocks) = self.notarized.remove(&slot) {
            for block in blocks {
                self.locks.remove(&block.hash);
            }
        }
    }

    /// The unique-branch variant: finalized without a certificate
    fn finalize_derived(&mut self, slot: AccountSlot, block: PlacedBlock) {
        self.finalize(slot, block);
        self.derived.insert(slot);
    }

    fn keep(&mut self, slot: AccountSlot, block: PlacedBlock) {
        self.notarized.entry(slot).or_default().insert(block);
    }
}

/// RAI: where every candidate of an epoch derivation sits. A report's
/// certified inventory and its residual object carry the account, the height
/// and the parent of every block they name - "The inventory includes the
/// required account ancestry" - so two validators that reconstructed the
/// same reports place the same blocks whether or not each holds every body.
/// That is what makes `BuildState` a function of `(S_{e-1}, Q_e)` alone, as
/// the derivation requires: a validator that happened to be missing a body
/// would otherwise derive a different state and be unable to vote.
///
/// The predecessor's own blocks are placed too: an inherited provisional
/// fork is a candidate whether or not the current reports mention it.
pub struct ReportIndex {
    placements: BTreeMap<BlockHash, BlockPlacement>,
}

impl ReportIndex {
    pub fn new(previous: &EpochLedger, selection: &[SelectedReport]) -> Self {
        let mut placements: BTreeMap<BlockHash, BlockPlacement> = BTreeMap::new();
        for (hash, placement) in previous.placements() {
            placements.insert(hash, placement);
        }
        for report in selection {
            for (block, entry) in report.certified.entries() {
                placements.entry(block.hash).or_insert(BlockPlacement {
                    slot: AccountSlot::new(block.account, block.height),
                    previous: entry.previous,
                });
            }
            for (block, _, previous) in report.residual.entries() {
                placements.entry(block.hash).or_insert(BlockPlacement {
                    slot: AccountSlot::new(block.account, block.height),
                    previous,
                });
            }
        }
        Self { placements }
    }
}

impl BlockIndex for ReportIndex {
    fn placement(&self, hash: &BlockHash) -> Option<BlockPlacement> {
        self.placements.get(hash).copied()
    }
}

/// Structural errors in closure inputs. Cryptographic evidence and application
/// validity must be checked by the caller before a selected report is usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildStateError {
    MissingAncestry,
    InvalidAncestry,
    ConflictingFinality,
    ConflictingNotarizations,
    RepeatedReporter,
    InvalidRecoveryEntry,
}

/// RAI, Rule 3: whether a represented epoch-e NC is predecessor-backed,
/// "at least q members of K_{e-1} are among B's epoch-e first voters". Read
/// off the signed votes the deriving validator retains. Correct validators
/// converge on it as the finite vote set of the epoch reaches them; until
/// then a validator that lacks the votes refuses a value that supersedes,
/// and keeps collecting.
pub trait PredecessorBacking {
    fn predecessor_backed(&self, hash: &BlockHash) -> bool;
}

/// No represented NC is predecessor-backed: Rule 3 never supersedes
impl PredecessorBacking for () {
    fn predecessor_backed(&self, _: &BlockHash) -> bool {
        false
    }
}

impl PredecessorBacking for BTreeSet<BlockHash> {
    fn predecessor_backed(&self, hash: &BlockHash) -> bool {
        self.contains(hash)
    }
}

/// The checkpoint-finalization rule (see CHECKPOINT-FINALIZATION-VARIANT.md).
/// `CertificateOnly` is the 2026-09-24 R/N/F PDF's rule; `UniqueBranch` the
/// requested experimental variant, which finalizes any sole preserved branch
/// and is unsafe with the overlap exceptions; `NotarizedUniquePrefix` the
/// EuroSys manuscript's Rule 3, which promotes only a uniquely retained
/// prefix whose every block holds a verified closing-epoch NC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CheckpointFinalization {
    #[default]
    CertificateOnly,
    UniqueBranch,
    NotarizedUniquePrefix,
}

impl CheckpointFinalization {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CertificateOnly => "certificate_only",
            Self::UniqueBranch => "unique_branch",
            Self::NotarizedUniquePrefix => "notarized_unique_prefix",
        }
    }

    pub fn parse(name: &str) -> Self {
        match name {
            "unique_branch" => Self::UniqueBranch,
            "notarized_unique_prefix" => Self::NotarizedUniquePrefix,
            _ => Self::CertificateOnly,
        }
    }
}

/// What BuildState needs besides the predecessor and the selected reports
#[derive(Clone, Copy)]
pub struct BuildRules<'a> {
    /// r = f + p + 1 as weight: the reporter first votes a recovery lock needs
    pub many: Amount,
    /// Rule 3
    pub backing: &'a dyn PredecessorBacking,
    pub finalization: CheckpointFinalization,
}

/// Revised BuildState: explicit F entries alone extend finality. A represented
/// NC or enough reporter first votes retain a parent-closed lock, without any
/// application effect. Inherited unresolved branches survive report omission.
/// No fixed-point or sole-survivor finalization is performed unless the
/// experimental variant is selected.
pub fn build_state(
    previous: &EpochLedger,
    selection: &[SelectedReport],
    index: &dyn BlockIndex,
    rules: BuildRules,
) -> Result<EpochLedger, BuildStateError> {
    let many = rules.many;
    #[derive(Default)]
    struct Evidence {
        notarized: BTreeSet<BlockHash>,
        finalized: BTreeSet<BlockHash>,
        first: BTreeMap<BlockHash, Amount>,
    }
    let mut evidence: BTreeMap<AccountSlot, Evidence> = BTreeMap::new();
    let mut reporters = BTreeSet::new();
    for report in selection {
        if !reporters.insert(report.reporter) {
            return Err(BuildStateError::RepeatedReporter);
        }
        for (block, entry) in report.certified.entries() {
            let slot = AccountSlot::new(block.account, block.height);
            if index.placement(&block.hash)
                != Some(BlockPlacement {
                    slot,
                    previous: entry.previous,
                })
            {
                return Err(BuildStateError::InvalidAncestry);
            }
            let at = evidence.entry(slot).or_default();
            match entry.status {
                super::CertifiedStatus::Finalized => {
                    at.finalized.insert(block.hash);
                }
                super::CertifiedStatus::Notarized => {
                    at.notarized.insert(block.hash);
                }
                super::CertifiedStatus::Recovery => {
                    if !previous.valid_recovery_entry(&slot, block.hash, entry.previous) {
                        return Err(BuildStateError::InvalidRecoveryEntry);
                    }
                    // Already carried by `previous`; R supplies no new support.
                }
            }
        }
        // A reporter is counted only for its own first vote outside T_i.
        // The residual set is canonical, and the identity is counted once.
        for block in report.residual.first_votes() {
            if report.certified.contains_hash(&block.hash) {
                continue;
            }
            let at = evidence
                .entry(AccountSlot::new(block.account, block.height))
                .or_default();
            let weight = at.first.entry(block.hash).or_insert(Amount::ZERO);
            *weight = weight.checked_add(report.weight).unwrap_or(Amount::MAX);
        }
    }
    let mut ledger = previous.clone();
    for (slot, at) in &evidence {
        for hash in &at.finalized {
            let path = selected_path(&ledger, index, *slot, *hash)?;
            for (slot, block) in path.into_iter().rev() {
                ledger.finalize(slot, block);
            }
        }
    }
    // Rule 3: the inherited recovery-only locks a predecessor-backed
    // represented NC superseded, with every retained descendant of theirs
    let mut superseded: BTreeSet<BlockHash> = BTreeSet::new();
    for (slot, at) in &evidence {
        if ledger.finalized(slot).is_some() {
            continue;
        }
        let represented: Vec<_> = at.notarized.iter().copied().collect();
        let target = match represented.as_slice() {
            [hash] => Some((*hash, RetainedKind::Notarized)),
            [] => {
                let recovered: Vec<_> = at
                    .first
                    .iter()
                    .filter(|(_, weight)| **weight >= many)
                    .map(|(hash, _)| *hash)
                    .collect();
                match recovered.as_slice() {
                    [hash] => Some((*hash, RetainedKind::Recovery)),
                    _ => None,
                }
            }
            _ => return Err(BuildStateError::ConflictingNotarizations),
        };
        if let Some((hash, kind)) = target {
            // Rule 3: "If S_{e-1} carries only recovery-only locks at (a, v)
            // and a selected report exposes a represented epoch-e NC for a
            // different block at (a, v) that is predecessor-backed, the
            // inherited recovery lock is dropped and Rule 1 applies. A
            // represented NC that is not predecessor-backed does not
            // supersede an inherited lock; the inherited lock is retained
            // and the new block is ineligible early work."
            let inherited = previous.notarized(slot);
            if kind == RetainedKind::Notarized
                && !inherited.is_empty()
                && !inherited.contains(&hash)
                && inherited
                    .iter()
                    .all(|held| previous.retained_kind(held) != RetainedKind::Notarized)
            {
                if !rules.backing.predecessor_backed(&hash) {
                    continue;
                }
                for held in inherited {
                    superseded.insert(held);
                    ledger.locks.remove(&held);
                }
                ledger
                    .notarized
                    .get_mut(slot)
                    .unwrap()
                    .retain(|block| !superseded.contains(&block.hash));
            }
            match selected_path(&ledger, index, *slot, hash) {
                Ok(path) => {
                    for (slot, block) in path.into_iter().rev() {
                        ledger.keep(slot, block);
                    }
                    // Do not weaken inherited represented notarization.
                    if ledger.retained_kind(&hash) != RetainedKind::Notarized {
                        ledger.locks.insert(hash, kind);
                    }
                }
                // Explicit finality excludes an incompatible nonfinal branch.
                Err(BuildStateError::ConflictingFinality) => {}
                Err(error) => return Err(error),
            }
        }
    }
    // A retained descendant of a superseded lock goes with it: slots are
    // walked in account and height order, so a parent is seen first
    if !superseded.is_empty() {
        for blocks in ledger.notarized.values_mut() {
            let gone: Vec<PlacedBlock> = blocks
                .iter()
                .filter(|block| superseded.contains(&block.previous))
                .copied()
                .collect();
            for block in gone {
                superseded.insert(block.hash);
                ledger.locks.remove(&block.hash);
                blocks.remove(&block);
            }
        }
    }
    // Prune the full incompatible branch, including descendants several
    // positions beyond the finalized fork; checking only its parent is unsafe.
    let retained: Vec<_> = ledger
        .notarized
        .iter()
        .flat_map(|(slot, blocks)| blocks.iter().map(move |b| (*slot, *b)))
        .collect();
    for (slot, block) in retained {
        if let Err(error) = selected_path(&ledger, index, slot, block.hash) {
            if error != BuildStateError::ConflictingFinality {
                return Err(error);
            }
            ledger.notarized.get_mut(&slot).unwrap().remove(&block);
            ledger.locks.remove(&block.hash);
        }
    }
    ledger.notarized.retain(|_, blocks| !blocks.is_empty());
    match rules.finalization {
        CheckpointFinalization::CertificateOnly => {}
        CheckpointFinalization::UniqueBranch => finalize_unique_branches(&mut ledger, false),
        CheckpointFinalization::NotarizedUniquePrefix => {
            finalize_unique_branches(&mut ledger, true)
        }
    }
    Ok(ledger)
}

/// Checkpoint promotion, applied after every step of the construction (see
/// CHECKPOINT-FINALIZATION-VARIANT.md): for each account, walk the
/// preserved positions upward from the finalized frontier; while a position
/// holds exactly one preserved block and that block attaches to the block
/// finalized below it, checkpoint-finalize it with origin `Derived`. A
/// position with two preserved rivals stops the walk; so does a sole block
/// that does not attach. Uniqueness is decided among preserved branches
/// only, never from missing evidence.
///
/// With `notarized_only`, this is the EuroSys manuscript's Rule 3: the walk
/// also stops at the first block without a represented closing-epoch NC,
/// so "recovery-only blocks are never promoted". Without it, the requested
/// variant promotes recovery-only branches too, which the manuscript
/// forbids and which the counterexample shows unsafe.
fn finalize_unique_branches(ledger: &mut EpochLedger, notarized_only: bool) {
    let accounts: BTreeSet<Account> = ledger.notarized.keys().map(|slot| slot.account).collect();
    for account in accounts {
        let Some(lowest) = ledger
            .notarized
            .range(AccountSlot::new(account, 0)..=AccountSlot::new(account, u64::MAX))
            .next()
            .map(|(slot, _)| slot.height)
        else {
            continue;
        };
        let mut height = lowest;
        loop {
            let slot = AccountSlot::new(account, height);
            let Some(blocks) = ledger.notarized.get(&slot) else {
                break;
            };
            if blocks.len() != 1 {
                break;
            }
            let block = *blocks.iter().next().unwrap();
            let attached = if height <= 1 {
                block.previous.is_zero()
            } else {
                ledger.finalized(&AccountSlot::new(account, height - 1)) == Some(block.previous)
            };
            if !attached
                || (notarized_only && ledger.retained_kind(&block.hash) != RetainedKind::Notarized)
            {
                break;
            }
            ledger.finalize_derived(slot, block);
            height += 1;
        }
    }
}

/// Return a complete selected prefix ending at already-final history or an
/// account open. Strictly decreasing heights bound traversal and reject cycles.
fn selected_path(
    ledger: &EpochLedger,
    index: &dyn BlockIndex,
    slot: AccountSlot,
    hash: BlockHash,
) -> Result<Vec<(AccountSlot, PlacedBlock)>, BuildStateError> {
    let mut path = Vec::new();
    let (mut at, mut hash) = (slot, hash);
    loop {
        if let Some(finalized) = ledger.finalized(&at) {
            return if finalized == hash {
                Ok(path)
            } else {
                Err(BuildStateError::ConflictingFinality)
            };
        }
        let placement = index
            .placement(&hash)
            .ok_or(BuildStateError::MissingAncestry)?;
        if placement.slot != at || at.height == 0 || hash.is_zero() {
            return Err(BuildStateError::InvalidAncestry);
        }
        path.push((at, PlacedBlock::new(hash, placement.previous)));
        if at.height == 1 {
            return if placement.previous.is_zero() {
                Ok(path)
            } else {
                Err(BuildStateError::InvalidAncestry)
            };
        }
        if placement.previous.is_zero() {
            return Err(BuildStateError::MissingAncestry);
        }
        at = AccountSlot::new(at.account, at.height - 1);
        hash = placement.previous;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::{CertifiedBlock, CertifiedStatus, ResidualKind};
    use rsnano_types::PrivateKey;
    use std::collections::HashMap;

    /// A unique represented NC is a lock, not application finality.
    #[test]
    fn a_unique_notarization_is_retained_without_finality() {
        let mut index = StubIndex::default();
        let block = index.add(1, 1, BlockHash::ZERO);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, block, CertifiedStatus::Notarized);
        let residual = ResidualVotes::new();

        let ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &residual)],
            &index,
        );

        assert_eq!(ledger.finalized(&slot(1, 1)), None);
        assert_eq!(ledger.notarized(&slot(1, 1)), vec![block]);
        assert_eq!(ledger.retained_kind(&block), RetainedKind::Notarized);
        assert_eq!(ledger.finalized_count(), 0);
    }

    #[test]
    fn conflicting_new_notarizations_are_rejected() {
        let mut index = StubIndex::default();
        let one = index.add(1, 1, BlockHash::ZERO);
        let other = index.add(1, 1, BlockHash::ZERO);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, one, CertifiedStatus::Notarized);
        certify(&mut certified, &index, other, CertifiedStatus::Notarized);
        assert_eq!(
            build_state(
                &EpochLedger::new(),
                &[report(&certified, &ResidualVotes::new())],
                &index,
                certificate_rules()
            ),
            Err(BuildStateError::ConflictingNotarizations)
        );
    }

    /// Represented notarization outranks weaker residual support.
    #[test]
    fn a_unique_certified_block_outranks_a_residual_conflict() {
        let mut index = StubIndex::default();
        let certified_block = index.add(1, 1, BlockHash::ZERO);
        let supported = index.add(1, 1, BlockHash::ZERO);
        let mut certified = CertifiedState::new();
        certify(
            &mut certified,
            &index,
            certified_block,
            CertifiedStatus::Notarized,
        );
        let mut residual = ResidualVotes::new();
        record(&mut residual, &index, supported, ResidualKind::First);

        let ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &residual)],
            &index,
        );

        assert_eq!(ledger.finalized(&slot(1, 1)), None);
        assert_eq!(ledger.notarized(&slot(1, 1)), vec![certified_block]);
        assert_eq!(
            ledger.retained_kind(&certified_block),
            RetainedKind::Notarized
        );
    }

    /// Recovery preserves the branch of a possible latent FF, not finality.
    #[test]
    fn a_hidden_fast_certificate_is_recovered_from_first_votes() {
        let mut index = StubIndex::default();
        let hidden = index.add(1, 1, BlockHash::ZERO);
        let other = index.add(1, 1, BlockHash::ZERO);
        let certified = CertifiedState::new();
        let mut for_hidden = ResidualVotes::new();
        record(&mut for_hidden, &index, hidden, ResidualKind::First);
        let mut for_other = ResidualVotes::new();
        record(&mut for_other, &index, other, ResidualKind::First);

        // Three reporters of REPORTER_WEIGHT reach MANY for `hidden`
        let ledger = derive(
            &EpochLedger::new(),
            &[
                reported_by(1, &certified, &for_hidden),
                reported_by(2, &certified, &for_hidden),
                reported_by(3, &certified, &for_hidden),
                reported_by(4, &certified, &for_other),
            ],
            &index,
        );

        assert_eq!(ledger.finalized(&slot(1, 1)), None);
        assert_eq!(ledger.notarized(&slot(1, 1)), vec![hidden]);
        assert_eq!(ledger.retained_kind(&hidden), RetainedKind::Recovery);
        assert!(ledger.frontiers().is_empty());
    }

    /// Below-threshold new residual branches may be omitted.
    #[test]
    fn first_votes_below_the_threshold_recover_nothing() {
        let mut index = StubIndex::default();
        let one = index.add(1, 1, BlockHash::ZERO);
        let other = index.add(1, 1, BlockHash::ZERO);
        let certified = CertifiedState::new();
        let mut for_one = ResidualVotes::new();
        record(&mut for_one, &index, one, ResidualKind::First);
        let mut for_other = ResidualVotes::new();
        record(&mut for_other, &index, other, ResidualKind::First);

        let ledger = derive(
            &EpochLedger::new(),
            &[
                reported_by(1, &certified, &for_one),
                reported_by(2, &certified, &for_one),
                reported_by(3, &certified, &for_other),
            ],
            &index,
        );

        assert_eq!(ledger.finalized(&slot(1, 1)), None);
        assert!(ledger.notarized(&slot(1, 1)).is_empty());
    }

    /// Only first votes build a fast certificate, so second-look notarization
    /// support does not reach the recovery threshold however much of it there
    /// is. This is why the residual object tells the two kinds apart.
    #[test]
    fn notarization_support_does_not_recover_a_fast_certificate() {
        let mut index = StubIndex::default();
        let supported = index.add(1, 1, BlockHash::ZERO);
        let other = index.add(1, 1, BlockHash::ZERO);
        let certified = CertifiedState::new();
        let mut looked_at = ResidualVotes::new();
        record(&mut looked_at, &index, supported, ResidualKind::Notar);
        let mut for_other = ResidualVotes::new();
        record(&mut for_other, &index, other, ResidualKind::First);

        let ledger = derive(
            &EpochLedger::new(),
            &[
                reported_by(1, &certified, &looked_at),
                reported_by(2, &certified, &looked_at),
                reported_by(3, &certified, &looked_at),
                reported_by(4, &certified, &for_other),
            ],
            &index,
        );

        assert_eq!(ledger.finalized(&slot(1, 1)), None);
        assert!(ledger.notarized(&slot(1, 1)).is_empty());
    }

    /// Rule 3: a block the reports show finalized is finalized, and the
    /// conflicting sibling is excluded rather than kept
    #[test]
    fn a_final_visible_block_excludes_its_sibling() {
        let mut index = StubIndex::default();
        let winner = index.add(1, 1, BlockHash::ZERO);
        let loser = index.add(1, 1, BlockHash::ZERO);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, winner, CertifiedStatus::Finalized);
        certify(&mut certified, &index, loser, CertifiedStatus::Notarized);
        let residual = ResidualVotes::new();

        let ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &residual)],
            &index,
        );

        assert_eq!(ledger.finalized(&slot(1, 1)), Some(winner));
        assert!(ledger.notarized(&slot(1, 1)).is_empty());
    }

    #[test]
    fn a_notarized_child_retains_ancestry_but_does_not_finalize_it() {
        let mut index = StubIndex::default();
        let parent = index.add(1, 1, BlockHash::ZERO);
        let child = index.add(1, 2, parent);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, child, CertifiedStatus::Notarized);
        let ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &ResidualVotes::new())],
            &index,
        );
        assert_eq!(ledger.notarized(&slot(1, 1)), vec![parent]);
        assert_eq!(ledger.notarized(&slot(1, 2)), vec![child]);
        assert_eq!(ledger.retained_kind(&parent), RetainedKind::Ancestor);
        assert_eq!(ledger.retained_kind(&child), RetainedKind::Notarized);
        assert!(ledger.frontiers().is_empty());
    }

    #[test]
    fn an_explicitly_final_child_finalizes_its_selected_prefix() {
        let mut index = StubIndex::default();
        let parent = index.add(1, 1, BlockHash::ZERO);
        let child = index.add(1, 2, parent);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, child, CertifiedStatus::Finalized);
        let ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &ResidualVotes::new())],
            &index,
        );
        assert_eq!(ledger.finalized(&slot(1, 1)), Some(parent));
        assert_eq!(ledger.finalized(&slot(1, 2)), Some(child));
        assert_eq!(ledger.frontiers()[&Account::from(1)], (2, child));
    }

    /// Rule 1: the predecessor's finalized state is carried forward and a
    /// block conflicting with it is excluded
    #[test]
    fn the_predecessor_finalized_state_is_never_rolled_back() {
        let mut index = StubIndex::default();
        let finalized = index.add(1, 1, BlockHash::ZERO);
        let conflicting = index.add(1, 1, BlockHash::ZERO);
        let mut previous = EpochLedger::new();
        previous.finalize(slot(1, 1), placed(&index, finalized));

        let mut certified = CertifiedState::new();
        certify(
            &mut certified,
            &index,
            conflicting,
            CertifiedStatus::Notarized,
        );
        let residual = ResidualVotes::new();

        let ledger = derive(&previous, &[report(&certified, &residual)], &index);

        assert_eq!(ledger.finalized(&slot(1, 1)), Some(finalized));
        assert!(ledger.notarized(&slot(1, 1)).is_empty());
    }

    /// Rule 5: "Remove inherited provisional blocks only when they conflict
    /// with the selected finalized branch." An inherited fork whose sibling
    /// the reports show finalized goes; one with no finalized branch against
    /// it stays, whatever the reports say about it.
    #[test]
    fn an_inherited_fork_goes_only_against_a_finalized_branch() {
        let mut index = StubIndex::default();
        let loser = index.add(1, 1, BlockHash::ZERO);
        let winner = index.add(1, 1, BlockHash::ZERO);
        let unresolved_one = index.add(2, 1, BlockHash::ZERO);
        let unresolved_other = index.add(2, 1, BlockHash::ZERO);
        let mut previous = EpochLedger::new();
        previous.keep(slot(1, 1), placed(&index, loser));
        previous.keep(slot(1, 1), placed(&index, winner));
        previous.keep(slot(2, 1), placed(&index, unresolved_one));
        previous.keep(slot(2, 1), placed(&index, unresolved_other));

        // The reports finalize one of the two at the first slot and say
        // nothing at all about the second slot
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, winner, CertifiedStatus::Finalized);
        let residual = ResidualVotes::new();

        let ledger = derive(&previous, &[report(&certified, &residual)], &index);

        assert_eq!(ledger.finalized(&slot(1, 1)), Some(winner));
        assert!(ledger.notarized(&slot(1, 1)).is_empty());
        assert_eq!(
            ledger.notarized(&slot(2, 1)),
            vec_sorted(&[unresolved_one, unresolved_other])
        );
    }

    /// The frontier stops below a slot the epoch did not decide. Counting
    /// the highest finalized slot instead would credit the account with a
    /// balance it reached through blocks the state does not hold, which is
    /// the same money counted twice once the receivers are counted too.
    #[test]
    fn a_frontier_stops_below_an_undecided_slot() {
        let mut index = StubIndex::default();
        let first = index.add(1, 1, BlockHash::ZERO);
        let second = index.add(1, 2, first);
        let conflicting = index.add(1, 3, second);
        let sibling = index.add(1, 3, second);
        let later = index.add(1, 4, conflicting);

        let mut ledger = EpochLedger::new();
        ledger.finalize(slot(1, 1), placed(&index, first));
        ledger.finalize(slot(1, 2), placed(&index, second));
        ledger.keep(slot(1, 3), placed(&index, conflicting));
        ledger.keep(slot(1, 3), placed(&index, sibling));
        ledger.finalize(slot(1, 4), placed(&index, later));

        let frontiers = ledger.frontiers();
        assert_eq!(frontiers[&Account::from(1)], (2, second));
    }

    /// Two validators deriving from the same reports obtain the same hash,
    /// and a different state gives a different one
    #[test]
    fn the_state_hash_is_the_derived_state() {
        let mut index = StubIndex::default();
        let one = index.add(1, 1, BlockHash::ZERO);
        let other = index.add(2, 1, BlockHash::ZERO);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, one, CertifiedStatus::Notarized);
        certify(&mut certified, &index, other, CertifiedStatus::Notarized);
        let residual = ResidualVotes::new();
        let selection = [report(&certified, &residual)];

        let first = derive(&EpochLedger::new(), &selection, &index);
        let second = derive(&EpochLedger::new(), &selection, &index);
        assert_eq!(first.state_hash(), second.state_hash());

        let mut fewer = CertifiedState::new();
        certify(&mut fewer, &index, one, CertifiedStatus::Notarized);
        let third = derive(&EpochLedger::new(), &[report(&fewer, &residual)], &index);
        assert_ne!(first.state_hash(), third.state_hash());
    }

    #[test]
    fn a_block_without_a_placement_is_rejected() {
        let index = StubIndex::default();
        let mut certified = CertifiedState::new();
        certified.certify(
            CertifiedBlock::new(Account::from(1), 1, BlockHash::from(999)),
            BlockHash::ZERO,
            CertifiedStatus::Notarized,
        );
        assert_eq!(
            build_state(
                &EpochLedger::new(),
                &[report(&certified, &ResidualVotes::new())],
                &index,
                certificate_rules()
            ),
            Err(BuildStateError::InvalidAncestry)
        );
    }

    /// RAI: the reports themselves place every block they name, so a
    /// validator derives the same state whether or not it holds the bodies.
    /// This is what `ReportIndex` is for; without it two validators holding
    /// different bodies would derive different states from one selection.
    #[test]
    fn r_reports_carry_protection_without_notarization_or_fresh_support() {
        let mut index = StubIndex::default();
        let hash = index.add(1, 1, BlockHash::ZERO);
        let mut previous = EpochLedger::new();
        previous.keep(slot(1, 1), PlacedBlock::new(hash, BlockHash::ZERO));
        previous.locks.insert(hash, RetainedKind::Recovery);
        let t = previous.report_ledger();
        let g = ResidualVotes::new();
        let reports: Vec<_> = (1..=6)
            .map(|i| SelectedReport {
                reporter: PublicKey::from(i),
                weight: MANY,
                certified: &t,
                residual: &g,
            })
            .collect();
        let next = build_state(&previous, &reports, &index, certificate_rules()).unwrap();
        assert_eq!(next, previous);
        assert_eq!(next.retained_kind(&hash), RetainedKind::Recovery);
        assert_eq!(
            build_state(&EpochLedger::new(), &reports, &index, certificate_rules()),
            Err(BuildStateError::InvalidRecoveryEntry)
        );
        let t2 = next.report_ledger();
        assert_eq!(
            build_state(&next, &[report(&t2, &g)], &index, certificate_rules()).unwrap(),
            previous
        );
    }

    #[test]
    fn the_reports_place_the_blocks_they_name() {
        let mut index = StubIndex::default();
        let block = index.add(1, 1, BlockHash::ZERO);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, block, CertifiedStatus::Notarized);
        let residual = ResidualVotes::new();
        let selection = [report(&certified, &residual)];

        let from_reports = ReportIndex::new(&EpochLedger::new(), &selection);
        let ledger = derive(&EpochLedger::new(), &selection, &from_reports);
        assert_eq!(ledger.notarized(&slot(1, 1)), vec![block]);
    }

    /// Rule 1: "Inherited provisional forks are not omitted merely because
    /// the current reports contain no new vote for them." A promised
    /// recovery tip can not disappear through report omission.
    #[test]
    fn an_inherited_fork_survives_reports_that_do_not_mention_it() {
        let mut index = StubIndex::default();
        let one = index.add(1, 1, BlockHash::ZERO);
        let other = index.add(1, 1, BlockHash::ZERO);
        let mut inherited = EpochLedger::new();
        inherited.keep(slot(1, 1), PlacedBlock::new(one, BlockHash::ZERO));
        inherited.keep(slot(1, 1), PlacedBlock::new(other, BlockHash::ZERO));

        // Reports of the next epoch that say nothing about either block
        let certified = CertifiedState::new();
        let residual = ResidualVotes::new();
        let selection = [report(&certified, &residual)];
        let ledger = derive(
            &inherited,
            &selection,
            &ReportIndex::new(&inherited, &selection),
        );

        assert_eq!(ledger.notarized(&slot(1, 1)), vec_sorted(&[one, other]));
        assert_eq!(ledger.finalized(&slot(1, 1)), None);
    }

    #[test]
    fn recovery_lock_survives_omission_and_later_explicit_finality_promotes_it() {
        let mut index = StubIndex::default();
        let block = index.add(1, 1, BlockHash::ZERO);
        let empty = CertifiedState::new();
        let mut votes = ResidualVotes::new();
        record(&mut votes, &index, block, ResidualKind::First);
        let selected: Vec<_> = (1..=3).map(|i| reported_by(i, &empty, &votes)).collect();
        let locked = derive(&EpochLedger::new(), &selected, &index);
        let carried = derive(&locked, &[], &index);
        assert_eq!(locked, carried);
        assert_eq!(carried.retained_kind(&block), RetainedKind::Recovery);
        let mut finalized = CertifiedState::new();
        certify(&mut finalized, &index, block, CertifiedStatus::Finalized);
        let promoted = derive(
            &carried,
            &[report(&finalized, &ResidualVotes::new())],
            &index,
        );
        assert_eq!(promoted.finalized(&slot(1, 1)), Some(block));
        assert!(promoted.notarized(&slot(1, 1)).is_empty());
        assert!(promoted.locks.is_empty());
        assert_eq!(derive(&promoted, &[], &index), promoted);
    }

    /// RAI, Rule 3: only a predecessor-backed represented NC supersedes an
    /// inherited recovery-only lock, and it takes the lock's retained
    /// descendants with it; an unbacked NC is ineligible early work, and a
    /// represented notarization lock is never superseded
    #[test]
    fn a_predecessor_backed_notarization_supersedes_an_inherited_recovery_lock_only() {
        let mut index = StubIndex::default();
        let base = index.add(1, 1, BlockHash::ZERO);
        let carried = index.add(1, 2, base);
        let child = index.add(1, 3, carried);
        let fresh = index.add(1, 2, base);
        let mut previous = EpochLedger::new();
        previous.finalize_genesis_block(slot(1, 1), PlacedBlock::new(base, BlockHash::ZERO));
        previous.keep(slot(1, 2), PlacedBlock::new(carried, base));
        previous.keep(slot(1, 3), PlacedBlock::new(child, carried));
        previous.locks.insert(carried, RetainedKind::Recovery);
        previous.locks.insert(child, RetainedKind::Recovery);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, fresh, CertifiedStatus::Notarized);
        let no_votes = ResidualVotes::new();
        let reports = [report(&certified, &no_votes)];

        let unbacked = build_state(&previous, &reports, &index, certificate_rules()).unwrap();
        assert_eq!(unbacked.notarized(&slot(1, 2)), vec![carried]);
        assert_eq!(unbacked.notarized(&slot(1, 3)), vec![child]);
        assert_eq!(unbacked.retained_kind(&carried), RetainedKind::Recovery);

        let backing: BTreeSet<BlockHash> = [fresh].into_iter().collect();
        let rules = BuildRules {
            many: MANY,
            backing: &backing,
            finalization: CheckpointFinalization::CertificateOnly,
        };
        let backed = build_state(&previous, &reports, &index, rules).unwrap();
        assert_eq!(backed.notarized(&slot(1, 2)), vec![fresh]);
        assert!(backed.notarized(&slot(1, 3)).is_empty());
        assert_eq!(backed.retained_kind(&fresh), RetainedKind::Notarized);
        assert_eq!(backed.retained_kind(&carried), RetainedKind::Ancestor);
        assert_eq!(backed.retained_kind(&child), RetainedKind::Ancestor);
        assert!(backed.finalized(&slot(1, 2)).is_none());

        previous.locks.insert(carried, RetainedKind::Notarized);
        let kept = build_state(&previous, &reports, &index, rules).unwrap();
        assert_eq!(
            vec_sorted(&kept.notarized(&slot(1, 2))),
            vec_sorted(&[carried, fresh])
        );
        assert_eq!(kept.notarized(&slot(1, 3)), vec![child]);
    }

    /// The unique-branch variant: a sole preserved block is finalized with
    /// origin Derived, the walk continues up a unique prefix and stops at a
    /// position with two preserved rivals; a preserved rival at the same
    /// position prevents finalization even for a one-block branch. The
    /// certificate-only control carries the same blocks as locks, and the
    /// two states never hash alike.
    #[test]
    fn the_unique_branch_variant_finalizes_sole_preserved_branches_only() {
        let mut index = StubIndex::default();
        // Account 1: a unique recovery-locked block over a finalized base
        let base = index.add(1, 1, BlockHash::ZERO);
        let sole = index.add(1, 2, base);
        // Account 2: a unique block with two rival children above it
        let root = index.add(2, 1, BlockHash::ZERO);
        let child_a = index.add(2, 2, root);
        let child_b = index.add(2, 2, root);
        // Account 3: two rivals at the first position
        let rival_a = index.add(3, 1, BlockHash::ZERO);
        let rival_b = index.add(3, 1, BlockHash::ZERO);
        let mut previous = EpochLedger::new();
        previous.finalize_genesis_block(slot(1, 1), PlacedBlock::new(base, BlockHash::ZERO));
        let empty = CertifiedState::new();
        let mut votes = ResidualVotes::new();
        for hash in [sole, root, child_a, child_b, rival_a, rival_b] {
            record(&mut votes, &index, hash, ResidualKind::First);
        }
        let selected: Vec<_> = (1..=3).map(|i| reported_by(i, &empty, &votes)).collect();
        let variant = BuildRules {
            many: MANY,
            backing: &(),
            finalization: CheckpointFinalization::UniqueBranch,
        };
        // Recovery counts: every block has three first votes, so each
        // position with one candidate is a recovery lock; the two-candidate
        // positions retain nothing, as under the paper's Rule 2
        let control = build_state(&previous, &selected, &index, certificate_rules()).unwrap();
        assert_eq!(control.retained_kind(&sole), RetainedKind::Recovery);
        assert_eq!(control.retained_kind(&root), RetainedKind::Recovery);
        assert!(control.finalized(&slot(1, 2)).is_none());
        assert_eq!(control.derived_count(), 0);

        let derived = build_state(&previous, &selected, &index, variant).unwrap();
        assert_eq!(derived.finalized(&slot(1, 2)), Some(sole));
        assert_eq!(
            derived.finality_origin(&slot(1, 2)),
            Some(FinalityOrigin::Derived)
        );
        assert_eq!(
            derived.finality_origin(&slot(1, 1)),
            Some(FinalityOrigin::Certificate)
        );
        assert_eq!(derived.finalized(&slot(2, 1)), Some(root));
        assert!(derived.finalized(&slot(2, 2)).is_none());
        assert!(derived.finalized(&slot(3, 1)).is_none());
        assert_eq!(derived.derived_count(), 2);
        assert_ne!(derived.state_hash(), control.state_hash());
        // The origin survives the checkpoint wire records
        #[cfg(feature = "rai_protocol")]
        {
            let rebuilt =
                EpochLedger::from_checkpoint_entries(&derived.checkpoint_entries()).unwrap();
            assert_eq!(rebuilt, derived);
            assert_eq!(rebuilt.state_hash(), derived.state_hash());
        }
        assert_eq!(
            derived
                .checkpoint_records()
                .iter()
                .filter(|(_, _, s)| *s == 4)
                .count(),
            2
        );

        // With rivals preserved at the first position (two locks inherited),
        // a one-block branch is not unique
        let mut contested = EpochLedger::new();
        contested.keep(slot(3, 1), PlacedBlock::new(rival_a, BlockHash::ZERO));
        contested.keep(slot(3, 1), PlacedBlock::new(rival_b, BlockHash::ZERO));
        contested.locks.insert(rival_a, RetainedKind::Recovery);
        contested.locks.insert(rival_b, RetainedKind::Recovery);
        let still = build_state(&contested, &[], &index, variant).unwrap();
        assert!(still.finalized(&slot(3, 1)).is_none());
        assert_eq!(still.notarized(&slot(3, 1)).len(), 2);
    }

    /// The EuroSys manuscript's Rule 3: promote the uniquely retained prefix
    /// while each block holds a represented closing-epoch NC; a recovery-only
    /// block stops the walk and is never promoted, unlike under the requested
    /// unique-branch variant
    #[test]
    fn the_notarized_unique_prefix_rule_never_promotes_recovery_only_blocks() {
        let mut index = StubIndex::default();
        // Account 1: notarized base, notarized child, recovery-only grandchild
        let base = index.add(1, 1, BlockHash::ZERO);
        let child = index.add(1, 2, base);
        let grandchild = index.add(1, 3, child);
        // Account 2: a recovery-only sole block
        let lone = index.add(2, 1, BlockHash::ZERO);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, base, CertifiedStatus::Notarized);
        certify(&mut certified, &index, child, CertifiedStatus::Notarized);
        let mut votes = ResidualVotes::new();
        record(&mut votes, &index, grandchild, ResidualKind::First);
        record(&mut votes, &index, lone, ResidualKind::First);
        let selected: Vec<_> = (1..=3)
            .map(|i| reported_by(i, &certified, &votes))
            .collect();
        let rules = |finalization| BuildRules {
            many: MANY,
            backing: &(),
            finalization,
        };
        let promoted = build_state(
            &EpochLedger::new(),
            &selected,
            &index,
            rules(CheckpointFinalization::NotarizedUniquePrefix),
        )
        .unwrap();
        assert_eq!(promoted.finalized(&slot(1, 1)), Some(base));
        assert_eq!(promoted.finalized(&slot(1, 2)), Some(child));
        assert!(promoted.finalized(&slot(1, 3)).is_none());
        assert_eq!(promoted.retained_kind(&grandchild), RetainedKind::Recovery);
        assert!(promoted.finalized(&slot(2, 1)).is_none());
        assert_eq!(promoted.retained_kind(&lone), RetainedKind::Recovery);
        assert_eq!(promoted.derived_count(), 2);
        let requested = build_state(
            &EpochLedger::new(),
            &selected,
            &index,
            rules(CheckpointFinalization::UniqueBranch),
        )
        .unwrap();
        assert_eq!(requested.finalized(&slot(1, 3)), Some(grandchild));
        assert_eq!(requested.finalized(&slot(2, 1)), Some(lone));
        assert_eq!(requested.derived_count(), 4);
        assert_eq!(
            CheckpointFinalization::parse("notarized_unique_prefix").as_str(),
            "notarized_unique_prefix"
        );
    }

    #[test]
    fn duplicate_reporter_cannot_create_recovery_support() {
        let mut index = StubIndex::default();
        let block = index.add(1, 1, BlockHash::ZERO);
        let certified = CertifiedState::new();
        let mut votes = ResidualVotes::new();
        record(&mut votes, &index, block, ResidualKind::First);
        assert_eq!(
            build_state(
                &EpochLedger::new(),
                &[report(&certified, &votes); 3],
                &index,
                certificate_rules()
            ),
            Err(BuildStateError::RepeatedReporter)
        );
    }

    #[test]
    fn missing_or_wrong_account_ancestry_never_finalizes_a_prefix() {
        let mut index = StubIndex::default();
        let parent = index.add(2, 1, BlockHash::ZERO);
        let child = index.add(1, 2, parent);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, child, CertifiedStatus::Finalized);
        let residual = ResidualVotes::new();
        assert_eq!(
            build_state(
                &EpochLedger::new(),
                &[report(&certified, &residual)],
                &index,
                certificate_rules()
            ),
            Err(BuildStateError::InvalidAncestry)
        );
        index.blocks.remove(&parent);
        assert_eq!(
            build_state(
                &EpochLedger::new(),
                &[report(&certified, &residual)],
                &index,
                certificate_rules()
            ),
            Err(BuildStateError::MissingAncestry)
        );
    }

    #[test]
    fn conflicting_explicit_finality_is_rejected() {
        let mut index = StubIndex::default();
        let a = index.add(1, 1, BlockHash::ZERO);
        let b = index.add(1, 1, BlockHash::ZERO);
        let mut certified = CertifiedState::new();
        for hash in [a, b] {
            certify(&mut certified, &index, hash, CertifiedStatus::Finalized);
        }
        assert_eq!(
            build_state(
                &EpochLedger::new(),
                &[report(&certified, &ResidualVotes::new())],
                &index,
                certificate_rules()
            ),
            Err(BuildStateError::ConflictingFinality)
        );
    }

    #[test]
    fn explicit_finality_prunes_all_descendants_of_an_inherited_conflict() {
        let mut index = StubIndex::default();
        let winner = index.add(1, 1, BlockHash::ZERO);
        let loser = index.add(1, 1, BlockHash::ZERO);
        let child = index.add(1, 2, loser);
        let grandchild = index.add(1, 3, child);
        let mut previous = EpochLedger::new();
        for hash in [loser, child, grandchild] {
            previous.keep(index.placement(&hash).unwrap().slot, placed(&index, hash));
            previous.locks.insert(hash, RetainedKind::Recovery);
        }
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, winner, CertifiedStatus::Finalized);
        let ledger = derive(
            &previous,
            &[report(&certified, &ResidualVotes::new())],
            &index,
        );
        assert_eq!(ledger.finalized(&slot(1, 1)), Some(winner));
        assert_eq!(ledger.notarized_count(), 0);
        assert!(ledger.locks.is_empty());
    }

    #[test]
    fn lock_kind_is_committed_and_report_order_does_not_change_state() {
        let mut index = StubIndex::default();
        let block = index.add(1, 1, BlockHash::ZERO);
        let empty = CertifiedState::new();
        let mut votes = ResidualVotes::new();
        record(&mut votes, &index, block, ResidualKind::First);
        let mut selected: Vec<_> = (1..=3).map(|i| reported_by(i, &empty, &votes)).collect();
        let recovered = derive(&EpochLedger::new(), &selected, &index);
        selected.reverse();
        assert_eq!(recovered, derive(&EpochLedger::new(), &selected, &index));
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, block, CertifiedStatus::Notarized);
        let notarized = derive(
            &EpochLedger::new(),
            &[report(&certified, &ResidualVotes::new())],
            &index,
        );
        assert_ne!(recovered.state_hash(), notarized.state_hash());
        assert_eq!(
            recovered.notarized(&slot(1, 1)),
            notarized.notarized(&slot(1, 1))
        );
    }

    #[test]
    fn genesis_setup_reports_require_complete_history_not_synthetic_frontiers() {
        let mut index = StubIndex::default();
        let open = index.add(1, 1, BlockHash::ZERO);
        let second = index.add(1, 2, open);
        let frontier = index.add(1, 3, second);
        let mut certified = CertifiedState::new();
        for hash in [second, frontier] {
            certify(&mut certified, &index, hash, CertifiedStatus::Finalized);
        }
        let residual = ResidualVotes::new();
        let selection = [report(&certified, &residual)];
        let mut sparse = EpochLedger::new();
        sparse.finalize_genesis(slot(1, 3), frontier);
        assert!(
            build_state(
                &sparse,
                &selection,
                &ReportIndex::new(&sparse, &selection),
                certificate_rules()
            )
            .is_err()
        );
        let mut complete = EpochLedger::new();
        for hash in [open, second, frontier] {
            complete
                .finalize_genesis_block(index.placement(&hash).unwrap().slot, placed(&index, hash));
        }
        assert_eq!(
            derive(
                &complete,
                &selection,
                &ReportIndex::new(&complete, &selection)
            ),
            complete
        );
    }

    /* Test helpers */

    /// f + p + 1 for the tests. A reporter weighs `REPORTER_WEIGHT`, so one
    /// report never reaches the recovery threshold and three do: a test that
    /// means to exercise `A_Q` has to say so by selecting three reporters.
    const MANY: Amount = Amount::raw(25);
    const REPORTER_WEIGHT: Amount = Amount::raw(10);

    fn derive(
        previous: &EpochLedger,
        selection: &[SelectedReport],
        index: &dyn BlockIndex,
    ) -> EpochLedger {
        build_state(previous, selection, index, certificate_rules()).unwrap()
    }

    /// The paper's rules, with no represented NC predecessor-backed
    fn certificate_rules() -> BuildRules<'static> {
        BuildRules {
            many: MANY,
            backing: &(),
            finalization: CheckpointFinalization::CertificateOnly,
        }
    }

    fn report<'a>(
        certified: &'a CertifiedState,
        residual: &'a ResidualVotes,
    ) -> SelectedReport<'a> {
        reported_by(1, certified, residual)
    }

    fn reported_by<'a>(
        key: u64,
        certified: &'a CertifiedState,
        residual: &'a ResidualVotes,
    ) -> SelectedReport<'a> {
        SelectedReport {
            reporter: PrivateKey::from(key).public_key(),
            weight: REPORTER_WEIGHT,
            certified,
            residual,
        }
    }

    fn slot(account: u64, height: u64) -> AccountSlot {
        AccountSlot::new(Account::from(account), height)
    }

    fn vec_sorted(hashes: &[BlockHash]) -> Vec<BlockHash> {
        let mut sorted = hashes.to_vec();
        sorted.sort();
        sorted
    }

    /// A block of the stub index with the branch it continues
    fn placed(index: &StubIndex, hash: BlockHash) -> PlacedBlock {
        PlacedBlock::new(hash, index.placement(&hash).unwrap().previous)
    }

    fn certified_at(index: &StubIndex, hash: BlockHash) -> CertifiedBlock {
        let placement = index.placement(&hash).unwrap();
        CertifiedBlock::new(placement.slot.account, placement.slot.height, hash)
    }

    /// Certifies a block of the stub index, with the branch it continues
    fn certify(
        state: &mut CertifiedState,
        index: &StubIndex,
        hash: BlockHash,
        status: CertifiedStatus,
    ) {
        let placement = index.placement(&hash).unwrap();
        state.certify(certified_at(index, hash), placement.previous, status);
    }

    /// Records a residual vote for a block of the stub index
    fn record(votes: &mut ResidualVotes, index: &StubIndex, hash: BlockHash, kind: ResidualKind) {
        let placement = index.placement(&hash).unwrap();
        votes.record(certified_at(index, hash), placement.previous, kind);
    }

    /// The blocks the deriving validator holds, as a stub of the ledger
    #[derive(Default)]
    struct StubIndex {
        blocks: HashMap<BlockHash, BlockPlacement>,
    }

    impl StubIndex {
        /// A block of an account at a height, following the given parent.
        /// The hash is made unique so that siblings differ.
        fn add(&mut self, account: u64, height: u64, previous: BlockHash) -> BlockHash {
            let hash = BlockHash::from(1000 + self.blocks.len() as u64);
            self.blocks.insert(
                hash,
                BlockPlacement {
                    slot: AccountSlot::new(Account::from(account), height),
                    previous,
                },
            );
            hash
        }
    }

    impl BlockIndex for StubIndex {
        fn placement(&self, hash: &BlockHash) -> Option<BlockPlacement> {
            self.blocks.get(hash).copied()
        }
    }
}
