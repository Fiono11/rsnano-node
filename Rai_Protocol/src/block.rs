use std::collections::{BTreeMap, BTreeSet};

use crate::crypto::{verify_ed25519, AccountKeyStore, Ed25519PublicKey, Signature};
use crate::error::{RaiError, Result};
use crate::types::{
    put_u128, put_u32, put_u64, AccountId, Amount, Hash32, ReplicaId, Slot, Weight,
};

pub const DEFAULT_GENESIS_BALANCE: Amount = 1_000;
pub const DEFAULT_GENESIS_REPRESENTATIVE: ReplicaId = 1;

/// One spend output created by an account block.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Send {
    pub destination: AccountId,
    pub amount: Amount,
}

impl Send {
    fn encode(&self, out: &mut Vec<u8>) {
        put_u64(out, self.destination);
        put_u128(out, self.amount);
    }
}

/// Globally unique reference to one output in a finalized source block.
///
/// The output index is required because one block may contain two sends with
/// identical destination and amount.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SendId {
    pub source_block: Hash32,
    pub output_index: u32,
}

impl SendId {
    pub const fn new(source_block: Hash32, output_index: u32) -> Self {
        Self {
            source_block,
            output_index,
        }
    }

    pub fn encode(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.source_block.0);
        put_u32(out, self.output_index);
    }

    pub fn hash(self, send: &Send) -> Hash32 {
        let mut bytes = Vec::with_capacity(96);
        bytes.extend_from_slice(b"rai-send-v1");
        self.encode(&mut bytes);
        send.encode(&mut bytes);
        Hash32::digest(&bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Receive {
    pub send: SendId,
}

impl Receive {
    fn encode(self, out: &mut Vec<u8>) {
        self.send.encode(out);
    }
}

/// Canonical account state committed by the certified frontier.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct AccountState {
    pub frontier: Hash32,
    pub balance: Amount,
    pub representative: ReplicaId,
    pub owner: Ed25519PublicKey,
}

impl AccountState {
    pub fn encode(&self, account: AccountId, out: &mut Vec<u8>) {
        put_u64(out, account);
        out.extend_from_slice(&self.frontier.0);
        put_u128(out, self.balance);
        put_u64(out, self.representative);
        out.extend_from_slice(&self.owner.0);
    }
}

/// Hardcoded initial account state. Genesis blocks are unsigned and are part of
/// the deterministic network configuration.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct GenesisAccount {
    pub account: AccountId,
    pub balance: Amount,
    pub representative: ReplicaId,
    pub owner: Ed25519PublicKey,
}

impl GenesisAccount {
    pub const fn new(
        account: AccountId,
        balance: Amount,
        representative: ReplicaId,
        owner: Ed25519PublicKey,
    ) -> Self {
        Self {
            account,
            balance,
            representative,
            owner,
        }
    }

    /// Reproducible genesis helper for tests and simulations. Production
    /// configurations should use `new` with an independently generated key.
    pub fn deterministic(account: AccountId, balance: Amount, representative: ReplicaId) -> Self {
        let keys = AccountKeyStore::deterministic([account]);
        Self::new(
            account,
            balance,
            representative,
            keys.public_key(account).expect("deterministic account key"),
        )
    }

    pub fn block(&self) -> Block {
        Block {
            slot: Slot::new(self.account, 0),
            parent: Hash32::ZERO,
            balance: self.balance,
            representative: self.representative,
            sends: Vec::new(),
            receives: Vec::new(),
        }
    }

    pub fn hash(&self) -> Hash32 {
        SignedBlock::configured_genesis(self.block(), self.owner).hash()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    pub slot: Slot,
    pub parent: Hash32,
    /// Post-state balance after all sends and receives in this block.
    pub balance: Amount,
    /// Replica receiving this account's voting weight after finalization and
    /// the lagged epoch-close committee transition.
    pub representative: ReplicaId,
    pub sends: Vec<Send>,
    pub receives: Vec<Receive>,
}

impl Block {
    /// Canonical protocol body from rai_spec.tex. `balance` is retained as
    /// non-consensus compatibility metadata and is deliberately excluded from
    /// authorization and non-genesis block identity.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        self.slot.encode(&mut out);
        out.extend_from_slice(&self.parent.0);
        put_u64(&mut out, self.sends.len() as u64);
        for send in &self.sends {
            send.encode(&mut out);
        }
        put_u64(&mut out, self.receives.len() as u64);
        for receive in &self.receives {
            receive.encode(&mut out);
        }
        put_u64(&mut out, self.representative);
        // Genesis is configuration, not an owner-authorized account block.
        // Its identity commits to the configured balance. The complete genesis
        // state separately commits to the configured owner keys.
        if self.slot.sequence == 0 {
            put_u128(&mut out, self.balance);
        }
        out
    }

    pub fn account(&self) -> AccountId {
        self.slot.account
    }

    pub fn sequence(&self) -> u64 {
        self.slot.sequence
    }

    pub fn hash(&self) -> Hash32 {
        // This is the unsigned body hash. Non-genesis protocol block identity
        // is SignedBlock::hash(), which also commits to the owner signature.
        Hash32::digest(&self.canonical_bytes())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedBlock {
    pub block: Block,
    pub signature: Signature,
}

impl SignedBlock {
    pub fn sign(keys: &AccountKeyStore, block: Block) -> Result<Self> {
        let signature = keys
            .sign(block.account(), &Self::authorization_bytes_for(&block))
            .ok_or(RaiError::InvalidSignature)?;
        Ok(Self { block, signature })
    }

    /// Signs an account block while documenting the relay that first receives
    /// it. Relay identity is deliberately excluded from both the block hash and
    /// authorization signature, so the same envelope can be forwarded intact.
    pub fn new(keys: &AccountKeyStore, block: Block, _initial_relay: ReplicaId) -> Result<Self> {
        Self::sign(keys, block)
    }

    fn configured_genesis(block: Block, owner: Ed25519PublicKey) -> Self {
        let mut genesis_commitment = [0; 64];
        genesis_commitment[..32].copy_from_slice(&owner.0);
        Self {
            block,
            signature: Signature(genesis_commitment),
        }
    }

    pub fn authorization_bytes(&self) -> Vec<u8> {
        Self::authorization_bytes_for(&self.block)
    }

    fn authorization_bytes_for(block: &Block) -> Vec<u8> {
        let canonical = block.canonical_bytes();
        let mut bytes = Vec::with_capacity(32 + canonical.len());
        bytes.extend_from_slice(b"RAI/AccountBlock/v1");
        bytes.extend_from_slice(&canonical);
        Hash32::digest(&bytes).0.to_vec()
    }

    pub fn verify(&self, owner: Ed25519PublicKey) -> bool {
        verify_ed25519(owner, &self.authorization_bytes(), &self.signature)
    }

    pub fn hash(&self) -> Hash32 {
        let body = self.block.canonical_bytes();
        let mut bytes = Vec::with_capacity(body.len() + self.signature.0.len());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(&self.signature.0);
        Hash32::digest(&bytes)
    }

    pub fn send_id(&self, output_index: usize) -> Option<SendId> {
        let output_index = u32::try_from(output_index).ok()?;
        self.block
            .sends
            .get(output_index as usize)
            .map(|_| SendId::new(self.hash(), output_index))
    }
}

#[derive(Clone, Debug)]
pub struct BlockStore {
    candidates: BTreeMap<Hash32, SignedBlock>,
    complete: BTreeSet<Hash32>,
    finalized_slots: BTreeMap<Slot, Hash32>,
    genesis_accounts: BTreeMap<AccountId, GenesisAccount>,
    consumed_sends: BTreeMap<SendId, Hash32>,
}

impl Default for BlockStore {
    fn default() -> Self {
        Self {
            candidates: BTreeMap::new(),
            complete: BTreeSet::new(),
            finalized_slots: BTreeMap::new(),
            genesis_accounts: BTreeMap::new(),
            consumed_sends: BTreeMap::new(),
        }
    }
}

impl BlockStore {
    pub fn with_genesis(accounts: impl IntoIterator<Item = GenesisAccount>) -> Result<Self> {
        let mut store = Self::default();
        for account in accounts {
            if !account.owner.is_valid() {
                return Err(RaiError::InvalidConfiguration(format!(
                    "genesis account {} has an invalid Ed25519 public key",
                    account.account
                )));
            }
            if account.representative == 0 {
                return Err(RaiError::InvalidConfiguration(format!(
                    "genesis account {} has replica 0 as representative",
                    account.account
                )));
            }
            if store.genesis_accounts.contains_key(&account.account) {
                return Err(RaiError::InvalidConfiguration(format!(
                    "duplicate genesis account {}",
                    account.account
                )));
            }
            let block = SignedBlock::configured_genesis(account.block(), account.owner);
            let hash = block.hash();
            store.complete.insert(hash);
            store.finalized_slots.insert(block.block.slot, hash);
            store.candidates.insert(hash, block);
            store.genesis_accounts.insert(account.account, account);
        }
        if store.genesis_accounts.is_empty() {
            return Err(RaiError::InvalidConfiguration(
                "genesis must contain at least one account".into(),
            ));
        }
        Ok(store)
    }

    /// Compatibility genesis hash for tests and callers that have not supplied
    /// an explicit genesis configuration. The simulator uses `with_genesis`.
    pub fn genesis(account: AccountId) -> Hash32 {
        GenesisAccount::deterministic(
            account,
            DEFAULT_GENESIS_BALANCE,
            DEFAULT_GENESIS_REPRESENTATIVE,
        )
        .hash()
    }

    pub fn genesis_accounts(&self) -> &BTreeMap<AccountId, GenesisAccount> {
        &self.genesis_accounts
    }

    pub fn consumed_sends(&self) -> &BTreeMap<SendId, Hash32> {
        &self.consumed_sends
    }

    /// Returns the canonical account-to-frontier map for the currently
    /// finalized ledger state. Every configured account is present, including
    /// accounts whose frontier is still their genesis block.
    pub fn frontier_map(&self) -> Result<BTreeMap<AccountId, Hash32>> {
        Ok(self
            .account_states()?
            .into_iter()
            .map(|(account, state)| (account, state.frontier))
            .collect())
    }

    pub fn ledger_root(&self) -> Result<Hash32> {
        Ok(hash_ledger_frontiers(&self.frontier_map()?))
    }

    pub fn genesis_frontiers(&self) -> BTreeMap<AccountId, Hash32> {
        self.genesis_accounts
            .iter()
            .map(|(account, genesis)| (*account, genesis.hash()))
            .collect()
    }

    pub fn genesis_ledger_root(&self) -> Hash32 {
        hash_ledger_frontiers(&self.genesis_frontiers())
    }

    /// Stages an authenticated candidate for later full-ledger replay. This
    /// intentionally does not require referenced sends to be finalized yet; a
    /// bootstrapping close package may contain both the send and its receive.
    /// `validate_ledger_frontiers` performs the authoritative transition replay.
    pub(crate) fn stage_candidate_for_replay(&mut self, signed: SignedBlock) -> Result<Hash32> {
        let owner = self
            .genesis_accounts
            .get(&signed.block.account())
            .map(|account| account.owner)
            .ok_or_else(|| {
                RaiError::Inadmissible(format!(
                    "account {} has no configured authorization key",
                    signed.block.account()
                ))
            })?;
        if !signed.verify(owner) {
            return Err(RaiError::InvalidSignature);
        }
        let hash = signed.hash();
        if let Some(existing) = self.candidates.get(&hash) {
            if existing.block.canonical_bytes() != signed.block.canonical_bytes()
                || existing.signature != signed.signature
            {
                return Err(RaiError::SafetyFault(
                    "same block hash mapped to different block bytes".into(),
                ));
            }
        } else {
            self.candidates.insert(hash, signed);
        }
        Ok(hash)
    }

    pub(crate) fn mark_complete_for_replay(&mut self, hash: Hash32) -> Result<bool> {
        let signed = self
            .candidates
            .get(&hash)
            .ok_or_else(|| RaiError::UnknownCandidate(hash.to_string()))?;
        if !self.parent_complete(&signed.block) {
            return Err(RaiError::Incomplete(format!(
                "parent of block {} is not complete",
                hash.short()
            )));
        }
        Ok(self.complete.insert(hash))
    }

    pub fn insert_candidate(&mut self, signed: SignedBlock) -> Result<Hash32> {
        let owner = self
            .genesis_accounts
            .get(&signed.block.account())
            .map(|account| account.owner)
            .ok_or_else(|| {
                RaiError::Inadmissible(format!(
                    "account {} has no configured authorization key",
                    signed.block.account()
                ))
            })?;
        if !signed.verify(owner) {
            return Err(RaiError::InvalidSignature);
        }
        let hash = signed.hash();
        self.validate_ledger_transition(&signed.block, hash)?;
        if let Some(existing) = self.candidates.get(&hash) {
            if existing.block.canonical_bytes() != signed.block.canonical_bytes()
                || existing.signature != signed.signature
            {
                return Err(RaiError::SafetyFault(
                    "same block hash mapped to different block bytes".into(),
                ));
            }
        } else {
            self.candidates.insert(hash, signed);
        }
        Ok(hash)
    }

    pub fn candidate(&self, hash: Hash32) -> Option<&SignedBlock> {
        self.candidates.get(&hash)
    }

    pub(crate) fn signed_candidates(
        &self,
        hashes: impl IntoIterator<Item = Hash32>,
    ) -> Result<Vec<SignedBlock>> {
        let mut unique = hashes.into_iter().collect::<BTreeSet<_>>();
        let mut blocks = Vec::with_capacity(unique.len());
        for hash in std::mem::take(&mut unique) {
            let signed = self
                .candidates
                .get(&hash)
                .cloned()
                .ok_or_else(|| RaiError::UnknownCandidate(hash.to_string()))?;
            // Genesis account state is configuration and need not be repeated in
            // every close package.
            if signed.block.slot.sequence != 0 {
                blocks.push(signed);
            }
        }
        blocks.sort_by_key(|signed| {
            (
                signed.block.slot.account,
                signed.block.slot.sequence,
                signed.hash(),
            )
        });
        Ok(blocks)
    }

    pub fn is_complete(&self, hash: Hash32) -> bool {
        self.complete.contains(&hash)
    }

    pub fn finalized(&self, slot: Slot) -> Option<Hash32> {
        self.finalized_slots.get(&slot).copied()
    }

    pub fn finalized_slots(&self) -> &BTreeMap<Slot, Hash32> {
        &self.finalized_slots
    }

    pub fn deepest_complete_parents(&self, account: AccountId) -> Vec<(u64, Hash32)> {
        let deepest = self
            .complete
            .iter()
            .filter_map(|hash| {
                self.candidates.get(hash).and_then(|signed| {
                    (signed.block.slot.account == account)
                        .then_some((signed.block.slot.sequence, *hash))
                })
            })
            .max_by_key(|(sequence, _)| *sequence)
            .map(|(sequence, _)| sequence);

        match deepest {
            None => vec![(0, self.genesis_hash(account))],
            Some(sequence) => self
                .complete
                .iter()
                .filter_map(|hash| {
                    self.candidates.get(hash).and_then(|signed| {
                        (signed.block.slot.account == account
                            && signed.block.slot.sequence == sequence)
                            .then_some((sequence, *hash))
                    })
                })
                .collect(),
        }
    }

    pub fn complete_candidates_at_slot(&self, slot: Slot) -> Vec<Hash32> {
        self.complete
            .iter()
            .filter_map(|hash| {
                self.candidates
                    .get(hash)
                    .and_then(|signed| (signed.block.slot == slot).then_some(*hash))
            })
            .collect()
    }

    pub fn candidates_at_slot(&self, slot: Slot) -> Vec<Hash32> {
        self.candidates
            .iter()
            .filter_map(|(hash, signed)| (signed.block.slot == slot).then_some(*hash))
            .collect()
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    pub fn complete_count(&self) -> usize {
        self.complete.len()
    }

    pub fn parent_complete(&self, block: &Block) -> bool {
        if block.slot.sequence == 0 {
            return block.parent == Hash32::ZERO;
        }
        if block.slot.sequence == 1 {
            return block.parent == self.genesis_hash(block.slot.account);
        }
        let Some(parent) = self.candidates.get(&block.parent) else {
            return false;
        };
        self.complete.contains(&block.parent)
            && parent.block.slot.account == block.slot.account
            && parent.block.slot.sequence + 1 == block.slot.sequence
    }

    pub fn ancestors_consistent(&self, target: Hash32) -> Result<bool> {
        for hash in self.chain_to_genesis(target)? {
            let block = &self
                .candidates
                .get(&hash)
                .expect("chain contains known block")
                .block;
            if let Some(finalized) = self.finalized(block.slot) {
                if finalized != hash {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub fn stable_prefix(&self, target: Hash32) -> Result<bool> {
        let chain = self.chain_to_genesis(target)?;
        for ancestor in chain.iter().take(chain.len().saturating_sub(1)) {
            let block = self
                .candidates
                .get(ancestor)
                .ok_or_else(|| RaiError::UnknownCandidate(ancestor.to_string()))?;
            if self.finalized(block.block.slot) != Some(*ancestor) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn admissible_for_slot(&self, slot: Slot, hash: Hash32) -> Result<bool> {
        let Some(signed) = self.candidates.get(&hash) else {
            return Ok(false);
        };
        if signed.block.slot != slot || !self.parent_complete(&signed.block) {
            return Ok(false);
        }
        if self
            .validate_ledger_transition(&signed.block, hash)
            .is_err()
        {
            return Ok(false);
        }
        if !self.ancestors_consistent(hash)? {
            return Ok(false);
        }
        Ok(match self.finalized(slot) {
            None => true,
            Some(value) => value == hash,
        })
    }

    pub(crate) fn mark_complete(&mut self, hash: Hash32) -> Result<bool> {
        let Some(signed) = self.candidates.get(&hash) else {
            return Err(RaiError::UnknownCandidate(hash.to_string()));
        };
        self.validate_ledger_transition(&signed.block, hash)?;
        if !self.parent_complete(&signed.block) {
            return Err(RaiError::Incomplete(format!(
                "parent of block {} is not complete",
                hash.short()
            )));
        }
        if !self.ancestors_consistent(hash)? {
            return Err(RaiError::SafetyFault(
                "block ancestry conflicts with an already finalized slot".into(),
            ));
        }
        Ok(self.complete.insert(hash))
    }

    pub fn finalize_chain(&mut self, target: Hash32) -> Result<Vec<(Slot, Hash32)>> {
        if !self.complete.contains(&target) {
            return Err(RaiError::Incomplete(format!(
                "cannot finalize incomplete block {}",
                target.short()
            )));
        }
        if !self.ancestors_consistent(target)? {
            return Err(RaiError::SafetyFault(
                "certificate chain conflicts with finalized state".into(),
            ));
        }
        if !self.stable_prefix(target)? {
            return Err(RaiError::SafetyFault(
                "finality proof does not extend a certified stable prefix".into(),
            ));
        }

        let chain = self.chain_to_genesis(target)?;
        let mut staged_slots = self.finalized_slots.clone();
        let mut staged_consumed = self.consumed_sends.clone();
        let mut changed = Vec::new();

        for hash in &chain {
            let block = &self
                .candidates
                .get(hash)
                .expect("chain contains known block")
                .block;
            self.validate_ledger_transition_against(block, *hash, &staged_consumed)?;
            match staged_slots.get(&block.slot) {
                None => {
                    staged_slots.insert(block.slot, *hash);
                    changed.push((block.slot, *hash));
                }
                Some(existing) if *existing == *hash => {}
                Some(existing) => {
                    return Err(RaiError::SafetyFault(format!(
                        "slot {} already finalized as {}, cannot finalize {}",
                        block.slot,
                        existing.short(),
                        hash.short()
                    )));
                }
            }
            for receive in &block.receives {
                match staged_consumed.get(&receive.send) {
                    None => {
                        staged_consumed.insert(receive.send, *hash);
                    }
                    Some(existing) if *existing == *hash => {}
                    Some(existing) => {
                        return Err(RaiError::SafetyFault(format!(
                            "send {}:{} already received by block {}",
                            receive.send.source_block.short(),
                            receive.send.output_index,
                            existing.short()
                        )));
                    }
                }
            }
        }

        self.finalized_slots = staged_slots;
        self.consumed_sends = staged_consumed;
        Ok(changed)
    }

    /// Reconstructs and validates the complete ledger selected by a canonical
    /// frontier map. Validation starts from genesis and replays every block on
    /// every committed account chain. The returned store contains exactly the
    /// ledger finalized by those frontiers, irrespective of unrelated live
    /// finality in `self`.
    pub fn validate_ledger_frontiers(
        &self,
        frontiers: &BTreeMap<AccountId, Hash32>,
    ) -> Result<Self> {
        let expected_accounts = self
            .genesis_accounts
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        let supplied_accounts = frontiers.keys().copied().collect::<BTreeSet<_>>();
        if supplied_accounts != expected_accounts {
            return Err(RaiError::InvalidClosePackage(
                "ledger frontier map must contain exactly every configured account".into(),
            ));
        }

        let mut replay = self.clone();
        replay.finalized_slots.clear();
        replay.consumed_sends.clear();
        for genesis in replay.genesis_accounts.values() {
            replay
                .finalized_slots
                .insert(genesis.block().slot, genesis.hash());
        }

        let mut required = BTreeSet::<Hash32>::new();
        for (account, frontier) in frontiers {
            let genesis = self.genesis_hash(*account);
            if *frontier == genesis {
                continue;
            }
            let tip = self
                .candidates
                .get(frontier)
                .ok_or_else(|| RaiError::UnknownCandidate(frontier.to_string()))?;
            if tip.block.slot.account != *account {
                return Err(RaiError::InvalidClosePackage(format!(
                    "frontier {} belongs to account {}, not account {account}",
                    frontier.short(),
                    tip.block.slot.account
                )));
            }
            let owner = self
                .genesis_accounts
                .get(account)
                .expect("frontier account checked against genesis")
                .owner;
            for hash in self.chain_to_genesis(*frontier)? {
                let signed = self
                    .candidates
                    .get(&hash)
                    .ok_or_else(|| RaiError::UnknownCandidate(hash.to_string()))?;
                if signed.block.slot.account != *account || !signed.verify(owner) {
                    return Err(RaiError::InvalidSignature);
                }
                if !self.complete.contains(&hash) {
                    return Err(RaiError::Incomplete(format!(
                        "certified frontier references incomplete block {}",
                        hash.short()
                    )));
                }
                required.insert(hash);
            }
        }

        let mut pending = required.into_iter().collect::<Vec<_>>();
        pending.sort_by_key(|hash| {
            self.candidates
                .get(hash)
                .map(|signed| (signed.block.slot.sequence, signed.block.slot.account, *hash))
                .unwrap_or((u64::MAX, u64::MAX, *hash))
        });

        while !pending.is_empty() {
            let mut deferred = Vec::new();
            let mut first_error = None;
            let mut progressed = false;
            for hash in pending {
                match replay.finalize_chain(hash) {
                    Ok(_) => progressed = true,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error.clone());
                        }
                        deferred.push(hash);
                    }
                }
            }
            if deferred.is_empty() {
                break;
            }
            if !progressed {
                return Err(first_error.expect("a deferred frontier block has an error"));
            }
            pending = deferred;
        }

        if replay.frontier_map()? != *frontiers {
            return Err(RaiError::InvalidClosePackage(
                "replayed ledger frontiers differ from the committed frontier map".into(),
            ));
        }
        Ok(replay)
    }

    /// Reconstructs the ledger baseline visible to one epoch close. Finalized
    /// blocks from the adjacent open epoch remain in the live store but are not
    /// allowed to affect the closing epoch's balance/delegation snapshot.
    pub fn certified_baseline(
        &self,
        accounts: Option<&BTreeMap<AccountId, AccountState>>,
        consumed_sends: Option<&BTreeMap<SendId, Hash32>>,
    ) -> Result<Self> {
        let mut baseline = self.clone();
        baseline.finalized_slots.clear();
        baseline.consumed_sends.clear();

        for genesis in baseline.genesis_accounts.values() {
            baseline
                .finalized_slots
                .insert(genesis.block().slot, genesis.hash());
        }

        match (accounts, consumed_sends) {
            (None, None) => return Ok(baseline),
            (Some(accounts), Some(consumed_sends)) => {
                for (account, state) in accounts {
                    let genesis = baseline.genesis_hash(*account);
                    if state.frontier == genesis {
                        continue;
                    }
                    for hash in baseline.chain_to_genesis(state.frontier)? {
                        if !baseline.complete.contains(&hash) {
                            return Err(RaiError::Incomplete(format!(
                                "certified frontier references incomplete block {}",
                                hash.short()
                            )));
                        }
                        let block = baseline
                            .candidates
                            .get(&hash)
                            .ok_or_else(|| RaiError::UnknownCandidate(hash.to_string()))?;
                        match baseline.finalized_slots.get(&block.block.slot) {
                            None => {
                                baseline.finalized_slots.insert(block.block.slot, hash);
                            }
                            Some(existing) if *existing == hash => {}
                            Some(existing) => {
                                return Err(RaiError::SafetyFault(format!(
                                    "certified account frontier conflicts at slot {}: {} versus {}",
                                    block.block.slot,
                                    existing.short(),
                                    hash.short()
                                )));
                            }
                        }
                    }
                }
                baseline.consumed_sends = consumed_sends.clone();
                Ok(baseline)
            }
            _ => Err(RaiError::InvalidConfiguration(
                "certified account and consumed-send snapshots must be supplied together".into(),
            )),
        }
    }

    /// Verifies that a set of finalization targets can be installed atomically,
    /// including the global receive-once rule.
    pub fn validate_finalization_set(
        &self,
        targets: impl IntoIterator<Item = Hash32>,
    ) -> Result<Self> {
        let mut pending = targets
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        pending.sort_by_key(|target| {
            self.candidates
                .get(target)
                .map(|block| (block.block.slot.account, block.block.slot.sequence, *target))
                .unwrap_or((u64::MAX, u64::MAX, *target))
        });

        // Cross-account receives can make the valid finalization order differ
        // from account-id order. Repeated deterministic passes allow a source
        // send finalized in this same atomic set to unlock its destination
        // receive without weakening the rule that the send must be final first.
        let mut staged = self.clone();
        while !pending.is_empty() {
            let mut deferred = Vec::new();
            let mut first_error = None;
            let mut progressed = false;

            for target in pending {
                match staged.finalize_chain(target) {
                    Ok(_) => progressed = true,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error.clone());
                        }
                        deferred.push(target);
                    }
                }
            }

            if deferred.is_empty() {
                return Ok(staged);
            }
            if !progressed {
                return Err(first_error.expect("a deferred target has an error"));
            }
            pending = deferred;
        }
        Ok(staged)
    }

    pub fn descends_from(&self, target: Hash32, ancestor: Hash32) -> Result<bool> {
        if target == ancestor {
            return Ok(false);
        }
        let mut current = target;
        loop {
            let Some(signed) = self.candidates.get(&current) else {
                return Ok(false);
            };
            if signed.block.parent == ancestor {
                return Ok(true);
            }
            if signed.block.slot.sequence <= 1 {
                return Ok(false);
            }
            current = signed.block.parent;
        }
    }

    pub fn chain_to_genesis(&self, target: Hash32) -> Result<Vec<Hash32>> {
        let mut reverse = Vec::new();
        let mut current = target;
        loop {
            let Some(signed) = self.candidates.get(&current) else {
                return Err(RaiError::UnknownCandidate(current.to_string()));
            };
            if signed.block.slot.sequence == 0 {
                break;
            }
            reverse.push(current);
            if signed.block.slot.sequence == 1 {
                if signed.block.parent != self.genesis_hash(signed.block.slot.account) {
                    return Err(RaiError::Incomplete(
                        "sequence-1 block does not point to account genesis".into(),
                    ));
                }
                break;
            }
            current = signed.block.parent;
        }
        reverse.reverse();
        Ok(reverse)
    }

    pub fn account_states(&self) -> Result<BTreeMap<AccountId, AccountState>> {
        let mut accounts = self
            .genesis_accounts
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        accounts.extend(
            self.candidates
                .values()
                .map(|signed| signed.block.slot.account),
        );
        let mut states = BTreeMap::new();
        for account in accounts {
            let tip = self
                .finalized_slots
                .iter()
                .filter(|(slot, _)| slot.account == account)
                .max_by_key(|(slot, _)| slot.sequence)
                .map(|(_, hash)| *hash)
                .unwrap_or_else(|| self.genesis_hash(account));
            let block = self.block_or_synthetic_genesis(account, tip)?;
            states.insert(
                account,
                AccountState {
                    frontier: tip,
                    balance: self.derived_balance(account, tip)?,
                    representative: block.representative,
                    owner: self
                        .genesis_accounts
                        .get(&account)
                        .ok_or_else(|| {
                            RaiError::InvalidConfiguration(format!(
                                "account {account} has no configured owner key"
                            ))
                        })?
                        .owner,
                },
            );
        }
        Ok(states)
    }

    pub fn account_state_root(&self) -> Result<Hash32> {
        hash_account_state(&self.account_states()?, &self.consumed_sends)
    }

    pub fn genesis_close_hash(&self) -> Result<Hash32> {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(b"rai-genesis-close-v2-frontiers");
        bytes.extend_from_slice(&self.genesis_ledger_root().0);
        Ok(Hash32::digest(&bytes))
    }

    pub fn representative_weights(&self) -> Result<BTreeMap<ReplicaId, Weight>> {
        let mut weights = BTreeMap::<ReplicaId, Weight>::new();
        for state in self.account_states()?.into_values() {
            let entry = weights.entry(state.representative).or_default();
            *entry = entry.checked_add(state.balance).ok_or_else(|| {
                RaiError::InvalidConfiguration("representative weight overflow".into())
            })?;
        }
        weights.retain(|_, weight| *weight > 0);
        Ok(weights)
    }

    fn genesis_hash(&self, account: AccountId) -> Hash32 {
        self.genesis_accounts
            .get(&account)
            .map(GenesisAccount::hash)
            .unwrap_or_else(|| Self::genesis(account))
    }

    fn synthetic_genesis(&self, account: AccountId) -> Block {
        self.genesis_accounts
            .get(&account)
            .cloned()
            .unwrap_or_else(|| {
                GenesisAccount::deterministic(
                    account,
                    DEFAULT_GENESIS_BALANCE,
                    DEFAULT_GENESIS_REPRESENTATIVE,
                )
            })
            .block()
    }

    fn block_or_synthetic_genesis(&self, account: AccountId, hash: Hash32) -> Result<Block> {
        if let Some(block) = self.candidates.get(&hash) {
            return Ok(block.block.clone());
        }
        let genesis = self.synthetic_genesis(account);
        if genesis.hash() == hash {
            Ok(genesis)
        } else {
            Err(RaiError::UnknownCandidate(hash.to_string()))
        }
    }

    fn validate_ledger_transition(&self, block: &Block, block_hash: Hash32) -> Result<()> {
        self.validate_ledger_transition_against(block, block_hash, &self.consumed_sends)
    }

    fn validate_ledger_transition_against(
        &self,
        block: &Block,
        block_hash: Hash32,
        consumed: &BTreeMap<SendId, Hash32>,
    ) -> Result<()> {
        if block.slot.sequence == 0 {
            return Err(RaiError::Inadmissible(
                "runtime proposals cannot replace a configured genesis block".into(),
            ));
        }
        if !self.genesis_accounts.is_empty()
            && !self.genesis_accounts.contains_key(&block.slot.account)
        {
            return Err(RaiError::Inadmissible(format!(
                "account {} is absent from the configured genesis state",
                block.slot.account
            )));
        }
        if block.representative == 0 {
            return Err(RaiError::Inadmissible(
                "replica 0 cannot be an account representative".into(),
            ));
        }
        let parent = if block.slot.sequence == 1 {
            let genesis = self.synthetic_genesis(block.slot.account);
            if block.parent != self.genesis_hash(block.slot.account) {
                return Err(RaiError::Inadmissible(format!(
                    "sequence-1 block parent {} does not reference configured genesis {}",
                    block.parent.short(),
                    self.genesis_hash(block.slot.account).short()
                )));
            }
            genesis
        } else {
            self.candidates
                .get(&block.parent)
                .ok_or_else(|| RaiError::UnknownCandidate(block.parent.to_string()))?
                .block
                .clone()
        };
        if parent.slot.account != block.slot.account
            || parent.slot.sequence + 1 != block.slot.sequence
        {
            return Err(RaiError::Inadmissible(
                "block does not extend the immediately preceding account slot".into(),
            ));
        }

        if block.sends.len() > u32::MAX as usize {
            return Err(RaiError::Inadmissible(
                "block has more sends than the canonical output index can address".into(),
            ));
        }
        let mut debits: Amount = 0;
        for send in &block.sends {
            if send.amount == 0 {
                return Err(RaiError::Inadmissible(
                    "zero-amount sends are not canonical".into(),
                ));
            }
            if !self.genesis_accounts.is_empty()
                && !self.genesis_accounts.contains_key(&send.destination)
            {
                return Err(RaiError::Inadmissible(format!(
                    "send destination account {} is absent from genesis",
                    send.destination
                )));
            }
            debits = debits
                .checked_add(send.amount)
                .ok_or_else(|| RaiError::Inadmissible("send total overflow".into()))?;
        }
        let mut unique_receives = BTreeSet::new();
        let mut credits: Amount = 0;
        for receive in &block.receives {
            if !unique_receives.insert(receive.send) {
                return Err(RaiError::Inadmissible(
                    "one block references the same send more than once".into(),
                ));
            }
            if let Some(existing) = consumed.get(&receive.send) {
                if *existing != block_hash {
                    return Err(RaiError::Inadmissible(format!(
                        "send {}:{} has already been received by block {}",
                        receive.send.source_block.short(),
                        receive.send.output_index,
                        existing.short()
                    )));
                }
            }
            let source = self
                .candidates
                .get(&receive.send.source_block)
                .ok_or_else(|| RaiError::UnknownCandidate(receive.send.source_block.to_string()))?;
            if self.finalized(source.block.slot) != Some(receive.send.source_block) {
                return Err(RaiError::Inadmissible(
                    "a receive may reference only a finalized send block".into(),
                ));
            }
            let send = source
                .block
                .sends
                .get(receive.send.output_index as usize)
                .ok_or_else(|| {
                    RaiError::Inadmissible("receive output index is out of range".into())
                })?;
            if send.destination != block.slot.account {
                return Err(RaiError::Inadmissible(
                    "send destination does not match the receiving account".into(),
                ));
            }
            credits = credits
                .checked_add(send.amount)
                .ok_or_else(|| RaiError::Inadmissible("receive total overflow".into()))?;
        }

        let parent_balance = self.derived_balance(block.slot.account, block.parent)?;
        let available = parent_balance
            .checked_add(credits)
            .ok_or_else(|| RaiError::Inadmissible("post-state balance overflow".into()))?;
        let expected = available.checked_sub(debits).ok_or_else(|| {
            RaiError::Inadmissible(format!(
                "account {} spends more than its parent balance plus finalized receives",
                block.slot.account
            ))
        })?;
        let _derived_post_state_balance = expected;
        Ok(())
    }

    /// Derives spendable balance exclusively from genesis and ledger
    /// transitions; no balance claimed by a block is trusted.
    fn derived_balance(&self, account: AccountId, frontier: Hash32) -> Result<Amount> {
        let genesis = self.genesis_accounts.get(&account).ok_or_else(|| {
            RaiError::InvalidConfiguration(format!(
                "account {account} has no configured genesis balance"
            ))
        })?;
        let mut balance = genesis.balance;
        if frontier == genesis.hash() {
            return Ok(balance);
        }
        for hash in self.chain_to_genesis(frontier)? {
            let block = &self
                .candidates
                .get(&hash)
                .ok_or_else(|| RaiError::UnknownCandidate(hash.to_string()))?
                .block;
            let credits = block.receives.iter().try_fold(0u128, |sum, receive| {
                let source = self
                    .candidates
                    .get(&receive.send.source_block)
                    .ok_or_else(|| {
                        RaiError::UnknownCandidate(receive.send.source_block.to_string())
                    })?;
                let send = source
                    .block
                    .sends
                    .get(receive.send.output_index as usize)
                    .ok_or_else(|| {
                        RaiError::Inadmissible("receive output index is out of range".into())
                    })?;
                sum.checked_add(send.amount)
                    .ok_or_else(|| RaiError::Inadmissible("receive total overflow".into()))
            })?;
            let debits = block.sends.iter().try_fold(0u128, |sum, send| {
                sum.checked_add(send.amount)
                    .ok_or_else(|| RaiError::Inadmissible("send total overflow".into()))
            })?;
            balance = balance
                .checked_add(credits)
                .and_then(|available| available.checked_sub(debits))
                .ok_or_else(|| {
                    RaiError::Inadmissible("derived account balance underflow or overflow".into())
                })?;
        }
        Ok(balance)
    }
}

pub fn hash_ledger_frontiers(frontiers: &BTreeMap<AccountId, Hash32>) -> Hash32 {
    let mut bytes = Vec::new();
    put_u64(&mut bytes, frontiers.len() as u64);
    for (account, frontier) in frontiers {
        put_u64(&mut bytes, *account);
        bytes.extend_from_slice(&frontier.0);
    }
    Hash32::digest(&bytes)
}

pub fn hash_account_state(
    accounts: &BTreeMap<AccountId, AccountState>,
    consumed_sends: &BTreeMap<SendId, Hash32>,
) -> Result<Hash32> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"rai-account-state-v2-ed25519");
    put_u64(&mut bytes, accounts.len() as u64);
    for (account, state) in accounts {
        state.encode(*account, &mut bytes);
    }
    put_u64(&mut bytes, consumed_sends.len() as u64);
    for (send, receiving_block) in consumed_sends {
        send.encode(&mut bytes);
        bytes.extend_from_slice(&receiving_block.0);
    }
    Ok(Hash32::digest(&bytes))
}
