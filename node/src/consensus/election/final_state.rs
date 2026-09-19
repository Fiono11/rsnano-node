use rsnano_types::{Account, Blake2HashBuilder, BlockHash, ConsensusEpoch};

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

/// RAI: the final state of one consensus epoch on this node: the hash of
/// the blocks finalized by a certificate of the epoch and of its settled
/// single notarization certificates, and how many of its instances are
/// still open
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpochState {
    pub hash: FinalStateHash,
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
    pub fn with_finalized(finalized: &FinalStateHash) -> Self {
        Self {
            hash: finalized.clone(),
            finalized: finalized.entries(),
            ..Default::default()
        }
    }

    /// Counts one of the epoch's instances
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
        self.add(account, height, slot_outcome(state, certificates));
    }

    /// Counts the outcome of one of the epoch's instances
    pub fn add(&mut self, account: &Account, height: u64, outcome: SlotOutcome) {
        match outcome {
            SlotOutcome::Pending => self.pending += 1,
            SlotOutcome::Single(block) => {
                self.hash.add(account, height, &block);
                self.single_notarized += 1;
            }
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

    /// The value this node attests in the close election of the epoch. The
    /// state hash of an empty epoch is zero, and the value is a candidate hash
    /// among block hashes: it is domain separated.
    pub fn close_value(&self, epoch: ConsensusEpoch) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI epoch close")
            .update(epoch.as_u64().to_le_bytes())
            .update(self.hash.value().as_bytes())
            .update(self.hash.entries().to_le_bytes())
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_state_counts_outcomes_and_derives_its_close_value() {
        let account = Account::from(1);
        let mut finalized = FinalStateHash::default();
        finalized.add(&account, 1, &BlockHash::from(1));
        let mut state = EpochState::with_finalized(&finalized);
        assert_eq!(state.finalized, 1);
        assert!(state.is_settled());
        let empty_value = EpochState::default().close_value(ConsensusEpoch::ZERO);
        assert!(!empty_value.is_zero());
        assert_ne!(
            empty_value,
            EpochState::default().close_value(ConsensusEpoch::new(1))
        );

        state.add(&account, 2, SlotOutcome::Pending);
        assert!(!state.is_settled());
        assert!(state.is_terminated());
        let before = state.close_value(ConsensusEpoch::ZERO);
        state.add(&account, 3, SlotOutcome::Empty);
        state.add(&account, 4, SlotOutcome::Conflicting);
        // Only entries of the state change the value
        assert_eq!(state.close_value(ConsensusEpoch::ZERO), before);
        state.add(&account, 2, SlotOutcome::Single(BlockHash::from(2)));
        assert_ne!(state.close_value(ConsensusEpoch::ZERO), before);
        assert_eq!(state.pending, 1);
        assert_eq!(state.single_notarized, 1);
        assert_eq!(state.empty, 1);
        assert_eq!(state.conflicting, 1);
        assert_eq!(state.hash.entries(), 2);

        // Terminated but not settled: a certificate, and another may still form
        let mut certs = Certificates::default();
        certs.notar = vec![BlockHash::from(5)];
        state.add_election(&account, 5, ElectionState::Terminated, &certs);
        assert!(state.is_terminated());
        assert!(!state.is_settled());
        assert_eq!(state.pending, 2);
        state.add_election(&account, 6, ElectionState::Active, &Certificates::default());
        assert!(!state.is_terminated());
        assert_eq!(state.unterminated, 1);
        assert_eq!(state.pending, 3);
    }

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

        let timeout_only = Certificates {
            timeout: true,
            ..Default::default()
        };
        assert_eq!(
            slot_outcome(ElectionState::Settled, &timeout_only),
            SlotOutcome::Empty
        );
    }
}
