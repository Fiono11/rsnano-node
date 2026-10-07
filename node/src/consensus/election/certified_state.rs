use std::collections::BTreeMap;

use rsnano_types::{Account, Blake2HashBuilder, BlockHash, ConsensusEpoch, PublicKey};

use super::{ITEM_SIZE, Item, Recovered};

/// RAI: what a validator has locally constructed for a block of one epoch.
/// The statuses are ordered: a block enters the certified tree notarized and
/// may later gain a finalization status, never the other way round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CertifiedStatus {
    /// Inherited unresolved protection: a position the predecessor
    /// checkpoint retains without a notarization of its own; neither
    /// notarization nor finality
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
/// block tree of one validator for one epoch. It holds every complete
/// notarized block the validator knows, with the finalization status it has
/// been able to construct for it, and nothing else: no votes, no bodies.
///
/// The root is an order-independent hash of the entries, so two validators
/// that have constructed the same certificates hold the same root whatever
/// order the votes reached them in. A report signs the root of a frozen
/// snapshot; the live tree goes on growing as gossip delivers more votes,
/// which is what lets a later common state bridge to a historical root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CertifiedState {
    entries: BTreeMap<CertifiedBlock, Certification>,
    /// The block hashes the entries name: G excludes every vote for one
    hashes: std::collections::BTreeSet<BlockHash>,
    /// The XOR of the entry digests, so that a status upgrade or an added
    /// block is a constant-time update of the root
    digest: [u8; 32],
}

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

    /// Whether an entry names this block, under any tag
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
                if let Some(held) = held {
                    self.toggle(&block, held);
                }
                self.toggle(&block, entry);
                self.entries.insert(block, entry);
                self.hashes.insert(block.hash);
                true
            }
        }
    }

    /// The root the report signs
    pub fn root(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI certified state")
            .update(self.digest)
            .update(self.entries.len().to_le_bytes())
            .build()
    }

    /// Records an entry exactly as given, weaker status included: a
    /// reconstruction rebuilds the reporter's state, which is not this
    /// node's own and is not required to grow
    pub fn set(&mut self, block: CertifiedBlock, entry: Certification) {
        if let Some(held) = self.entries.insert(block, entry) {
            self.toggle(&block, held);
        }
        self.toggle(&block, entry);
        self.hashes.insert(block.hash);
    }

    pub fn remove(&mut self, block: &CertifiedBlock) {
        if let Some(held) = self.entries.remove(block) {
            self.toggle(block, held);
            self.hashes.remove(&block.hash);
        }
    }

    /// RAI: F applies to the selected inherited prefix as well as to the
    /// explicit certificate target: a finalized block's unfinalized
    /// ancestors become finalized, and the competing unresolved branches
    /// leave the live state
    pub fn project_final_prefixes(&mut self) {
        let targets: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, e)| e.status.is_finalized())
            .map(|(b, e)| (*b, *e))
            .collect();
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
        let finals: BTreeMap<_, _> = self
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

    /// RAI: the digest one entry contributes to the state root
    fn entry_digest(block: &CertifiedBlock, entry: &Certification) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI certified entry")
            .update(block.account.as_bytes())
            .update(block.height.to_le_bytes())
            .update(block.hash.as_bytes())
            .update(entry.previous.as_bytes())
            .update([entry.status.as_byte()])
            .build()
    }

    fn toggle(&mut self, block: &CertifiedBlock, entry: Certification) {
        let digest = Self::entry_digest(block, &entry);
        for (d, e) in self.digest.iter_mut().zip(digest.as_bytes()) {
            *d ^= e;
        }
    }
}

/// RAI: the certificates a node can assemble for one block from the signed
/// votes it holds
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CertificateKinds {
    /// First and final votes of a certificate's weight
    pub notarization: bool,
    /// Final votes of a certificate's weight
    pub finalization: bool,
    /// First votes of the fast threshold's weight
    pub fast: bool,
}

/// Vote kinds retained in a reporter's residual evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResidualKind {
    /// A first vote, for a block with no notarization certificate in the
    /// certified state. First votes can stand for a hidden fast finalization
    /// certificate in the checkpoint recovery rule.
    First,
    /// A final vote for a block the certified state holds as notarized only
    Final,
}

impl ResidualKind {
    /// The encoding a fetch of the object uses
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(ResidualKind::First),
            2 => Some(ResidualKind::Final),
            _ => None,
        }
    }

    pub fn as_byte(self) -> u8 {
        match self {
            ResidualKind::First => 0,
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

    /// RAI, "Reports that remain reconstructible": the residual object a
    /// reporter's votes and its certified inventory determine. "An
    /// unsummarized first or notarization vote remains ... A final vote on a
    /// notarized but not yet finalized block also remains. A summarized vote
    /// must instead be covered by its corresponding certified status." The
    /// reporter derives it from the votes it issued; a validator that holds
    /// those votes, gossiped and signature-checked on receipt, derives the
    /// same object from them and the reconstructed inventory, and checks it
    /// against the signed root. Timeout and abstaining votes are no part of
    /// it: an account domain has none.
    pub fn derive(
        certified: &CertifiedState,
        votes: impl IntoIterator<Item = (CertifiedBlock, ResidualKind, BlockHash)>,
    ) -> Self {
        // Exact G = V \ keys(T): a vote for a hash under any R, N or F tag
        // is summarized by T
        let mut residual = Self::new();
        for (block, kind, previous) in votes {
            let summarized = certified.contains_hash(&block.hash);
            if !summarized {
                residual.record(block, previous, kind);
            }
        }
        residual
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
            .filter(|(_, kind)| matches!(kind, ResidualKind::First))
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

    /// Drops a residual record and updates the root.
    pub fn remove(&mut self, block: &CertifiedBlock, kind: ResidualKind) -> bool {
        let Some(previous) = self.entries.remove(&(*block, kind)) else {
            return false;
        };
        self.toggle(&Self::entry_digest(block, kind, previous));
        true
    }

    /// The digest one record contributes to the root.
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
        Blake2HashBuilder::new()
            .update(b"RAI residual votes")
            .update(self.digest)
            .update(self.entries.len().to_le_bytes())
            .build()
    }
}

/// RAI: the 105-byte encodings of a report's entries that the rateless
/// reconciliation streams: block (account, height, hash), then the parent,
/// then one byte, the certified status or the residual vote kind
fn encode_item(block: &CertifiedBlock, previous: &BlockHash, tag: u8) -> Item {
    let mut item = [0; ITEM_SIZE];
    item[..32].copy_from_slice(block.account.as_bytes());
    item[32..40].copy_from_slice(&block.height.to_le_bytes());
    item[40..72].copy_from_slice(block.hash.as_bytes());
    item[72..104].copy_from_slice(previous.as_bytes());
    item[104] = tag;
    item
}

fn decode_item(item: &Item) -> (CertifiedBlock, BlockHash, u8) {
    let mut account = [0; 32];
    account.copy_from_slice(&item[..32]);
    let mut height = [0; 8];
    height.copy_from_slice(&item[32..40]);
    let mut hash = [0; 32];
    hash.copy_from_slice(&item[40..72]);
    let mut previous = [0; 32];
    previous.copy_from_slice(&item[72..104]);
    (
        CertifiedBlock::new(
            Account::from_bytes(account),
            u64::from_le_bytes(height),
            BlockHash::from_bytes(hash),
        ),
        BlockHash::from_bytes(previous),
        item[104],
    )
}

impl CertifiedState {
    /// Every entry as a reconciliation item
    pub fn items(&self) -> impl Iterator<Item = Item> + '_ {
        self.entries
            .iter()
            .map(|(block, entry)| encode_item(block, &entry.previous, entry.status.as_byte()))
    }

    /// The entry an item encodes; None for an item no entry encodes to
    pub fn entry_of(item: &Item) -> Option<(CertifiedBlock, Certification)> {
        let (block, previous, tag) = decode_item(item);
        let status = CertifiedStatus::from_byte(tag)?;
        Some((block, Certification { status, previous }))
    }

    /// This state with a reconciled difference applied: the items only the
    /// local base held go, those only the remote set held come in. None if
    /// an item encodes no entry, or a removed one is not held as encoded.
    pub fn with_difference(&self, difference: impl IntoIterator<Item = Recovered>) -> Option<Self> {
        let mut state = self.clone();
        let mut inserts = Vec::new();
        for recovered in difference {
            match recovered {
                Recovered::Local(item) => {
                    let (block, entry) = Self::entry_of(&item)?;
                    if state.certification(&block) != Some(entry) {
                        return None;
                    }
                    state.remove(&block);
                }
                Recovered::Remote(item) => inserts.push(Self::entry_of(&item)?),
            }
        }
        for (block, entry) in inserts {
            if state.certification(&block).is_some() {
                return None;
            }
            state.set(block, entry);
        }
        Some(state)
    }
}

impl ResidualVotes {
    /// Every record as a reconciliation item
    pub fn items(&self) -> impl Iterator<Item = Item> + '_ {
        self.entries()
            .map(|(block, kind, previous)| encode_item(&block, &previous, kind.as_byte()))
    }

    /// The record an item encodes; None for an item no record encodes to
    pub fn record_of(item: &Item) -> Option<(CertifiedBlock, ResidualKind, BlockHash)> {
        let (block, previous, tag) = decode_item(item);
        Some((block, ResidualKind::from_byte(tag)?, previous))
    }

    /// These records with a reconciled difference applied; None if an item
    /// encodes no record or a removed one is not held
    pub fn with_difference(&self, difference: impl IntoIterator<Item = Recovered>) -> Option<Self> {
        let mut records = self.clone();
        let mut inserts = Vec::new();
        for recovered in difference {
            match recovered {
                Recovered::Local(item) => {
                    let (block, kind, previous) = Self::record_of(&item)?;
                    if records.entries.get(&(block, kind)) != Some(&previous) {
                        return None;
                    }
                    records.remove(&block, kind);
                }
                Recovered::Remote(item) => inserts.push(Self::record_of(&item)?),
            }
        }
        for (block, kind, previous) in inserts {
            if !records.record(block, previous, kind) {
                return None;
            }
        }
        Some(records)
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

    /// The same block first voted and then notarized is two entries, and the
    /// root distinguishes them from either alone
    #[test]
    fn the_two_support_kinds_hash_apart() {
        let mut first_only = ResidualVotes::new();
        first_only.record(block(1), parent(block(1)), ResidualKind::First);
        let mut notar_only = ResidualVotes::new();
        notar_only.record(block(1), parent(block(1)), ResidualKind::Final);
        assert_ne!(first_only.root(), notar_only.root());

        let mut both = ResidualVotes::new();
        both.record(block(1), parent(block(1)), ResidualKind::First);
        both.record(block(1), parent(block(1)), ResidualKind::Final);
        assert_eq!(both.len(), 2);
        assert_ne!(both.root(), first_only.root());
        assert_eq!(both.first_votes().collect::<Vec<_>>(), vec![&block(1)]);
    }

    /// The residual object is what the inventory does not summarize, exact
    /// G = V \ keys(T): every vote for a block with no tag in T. Derived
    /// from the same votes and inventory on either side, it hashes the same.
    #[test]
    fn the_residual_is_derived_from_the_votes_the_inventory_does_not_summarize() {
        let mut certified = CertifiedState::new();
        certified.certify(block(1), parent(block(1)), CertifiedStatus::Notarized);
        certified.certify(block(2), parent(block(2)), CertifiedStatus::Finalized);
        let votes = vec![
            // Summarized by the notarization, final vote included
            (block(1), ResidualKind::First, parent(block(1))),
            (block(1), ResidualKind::Final, parent(block(1))),
            // Summarized by the finalization
            (block(2), ResidualKind::First, parent(block(2))),
            (block(2), ResidualKind::Final, parent(block(2))),
            // No certificate at all: both remain
            (block(3), ResidualKind::First, parent(block(3))),
            (block(3), ResidualKind::Final, parent(block(3))),
        ];
        let derived = ResidualVotes::derive(&certified, votes.clone());
        assert_eq!(derived.len(), 2);
        assert!(!derived.contains(&block(1), ResidualKind::Final));
        assert!(derived.contains(&block(3), ResidualKind::First));
        assert!(derived.contains(&block(3), ResidualKind::Final));
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
            ResidualVotes::derive(&certified, votes[..5].to_vec()).root(),
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

    /// The reconciliation items encode the entries exactly, and a decoded
    /// difference applied to the base rebuilds the reporter's state
    #[test]
    fn a_reconciled_difference_rebuilds_the_remote_state() {
        use crate::consensus::election::{Decoder, Encoder};
        let block = |i: u64| CertifiedBlock::new(Account::from(i), i, BlockHash::from(i + 100));
        let mut remote = CertifiedState::new();
        let mut local = CertifiedState::new();
        for i in 1..50 {
            remote.certify(block(i), BlockHash::from(i), CertifiedStatus::Notarized);
            local.certify(block(i), BlockHash::from(i), CertifiedStatus::Notarized);
        }
        // Only the reporter finalized one, only it holds two, only we hold one
        remote.certify(block(3), BlockHash::from(3), CertifiedStatus::Finalized);
        remote.certify(block(60), BlockHash::from(60), CertifiedStatus::Notarized);
        remote.certify(block(61), BlockHash::from(61), CertifiedStatus::Finalized);
        local.certify(block(70), BlockHash::from(70), CertifiedStatus::Notarized);
        let mut encoder = Encoder::new(remote.items());
        let mut decoder = Decoder::new(local.items());
        while !decoder.is_done() {
            let from = decoder.received();
            let symbols = encoder.symbols(from, 8).to_vec();
            decoder.add_symbols(&symbols);
        }
        // The upgraded entry counts twice: its old and its new encoding
        assert_eq!(decoder.recovered().count(), 5);
        let rebuilt = local.with_difference(decoder.recovered()).unwrap();
        assert_eq!(rebuilt.root(), remote.root());
        for (block, entry) in remote.entries() {
            let item = encode_item(block, &entry.previous, entry.status.as_byte());
            assert_eq!(CertifiedState::entry_of(&item), Some((*block, *entry)));
        }
    }

    #[test]
    fn residual_records_round_trip_through_items() {
        let block = CertifiedBlock::new(Account::from(1), 2, BlockHash::from(3));
        let mut local = ResidualVotes::new();
        local.record(block, BlockHash::from(4), ResidualKind::First);
        let mut remote = local.clone();
        remote.record(block, BlockHash::from(4), ResidualKind::Final);
        let items: Vec<Item> = remote.items().collect();
        assert_eq!(items.len(), 2);
        let difference = remote
            .items()
            .filter(|item| !local.items().any(|held| held == *item))
            .map(Recovered::Remote);
        assert_eq!(
            local.with_difference(difference).unwrap().root(),
            remote.root()
        );
    }

    #[test]
    fn descendant_finality_finalizes_its_prefix_and_removes_the_rival() {
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
        assert!(!live.contains_hash(&rival_child.hash));
        assert_eq!(frozen.status(&parent), Some(CertifiedStatus::Recovery));
        assert_eq!(frozen.len(), 3);
    }

    #[test]
    fn every_tag_excludes_every_vote_kind_from_g() {
        let block = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(5));
        for status in [
            CertifiedStatus::Recovery,
            CertifiedStatus::Notarized,
            CertifiedStatus::Finalized,
        ] {
            let mut t = CertifiedState::new();
            t.certify(block, BlockHash::ZERO, status);
            let votes = [
                (block, ResidualKind::First, BlockHash::ZERO),
                (block, ResidualKind::Final, BlockHash::ZERO),
            ];
            assert!(ResidualVotes::derive(&t, votes).is_empty(), "{status:?}");
        }
        // A recovery upgrade grows the live state, never a frozen one
        let mut live = CertifiedState::new();
        live.certify(block, BlockHash::ZERO, CertifiedStatus::Recovery);
        let frozen = live.clone();
        assert!(live.certify(block, BlockHash::ZERO, CertifiedStatus::Notarized));
        assert!(!live.certify(block, BlockHash::ZERO, CertifiedStatus::Recovery));
        assert_ne!(live.root(), frozen.root());
    }
}
