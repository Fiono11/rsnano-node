use std::collections::{BTreeMap, BTreeSet};

use rsnano_types::{Account, Amount, Blake2HashBuilder, BlockHash, PublicKey};

use super::{CertifiedBlock, CertifiedState, ResidualVotes};

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

    fn parent(&self) -> Option<AccountSlot> {
        self.height
            .checked_sub(1)
            .filter(|height| *height > 0)
            .map(|height| AccountSlot::new(self.account, height))
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpochLedger {
    /// The finalized block at a slot, by account finalization or by the
    /// epoch decision
    finalized: BTreeMap<AccountSlot, PlacedBlock>,
    /// The conflicting blocks a slot kept: checkpoint-notarized, provisional,
    /// with no application effect
    notarized: BTreeMap<AccountSlot, BTreeSet<PlacedBlock>>,
}

impl EpochLedger {
    pub fn new() -> Self {
        Self::default()
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

    /// The blocks a slot kept, with the branch each continues
    fn notarized_slots(&self) -> impl Iterator<Item = (&AccountSlot, &BTreeSet<PlacedBlock>)> {
        self.notarized.iter()
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
        let mut builder = Blake2HashBuilder::new().update(b"RAI epoch state");
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
                    .update(b"n")
                    .update(slot.account.as_bytes())
                    .update(slot.height.to_le_bytes())
                    .update(block.hash.as_bytes())
                    .update(block.previous.as_bytes());
            }
        }
        builder.build()
    }

    /// RAI: seeds the closed genesis state `S_G`: every account at the
    /// block it stood at when the epochs started. Those positions are
    /// finalized, and rule 1 never rolls them back.
    pub fn finalize_genesis(&mut self, slot: AccountSlot, hash: BlockHash) {
        self.finalize(slot, PlacedBlock::new(hash, BlockHash::ZERO));
    }

    fn finalize(&mut self, slot: AccountSlot, block: PlacedBlock) {
        self.finalized.insert(slot, block);
        // A finalized position keeps no conflicting survivor
        self.notarized.remove(&slot);
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
            for (block, _, record) in report.residual.entries() {
                placements.entry(block.hash).or_insert(BlockPlacement {
                    slot: AccountSlot::new(block.account, block.height),
                    previous: record.previous,
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

/// RAI, "Deriving the epoch state from reports": `BuildState(S_{e-1}, Q_e)`.
/// Every validator that accepts a proposal reconstructs the selected reports
/// and derives this same state, so the proposal carries only its hash.
///
/// The rules are applied to a fixed point: what the predecessor finalized
/// stays finalized; a block the reports show finalized is finalized with its
/// unresolved ancestor prefix; a slot with one surviving block is
/// checkpoint-finalized, which may in turn leave its parent slot with one
/// survivor; a slot with several keeps them all as checkpoint-notarized,
/// without application effect; and a provisional block of the predecessor
/// that no longer appears is rolled back.
pub fn build_state(
    previous: &EpochLedger,
    selection: &[SelectedReport],
    index: &dyn BlockIndex,
    many: Amount,
) -> EpochLedger {
    let mut ledger = EpochLedger::new();

    // Rule 1: the predecessor's finalized state is never rolled back
    for (slot, block) in previous.finalized_slots() {
        ledger.finalize(*slot, *block);
    }

    // The candidates: what the reports show certified, and what a single
    // selected reporter supported with a vote its certified state does not
    // summarize (Include_Q). Rule 2 drops what this validator can not place.
    let mut candidates: BTreeMap<AccountSlot, BTreeSet<BlockHash>> = BTreeMap::new();
    let mut final_visible: BTreeSet<BlockHash> = BTreeSet::new();
    let place = |hash: BlockHash, candidates: &mut BTreeMap<_, BTreeSet<_>>| {
        if let Some(placement) = index.placement(&hash) {
            candidates.entry(placement.slot).or_default().insert(hash);
        }
    };
    for report in selection {
        for (block, entry) in report.certified.entries() {
            place(block.hash, &mut candidates);
            if entry.status.is_finalized() {
                final_visible.insert(block.hash);
            }
        }
        for block in report.residual.supported() {
            place(block.hash, &mut candidates);
        }
    }
    // Rule 1: "Inherited provisional forks are not omitted merely because the
    // current reports contain no new vote for them." A block the predecessor
    // kept is a candidate of this epoch too, and rule 5 removes it only when
    // a finalized branch excludes it: a promised recovery tip can not
    // disappear through report omission.
    for (slot, blocks) in previous.notarized_slots() {
        for block in blocks {
            candidates.entry(*slot).or_default().insert(block.hash);
        }
    }

    // Rule 2: a block incompatible with an already finalized position is
    // excluded, and so is one whose parent slot finalized a different block
    let compatible = |ledger: &EpochLedger, slot: &AccountSlot, hash: &BlockHash| -> bool {
        if let Some(finalized) = ledger.finalized(slot) {
            return finalized == *hash;
        }
        let Some(placement) = index.placement(hash) else {
            return false;
        };
        match slot.parent() {
            Some(parent) => match ledger.finalized(&parent) {
                Some(finalized) => placement.previous == finalized,
                None => true,
            },
            None => true,
        }
    };

    let placed = |hash: &BlockHash| {
        PlacedBlock::new(
            *hash,
            index
                .placement(hash)
                .map(|placement| placement.previous)
                .unwrap_or(BlockHash::ZERO),
        )
    };

    // RAI, before any pruning: the mandatory recovery targets, which
    // preserve a finalization the reports do not show because it was
    // assembled after they were signed.
    //
    // V_Q(a,v) are the hashes at the slot whose notarization certificate the
    // selected reports make visible; a finalization annotation establishes
    // the certificate too, so every certified status counts. A_Q(a,v) are
    // the hashes f + p + 1 of the selected weight first voted: enough to
    // cover the q_fast first voters of a fast certificate nobody reported.
    //
    // A unique member of V_Q is protected because a final voter records the
    // certificate before it final votes, so a certificate assembled later
    // rests on it. When no certificate is visible at all, a unique member of
    // A_Q is protected in its place. A slot with two of either is left to the
    // ordinary rules: a conflict there cannot have finalized (Lemma 3.3).
    let mut protect: BTreeMap<AccountSlot, BlockHash> = BTreeMap::new();
    for (slot, hashes) in &candidates {
        let certified_visible: Vec<BlockHash> = hashes
            .iter()
            .copied()
            .filter(|hash| {
                selection.iter().any(|report| {
                    report
                        .certified
                        .status(&CertifiedBlock::new(slot.account, slot.height, *hash))
                        .is_some()
                })
            })
            .collect();
        let target = match certified_visible.as_slice() {
            [unique] => Some(*unique),
            [] => {
                let first_voted: Vec<BlockHash> = hashes
                    .iter()
                    .copied()
                    .filter(|hash| {
                        let block = CertifiedBlock::new(slot.account, slot.height, *hash);
                        // A reporter counts once however many votes it
                        // recorded, and the selection holds distinct
                        // identities, so the set guards against a repeat
                        let mut counted: BTreeSet<PublicKey> = BTreeSet::new();
                        let mut weight = Amount::ZERO;
                        for report in selection {
                            if report.residual.first_votes().any(|voted| *voted == block)
                                && counted.insert(report.reporter)
                            {
                                weight = weight.checked_add(report.weight).unwrap_or(Amount::MAX);
                            }
                        }
                        weight >= many
                    })
                    .collect();
                match first_voted.as_slice() {
                    [unique] => Some(*unique),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(target) = target {
            protect.insert(*slot, target);
        }
    }

    // Rule 3: a block the reports show finalized is finalized here, and so is
    // every mandatory recovery target, each with the unresolved ancestors it
    // rests on
    for (slot, hashes) in &candidates {
        for hash in hashes {
            let mandatory = final_visible.contains(hash) || protect.get(slot) == Some(hash);
            if mandatory && compatible(&ledger, slot, hash) {
                finalize_with_ancestors(&mut ledger, index, *slot, *hash);
            }
        }
    }

    // Rules 4 and 5 to a fixed point: a slot with one surviving compatible
    // block is checkpoint-finalized, which can leave the slot after it with
    // one survivor in turn
    loop {
        let mut changed = false;
        for (slot, hashes) in &candidates {
            if ledger.finalized(slot).is_some() {
                continue;
            }
            let surviving: Vec<BlockHash> = hashes
                .iter()
                .copied()
                .filter(|hash| compatible(&ledger, slot, hash))
                .collect();
            if let [unique] = surviving.as_slice() {
                // Rule 4: with the eligible unresolved ancestor prefix, so a
                // unique child selects the branch its parent belongs to
                finalize_with_ancestors(&mut ledger, index, *slot, *unique);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Rule 5: the slots that kept more than one compatible block keep them
    // all, without application effect
    for (slot, hashes) in &candidates {
        if ledger.finalized(slot).is_some() {
            continue;
        }
        for hash in hashes {
            if compatible(&ledger, slot, hash) {
                ledger.keep(*slot, placed(hash));
            }
        }
    }

    ledger
}

/// RAI, ancestral finalization closure: finalizing a block finalizes the
/// unresolved notarized ancestors it rests on, walking back until the first
/// ancestor that is already finalized. An ancestor slot that finalized a
/// different block is left alone: rule 1 protects it, and a block resting on
/// it would not have been compatible in the first place.
fn finalize_with_ancestors(
    ledger: &mut EpochLedger,
    index: &dyn BlockIndex,
    slot: AccountSlot,
    hash: BlockHash,
) {
    let mut at = (slot, hash);
    loop {
        if ledger.is_finalized(&at.0, &at.1) {
            return;
        }
        let Some(placement) = index.placement(&at.1) else {
            ledger.finalize(at.0, PlacedBlock::new(at.1, BlockHash::ZERO));
            return;
        };
        ledger.finalize(at.0, PlacedBlock::new(at.1, placement.previous));
        if placement.previous.is_zero() {
            return;
        }
        let Some(parent) = at.0.parent() else {
            return;
        };
        if ledger.finalized(&parent).is_some() {
            return;
        }
        at = (parent, placement.previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::{CertifiedBlock, CertifiedStatus, ResidualKind};
    use rsnano_types::PrivateKey;
    use std::collections::HashMap;

    /// A slot whose only included block is the one the reports certified is
    /// checkpoint-finalized (rule 4)
    #[test]
    fn a_unique_survivor_is_finalized() {
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

        assert_eq!(ledger.finalized(&slot(1, 1)), Some(block));
        assert!(ledger.notarized(&slot(1, 1)).is_empty());
        assert_eq!(ledger.finalized_count(), 1);
    }

    /// Two conflicting blocks at one slot are both kept, without application
    /// effect, and neither is finalized (rule 5)
    #[test]
    fn conflicting_survivors_are_checkpoint_notarized() {
        let mut index = StubIndex::default();
        let one = index.add(1, 1, BlockHash::ZERO);
        let other = index.add(1, 1, BlockHash::ZERO);
        let mut certified = CertifiedState::new();
        certify(&mut certified, &index, one, CertifiedStatus::Notarized);
        certify(&mut certified, &index, other, CertifiedStatus::Notarized);
        let residual = ResidualVotes::new();

        let ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &residual)],
            &index,
        );

        assert_eq!(ledger.finalized(&slot(1, 1)), None);
        assert_eq!(ledger.notarized(&slot(1, 1)), vec_sorted(&[one, other]));
        assert!(ledger.frontiers().is_empty(), "no application effect");
    }

    /// Include_Q: one selected reporter's residual support is enough to make
    /// RAI: a unique certified-visible block is a mandatory recovery target,
    /// so a single residual conflict cannot defeat it. A correct final voter
    /// records the notarization certificate before it final votes, so a
    /// finalization assembled after the reports were signed rests on this
    /// block, and keeping it merely provisional would lose that obligation.
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

        assert_eq!(ledger.finalized(&slot(1, 1)), Some(certified_block));
        assert_eq!(ledger.notarized(&slot(1, 1)), vec![]);
    }

    /// RAI: with no certificate visible at all, f + p + 1 of the selected
    /// weight having first voted one block stands for a fast finalization
    /// certificate nobody reported, and recovers it
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

        assert_eq!(ledger.finalized(&slot(1, 1)), Some(hidden));
    }

    /// Below the threshold nothing is recovered and the slot keeps both
    /// blocks: two reporters are short of f + p + 1
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
        assert_eq!(ledger.notarized(&slot(1, 1)), vec_sorted(&[one, other]));
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
        assert_eq!(
            ledger.notarized(&slot(1, 1)),
            vec_sorted(&[supported, other])
        );
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

    /// Rule 4 to a fixed point: a unique block at the next slot selects the
    /// branch its parent belongs to, and the sibling of that parent goes
    #[test]
    fn a_unique_child_selects_its_parent_branch() {
        let mut index = StubIndex::default();
        let parent = index.add(1, 1, BlockHash::ZERO);
        let sibling = index.add(1, 1, BlockHash::ZERO);
        let child = index.add(1, 2, parent);
        let mut certified = CertifiedState::new();
        for hash in [parent, sibling, child] {
            certify(&mut certified, &index, hash, CertifiedStatus::Notarized);
        }
        let residual = ResidualVotes::new();

        let ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &residual)],
            &index,
        );

        // The child is the only block at slot 2, so it is finalized; its
        // parent is then the only compatible block at slot 1
        assert_eq!(ledger.finalized(&slot(1, 2)), Some(child));
        assert_eq!(ledger.finalized(&slot(1, 1)), Some(parent));
        assert!(ledger.notarized(&slot(1, 1)).is_empty());
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

        let mut certified = CertifiedState::new();
        for hash in [first, second, conflicting, sibling] {
            certify(&mut certified, &index, hash, CertifiedStatus::Notarized);
        }
        // The block above the conflict is represented but its slot is not
        // decided, so the account's chain has a gap at slot 3
        let residual = ResidualVotes::new();
        let mut ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &residual)],
            &index,
        );
        assert_eq!(
            ledger.notarized(&slot(1, 3)).len(),
            2,
            "slot 3 is undecided"
        );
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

    /// Rule 2: a block the derivation can not place is not included.
    #[test]
    fn a_block_without_a_placement_is_not_included() {
        let index = StubIndex::default();
        let mut certified = CertifiedState::new();
        certified.certify(
            CertifiedBlock::new(Account::from(1), 1, BlockHash::from(999)),
            BlockHash::ZERO,
            CertifiedStatus::Notarized,
        );
        let residual = ResidualVotes::new();

        let ledger = derive(
            &EpochLedger::new(),
            &[report(&certified, &residual)],
            &index,
        );

        assert_eq!(ledger.finalized_count(), 0);
        assert_eq!(ledger.notarized_count(), 0);
    }

    /// RAI: the reports themselves place every block they name, so a
    /// validator derives the same state whether or not it holds the bodies.
    /// This is what `ReportIndex` is for; without it two validators holding
    /// different bodies would derive different states from one selection.
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

    /*
     * Test helpers
     */

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
        build_state(previous, selection, index, MANY)
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
