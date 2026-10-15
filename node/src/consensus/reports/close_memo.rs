//! RAI: the derivation of an epoch's checkpoint, amortized over the epoch.
//!
//! A close derives `BuildState(S_{e-1}, Q_e)` and the claims of its evidence
//! manifest from the `N - f` selected reports, once at the leader and once at
//! every validator checking its proposal. Each report names nearly every
//! block of the epoch, and the reports agree on nearly all of them, so a
//! close would redo per block work the epoch has long since made possible.
//! Every rule of the derivation reads and writes one account at a time (see
//! `build_state_of`): an account's part of the checkpoint is a function of
//! the predecessor's part for that account and the reports' entries naming
//! it. While the epoch runs, the memo derives each account from this node's
//! own certified view and records what that view names for it. At the close
//! an account that every selected report names exactly as the memo recorded,
//! with no residual vote, takes the memoized part; every other account is
//! derived from the reports' entries as before. The result is the state and
//! the claims the whole derivation makes, or nothing when the reports fall
//! outside what the memo can stand for, and the caller derives in full.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::RangeInclusive,
    sync::Arc,
};

use rsnano_types::{Account, Amount, BlockHash, ConsensusEpoch, PublicKey};

use super::{entry_claims, report_service::inherited_from};
use crate::consensus::election::{
    AccountOutcome, AccountSlot, BlockIndex, BlockPlacement, BuildRules, BuildStateError,
    Certification, CertifiedBlock, CertifiedState, CertifiedStatus, EpochLedger, FreshFinalized,
    ReportView, ResidualKind, SelectedReport, build_state_of, overlap_claims, witness_claims_of,
};

/// What a report's delta holds for an account: entries added or changed
/// (Some) and entries of the base removed (None)
type Slice = Vec<(CertifiedBlock, Option<Certification>)>;

/// The memoized derivation of the accounts an epoch's certified view names,
/// against one predecessor checkpoint and report base
#[derive(Clone)]
pub(crate) struct CloseMemo {
    epoch: ConsensusEpoch,
    predecessor: BlockHash,
    previous: Arc<EpochLedger>,
    base: Arc<CertifiedState>,
    anchor: Option<Arc<EpochLedger>>,
    accounts: BTreeMap<Account, AccountMemo>,
    /// The account each memoized entry's block hash is named for
    owners: HashMap<BlockHash, Account>,
    /// Accounts naming a block hash another account names as well: they
    /// are derived at the close, where the whole derivation sees them
    tainted: BTreeSet<Account>,
    /// Refreshed from this node's signed report: the reports are frozen
    frozen: bool,
}

#[derive(Clone)]
struct AccountMemo {
    slice: Slice,
    /// Why the memo does not stand for the account, if it does not
    derived: Result<Derived, Unmemoized>,
}

/// Why an account is derived at the close rather than memoized
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Unmemoized {
    /// A block hash of it is named for another account as well
    Shared,
    /// The predecessor retains a position of it
    Retained,
    /// The overlap anchor retains a position of it
    Anchored,
    /// It names a block of the inherited base
    InheritedBlock,
    /// It removes an entry of the base
    Removal,
    /// Its derivation failed or did not record what it finalized
    Failed,
}

#[derive(Clone)]
struct Derived {
    outcome: AccountOutcome,
    claims: Vec<(ConsensusEpoch, BlockHash)>,
    fresh: Vec<FreshFinalized>,
}

/// Why the memo could not stand for a selection
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoMiss {
    OtherBase,
    RepeatedReporter,
    SharedHash,
}

impl CloseMemo {
    pub fn new(
        epoch: ConsensusEpoch,
        predecessor: BlockHash,
        previous: Arc<EpochLedger>,
        base: Arc<CertifiedState>,
        anchor: Option<Arc<EpochLedger>>,
    ) -> Self {
        Self {
            epoch,
            predecessor,
            previous,
            base,
            anchor,
            accounts: BTreeMap::new(),
            owners: HashMap::new(),
            tainted: BTreeSet::new(),
            frozen: false,
        }
    }

    /// Whether the memo derives against this predecessor and base
    pub fn derives_against(&self, predecessor: BlockHash, base: &Arc<CertifiedState>) -> bool {
        self.predecessor == predecessor && Arc::ptr_eq(&self.base, base)
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// The accounts memoized, and those of them the memo stands for
    pub fn counts(&self) -> (usize, usize) {
        (
            self.accounts.len(),
            self.accounts
                .values()
                .filter(|memo| memo.derived.is_ok())
                .count(),
        )
    }

    /// How many accounts the memo does not stand for, by reason
    pub fn unmemoized(&self) -> BTreeMap<Unmemoized, usize> {
        let mut reasons = BTreeMap::new();
        for memo in self.accounts.values() {
            if let Err(reason) = memo.derived {
                *reasons.entry(reason).or_default() += 1;
            }
        }
        reasons
    }

    /// Brings the memo up to `view`, a certified state over the memo's
    /// base: an account whose entries changed is derived again, one the
    /// view no longer names is forgotten. `frozen` marks the view as this
    /// node's signed report, after which nothing changes. The number of
    /// accounts derived.
    pub fn refresh(&mut self, view: &CertifiedState, frozen: bool) -> usize {
        if !view
            .base()
            .is_some_and(|base| Arc::ptr_eq(base, &self.base))
        {
            return 0;
        }
        let mut held = std::mem::take(&mut self.accounts);
        let mut next = BTreeMap::new();
        let mut derived = 0;
        let mut changes = view.changes().peekable();
        let mut slice: Slice = Vec::new();
        while let Some((first, _)) = changes.peek() {
            let account = first.account;
            slice.clear();
            while let Some((block, entry)) = changes.next_if(|(block, _)| block.account == account)
            {
                slice.push((*block, entry.copied()));
            }
            match held.remove(&account) {
                Some(memo) if memo.slice == slice => {
                    next.insert(account, memo);
                }
                old => {
                    if let Some(old) = old {
                        self.forget_owners(account, &old.slice);
                    }
                    let memo = self.derive(account, slice.clone());
                    next.insert(account, memo);
                    derived += 1;
                }
            }
        }
        for (account, old) in held {
            self.forget_owners(account, &old.slice);
        }
        self.accounts = next;
        self.frozen = frozen;
        derived
    }

    fn forget_owners(&mut self, account: Account, slice: &Slice) {
        for (block, _) in slice {
            if self.owners.get(&block.hash) == Some(&account) {
                self.owners.remove(&block.hash);
            }
        }
    }

    /// An account's memo: its part derived from `slice` as the only report,
    /// or nothing for an account the derivation must see in full - one the
    /// predecessor or the overlap anchor retains a position of, one naming a
    /// block of the inherited base or of another account, or one whose
    /// derivation fails
    fn derive(&mut self, account: Account, slice: Slice) -> AccountMemo {
        let mut shared = false;
        for (block, _) in &slice {
            match self.owners.insert(block.hash, account) {
                Some(other) if other != account => {
                    self.tainted.insert(other);
                    shared = true;
                }
                _ => {}
            }
        }
        if shared {
            self.tainted.insert(account);
        }
        let derived = if shared {
            Err(Unmemoized::Shared)
        } else if self.previous.retained_depth(account).is_some() {
            Err(Unmemoized::Retained)
        } else if self
            .anchor
            .as_ref()
            .is_some_and(|anchor| anchor.retained_depth(account).is_some())
        {
            Err(Unmemoized::Anchored)
        } else if slice.iter().any(|(_, entry)| entry.is_none()) {
            Err(Unmemoized::Removal)
        } else if slice
            .iter()
            .any(|(block, _)| self.base.entry_of_hash(&block.hash).is_some())
        {
            Err(Unmemoized::InheritedBlock)
        } else {
            self.derive_part(account, &slice).ok_or(Unmemoized::Failed)
        };
        AccountMemo { slice, derived }
    }

    fn derive_part(&self, account: Account, slice: &Slice) -> Option<Derived> {
        let entries: Vec<(CertifiedBlock, Certification)> = slice
            .iter()
            .filter_map(|(block, entry)| Some((*block, (*entry)?)))
            .collect();
        let view = AccountView {
            reporter: PublicKey::ZERO,
            weight: Amount::raw(1),
            entries,
            residual: Vec::new(),
            summarized: None,
        };
        let views = std::slice::from_ref(&view);
        let part = self.previous.account_part(account);
        let index = AccountIndex::new(&self.base, views);
        let rules = BuildRules { epoch: self.epoch };
        let state = build_state_of(&part, views, &index, rules, &|_, _| false).ok()?;
        let outcome = state.account_outcome()?;
        let mut claims = Vec::new();
        let mut fresh = BTreeSet::new();
        for (block, entry) in &view.entries {
            if !inherited_from(&self.previous, block, *entry) {
                entry_claims(self.epoch, block, entry, &mut claims);
            }
            add_fresh(&self.previous, block, entry, &mut fresh);
        }
        Some(Derived {
            outcome,
            claims,
            fresh: fresh.into_iter().collect(),
        })
    }

    /// What the close derives from `selection` with this memo: the claims
    /// the evidence manifest must cover, the fresh finalized blocks, and the
    /// accounts to derive from the reports. A miss when the reports are not
    /// laid over the memo's base, name a reporter twice, or name a block
    /// hash for two accounts; the caller then derives in full.
    pub fn prepare<'m, 'r>(
        &'m self,
        selection: &[SelectedReport<'r>],
    ) -> Result<PreparedClose<'m, 'r>, MemoMiss> {
        if !selection.iter().all(|report| {
            report
                .certified
                .base()
                .is_some_and(|base| Arc::ptr_eq(base, &self.base))
        }) {
            return Err(MemoMiss::OtherBase);
        }
        let mut reporters = BTreeSet::new();
        if !selection
            .iter()
            .all(|report| reporters.insert(report.reporter))
        {
            return Err(MemoMiss::RepeatedReporter);
        }
        let mut deltas: Vec<_> = selection
            .iter()
            .map(|report| report.certified.changes().peekable())
            .collect();
        let mut residuals: Vec<_> = selection
            .iter()
            .map(|report| report.residual.entries().peekable())
            .collect();
        // The accounts with an inherited position take part in the whole
        // derivation whether or not a report names them
        let mut inherited_accounts = self.previous.retained_accounts();
        inherited_accounts.extend(self.base.unfinalized().map(|(block, _)| block.account));
        let mut retained = inherited_accounts.into_iter().peekable();
        let mut clean: Vec<&'m Derived> = Vec::new();
        let mut clean_accounts: BTreeSet<Account> = BTreeSet::new();
        let mut dirty: Vec<(Account, Vec<AccountView<'r>>)> = Vec::new();
        let mut slices: Vec<Slice> = vec![Vec::new(); selection.len()];
        loop {
            let next = deltas
                .iter_mut()
                .filter_map(|delta| delta.peek().map(|(block, _)| block.account))
                .chain(
                    residuals
                        .iter_mut()
                        .filter_map(|residual| residual.peek().map(|(block, _, _)| block.account)),
                )
                .chain(retained.peek().copied())
                .min();
            let Some(account) = next else {
                break;
            };
            let mut voted = false;
            for (delta, slice) in deltas.iter_mut().zip(slices.iter_mut()) {
                slice.clear();
                while let Some((block, entry)) =
                    delta.next_if(|(block, _)| block.account == account)
                {
                    slice.push((*block, entry.copied()));
                }
            }
            for residual in residuals.iter_mut() {
                while residual
                    .next_if(|(block, _, _)| block.account == account)
                    .is_some()
                {
                    voted = true;
                }
            }
            let inherits = retained.next_if_eq(&account).is_some();
            let memo = self
                .accounts
                .get(&account)
                .filter(|_| !inherits && !voted && !self.tainted.contains(&account))
                .filter(|memo| slices.iter().all(|slice| *slice == memo.slice))
                .and_then(|memo| memo.derived.as_ref().ok());
            match memo {
                Some(derived) => {
                    clean.push(derived);
                    clean_accounts.insert(account);
                }
                None => {
                    let range = account_range(account);
                    let views = selection
                        .iter()
                        .map(|report| AccountView::of(report, range.clone()))
                        .collect();
                    dirty.push((account, views));
                }
            }
        }
        // A block hash the reports name for two accounts places the block
        // for one of them in the whole derivation's index: such reports are
        // derived in full
        let mut named: HashMap<BlockHash, Account> = HashMap::new();
        for (account, views) in &dirty {
            for view in views {
                let hashes = view
                    .entries
                    .iter()
                    .map(|(block, _)| block.hash)
                    .chain(view.residual.iter().map(|(block, _, _)| block.hash));
                for hash in hashes {
                    if self
                        .owners
                        .get(&hash)
                        .is_some_and(|owner| owner != account && clean_accounts.contains(owner))
                    {
                        return Err(MemoMiss::SharedHash);
                    }
                    if *named.entry(hash).or_insert(*account) != *account {
                        return Err(MemoMiss::SharedHash);
                    }
                }
            }
        }
        Ok(PreparedClose {
            memo: self,
            clean,
            dirty,
        })
    }
}

/// A selection prepared for derivation with a memo (see `CloseMemo::prepare`)
pub(crate) struct PreparedClose<'m, 'r> {
    memo: &'m CloseMemo,
    clean: Vec<&'m Derived>,
    dirty: Vec<(Account, Vec<AccountView<'r>>)>,
}

impl PreparedClose<'_, '_> {
    /// Accounts taken from the memo, and accounts derived from the reports
    pub fn counts(&self) -> (usize, usize) {
        (self.clean.len(), self.dirty.len())
    }

    /// The fresh finalized blocks the selected reports name, each once, in
    /// canonical order (`fresh_finalized`)
    pub fn fresh(&self) -> Vec<FreshFinalized> {
        let mut fresh: Vec<FreshFinalized> = Vec::new();
        for derived in &self.clean {
            fresh.extend_from_slice(&derived.fresh);
        }
        for (_, views) in &self.dirty {
            let mut part = BTreeSet::new();
            for view in views {
                for (block, entry) in view.changed() {
                    add_fresh(&self.memo.previous, block, entry, &mut part);
                }
            }
            fresh.extend(part);
        }
        fresh.sort_unstable();
        fresh
    }

    /// The claims a candidate's manifest must cover (`candidate_claims`):
    /// the votes behind the reports' entries, the exclusion witnesses for
    /// the inherited records they bypass, and those for the records of the
    /// overlap anchor the fresh finalized blocks bypass
    pub fn claims(&self, fresh: &[FreshFinalized]) -> Vec<(ConsensusEpoch, BlockHash)> {
        let memo = self.memo;
        let mut claims: Vec<(ConsensusEpoch, BlockHash)> = Vec::new();
        for derived in &self.clean {
            claims.extend_from_slice(&derived.claims);
        }
        for (account, views) in &self.dirty {
            for view in views {
                for (block, entry) in view.changed() {
                    if !inherited_from(&memo.previous, block, *entry) {
                        entry_claims(memo.epoch, block, entry, &mut claims);
                    }
                }
                claims.extend(
                    view.residual
                        .iter()
                        .map(|(block, _, _)| (memo.epoch, block.hash)),
                );
            }
            if memo.previous.retained_depth(*account).is_some() {
                let part = memo.previous.account_part(*account);
                let index = AccountIndex::new(&memo.base, views);
                claims.extend(witness_claims_of(&part, views, &index));
            }
        }
        if let Some(anchor) = &memo.anchor {
            let anchored: Vec<FreshFinalized> = fresh
                .iter()
                .filter(|block| anchor.retained_depth(block.slot.account).is_some())
                .copied()
                .collect();
            claims.extend(overlap_claims(anchor, &anchored));
        }
        claims.sort_unstable();
        claims.dedup();
        claims
    }

    /// `BuildState(S_{e-1}, Q_e)`: the predecessor with every account's
    /// part laid over it, the memoized ones as derived during the epoch and
    /// the others derived here. `witness` is consulted for the accounts the
    /// predecessor retains a position of, which are never memoized. None if
    /// an account's derivation did not record what it finalized.
    pub fn build(
        &self,
        witness: &dyn Fn(ConsensusEpoch, &BlockHash) -> bool,
    ) -> Result<Option<EpochLedger>, BuildStateError> {
        let memo = self.memo;
        let rules = BuildRules { epoch: memo.epoch };
        let mut ledger = memo.previous.derivation_start();
        let empty = EpochLedger::new();
        for (account, views) in &self.dirty {
            let part = memo.previous.account_part(*account);
            let index = AccountIndex::new(&memo.base, views);
            let state = build_state_of(&part, views, &index, rules, witness)?;
            let Some(outcome) = state.account_outcome() else {
                return Ok(None);
            };
            ledger.apply_account_outcome(&part, &outcome);
        }
        // A memoized account has no retained position in the predecessor
        for derived in &self.clean {
            ledger.apply_account_outcome(&empty, &derived.outcome);
        }
        Ok(Some(ledger))
    }
}

/// The fresh finalized block an entry names, if any (`fresh_finalized`)
fn add_fresh(
    previous: &EpochLedger,
    block: &CertifiedBlock,
    entry: &Certification,
    fresh: &mut BTreeSet<FreshFinalized>,
) {
    let slot = AccountSlot::new(block.account, block.height);
    if entry.status == CertifiedStatus::Finalized && !previous.is_finalized(&slot, &block.hash) {
        fresh.insert(FreshFinalized {
            slot,
            hash: block.hash,
            previous: entry.previous,
        });
    }
}

fn account_range(account: Account) -> RangeInclusive<CertifiedBlock> {
    CertifiedBlock::new(account, 0, BlockHash::ZERO)
        ..=CertifiedBlock::new(account, u64::MAX, BlockHash::from_bytes([0xff; 32]))
}

/// The part of a selected report naming one account
pub(crate) struct AccountView<'r> {
    reporter: PublicKey,
    weight: Amount,
    /// `changed_or_unfinalized` within the account
    entries: Vec<(CertifiedBlock, Certification)>,
    residual: Vec<(CertifiedBlock, ResidualKind, BlockHash)>,
    /// The whole certified state, which says whether a block is tagged N or
    /// F wherever it sits; None for a memo's own view, which has no votes
    summarized: Option<&'r CertifiedState>,
}

impl<'r> AccountView<'r> {
    fn of(report: &SelectedReport<'r>, range: RangeInclusive<CertifiedBlock>) -> Self {
        let account = range.start().account;
        Self {
            reporter: report.reporter,
            weight: report.weight,
            entries: report
                .certified
                .changed_or_unfinalized_in(range)
                .map(|(block, entry)| (*block, *entry))
                .collect(),
            residual: report
                .residual
                .entries()
                .filter(|(block, _, _)| block.account == account)
                .collect(),
            summarized: Some(report.certified),
        }
    }

    /// The entries the report's delta adds or changes: those the claims and
    /// the fresh finalized blocks are read from (`changed_entries`). An
    /// inherited entry is one of the base's.
    fn changed(&self) -> impl Iterator<Item = (&CertifiedBlock, &Certification)> {
        let delta = self.summarized;
        self.entries.iter().filter_map(move |(block, entry)| {
            let changed = delta.is_none_or(|state| {
                state
                    .base()
                    .is_none_or(|base| base.certification(block) != Some(*entry))
            });
            changed.then_some((block, entry))
        })
    }
}

impl ReportView for AccountView<'_> {
    fn reporter(&self) -> PublicKey {
        self.reporter
    }

    fn weight(&self) -> Amount {
        self.weight
    }

    fn derivation_entries(&self) -> impl Iterator<Item = (CertifiedBlock, Certification)> + '_ {
        self.entries.iter().copied()
    }

    fn residual_entries(
        &self,
    ) -> impl Iterator<Item = (CertifiedBlock, ResidualKind, BlockHash)> + '_ {
        self.residual.iter().copied()
    }

    fn summarizes(&self, hash: &BlockHash) -> bool {
        self.summarized.is_some_and(|state| state.summarizes(hash))
    }
}

/// `ReportIndex::over_base` for the blocks of one account: the base places
/// the predecessor's blocks, and the reports' entries place the rest, the
/// first report naming a block placing it
struct AccountIndex<'a> {
    base: &'a CertifiedState,
    placements: HashMap<BlockHash, BlockPlacement>,
}

impl<'a> AccountIndex<'a> {
    fn new(base: &'a CertifiedState, views: &[AccountView]) -> Self {
        let mut placements = HashMap::new();
        for view in views {
            let named = view
                .entries
                .iter()
                .map(|(block, entry)| (*block, entry.previous))
                .chain(
                    view.residual
                        .iter()
                        .map(|(block, _, previous)| (*block, *previous)),
                );
            for (block, previous) in named {
                if base.entry_of_hash(&block.hash).is_some() {
                    continue;
                }
                placements.entry(block.hash).or_insert(BlockPlacement {
                    slot: AccountSlot::new(block.account, block.height),
                    previous,
                });
            }
        }
        Self { base, placements }
    }
}

impl BlockIndex for AccountIndex<'_> {
    fn placement(&self, hash: &BlockHash) -> Option<BlockPlacement> {
        if let Some((block, entry)) = self.base.entry_of_hash(hash) {
            return Some(BlockPlacement {
                slot: AccountSlot::new(block.account, block.height),
                previous: entry.previous,
            });
        }
        self.placements.get(hash).copied()
    }
}
