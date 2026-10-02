use rsnano_types::{Account, Blake2HashBuilder, BlockHash};

use super::{Certificates, ElectionState};

/// What a settled slot contributes to the final state
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotOutcome {
    /// Not settled yet: a notarization certificate can still form
    Pending,
    /// Exactly one notarization certificate: this block is the account's next
    /// block, finalized or not
    Single(BlockHash),
    /// Two or more notarization certificates, none finalized: no block at this
    /// height is included and all of them are discarded
    Conflicting,
    /// Settled by a timeout certificate alone: no block at this height
    Empty,
}

pub fn slot_outcome(state: ElectionState, certificates: &Certificates) -> SlotOutcome {
    if let Some(finalized) = certificates.finalized() {
        return SlotOutcome::Single(finalized);
    }
    match state {
        ElectionState::Settled => match certificates.notar.as_slice() {
            [] => SlotOutcome::Empty,
            [single] => SlotOutcome::Single(*single),
            _ => SlotOutcome::Conflicting,
        },
        _ => SlotOutcome::Pending,
    }
}

/// Order-independent hash of the final state: one entry per account, the
/// block the account ends up at once every election has settled. The entry
/// is the settled single notarization certificate if there is one, otherwise
/// the cemented frontier. Certificate kinds do not enter: two replicas that
/// finalized a block by different evidence hash the same.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalStateHash {
    value: [u8; 32],
    entries: u64,
}

impl Default for FinalStateHash {
    fn default() -> Self {
        Self {
            value: [0; 32],
            entries: 0,
        }
    }
}

impl FinalStateHash {
    pub fn add(&mut self, account: &Account, height: u64, hash: &BlockHash) {
        self.toggle(account, height, hash);
        self.entries += 1;
    }

    /// Removes an entry that was added before
    pub fn remove(&mut self, account: &Account, height: u64, hash: &BlockHash) {
        debug_assert!(self.entries > 0);
        self.toggle(account, height, hash);
        self.entries -= 1;
    }

    pub fn value(&self) -> BlockHash {
        BlockHash::from_bytes(self.value)
    }

    pub fn entries(&self) -> u64 {
        self.entries
    }

    fn toggle(&mut self, account: &Account, height: u64, hash: &BlockHash) {
        let entry = Blake2HashBuilder::new()
            .update(account.as_bytes())
            .update(height.to_le_bytes())
            .update(hash.as_bytes())
            .build();
        for (v, e) in self.value.iter_mut().zip(entry.as_bytes()) {
            *v ^= e;
        }
    }
}

/// RAI: the state of one consensus epoch on this node: the hash of every
/// block notarized or finalized in an instance of the epoch (both blocks
/// of a conflicting slot included), and how many of its instances are
/// still open
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpochState {
    pub hash: FinalStateHash,
    /// Instances finalized by a certificate of the epoch
    pub finalized: u64,
    pub single_notarized: u64,
    /// Instances that are not settled: a notarization certificate can still form
    pub pending: u64,
    /// Instances without any certificate yet, a part of the pending ones
    pub unterminated: u64,
    pub empty: u64,
    pub conflicting: u64,
}

impl EpochState {
    /// Starts from the instances finalized in the epoch, which left the
    /// AEC: the hash of everything they notarized, and how many they are
    pub fn with_finalized(finalized: &FinalStateHash, instances: u64) -> Self {
        Self {
            hash: finalized.clone(),
            finalized: instances,
            ..Default::default()
        }
    }

    /// Counts one of the epoch's running instances: every block it
    /// notarized enters the state
    pub fn add_election(
        &mut self,
        account: &Account,
        height: u64,
        state: ElectionState,
        certificates: &Certificates,
    ) {
        if !state.is_terminated() {
            self.unterminated += 1;
        }
        for block in &certificates.notar {
            self.hash.add(account, height, block);
        }
        self.add(slot_outcome(state, certificates));
    }

    /// Counts the outcome of one of the epoch's instances
    pub fn add(&mut self, outcome: SlotOutcome) {
        match outcome {
            SlotOutcome::Pending => self.pending += 1,
            SlotOutcome::Single(_) => self.single_notarized += 1,
            SlotOutcome::Conflicting => self.conflicting += 1,
            SlotOutcome::Empty => self.empty += 1,
        }
    }

    /// Every instance of the epoch has settled: the hash is final
    pub fn is_settled(&self) -> bool {
        self.pending == 0
    }

    /// Every instance of the epoch holds a certificate (Protocol 1, lines
    /// 9–13: the slots are done), settled or not
    pub fn is_terminated(&self) -> bool {
        self.unterminated == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_order_independent_and_removable() {
        let a = (Account::from(1), 3, BlockHash::from(10));
        let b = (Account::from(2), 1, BlockHash::from(20));

        let mut ab = FinalStateHash::default();
        ab.add(&a.0, a.1, &a.2);
        ab.add(&b.0, b.1, &b.2);
        let mut ba = FinalStateHash::default();
        ba.add(&b.0, b.1, &b.2);
        ba.add(&a.0, a.1, &a.2);
        assert_eq!(ab, ba);
        assert_eq!(ab.entries(), 2);

        ab.remove(&b.0, b.1, &b.2);
        let mut only_a = FinalStateHash::default();
        only_a.add(&a.0, a.1, &a.2);
        assert_eq!(ab, only_a);
        assert_ne!(only_a.value(), FinalStateHash::default().value());
    }

    #[test]
    fn different_height_or_block_gives_a_different_hash() {
        let mut h1 = FinalStateHash::default();
        h1.add(&Account::from(1), 3, &BlockHash::from(10));
        let mut h2 = FinalStateHash::default();
        h2.add(&Account::from(1), 4, &BlockHash::from(10));
        let mut h3 = FinalStateHash::default();
        h3.add(&Account::from(1), 3, &BlockHash::from(11));
        assert_ne!(h1.value(), h2.value());
        assert_ne!(h1.value(), h3.value());
    }

    #[test]
    fn outcome_of_a_slot() {
        let block = BlockHash::from(1);
        let fork = BlockHash::from(2);
        let mut certs = Certificates::default();
        assert_eq!(
            slot_outcome(ElectionState::Active, &certs),
            SlotOutcome::Pending
        );

        certs.notar = vec![block];
        assert_eq!(
            slot_outcome(ElectionState::Terminated, &certs),
            SlotOutcome::Pending
        );
        assert_eq!(
            slot_outcome(ElectionState::Settled, &certs),
            SlotOutcome::Single(block)
        );

        certs.notar = vec![block, fork];
        assert_eq!(
            slot_outcome(ElectionState::Settled, &certs),
            SlotOutcome::Conflicting
        );

        // A finalized block is included whatever else the slot holds
        certs.final_ = Some(block);
        assert_eq!(
            slot_outcome(ElectionState::Confirmed, &certs),
            SlotOutcome::Single(block)
        );

        let unresolved = Certificates {
            ..Default::default()
        };
        assert_eq!(
            slot_outcome(ElectionState::Settled, &unresolved),
            SlotOutcome::Empty
        );
    }
}
