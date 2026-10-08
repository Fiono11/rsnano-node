use std::collections::{BTreeMap, BTreeSet};

use rsnano_types::{Account, Amount, Blake2HashBuilder, BlockHash, ConsensusEpoch, PublicKey};

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

/// The strength of a lock record: a notarization lock (Rule 1, a
/// represented NC) or a recovery record (Rule 2, reporter first votes)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum LockStrength {
    Notarization = 2,
    Recovery = 3,
}

/// RAI, Section 3.3: a lock record names its block, its strength, and its
/// origin, the epoch whose potential finality it protects. A block may carry
/// several recovery records, at most one per origin; each is discharged
/// separately, and only by evidence of its own origin epoch. The block stays
/// retained while any of its records survives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LockRecord {
    pub strength: LockStrength,
    pub origin: ConsensusEpoch,
}

impl LockRecord {
    pub fn notarization(origin: ConsensusEpoch) -> Self {
        Self {
            strength: LockStrength::Notarization,
            origin,
        }
    }

    pub fn recovery(origin: ConsensusEpoch) -> Self {
        Self {
            strength: LockStrength::Recovery,
            origin,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpochLedger {
    /// The finalized block at a slot, by account finalization or by the
    /// epoch decision
    finalized: BTreeMap<AccountSlot, PlacedBlock>,
    /// The conflicting blocks a slot kept: checkpoint-notarized, provisional,
    /// with no application effect
    notarized: BTreeMap<AccountSlot, BTreeSet<PlacedBlock>>,
    /// The lock records of each lock target. A retained block without an
    /// entry is held as an ancestor of one.
    locks: BTreeMap<BlockHash, BTreeSet<LockRecord>>,
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

    /// The height of the account's finalized tip
    pub fn finalized_depth(&self, account: Account) -> Option<u64> {
        self.finalized
            .range(AccountSlot::new(account, 0)..=AccountSlot::new(account, u64::MAX))
            .next_back()
            .map(|(s, _)| s.height)
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

    /// The strongest lock a block carries: a notarization lock if any record
    /// is one, a recovery lock if it carries recovery records only
    pub fn retained_kind(&self, hash: &BlockHash) -> RetainedKind {
        let Some(records) = self.locks.get(hash) else {
            return RetainedKind::Ancestor;
        };
        if records
            .iter()
            .any(|record| record.strength == LockStrength::Notarization)
        {
            RetainedKind::Notarized
        } else if records.is_empty() {
            RetainedKind::Ancestor
        } else {
            RetainedKind::Recovery
        }
    }

    /// Whether a block retained at a position carries a recovery record of
    /// the given origin: evidence of that epoch for a rival at the position
    /// may discharge it
    pub fn carries_recovery_record(&self, slot: &AccountSlot, origin: ConsensusEpoch) -> bool {
        self.notarized.get(slot).is_some_and(|blocks| {
            blocks.iter().any(|block| {
                self.lock_records(&block.hash).any(|record| {
                    record.strength == LockStrength::Recovery && record.origin == origin
                })
            })
        })
    }

    /// The lock records a block carries, in canonical order
    pub fn lock_records(&self, hash: &BlockHash) -> impl Iterator<Item = LockRecord> + '_ {
        self.locks.get(hash).into_iter().flatten().copied()
    }

    /// Adds a lock record to a block. A record of a given strength and
    /// origin is held once.
    fn add_lock(&mut self, hash: BlockHash, record: LockRecord) {
        self.locks.entry(hash).or_default().insert(record);
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
    /// Whether the checkpoint retains this block as a lock target. Called
    /// per evidence block on the network path, so a map lookup: retained
    /// ancestors without a lock of their own are not followed individually.
    pub fn retains_block(&self, hash: &BlockHash) -> bool {
        self.locks.contains_key(hash)
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
        let mut builder = Blake2HashBuilder::new().update(b"RAI epoch state v3 lock records");
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
                // The lock records are part of S_e: two states that retain
                // the same block under records of different origins differ
                for record in self.lock_records(&block.hash) {
                    builder = builder
                        .update([record.strength as u8])
                        .update(record.origin.as_u64().to_le_bytes());
                }
            }
        }
        builder.build()
    }

    /// A slot the checkpoint kept as a notarized fork, for tests
    #[cfg(test)]
    pub fn retain_for_test(&mut self, slot: AccountSlot, hash: BlockHash, previous: BlockHash) {
        self.keep(slot, PlacedBlock::new(hash, previous));
        self.set_lock_for_test(hash, RetainedKind::Notarized);
    }

    /// A slot the checkpoint kept as a recovery-only lock, for tests
    #[cfg(test)]
    pub fn retain_recovery_for_test(
        &mut self,
        slot: AccountSlot,
        hash: BlockHash,
        previous: BlockHash,
    ) {
        self.keep(slot, PlacedBlock::new(hash, previous));
        self.set_lock_for_test(hash, RetainedKind::Recovery);
    }

    /// Replaces a block's lock records with one of the given kind, of origin
    /// epoch zero, for tests
    #[cfg(test)]
    pub fn set_lock_for_test(&mut self, hash: BlockHash, kind: RetainedKind) {
        let record = match kind {
            RetainedKind::Ancestor => {
                self.locks.remove(&hash);
                return;
            }
            RetainedKind::Notarized => LockRecord::notarization(ConsensusEpoch::ZERO),
            RetainedKind::Recovery => LockRecord::recovery(ConsensusEpoch::ZERO),
        };
        self.locks.insert(hash, BTreeSet::from([record]));
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
        // A finalized position keeps no conflicting survivor
        if let Some(blocks) = self.notarized.remove(&slot) {
            for block in blocks {
                self.locks.remove(&block.hash);
            }
        }
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
    /// No placement for the block a selected path needs at this position
    MissingAncestry(AccountSlot, BlockHash),
    InvalidAncestry,
    ConflictingFinality,
    ConflictingNotarizations,
    RepeatedReporter,
    InvalidRecoveryEntry,
}

/// What BuildState needs besides the predecessor and the selected reports
#[derive(Clone, Copy)]
pub struct BuildRules {
    /// r = f + p + 1 as weight: the reporter first votes a recovery lock needs
    pub many: Amount,
    /// The closing epoch: the origin of every lock record Rules 1 and 2 add
    pub epoch: ConsensusEpoch,
}

/// Revised BuildState: explicit F entries extend finality, and so does Rule 3
/// promotion of a unique retained child with a closing-epoch NC at the
/// finalized tip. A represented NC or enough reporter first votes otherwise
/// retain a parent-closed lock, without any application effect. Inherited
/// unresolved branches survive report omission. No sole-survivor
/// finalization without an NC is performed: the unique-branch variant of
/// rai-cross-epoch-minimal is not ported, it is unsafe with the overlap
/// exceptions.
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
                    // Already carried by `previous`: an inherited R entry is
                    // not itself support. An epoch-e first vote for the block
                    // is, and reaches U_Q through G_i below.
                }
            }
        }
        // A reporter is counted only for its own first vote that T_i does
        // not tag N or F (Fix B): an R tag exposes no vote, so a first vote
        // for an R-tagged block counts. The residual set is canonical, and
        // the identity is counted once.
        for block in report.residual.first_votes() {
            if report.certified.summarizes(&block.hash) {
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
    for (slot, at) in &evidence {
        if ledger.finalized(slot).is_some() {
            continue;
        }
        let represented: Vec<_> = at.notarized.iter().copied().collect();
        let target = match represented.as_slice() {
            [hash] => Some((*hash, LockStrength::Notarization)),
            [] => {
                let recovered: Vec<_> = at
                    .first
                    .iter()
                    .filter(|(_, weight)| **weight >= many)
                    .map(|(hash, _)| *hash)
                    .collect();
                match recovered.as_slice() {
                    [hash] => Some((*hash, LockStrength::Recovery)),
                    _ => None,
                }
            }
            _ => return Err(BuildStateError::ConflictingNotarizations),
        };
        if let Some((hash, strength)) = target {
            // "Inherited state and discharge": a represented NC for a
            // different block at a position S_{e-1} locks only for recovery
            // is ineligible early work; the inherited lock is retained and
            // is discharged by explicit compatible finality alone. The
            // predecessor-backed supersession of an earlier revision is not
            // part of the core protocol.
            let inherited = previous.notarized(slot);
            if strength == LockStrength::Notarization
                && !inherited.is_empty()
                && !inherited.contains(&hash)
                && inherited
                    .iter()
                    .all(|held| previous.retained_kind(held) != RetainedKind::Notarized)
            {
                continue;
            }
            match selected_path(&ledger, index, *slot, hash) {
                Ok(path) => {
                    for (slot, block) in path.into_iter().rev() {
                        ledger.keep(slot, block);
                    }
                    // Rules 1 and 2 stamp the closing epoch. A recovery
                    // record is added whether or not the block already
                    // carries records of earlier origins, which are kept as
                    // they are. An inherited notarization lock is re-tagged N
                    // by every report and keeps its own record.
                    let inherited_notarization = strength == LockStrength::Notarization
                        && previous.retained_kind(&hash) == RetainedKind::Notarized;
                    if inherited_notarization {
                        continue;
                    }
                    ledger.add_lock(
                        hash,
                        LockRecord {
                            strength,
                            origin: rules.epoch,
                        },
                    );
                }
                // Explicit finality excludes an incompatible nonfinal branch.
                Err(BuildStateError::ConflictingFinality) => {}
                Err(error) => return Err(error),
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
    // Rule 3, checkpoint promotion: the notarized blocks a selected report
    // represents in this epoch
    let represented: BTreeSet<BlockHash> = evidence
        .values()
        .flat_map(|at| at.notarized.iter().copied())
        .collect();
    promote_unique_notarized_prefixes(&mut ledger, previous, &represented);
    Ok(ledger)
}

/// RAI, Rule 3 (checkpoint promotion): "For each account, starting at the tip
/// of F*, repeatedly promote the next block only while (i) it is the unique
/// child retained in R_Q and (ii) the manifest verifies a closing-epoch NC
/// for that block. Stop at the first fork or block without such an NC."
/// A recovery-only block is never promoted, nor is a notarization lock the
/// predecessor carried: its NC is of an earlier epoch. The promotion takes
/// effect only if the candidate is decided, which is when a derived state
/// is installed.
fn promote_unique_notarized_prefixes(
    ledger: &mut EpochLedger,
    previous: &EpochLedger,
    represented: &BTreeSet<BlockHash>,
) {
    let accounts: BTreeSet<Account> = ledger.notarized.keys().map(|slot| slot.account).collect();
    for account in accounts {
        loop {
            let (height, parent) = match ledger.finalized_depth(account) {
                Some(depth) => (
                    depth + 1,
                    ledger
                        .finalized(&AccountSlot::new(account, depth))
                        .expect("finalized depth names a finalized slot"),
                ),
                None => (1, BlockHash::ZERO),
            };
            let slot = AccountSlot::new(account, height);
            let Some(children) = ledger.notarized.get(&slot) else {
                break;
            };
            let [child] = children.iter().copied().collect::<Vec<_>>()[..] else {
                break;
            };
            let inherited = previous.is_locked(&slot, &child.hash)
                && previous.retained_kind(&child.hash) == RetainedKind::Notarized;
            if child.previous != parent
                || ledger.retained_kind(&child.hash) != RetainedKind::Notarized
                || !represented.contains(&child.hash)
                || inherited
            {
                break;
            }
            ledger.finalize(slot, child);
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
            .ok_or(BuildStateError::MissingAncestry(at, hash))?;
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
            return Err(BuildStateError::MissingAncestry(at, hash));
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

    /// RAI, Rule 3: a unique represented closing-epoch NC at the finalized
    /// tip is promoted by the checkpoint; the lock becomes finality
    #[test]
    fn a_unique_notarization_at_the_finalized_tip_is_promoted() {
        let mut index = StubIndex::default();
        let block = index.add(1, 1, BlockHash::ZERO);
        let child = index.add(1, 2, block);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, block, CertifiedStatus::Notarized);
        certify(&mut certified, &index, child, CertifiedStatus::Notarized);
        let residual = ResidualVotes::new();

        let ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &residual)],
            &index,
        );

        assert_eq!(ledger.finalized(&slot(1, 1)), Some(block));
        assert_eq!(ledger.finalized(&slot(1, 2)), Some(child));
        assert_eq!(ledger.notarized_count(), 0);
        assert!(ledger.locks.is_empty());
        assert_eq!(ledger.finalized_count(), 2);
    }

    /// RAI, Rule 3: promotion stops at the first fork, at a recovery-only
    /// lock, and at a block that does not continue the finalized tip
    #[test]
    fn promotion_stops_at_a_fork_a_recovery_lock_or_a_gap() {
        let mut index = StubIndex::default();
        let block = index.add(1, 1, BlockHash::ZERO);
        let recovered = index.add(1, 2, block);
        let grandchild = index.add(1, 3, recovered);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, block, CertifiedStatus::Notarized);
        certify(
            &mut certified,
            &index,
            grandchild,
            CertifiedStatus::Notarized,
        );
        let mut votes = ResidualVotes::new();
        record(&mut votes, &index, recovered, ResidualKind::First);
        let selected: Vec<_> = (1..=3)
            .map(|i| reported_by(i, &certified, &votes))
            .collect();

        let ledger = derive(&EpochLedger::new(), &selected, &index);

        assert_eq!(ledger.finalized(&slot(1, 1)), Some(block));
        assert_eq!(ledger.retained_kind(&recovered), RetainedKind::Recovery);
        assert_eq!(ledger.notarized(&slot(1, 2)), vec![recovered]);
        // Notarized, but above an unpromoted block: not promoted
        assert_eq!(ledger.notarized(&slot(1, 3)), vec![grandchild]);
        assert_eq!(ledger.finalized_count(), 1);

        // A fork of two inherited survivors at the tip stops promotion too
        let other = index.add(2, 1, BlockHash::ZERO);
        let rival = index.add(2, 1, BlockHash::ZERO);
        let mut previous = EpochLedger::new();
        for hash in [other, rival] {
            previous.keep(slot(2, 1), placed(&index, hash));
            previous.set_lock_for_test(hash, RetainedKind::Recovery);
        }
        let forked = derive(&previous, &[], &index);
        assert_eq!(forked.finalized(&slot(2, 1)), None);
        assert_eq!(forked.notarized(&slot(2, 1)).len(), 2);
    }

    /// RAI, Rule 3: the NC must be of the closing epoch; a notarization lock
    /// the predecessor carried is re-reported under N without a new NC and is
    /// not promoted
    #[test]
    fn an_inherited_notarization_lock_is_not_promoted() {
        let mut index = StubIndex::default();
        let block = index.add(1, 1, BlockHash::ZERO);
        let mut previous = EpochLedger::new();
        previous.keep(slot(1, 1), placed(&index, block));
        previous.set_lock_for_test(block, RetainedKind::Notarized);
        let base = previous.report_ledger();
        assert_eq!(
            base.certification(&certified_at(&index, block))
                .unwrap()
                .status,
            CertifiedStatus::Notarized
        );

        let ledger = derive(&previous, &[report(&base, &ResidualVotes::new())], &index);

        assert_eq!(ledger, previous);
        assert_eq!(ledger.finalized(&slot(1, 1)), None);
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

        // Unique at the tip with a closing-epoch NC: promoted (Rule 3); the
        // supported rival is not retained
        assert_eq!(ledger.finalized(&slot(1, 1)), Some(certified_block));
        assert!(ledger.notarized(&slot(1, 1)).is_empty());
        assert!(!ledger.locks.contains_key(&supported));
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
        previous.set_lock_for_test(hash, RetainedKind::Recovery);
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
        assert_eq!(ledger.finalized(&slot(1, 1)), Some(block));
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

    /// Lock records with an origin, and Fix B: a block recovery-locked by
    /// closure 1 and re-voted, but not finalized, in epoch 2 is reported R
    /// in T and first-voted in G. Closure 2 counts those first votes and
    /// adds a record of origin 2 next to the inherited one of origin 1.
    #[test]
    fn two_closures_over_a_re_voted_block_carry_two_records() {
        let mut index = StubIndex::default();
        let block = index.add(1, 1, BlockHash::ZERO);
        let empty = CertifiedState::new();
        let mut votes = ResidualVotes::new();
        record(&mut votes, &index, block, ResidualKind::First);
        let selected: Vec<_> = (1..=3).map(|i| reported_by(i, &empty, &votes)).collect();
        let first = build_state(&EpochLedger::new(), &selected, &index, rules_of(1)).unwrap();
        assert_eq!(
            first.lock_records(&block).collect::<Vec<_>>(),
            vec![LockRecord::recovery(ConsensusEpoch::new(1))]
        );

        // Epoch 2: the frozen reports tag the block R and hold the re-vote
        let t = first.report_ledger();
        assert_eq!(
            t.status(&certified_at(&index, block)),
            Some(CertifiedStatus::Recovery)
        );
        let g = ResidualVotes::derive(&t, votes.entries());
        assert!(g.contains(&certified_at(&index, block), ResidualKind::First));
        let selected: Vec<_> = (1..=3).map(|i| reported_by(i, &t, &g)).collect();
        let second = build_state(&first, &selected, &index, rules_of(2)).unwrap();
        assert_eq!(
            second.lock_records(&block).collect::<Vec<_>>(),
            vec![
                LockRecord::recovery(ConsensusEpoch::new(1)),
                LockRecord::recovery(ConsensusEpoch::new(2)),
            ]
        );
        assert_eq!(second.retained_kind(&block), RetainedKind::Recovery);
        assert_ne!(second.state_hash(), first.state_hash());

        // Without the re-vote the inherited record is carried alone
        let no_votes = ResidualVotes::new();
        let silent: Vec<_> = (1..=3).map(|i| reported_by(i, &t, &no_votes)).collect();
        let carried = build_state(&first, &silent, &index, rules_of(2)).unwrap();
        assert_eq!(carried, first);
    }

    /// An inherited notarization lock, re-tagged N by every report, keeps the
    /// origin of the closure that created it. The block sits above an
    /// unfinalized parent, so Rule 3 does not promote it.
    #[test]
    fn an_inherited_notarization_keeps_its_origin() {
        let mut index = StubIndex::default();
        let parent = index.add(1, 1, BlockHash::ZERO);
        let block = index.add(1, 2, parent);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, block, CertifiedStatus::Notarized);
        let no_votes = ResidualVotes::new();
        let first = build_state(
            &EpochLedger::new(),
            &[report(&certified, &no_votes)],
            &index,
            rules_of(1),
        )
        .unwrap();
        let t = first.report_ledger();
        let second = build_state(&first, &[report(&t, &no_votes)], &index, rules_of(2)).unwrap();
        assert_eq!(
            second.lock_records(&block).collect::<Vec<_>>(),
            vec![LockRecord::notarization(ConsensusEpoch::new(1))]
        );
        assert_eq!(second, first);
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

    /// "Inherited state and discharge": a represented NC for a different
    /// block never supersedes an inherited recovery-only lock, whatever its
    /// votes; the lock and its retained descendants stay, and the new block
    /// is ineligible early work. A represented notarization lock is kept
    /// alongside a new NC at its position.
    #[test]
    fn a_represented_notarization_never_supersedes_an_inherited_recovery_lock() {
        let mut index = StubIndex::default();
        let base = index.add(1, 1, BlockHash::ZERO);
        let carried = index.add(1, 2, base);
        let child = index.add(1, 3, carried);
        let fresh = index.add(1, 2, base);
        let mut previous = EpochLedger::new();
        previous.finalize_genesis_block(slot(1, 1), PlacedBlock::new(base, BlockHash::ZERO));
        previous.keep(slot(1, 2), PlacedBlock::new(carried, base));
        previous.keep(slot(1, 3), PlacedBlock::new(child, carried));
        previous.set_lock_for_test(carried, RetainedKind::Recovery);
        previous.set_lock_for_test(child, RetainedKind::Recovery);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, fresh, CertifiedStatus::Notarized);
        let no_votes = ResidualVotes::new();
        let reports = [report(&certified, &no_votes)];

        let kept = build_state(&previous, &reports, &index, certificate_rules()).unwrap();
        assert_eq!(kept.notarized(&slot(1, 2)), vec![carried]);
        assert_eq!(kept.notarized(&slot(1, 3)), vec![child]);
        assert_eq!(kept.retained_kind(&carried), RetainedKind::Recovery);
        assert!(kept.finalized(&slot(1, 2)).is_none());

        previous.set_lock_for_test(carried, RetainedKind::Notarized);
        let both = build_state(&previous, &reports, &index, certificate_rules()).unwrap();
        assert_eq!(
            vec_sorted(&both.notarized(&slot(1, 2))),
            vec_sorted(&[carried, fresh])
        );
        assert_eq!(both.retained_kind(&fresh), RetainedKind::Notarized);
        assert_eq!(both.retained_kind(&carried), RetainedKind::Notarized);
    }

    /// The unique-branch variant: a sole preserved block is finalized with
    /// origin Derived, the walk continues up a unique prefix and stops at a
    /// position with two preserved rivals; a preserved rival at the same
    /// position prevents finalization even for a one-block branch. The
    /// certificate-only control carries the same blocks as locks, and the
    /// two states never hash alike.
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
        assert!(matches!(
            build_state(
                &EpochLedger::new(),
                &[report(&certified, &residual)],
                &index,
                certificate_rules()
            ),
            Err(BuildStateError::MissingAncestry(_, missing)) if missing == parent
        ));
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
            previous.set_lock_for_test(hash, RetainedKind::Recovery);
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
        assert_eq!(recovered.notarized(&slot(1, 1)), vec![block]);
        assert_eq!(notarized.finalized(&slot(1, 1)), Some(block));
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
    fn certificate_rules() -> BuildRules {
        rules_of(1)
    }

    /// The rules of the closure of an epoch
    fn rules_of(epoch: u64) -> BuildRules {
        BuildRules {
            many: MANY,
            epoch: ConsensusEpoch::new(epoch),
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
