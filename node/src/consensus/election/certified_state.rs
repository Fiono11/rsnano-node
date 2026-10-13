use std::{
    collections::{BTreeMap, BTreeSet, btree_map},
    iter::Peekable,
    ops::RangeBounds,
    sync::{Arc, OnceLock},
};

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
///
/// A state may be laid over a shared base: an epoch's report holds the whole
/// inherited state `S_{e-1}`, which every report of the epoch shares, and
/// the epoch's own changes. The base is held once and never copied; the
/// state records what it adds, upgrades and removes, and keeps the root and
/// the length up to date, so that a copy, a root and every lookup cost what
/// the epoch changed, not the whole history. What a state holds does not
/// depend on how it is laid out: two states with the same entries are
/// equal, and hash to the same root.
#[derive(Clone, Debug, Default)]
pub struct CertifiedState {
    /// The shared state underneath; it has no base of its own
    base: Option<Arc<CertifiedState>>,
    /// Without a base, every entry. Over a base, the entries added or
    /// changed (Some) and those of the base removed (None)
    delta: BTreeMap<CertifiedBlock, Option<Certification>>,
    /// The block hashes the entries name, with the tag of each: G excludes
    /// a vote for one tagged N or F. Over a base, the changes to its index
    hashes: BTreeMap<BlockHash, Option<CertifiedStatus>>,
    /// The XOR of the entry digests, so that a status upgrade or an added
    /// block is a constant-time update of the root
    digest: [u8; 32],
    len: usize,
    /// Without a base: no change since the last prefix projection
    projected: bool,
    /// The entries not finalized, once asked for; a change forgets them
    unfinalized: OnceLock<BTreeMap<CertifiedBlock, Option<Certification>>>,
}

impl PartialEq for CertifiedState {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.digest == other.digest && self.entries().eq(other.entries())
    }
}

impl Eq for CertifiedState {}

impl CertifiedState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A state holding exactly what `base` holds, laid over it: what is
    /// changed from now on is recorded apart from the shared entries
    pub fn over(base: &Arc<CertifiedState>) -> Self {
        let base = if base.base.is_some() {
            Arc::new(base.flattened())
        } else {
            Arc::clone(base)
        };
        Self {
            digest: base.digest,
            len: base.len,
            base: Some(base),
            delta: BTreeMap::new(),
            hashes: BTreeMap::new(),
            projected: false,
            unfinalized: OnceLock::new(),
        }
    }

    /// The same entries without a base
    pub fn flattened(&self) -> Self {
        let mut flat = Self::new();
        for (block, entry) in self.entries() {
            flat.set(*block, *entry);
        }
        flat
    }

    /// Whether the two states are laid over the same shared base
    pub fn shares_base(&self, other: &CertifiedState) -> bool {
        match (&self.base, &other.base) {
            (Some(one), Some(other)) => Arc::ptr_eq(one, other),
            _ => false,
        }
    }

    /// The shared base this state is laid over, if any
    pub fn base(&self) -> Option<&Arc<CertifiedState>> {
        self.base.as_ref()
    }

    /// Over a base, what this state changes: entries added or changed
    /// (Some) and entries of the base removed (None). Without a base, every
    /// entry.
    pub fn changes(&self) -> impl Iterator<Item = (&CertifiedBlock, Option<&Certification>)> {
        self.delta
            .iter()
            .map(|(block, entry)| (block, entry.as_ref()))
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The entries a check of this state must look at when every entry of
    /// its base is inherited, i.e. justified by the predecessor checkpoint
    /// the base projects: those this state adds or changes. Without a base,
    /// every entry.
    pub fn changed_entries(&self) -> impl Iterator<Item = (&CertifiedBlock, &Certification)> {
        self.delta
            .iter()
            .filter_map(|(block, entry)| entry.as_ref().map(|entry| (block, entry)))
    }

    /// The changed entries and the unfinalized entries the state keeps of
    /// its base: what a derivation must see of a report whose base projects
    /// the predecessor checkpoint. An inherited finalized entry restates
    /// finality the derivation starts from; an inherited lock still takes
    /// part in it.
    /// In canonical order, as `entries` gives them.
    pub fn changed_or_unfinalized(&self) -> Entries<'_> {
        static NONE: BTreeMap<CertifiedBlock, Option<Certification>> = BTreeMap::new();
        let kept = self
            .base
            .as_ref()
            .map(|base| base.unfinalized_state())
            .unwrap_or(&NONE);
        Entries {
            base: kept.range(..).peekable(),
            delta: self.delta.range(..).peekable(),
        }
    }

    /// The report base this state makes: the state itself, marked as its
    /// own prefix projection if it is one, so that every report laid over
    /// it projects only what it changes. Whether it is one.
    pub fn into_report_base(self) -> (Self, bool) {
        let mut projected = self.clone();
        projected.project_final_prefixes();
        if projected == self {
            (projected, true)
        } else {
            (self, false)
        }
    }

    /// The entries not finalized: of an inherited state, the positions it
    /// retains, few next to its finalized history. Computed once per state.
    pub fn unfinalized(&self) -> impl Iterator<Item = (&CertifiedBlock, &Certification)> {
        self.unfinalized_state()
            .iter()
            .filter_map(|(block, entry)| entry.as_ref().map(|entry| (block, entry)))
    }

    fn unfinalized_state(&self) -> &BTreeMap<CertifiedBlock, Option<Certification>> {
        self.unfinalized.get_or_init(|| {
            self.entries()
                .filter(|(_, entry)| !entry.status.is_finalized())
                .map(|(block, entry)| (*block, Some(*entry)))
                .collect()
        })
    }

    fn hash_status(&self, hash: &BlockHash) -> Option<CertifiedStatus> {
        match self.hashes.get(hash) {
            Some(changed) => *changed,
            None => self
                .base
                .as_ref()
                .and_then(|base| base.hashes.get(hash).copied().flatten()),
        }
    }

    /// Whether an entry names this block, under any tag
    pub fn contains_hash(&self, hash: &BlockHash) -> bool {
        self.hash_status(hash).is_some()
    }

    /// RAI, Fix B: whether an entry tags this block N or F. Such an entry
    /// exposes every vote the reporter cast for the block; an R entry does
    /// not, since it records what the predecessor checkpoint carried, not
    /// what the reporter signed in the epoch.
    pub fn summarizes(&self, hash: &BlockHash) -> bool {
        self.hash_status(hash)
            .is_some_and(|status| status != CertifiedStatus::Recovery)
    }

    pub fn status(&self, block: &CertifiedBlock) -> Option<CertifiedStatus> {
        self.certification(block).map(|held| held.status)
    }

    /// The status of a block and the parent it names
    pub fn certification(&self, block: &CertifiedBlock) -> Option<Certification> {
        match self.delta.get(block) {
            Some(changed) => *changed,
            None => self
                .base
                .as_ref()
                .and_then(|base| base.delta.get(block).copied().flatten()),
        }
    }

    /// Every entry, in canonical order
    pub fn entries(&self) -> Entries<'_> {
        self.entries_in(..)
    }

    /// The entries in a range of blocks, in canonical order
    pub fn entries_in(&self, range: impl RangeBounds<CertifiedBlock> + Clone) -> Entries<'_> {
        Entries {
            base: self
                .base
                .as_ref()
                .map(|base| base.delta.range(range.clone()))
                .unwrap_or_default()
                .peekable(),
            delta: self.delta.range(range).peekable(),
        }
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
        match self.certification(&block) {
            Some(held) if held.status >= status => false,
            held => {
                let entry = Certification {
                    status,
                    previous: held.map(|held| held.previous).unwrap_or(previous),
                };
                self.put(block, held, Some(entry));
                true
            }
        }
    }

    /// Certifies every entry of `other` here, as `certify` would one by
    /// one. Over the same base only the entries either state changes can
    /// make a difference: every other one is the base's on both sides.
    pub fn certify_all(&mut self, other: &CertifiedState) {
        // Certifying every entry into an empty state copies them as they are
        if self.is_empty() && self.base.is_none() {
            *self = other.clone();
            return;
        }
        // A state without a base, `other` over one: the same entries come
        // of certifying this state's into a copy of `other`, since a status
        // only grows and a block names one parent, and the result is laid
        // over the base from then on
        if self.base.is_none() && other.base.is_some() {
            let held = std::mem::replace(self, other.clone());
            for (block, entry) in held.entries() {
                self.certify(*block, entry.previous, entry.status);
            }
            return;
        }
        if !self.shares_base(other) {
            for (block, entry) in other.entries() {
                self.certify(*block, entry.previous, entry.status);
            }
            return;
        }
        let mut changed: Vec<(CertifiedBlock, Certification)> = other
            .changed_entries()
            .map(|(block, entry)| (*block, *entry))
            .collect();
        changed.extend(
            self.delta
                .keys()
                .filter(|block| !other.delta.contains_key(block))
                .filter_map(|block| Some((*block, other.certification(block)?))),
        );
        for (block, entry) in changed {
            self.certify(block, entry.previous, entry.status);
        }
    }

    /// The root the report signs
    pub fn root(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI certified state")
            .update(self.digest)
            .update(self.len.to_le_bytes())
            .build()
    }

    /// Records an entry exactly as given, weaker status included: a
    /// reconstruction rebuilds the reporter's state, which is not this
    /// node's own and is not required to grow
    pub fn set(&mut self, block: CertifiedBlock, entry: Certification) {
        let held = self.certification(&block);
        self.put(block, held, Some(entry));
    }

    pub fn remove(&mut self, block: &CertifiedBlock) {
        if let Some(held) = self.certification(block) {
            self.put(*block, Some(held), None);
        }
    }

    /// Replaces the entry `held` of a block, the one this state holds, by
    /// `entry`, keeping the root, the length and the hash index in step.
    /// Over a base, a change back to the base's own entry is no change.
    fn put(
        &mut self,
        block: CertifiedBlock,
        held: Option<Certification>,
        entry: Option<Certification>,
    ) {
        if held == entry {
            return;
        }
        self.projected = false;
        self.unfinalized = OnceLock::new();
        if let Some(held) = held {
            self.toggle(&block, held);
            self.len -= 1;
        }
        if let Some(entry) = entry {
            self.toggle(&block, entry);
            self.len += 1;
        }
        let status = entry.map(|entry| entry.status);
        match &self.base {
            None => {
                match entry {
                    Some(entry) => self.delta.insert(block, Some(entry)),
                    None => self.delta.remove(&block),
                };
                match status {
                    Some(status) => self.hashes.insert(block.hash, Some(status)),
                    None => self.hashes.remove(&block.hash),
                };
            }
            Some(base) => {
                if base.delta.get(&block).copied().flatten() == entry {
                    self.delta.remove(&block);
                } else {
                    self.delta.insert(block, entry);
                }
                if base.hashes.get(&block.hash).copied().flatten() == status {
                    self.hashes.remove(&block.hash);
                } else {
                    self.hashes.insert(block.hash, status);
                }
            }
        }
    }

    /// RAI: F applies to the selected inherited prefix as well as to the
    /// explicit certificate target: a finalized block's unfinalized
    /// ancestors become finalized, and the competing unresolved branches
    /// leave the live state
    pub fn project_final_prefixes(&mut self) {
        match &self.base {
            Some(base) if base.projected => {
                let accounts: BTreeSet<Account> =
                    self.delta.keys().map(|block| block.account).collect();
                self.project(accounts);
            }
            _ => {
                let accounts: BTreeSet<Account> =
                    self.entries().map(|(block, _)| block.account).collect();
                self.project(accounts);
            }
        }
        if self.base.is_none() {
            self.projected = true;
        }
    }

    /// The projection of the given accounts. It is local to an account -
    /// a prefix, a rival and the finality excluding it all sit on one - and
    /// a projected state is its own projection, so over a projected base
    /// only the accounts this state changes need it: the result is the
    /// projection of the whole state, in the time of the changes.
    fn project(&mut self, accounts: BTreeSet<Account>) {
        let mut excluded = Vec::new();
        for account in accounts {
            let range = CertifiedBlock::new(account, 0, BlockHash::ZERO)
                ..=CertifiedBlock::new(account, u64::MAX, BlockHash::from_bytes([0xff; 32]));
            let targets: Vec<_> = self
                .entries_in(range.clone())
                .filter(|(_, entry)| entry.status.is_finalized())
                .map(|(block, entry)| (*block, *entry))
                .collect();
            for (mut block, mut entry) in targets {
                while block.height > 1 && !entry.previous.is_zero() {
                    let parent =
                        CertifiedBlock::new(block.account, block.height - 1, entry.previous);
                    let Some(held) = self.certification(&parent) else {
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
            let finals: BTreeMap<u64, BlockHash> = self
                .entries_in(range.clone())
                .filter(|(_, entry)| entry.status.is_finalized())
                .map(|(block, _)| (block.height, block.hash))
                .collect();
            excluded.extend(
                self.entries_in(range)
                    .filter(|(_, entry)| !entry.status.is_finalized())
                    .filter_map(|(candidate, _)| {
                        let mut current = *candidate;
                        loop {
                            if let Some(final_hash) = finals.get(&current.height) {
                                return (*final_hash != current.hash).then_some(*candidate);
                            }
                            let entry = self.certification(&current)?;
                            if current.height <= 1 || entry.previous.is_zero() {
                                return None;
                            }
                            current = CertifiedBlock::new(
                                current.account,
                                current.height - 1,
                                entry.previous,
                            );
                        }
                    }),
            );
        }
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

/// The entries of a `CertifiedState` in canonical order: the base's, merged
/// with what the state changes over it
pub struct Entries<'a> {
    base: Peekable<btree_map::Range<'a, CertifiedBlock, Option<Certification>>>,
    delta: Peekable<btree_map::Range<'a, CertifiedBlock, Option<Certification>>>,
}

impl<'a> Iterator for Entries<'a> {
    type Item = (&'a CertifiedBlock, &'a Certification);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let from_base = match (self.base.peek(), self.delta.peek()) {
                (None, None) => return None,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (Some((base, _)), Some((delta, _))) => {
                    if base < delta {
                        true
                    } else {
                        // A change replaces the base's entry for the block
                        if base == delta {
                            self.base.next();
                        }
                        false
                    }
                }
            };
            let (block, entry) = if from_base {
                self.base.next()?
            } else {
                self.delta.next()?
            };
            if let Some(entry) = entry {
                return Some((block, entry));
            }
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
        // Fix B: G = V \ {h : (h, N) or (h, F) in T}. An R tag summarizes
        // nothing the reporter signed in the epoch, so a vote for an
        // R-tagged block stays in G.
        let mut residual = Self::new();
        for (block, kind, previous) in votes {
            if !certified.summarizes(&block.hash) {
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
        self.entries()
            .map(|(block, entry)| encode_item(block, &entry.previous, entry.status.as_byte()))
    }

    /// What this state changes over `base` as signed reconciliation items:
    /// an entry `base` lacks +1, an entry of `base` this state lacks -1. A
    /// state laid over `base` holds them; any other is compared in full.
    pub fn changed_items(&self, base: &Arc<CertifiedState>) -> Vec<(Item, i64)> {
        let item = |block: &CertifiedBlock, entry: &Certification| {
            encode_item(block, &entry.previous, entry.status.as_byte())
        };
        let mut items = Vec::new();
        if self.base.as_ref().is_some_and(|own| Arc::ptr_eq(own, base)) {
            for (block, entry) in &self.delta {
                if let Some(held) = base.certification(block) {
                    items.push((item(block, &held), -1));
                }
                if let Some(entry) = entry {
                    items.push((item(block, entry), 1));
                }
            }
            return items;
        }
        let mut ours = self.entries().peekable();
        let mut theirs = base.entries().peekable();
        loop {
            match (ours.peek(), theirs.peek()) {
                (None, None) => return items,
                (Some(&(block, entry)), Some(&(base_block, base_entry))) if block == base_block => {
                    if entry != base_entry {
                        items.push((item(base_block, base_entry), -1));
                        items.push((item(block, entry), 1));
                    }
                    ours.next();
                    theirs.next();
                }
                (Some(&(block, entry)), Some(&(base_block, _))) if block < base_block => {
                    items.push((item(block, entry), 1));
                    ours.next();
                }
                (Some(&(block, entry)), None) => {
                    items.push((item(block, entry), 1));
                    ours.next();
                }
                (_, Some(&(base_block, base_entry))) => {
                    items.push((item(base_block, base_entry), -1));
                    theirs.next();
                }
            }
        }
    }

    /// The state `changed_items` describes: `base` with the changes laid
    /// over it. None if a change names no entry, or removes one `base` does
    /// not hold as encoded.
    pub fn with_changes(base: &Arc<CertifiedState>, changes: &[(Item, i64)]) -> Option<Self> {
        let mut state = Self::over(base);
        for (item, direction) in changes.iter().filter(|(_, direction)| *direction < 0) {
            let (block, entry) = Self::entry_of(item)?;
            if *direction != -1 || base.certification(&block) != Some(entry) {
                return None;
            }
            state.remove(&block);
        }
        for (item, direction) in changes.iter().filter(|(_, direction)| *direction > 0) {
            let (block, entry) = Self::entry_of(item)?;
            if *direction != 1 {
                return None;
            }
            state.set(block, entry);
        }
        Some(state)
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

    /// A state laid over a shared base holds, roots and answers exactly as
    /// the same state held flat, through any sequence of changes, prefix
    /// projections included, and its copies leave the base alone
    #[test]
    fn a_state_over_a_base_behaves_as_the_flat_state() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % bound
        };
        for _ in 0..200 {
            let mut flat = CertifiedState::new();
            for _ in 0..next(30) {
                random_change(&mut flat, &mut next);
            }
            flat.project_final_prefixes();
            let base = Arc::new(flat.clone());
            let mut over = CertifiedState::over(&base);
            assert_eq!(over, flat);
            assert_eq!(over.root(), flat.root());
            for _ in 0..next(30) {
                let mut both = [&mut flat, &mut over];
                let change = next(u64::MAX);
                for state in both.iter_mut() {
                    let mut replay = change;
                    random_change(state, &mut |bound| {
                        replay = replay.rotate_left(17) ^ 0x5bd1_e995;
                        replay % bound
                    });
                }
                if next(5) == 0 {
                    flat.project_final_prefixes();
                    over.project_final_prefixes();
                }
                assert_eq!(over.len(), flat.len());
                assert_eq!(over.root(), flat.root());
                assert!(over.entries().eq(flat.entries()));
                for i in 0..40 {
                    let block = chained(i % 6, 1 + i / 12, i % 2);
                    assert_eq!(over.certification(&block), flat.certification(&block));
                    assert_eq!(
                        over.contains_hash(&block.hash),
                        flat.contains_hash(&block.hash)
                    );
                    assert_eq!(over.summarizes(&block.hash), flat.summarizes(&block.hash));
                }
            }
            assert_eq!(*base, base.flattened());
            assert_eq!(over.flattened(), flat);
        }
    }

    /// Certifying every entry of another state, laid over the same base or
    /// not, is certifying them one by one
    #[test]
    fn certifying_a_whole_state_is_certifying_its_entries() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % bound
        };
        for _ in 0..300 {
            let mut base = CertifiedState::new();
            for _ in 0..next(20) {
                random_change(&mut base, &mut next);
            }
            base.project_final_prefixes();
            let base = Arc::new(base);
            let (mut one, mut other) = (CertifiedState::over(&base), CertifiedState::over(&base));
            for _ in 0..next(15) {
                random_change(&mut one, &mut next);
            }
            for _ in 0..next(15) {
                random_change(&mut other, &mut next);
            }
            let mut expected = one.flattened();
            for (block, entry) in other.entries() {
                expected.certify(*block, entry.previous, entry.status);
            }
            one.certify_all(&other);
            assert_eq!(one, expected);
            let mut empty = CertifiedState::new();
            empty.certify_all(&other);
            assert_eq!(empty, other);
            let mut flat = one.flattened();
            let mut expected = flat.clone();
            for (block, entry) in other.entries() {
                expected.certify(*block, entry.previous, entry.status);
            }
            flat.certify_all(&other);
            assert_eq!(flat, expected);
            assert!(flat.shares_base(&other));
        }
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

    /// The residual object is what the inventory does not summarize,
    /// G = V \ {N, F tags of T}: every vote for a block T does not tag N or
    /// F. Derived
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

    /// A block of a small forked tree: account, height, and one of two
    /// branches, each continuing the branch of the same index below
    fn chained(account: u64, height: u64, branch: u64) -> CertifiedBlock {
        CertifiedBlock::new(
            Account::from(account + 1),
            height,
            BlockHash::from(account * 1_000 + height * 10 + branch + 1),
        )
    }

    fn random_change(state: &mut CertifiedState, next: &mut dyn FnMut(u64) -> u64) {
        let height = 1 + next(4);
        let branch = next(2);
        let block = chained(next(6), height, branch);
        let previous = if height == 1 {
            BlockHash::ZERO
        } else {
            chained(block.account.as_bytes()[31] as u64 - 1, height - 1, branch).hash
        };
        let status = [
            CertifiedStatus::Recovery,
            CertifiedStatus::Notarized,
            CertifiedStatus::Finalized,
        ][next(3) as usize];
        match next(4) {
            0 => state.remove(&block),
            1 => state.set(block, Certification { status, previous }),
            _ => {
                state.certify(block, previous, status);
            }
        }
    }

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

    /// Fix B: an N or F tag summarizes every vote for its block; an R tag
    /// summarizes none, so a first vote for an R-tagged block stays in G
    #[test]
    fn only_n_and_f_tags_exclude_votes_from_g() {
        let block = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(5));
        let votes = [
            (block, ResidualKind::First, BlockHash::ZERO),
            (block, ResidualKind::Final, BlockHash::ZERO),
        ];
        for status in [CertifiedStatus::Notarized, CertifiedStatus::Finalized] {
            let mut t = CertifiedState::new();
            t.certify(block, BlockHash::ZERO, status);
            assert!(t.summarizes(&block.hash), "{status:?}");
            assert!(ResidualVotes::derive(&t, votes).is_empty(), "{status:?}");
        }
        let mut t = CertifiedState::new();
        t.certify(block, BlockHash::ZERO, CertifiedStatus::Recovery);
        assert!(t.contains_hash(&block.hash));
        assert!(!t.summarizes(&block.hash));
        let g = ResidualVotes::derive(&t, votes);
        assert!(g.contains(&block, ResidualKind::First));
        assert_eq!(g.first_votes().collect::<Vec<_>>(), vec![&block]);

        // A recovery upgrade grows the live state, never a frozen one
        let mut live = CertifiedState::new();
        live.certify(block, BlockHash::ZERO, CertifiedStatus::Recovery);
        let frozen = live.clone();
        assert!(live.certify(block, BlockHash::ZERO, CertifiedStatus::Notarized));
        assert!(live.summarizes(&block.hash));
        assert!(!frozen.summarizes(&block.hash));
        assert!(!live.certify(block, BlockHash::ZERO, CertifiedStatus::Recovery));
        assert_ne!(live.root(), frozen.root());
    }
}
