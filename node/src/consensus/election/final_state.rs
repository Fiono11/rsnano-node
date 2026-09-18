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
