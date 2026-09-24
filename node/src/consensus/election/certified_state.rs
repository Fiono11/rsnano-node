use std::collections::BTreeMap;

use rsnano_types::{Account, Blake2HashBuilder, BlockHash, ConsensusEpoch, PublicKey};

/// RAI: what a validator has locally constructed for a block of one epoch.
/// Status strength is F > N > R: inherited recovery protection is upgraded
/// when a stronger certificate becomes available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CertifiedStatus {
    /// Inherited unresolved protection; neither notarization nor finality.
    Recovery,
    /// A vote-notarization certificate: the block is complete
    Notarized,
    /// A finalization certificate, normal or fast. The inventory does not
    /// tell the two apart: a validator that finalized by the normal
    /// certificate and erased the instance never sees the last first votes
    /// of the fast one, and two correct inventories would then differ for
    /// good. A checkpoint asks only whether a block is finalized.
    Finalized,
}

impl CertifiedStatus {
    pub fn as_byte(self) -> u8 {
        match self {
            CertifiedStatus::Recovery => 2,
            CertifiedStatus::Notarized => 0,
            CertifiedStatus::Finalized => 1,
        }
    }

    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            2 => Some(CertifiedStatus::Recovery),
            0 => Some(CertifiedStatus::Notarized),
            1 => Some(CertifiedStatus::Finalized),
            _ => None,
        }
    }

    pub fn is_finalized(self) -> bool {
        matches!(self, CertifiedStatus::Finalized)
    }
}

/// RAI: the certificates a validator can assemble for one block from the
/// signed votes it retains: a notarization certificate, a normal final
/// certificate, a fast final certificate
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CertificateKinds {
    pub nc: bool,
    pub fc: bool,
    pub ff: bool,
}

/// A block of the certified tree: where it sits and which block it is
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CertifiedBlock {
    pub account: Account,
    pub height: u64,
    pub hash: BlockHash,
}

impl CertifiedBlock {
    pub fn new(account: Account, height: u64, hash: BlockHash) -> Self {
        Self {
            account,
            height,
            hash,
        }
    }
}

/// RAI: what a validator constructed for a block, and the parent the block
/// names. "The inventory includes the required account ancestry": two blocks
/// at one account slot are told apart by the branch each continues, and the
/// epoch derivation places every candidate by its parent. Carrying it here
/// makes `BuildState` a function of the selected reports alone, so two
/// validators that reconstructed the same reports derive the same state
/// whether or not each holds every body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Certification {
    pub status: CertifiedStatus,
    /// The parent the block names; zero when it opens the account
    pub previous: BlockHash,
}

/// RAI, "Certified-state reports and reconciliation": the canonical certified
/// block tree of one validator for one epoch. It includes inherited recovery
/// protection and known notarized/finalized blocks, with required ancestry.
/// Entries carry status and placement metadata, not votes or block bodies.
///
/// The root is an order-independent hash of the entries, so two validators
/// that have constructed the same certificates hold the same root whatever
/// order the votes reached them in. A report signs the root of a frozen
/// snapshot; the live tree goes on growing as gossip delivers more votes,
/// which is what lets a later common state bridge to a historical root.
#[derive(Clone, Debug, Default)]
pub struct CertifiedState {
    entries: BTreeMap<CertifiedBlock, Certification>,
    hashes: rustc_hash::FxHashSet<BlockHash>,
    /// A canonical cryptographic commitment, cached until entries change.
    root_cache: std::sync::OnceLock<BlockHash>,
}

impl PartialEq for CertifiedState {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}
impl Eq for CertifiedState {}

impl CertifiedState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn status_counts(&self) -> [usize; 3] {
        let mut counts = [0; 3];
        for entry in self.entries.values() {
            counts[match entry.status {
                CertifiedStatus::Recovery => 0,
                CertifiedStatus::Notarized => 1,
                CertifiedStatus::Finalized => 2,
            }] += 1;
        }
        counts
    }

    pub fn contains_hash(&self, hash: &BlockHash) -> bool {
        self.hashes.contains(hash)
    }

    pub fn status(&self, block: &CertifiedBlock) -> Option<CertifiedStatus> {
        self.entries.get(block).map(|held| held.status)
    }

    /// The status of a block and the parent it names
    pub fn certification(&self, block: &CertifiedBlock) -> Option<Certification> {
        self.entries.get(block).copied()
    }

    pub fn entries(&self) -> impl DoubleEndedIterator<Item = (&CertifiedBlock, &Certification)> {
        self.entries.iter()
    }

    /// Records a certified block, or upgrades the status of one already
    /// there. A status never weakens: a validator that has constructed a
    /// finalization certificate does not lose it. The parent is fixed by the
    /// block body, so the first one recorded stands.
    pub fn certify(
        &mut self,
        block: CertifiedBlock,
        previous: BlockHash,
        status: CertifiedStatus,
    ) -> bool {
        match self.entries.get(&block).copied() {
            Some(held) if held.status >= status => false,
            held => {
                let entry = Certification {
                    status,
                    previous: held.map(|held| held.previous).unwrap_or(previous),
                };
                self.root_cache.take();
                self.hashes.insert(block.hash);
                self.entries.insert(block, entry);
                true
            }
        }
    }

    /// The root the report signs
    pub fn root(&self) -> BlockHash {
        *self.root_cache.get_or_init(|| {
            let mut entries: Vec<_> = self
                .entries
                .iter()
                .map(|(block, entry)| {
                    (
                        block.hash,
                        entry.status.as_byte(),
                        block.account,
                        block.height,
                        entry.previous,
                    )
                })
                .collect();
            entries.sort_unstable();
            let mut builder = Blake2HashBuilder::new().update(b"RAI report ledger v2 RNF");
            for (hash, status, account, height, previous) in entries {
                // Placement is redundant for a validated block hash, but
                // remains authenticated in this experimental wire encoding.
                builder = builder
                    .update(hash.as_bytes())
                    .update([status])
                    .update(account.as_bytes())
                    .update(height.to_le_bytes())
                    .update(previous.as_bytes());
            }
            builder.build()
        })
    }

    /// F applies to the selected inherited prefix as well as the explicit
    /// certificate target. Competing unresolved branches leave the live ledger.
    pub fn project_final_prefixes(&mut self) {
        let mut targets = Vec::new();
        let mut needs_pruning = false;
        let mut previous_slot = None;
        for (block, entry) in &self.entries {
            let slot = (block.account, block.height);
            needs_pruning |= previous_slot == Some(slot);
            previous_slot = Some(slot);
            if block.height > 1 && !entry.previous.is_zero() {
                let parent = CertifiedBlock::new(block.account, block.height - 1, entry.previous);
                match self.entries.get(&parent) {
                    Some(held) if entry.status.is_finalized() && !held.status.is_finalized() => {
                        targets.push((*block, *entry));
                    }
                    None => needs_pruning = true,
                    _ => {}
                }
            }
        }
        for (mut block, mut entry) in targets {
            while block.height > 1 && !entry.previous.is_zero() {
                let parent = CertifiedBlock::new(block.account, block.height - 1, entry.previous);
                let Some(held) = self.entries.get(&parent).copied() else {
                    break;
                };
                if held.status.is_finalized() {
                    break;
                }
                self.certify(parent, held.previous, CertifiedStatus::Finalized);
                block = parent;
                entry = held;
            }
        }
        // A parent-closed projection with one block per position cannot
        // contain an excluded branch. Avoid rebuilding an all-history index.
        if !needs_pruning {
            return;
        }
        let finals: std::collections::BTreeMap<_, _> = self
            .entries
            .iter()
            .filter(|(_, e)| e.status.is_finalized())
            .map(|(b, _)| ((b.account, b.height), b.hash))
            .collect();
        let excluded: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, e)| !e.status.is_finalized())
            .filter_map(|(candidate, _)| {
                let mut current = *candidate;
                loop {
                    if let Some(final_hash) = finals.get(&(current.account, current.height)) {
                        return (*final_hash != current.hash).then_some(*candidate);
                    }
                    let entry = self.entries.get(&current)?;
                    if current.height <= 1 || entry.previous.is_zero() {
                        return None;
                    }
                    current =
                        CertifiedBlock::new(current.account, current.height - 1, entry.previous);
                }
            })
            .collect();
        for block in excluded {
            self.remove(&block);
        }
    }

    pub fn has_unique_hashes(&self) -> bool {
        self.entries.len() == self.hashes.len()
    }

    /// The digest one tagged entry contributes to a set sketch: two states
    /// hold the same entry exactly when they hold the same digest
    pub fn entry_digest(block: &CertifiedBlock, entry: &Certification) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI ledger entry")
            .update(block.hash.as_bytes())
            .update([entry.status.as_byte()])
            .update(block.account.as_bytes())
            .update(block.height.to_le_bytes())
            .update(entry.previous.as_bytes())
            .build()
    }

    /// Every entry with the digest that stands for it
    pub fn digests(&self) -> impl Iterator<Item = (BlockHash, CertifiedBlock, Certification)> + '_ {
        self.entries
            .iter()
            .map(|(block, entry)| (Self::entry_digest(block, entry), *block, *entry))
    }

    /// RAI: "It may add or remove blocks and change status annotations."
    /// The canonical edits that take this state to the other one. A live
    /// certified state only ever grows, but two validators' states are not
    /// comparable in general - each has constructed certificates the other
    /// has not - so a difference between two states a responder knows has to
    /// be able to drop an entry as well as add one. Without that, two
    /// incomparable historical states have no bridge at all and every
    /// reconciliation between them falls back to a full transfer.
    pub fn difference(&self, target: &CertifiedState) -> CertifiedDelta {
        let mut delta = CertifiedDelta::default();
        for (block, entry) in &target.entries {
            if self.entries.get(block) != Some(entry) {
                delta.added.push((*block, *entry));
            }
        }
        for block in self.entries.keys() {
            if !target.entries.contains_key(block) {
                delta.removed.push(*block);
            }
        }
        delta
    }

    /// Applies a difference. The caller checks the resulting root against the
    /// signed one, which is the whole of the reconciliation check.
    pub fn apply(&mut self, delta: &CertifiedDelta) {
        for block in &delta.removed {
            self.remove(block);
        }
        for (block, entry) in &delta.added {
            self.set(*block, *entry);
        }
    }

    /// Records an entry exactly as given, weaker status included: a
    /// reconstruction rebuilds the reporter's state, which is not this
    /// node's own and is not required to grow
    pub fn set(&mut self, block: CertifiedBlock, entry: Certification) {
        self.hashes.insert(block.hash);
        self.entries.insert(block, entry);
        self.root_cache.take();
    }

    pub fn remove(&mut self, block: &CertifiedBlock) {
        let unique = self.has_unique_hashes();
        if self.entries.remove(block).is_some() {
            self.root_cache.take();
            if unique || !self.entries.keys().any(|b| b.hash == block.hash) {
                self.hashes.remove(&block.hash);
            }
        }
    }
}

/// RAI: the reconstructive difference between two certified states
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CertifiedDelta {
    /// Entries the target holds and the source does not, or holds otherwise
    pub added: Vec<(CertifiedBlock, Certification)>,
    /// Entries the source holds and the target does not
    pub removed: Vec<CertifiedBlock>,
}

impl CertifiedDelta {
    pub fn len(&self) -> usize {
        self.added.len() + self.removed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// RAI: which of its own votes a reporter records for a block whose
/// certified status does not yet summarize them
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResidualKind {
    /// A first vote, for a block with no notarization certificate in the
    /// certified state. Kept apart from `Notar` because only first votes
    /// build a fast finalization certificate, so only they can stand for a
    /// hidden one in the checkpoint recovery rule.
    First,
    /// Notarization support added under the second-look rule, for a block
    /// with no notarization certificate in the certified state
    Notar,
    /// A final vote for a block the certified state holds as notarized only
    Final,
}

impl ResidualKind {
    /// The encoding a fetch of the object uses
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(ResidualKind::First),
            1 => Some(ResidualKind::Notar),
            2 => Some(ResidualKind::Final),
            _ => None,
        }
    }

    pub fn as_byte(self) -> u8 {
        match self {
            ResidualKind::First => 0,
            ResidualKind::Notar => 1,
            ResidualKind::Final => 2,
        }
    }
}

/// RAI: the reporter-local vote evidence an epoch's certified state does not
/// yet reflect. It is what makes a block report-visible when no certificate
/// for it could be constructed before the report was signed (Lemma 3.7), and
/// it holds this validator's own votes only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResidualVotes {
    /// The parent each voted block names, by the vote recorded for it
    entries: BTreeMap<(CertifiedBlock, ResidualKind), BlockHash>,
    digest: [u8; 32],
}

impl ResidualVotes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Exact G = V \ keys(T): no vote for a hash under any R/N/F tag
    /// remains in G. The evidence records retain vote kinds independently;
    /// only a verified first vote is eligible for recovery support.
    pub fn derive(
        certified: &CertifiedState,
        votes: impl IntoIterator<Item = (CertifiedBlock, ResidualKind, BlockHash)>,
    ) -> Self {
        let mut residual = Self::new();
        for (block, kind, previous) in votes {
            let summarized = certified.contains_hash(&block.hash);
            if !summarized {
                residual.record(block, previous, kind);
            }
        }
        residual
    }

    /// Account votes are single-support: a G hash is counted only once
    /// its reporter's first vote is available, not just a final-vote record.
    pub fn first_evidence_complete(&self) -> bool {
        let first: std::collections::BTreeSet<_> = self.first_votes().map(|b| b.hash).collect();
        self.entries.keys().all(|(b, _)| first.contains(&b.hash))
    }

    pub fn hash_count(&self) -> usize {
        self.entries
            .keys()
            .map(|(b, _)| b.hash)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, block: &CertifiedBlock, kind: ResidualKind) -> bool {
        self.entries.contains_key(&(*block, kind))
    }

    /// The blocks this reporter supported with a first or a notarization
    /// vote: what `M_Q` counts for candidate membership
    pub fn supported(&self) -> impl Iterator<Item = &CertifiedBlock> {
        self.entries
            .keys()
            .filter(|(_, kind)| matches!(kind, ResidualKind::First | ResidualKind::Notar))
            .map(|(block, _)| block)
    }

    /// The blocks this reporter first voted: what `FirstCount_Q` counts, and
    /// the only votes a hidden fast finalization certificate can rest on
    pub fn first_votes(&self) -> impl Iterator<Item = &CertifiedBlock> {
        self.entries
            .keys()
            .filter(|(_, kind)| *kind == ResidualKind::First)
            .map(|(block, _)| block)
    }

    /// The votes recorded, in canonical order, each with the parent its
    /// block names: what the root commits to
    pub fn entries(
        &self,
    ) -> impl DoubleEndedIterator<Item = (CertifiedBlock, ResidualKind, BlockHash)> + '_ {
        self.entries
            .iter()
            .map(|((block, kind), previous)| (*block, *kind, *previous))
    }

    /// The parent a recorded block names, whichever vote recorded it
    pub fn previous(&self, block: &CertifiedBlock) -> Option<BlockHash> {
        self.entries
            .iter()
            .find(|((held, _), _)| held == block)
            .map(|(_, previous)| *previous)
    }

    pub fn record(
        &mut self,
        block: CertifiedBlock,
        previous: BlockHash,
        kind: ResidualKind,
    ) -> bool {
        if self.entries.contains_key(&(block, kind)) {
            return false;
        }
        self.toggle(&Self::entry_digest(&block, kind, previous));
        self.entries.insert((block, kind), previous);
        true
    }

    /// Drops a record: what a sketch exchange found on this side only
    pub fn remove(&mut self, block: &CertifiedBlock, kind: ResidualKind) -> bool {
        let Some(previous) = self.entries.remove(&(*block, kind)) else {
            return false;
        };
        self.toggle(&Self::entry_digest(block, kind, previous));
        true
    }

    /// The digest one record contributes to the root, and the key a sketch
    /// reconciles on: two objects hold the same record exactly when they
    /// hold the same digest
    pub fn entry_digest(
        block: &CertifiedBlock,
        kind: ResidualKind,
        previous: BlockHash,
    ) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI residual vote")
            .update(block.account.as_bytes())
            .update(block.height.to_le_bytes())
            .update(block.hash.as_bytes())
            .update(previous.as_bytes())
            .update([kind.as_byte()])
            .build()
    }

    /// Every record with the digest that stands for it
    pub fn digests(
        &self,
    ) -> impl Iterator<Item = (BlockHash, CertifiedBlock, ResidualKind, BlockHash)> + '_ {
        self.entries().map(|(block, kind, previous)| {
            (
                Self::entry_digest(&block, kind, previous),
                block,
                kind,
                previous,
            )
        })
    }

    fn toggle(&mut self, digest: &BlockHash) {
        for (d, e) in self.digest.iter_mut().zip(digest.as_bytes()) {
            *d ^= e;
        }
    }

    pub fn root(&self) -> BlockHash {
        // G commits distinct block hashes. Vote kinds and placements are
        // separately validated evidence, not authenticated-set members.
        let hashes: std::collections::BTreeSet<_> =
            self.entries.keys().map(|(b, _)| b.hash).collect();
        let mut builder = Blake2HashBuilder::new().update(b"RAI report G v2 hashes");
        for hash in hashes {
            builder = builder.update(hash.as_bytes());
        }
        builder.build()
    }
}

/// RAI: what a report commits to. The signed message binds the epoch, the
/// old committee, the reporter and the two roots; the contents behind them
/// are reconstructed separately and checked against the roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportCommitment {
    pub epoch: ConsensusEpoch,
    /// The digest of the old committee, which issued the epoch's votes
    pub committee: BlockHash,
    /// d_{e-1}: the hash of the closed predecessor checkpoint the report is
    /// signed against
    pub predecessor: BlockHash,
    /// r_i, the certified-state root
    pub certified: BlockHash,
    /// g_i, the residual-vote root
    pub residual: BlockHash,
    pub reporter: PublicKey,
}

impl ReportCommitment {
    /// What the reporter signs
    pub fn payload(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI report")
            .update(self.epoch.as_u64().to_le_bytes())
            .update(self.committee.as_bytes())
            .update(self.predecessor.as_bytes())
            .update(self.certified.as_bytes())
            .update(self.residual.as_bytes())
            .update(self.reporter.as_bytes())
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ancestry_does_not_skip_conflicting_finality_pruning() {
        let parent = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(10));
        let child = CertifiedBlock::new(parent.account, 2, BlockHash::from(30));
        let mut state = CertifiedState::new();
        state.certify(parent, BlockHash::ZERO, CertifiedStatus::Finalized);
        state.certify(child, BlockHash::from(20), CertifiedStatus::Recovery);
        state.project_final_prefixes();
        assert_eq!(state.status(&child), None);
    }

    #[test]
    fn removing_a_duplicate_hash_placement_keeps_the_remaining_key() {
        let one = block(1);
        let alias = CertifiedBlock::new(Account::from(999), one.height, one.hash);
        let mut state = CertifiedState::new();
        state.certify(one, BlockHash::ZERO, CertifiedStatus::Recovery);
        state.certify(alias, BlockHash::ZERO, CertifiedStatus::Recovery);
        assert!(!state.has_unique_hashes());
        state.remove(&one);
        assert!(state.contains_hash(&one.hash));
        assert!(state.has_unique_hashes());
        state.remove(&alias);
        assert!(!state.contains_hash(&one.hash));
    }

    #[test]
    fn descendant_finality_upgrades_its_selected_r_prefix_and_removes_the_rival() {
        let parent = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(10));
        let rival = CertifiedBlock::new(parent.account, 1, BlockHash::from(20));
        let child = CertifiedBlock::new(parent.account, 2, BlockHash::from(30));
        let rival_child = CertifiedBlock::new(parent.account, 2, BlockHash::from(40));
        let mut live = CertifiedState::new();
        live.certify(parent, BlockHash::ZERO, CertifiedStatus::Recovery);
        live.certify(rival, BlockHash::ZERO, CertifiedStatus::Recovery);
        live.certify(rival_child, rival.hash, CertifiedStatus::Recovery);
        let frozen = live.clone();
        live.certify(child, parent.hash, CertifiedStatus::Finalized);
        live.project_final_prefixes();
        assert_eq!(live.status(&parent), Some(CertifiedStatus::Finalized));
        assert_eq!(live.status(&rival), None);
        assert_eq!(live.status(&rival_child), None);
        assert_eq!(frozen.status(&parent), Some(CertifiedStatus::Recovery));
        assert_eq!(frozen.len(), 3);
    }

    #[test]
    fn recovery_upgrade_does_not_mutate_a_frozen_report() {
        let mut live = CertifiedState::new();
        live.certify(block(1), parent(block(1)), CertifiedStatus::Recovery);
        assert!(!CertifiedStatus::Recovery.is_finalized());
        let frozen = live.clone();
        assert!(live.certify(block(1), parent(block(1)), CertifiedStatus::Notarized));
        assert_ne!(live.root(), frozen.root());
        assert!(live.certify(block(1), parent(block(1)), CertifiedStatus::Finalized));
        assert!(!live.certify(block(1), parent(block(1)), CertifiedStatus::Recovery));
        assert_eq!(frozen.status(&block(1)), Some(CertifiedStatus::Recovery));
        let delta = live.difference(&frozen);
        live.apply(&delta);
        assert_eq!(live, frozen);
    }

    #[test]
    fn every_t_status_excludes_every_vote_kind_from_g() {
        for status in [
            CertifiedStatus::Recovery,
            CertifiedStatus::Notarized,
            CertifiedStatus::Finalized,
        ] {
            let mut t = CertifiedState::new();
            t.certify(block(1), parent(block(1)), status);
            let votes = [
                ResidualKind::First,
                ResidualKind::Notar,
                ResidualKind::Final,
            ]
            .map(|kind| (block(1), kind, parent(block(1))));
            assert!(ResidualVotes::derive(&t, votes).is_empty());
        }
    }

    #[test]
    fn an_empty_state_has_a_root_of_its_own() {
        let state = CertifiedState::new();
        assert!(state.is_empty());
        assert_eq!(state.root(), CertifiedState::new().root());
        assert_ne!(state.root(), BlockHash::ZERO);
    }

    /// Two validators that constructed the same certificates hold the same
    /// root, whatever order the votes reached them in
    #[test]
    fn the_root_is_the_entries_and_not_their_order() {
        let mut one = CertifiedState::new();
        let mut other = CertifiedState::new();
        for i in 0..20 {
            one.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        for i in (0..20).rev() {
            other.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        assert_eq!(one.root(), other.root());

        // A status upgrade changes the root
        other.certify(block(3), parent(block(3)), CertifiedStatus::Finalized);
        assert_ne!(one.root(), other.root());
        one.certify(block(3), parent(block(3)), CertifiedStatus::Finalized);
        assert_eq!(one.root(), other.root());
    }

    /// A status never weakens: the certificate stays constructed
    #[test]
    fn a_status_is_upgraded_and_never_weakened() {
        let mut state = CertifiedState::new();
        assert!(state.certify(block(1), parent(block(1)), CertifiedStatus::Notarized));
        assert!(state.certify(block(1), parent(block(1)), CertifiedStatus::Finalized));
        assert_eq!(state.status(&block(1)), Some(CertifiedStatus::Finalized));
        // Back to notarized changes nothing
        assert!(!state.certify(block(1), parent(block(1)), CertifiedStatus::Notarized));
        assert_eq!(state.status(&block(1)), Some(CertifiedStatus::Finalized));
        assert_eq!(state.len(), 1);
    }

    /// The reconciliation check: apply the difference to the source state and
    /// the root has to come out as the signed one
    #[test]
    fn a_difference_reconstructs_the_target_root() {
        let mut source = CertifiedState::new();
        for i in 0..20 {
            source.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        let mut target = source.clone();
        for i in 20..25 {
            target.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        target.certify(block(2), parent(block(2)), CertifiedStatus::Finalized);

        let delta = source.difference(&target);
        assert_eq!(delta.added.len(), 6);
        assert!(delta.removed.is_empty());
        source.apply(&delta);
        assert_eq!(source.root(), target.root());
        assert_eq!(source.len(), target.len());
    }

    /// RAI: "It may add or remove blocks and change status annotations."
    /// Two validators' historical states are not comparable in general, so a
    /// difference has to drop what the target does not hold. Without that
    /// there is no bridge between them at all.
    #[test]
    fn a_difference_between_incomparable_states_removes_and_adds() {
        let mut source = CertifiedState::new();
        source.certify(block(1), parent(block(1)), CertifiedStatus::Notarized);
        source.certify(block(99), parent(block(99)), CertifiedStatus::Notarized);
        let mut target = CertifiedState::new();
        target.certify(block(1), parent(block(1)), CertifiedStatus::Finalized);
        target.certify(block(7), parent(block(7)), CertifiedStatus::Notarized);

        let delta = source.difference(&target);
        assert_eq!(delta.removed, vec![block(99)]);
        assert_eq!(delta.added.len(), 2);
        source.apply(&delta);
        assert_eq!(source.root(), target.root());

        // And back the other way, which a state that only grows could not do
        let mut ahead = CertifiedState::new();
        ahead.certify(block(1), parent(block(1)), CertifiedStatus::Finalized);
        let mut behind = CertifiedState::new();
        behind.certify(block(1), parent(block(1)), CertifiedStatus::Notarized);
        let delta = ahead.difference(&behind);
        ahead.apply(&delta);
        assert_eq!(ahead.root(), behind.root());
    }

    /// The parent a block names is part of what a report commits to: two
    /// validators that place one block on different branches hold different
    /// roots, so the derivation can not be fed two answers for one block
    #[test]
    fn the_root_commits_to_the_branch_a_block_continues() {
        let mut one = CertifiedState::new();
        one.certify(block(1), BlockHash::from(50), CertifiedStatus::Notarized);
        let mut other = CertifiedState::new();
        other.certify(block(1), BlockHash::from(51), CertifiedStatus::Notarized);
        assert_ne!(one.root(), other.root());
        assert_eq!(
            one.certification(&block(1)).unwrap().previous,
            BlockHash::from(50)
        );
    }

    /// A live state that has grown past the report keeps a bridge to it:
    /// the report's root is reconstructed from the difference
    #[test]
    fn a_grown_state_still_bridges_to_the_reported_one() {
        let mut reported = CertifiedState::new();
        for i in 0..10 {
            reported.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        let root = reported.root();
        let mut live = reported.clone();
        for i in 10..15 {
            live.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        // The live state is ahead, so it bridges the other way: a requester
        // whose own state is the smaller one reconstructs the report
        let mut requester = CertifiedState::new();
        for i in 0..8 {
            requester.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        let delta = requester.difference(&reported);
        requester.apply(&delta);
        assert_eq!(requester.root(), root);
    }

    #[test]
    fn residual_votes_hash_their_entries() {
        let mut one = ResidualVotes::new();
        let mut other = ResidualVotes::new();
        assert_eq!(one.root(), other.root());
        one.record(block(1), parent(block(1)), ResidualKind::First);
        one.record(block(2), parent(block(2)), ResidualKind::Final);
        other.record(block(2), parent(block(2)), ResidualKind::Final);
        other.record(block(1), parent(block(1)), ResidualKind::First);
        assert_eq!(one.root(), other.root());
        assert_eq!(one.len(), 2);
        assert!(one.contains(&block(1), ResidualKind::First));
        assert!(!one.contains(&block(1), ResidualKind::Final));
        assert_eq!(one.supported().collect::<Vec<_>>(), vec![&block(1)]);

        // Recording twice changes nothing
        assert!(!one.record(block(1), parent(block(1)), ResidualKind::First));
        assert_eq!(one.len(), 2);
    }

    /// A first vote and second-look notarization support are separate
    /// entries: only the first vote can witness a hidden fast certificate
    #[test]
    fn a_first_vote_is_told_apart_from_notarization_support() {
        let mut votes = ResidualVotes::new();
        votes.record(block(1), parent(block(1)), ResidualKind::First);
        votes.record(block(2), parent(block(2)), ResidualKind::Notar);
        assert_eq!(votes.len(), 2);

        // Both count as support for candidate membership
        assert_eq!(
            votes.supported().collect::<Vec<_>>(),
            vec![&block(1), &block(2)]
        );
        // Only the first vote counts for the recovery threshold
        assert_eq!(votes.first_votes().collect::<Vec<_>>(), vec![&block(1)]);
    }

    /// Evidence kinds remain separate, but G authenticates only block hashes.
    #[test]
    fn g_hashes_blocks_while_first_vote_evidence_stays_separate() {
        let mut first_only = ResidualVotes::new();
        first_only.record(block(1), parent(block(1)), ResidualKind::First);
        let mut notar_only = ResidualVotes::new();
        notar_only.record(block(1), parent(block(1)), ResidualKind::Notar);
        assert_eq!(first_only.root(), notar_only.root());
        assert_eq!(notar_only.first_votes().count(), 0);

        let mut both = ResidualVotes::new();
        both.record(block(1), parent(block(1)), ResidualKind::First);
        both.record(block(1), parent(block(1)), ResidualKind::Notar);
        assert_eq!(both.len(), 2);
        assert_eq!(both.root(), first_only.root());
        assert_eq!(both.first_votes().collect::<Vec<_>>(), vec![&block(1)]);
    }

    /// The residual object is what the inventory does not summarize: a
    /// first or notarization vote for a block with no certified status, a
    /// final vote for a block not finalized. Derived from the same votes and
    /// inventory on either side, it hashes the same.
    #[test]
    fn the_residual_is_derived_from_the_votes_the_inventory_does_not_summarize() {
        let mut certified = CertifiedState::new();
        certified.certify(block(1), parent(block(1)), CertifiedStatus::Notarized);
        certified.certify(block(2), parent(block(2)), CertifiedStatus::Finalized);
        let votes = vec![
            // Summarized by the notarization
            (block(1), ResidualKind::First, parent(block(1))),
            // A final vote on a notarized, not finalized block remains
            (block(1), ResidualKind::Final, parent(block(1))),
            // Summarized by the finalization
            (block(2), ResidualKind::First, parent(block(2))),
            (block(2), ResidualKind::Final, parent(block(2))),
            // No certificate at all: both remain
            (block(3), ResidualKind::First, parent(block(3))),
            (block(3), ResidualKind::Notar, parent(block(3))),
        ];
        let derived = ResidualVotes::derive(&certified, votes.clone());
        assert_eq!(derived.len(), 2);
        assert!(!derived.contains(&block(1), ResidualKind::Final));
        assert!(derived.contains(&block(3), ResidualKind::First));
        assert!(derived.contains(&block(3), ResidualKind::Notar));
        assert!(!derived.contains(&block(1), ResidualKind::First));
        assert!(!derived.contains(&block(2), ResidualKind::Final));
        // Order of the votes does not matter
        let mut reversed = votes.clone();
        reversed.reverse();
        assert_eq!(
            ResidualVotes::derive(&certified, reversed).root(),
            derived.root()
        );
        // A vote missing on one side changes the root
        assert_ne!(
            ResidualVotes::derive(&certified, votes[..4].to_vec()).root(),
            derived.root()
        );
    }

    #[test]
    fn the_signed_payload_covers_both_roots() {
        let commitment = ReportCommitment {
            epoch: ConsensusEpoch::new(3),
            committee: BlockHash::from(7),
            predecessor: BlockHash::from(6),
            certified: BlockHash::from(8),
            residual: BlockHash::from(9),
            reporter: PublicKey::from(1),
        };
        for changed in [
            ReportCommitment {
                epoch: ConsensusEpoch::new(4),
                ..commitment.clone()
            },
            ReportCommitment {
                predecessor: BlockHash::from(60),
                ..commitment.clone()
            },
            ReportCommitment {
                committee: BlockHash::from(70),
                ..commitment.clone()
            },
            ReportCommitment {
                certified: BlockHash::from(80),
                ..commitment.clone()
            },
            ReportCommitment {
                residual: BlockHash::from(90),
                ..commitment.clone()
            },
            ReportCommitment {
                reporter: PublicKey::from(2),
                ..commitment.clone()
            },
        ] {
            assert_ne!(commitment.payload(), changed.payload());
        }
    }

    /*
     * Test helpers
     */

    fn block(i: u64) -> CertifiedBlock {
        CertifiedBlock::new(Account::from(i), 1 + i % 4, BlockHash::from(i * 7 + 1))
    }

    /// The parent a test block names, distinct per block
    fn parent(block: CertifiedBlock) -> BlockHash {
        BlockHash::from(block.height * 100_000 + block.account.as_bytes()[31] as u64 + 3)
    }
}
