use super::close_proof::VerifiedCheckpoint;
use crate::consensus::election::EpochLedger;
use rsnano_messages::{CertifiedEntry, CheckpointReply, CheckpointReq, ReconReply};
use rsnano_types::{Account, BlockHash, ConsensusEpoch};
use std::{collections::BTreeSet, sync::Arc};

type Entry = (Account, u64, BlockHash, BlockHash, u8);
fn key(e: &CertifiedEntry) -> Entry {
    (e.account, e.height, e.hash, e.previous, e.status)
}
fn wire(e: Entry) -> CertifiedEntry {
    CertifiedEntry {
        account: e.0,
        height: e.1,
        hash: e.2,
        previous: e.3,
        status: e.4,
    }
}

/// Immutable canonical edits; any holder of both decided states can serve pages.
pub(crate) struct CheckpointDifference {
    epoch: ConsensusEpoch,
    source: BlockHash,
    target: BlockHash,
    removed: Vec<CertifiedEntry>,
    added: Vec<CertifiedEntry>,
}
impl CheckpointDifference {
    pub fn new(epoch: ConsensusEpoch, source: &EpochLedger, target: &EpochLedger) -> Self {
        let old: BTreeSet<_> = source.checkpoint_entries().iter().map(key).collect();
        let new: BTreeSet<_> = target.checkpoint_entries().iter().map(key).collect();
        Self {
            epoch,
            source: source.state_hash(),
            target: target.state_hash(),
            removed: old.difference(&new).copied().map(wire).collect(),
            added: new.difference(&old).copied().map(wire).collect(),
        }
    }
    pub fn page(&self, request: &CheckpointReq) -> Option<CheckpointReply> {
        let total = self.removed.len() + self.added.len();
        let offset = request.offset as usize;
        if request.epoch != self.epoch
            || request.source != self.source
            || request.target != self.target
            || total > CheckpointTransfer::MAX_EDITS
            || offset > total
        {
            return None;
        }
        let end = (offset + ReconReply::MAX_ENTRIES).min(total);
        let removed =
            self.removed[offset.min(self.removed.len())..end.min(self.removed.len())].to_vec();
        let added = self.added
            [offset.saturating_sub(self.removed.len())..end.saturating_sub(self.removed.len())]
            .to_vec();
        Some(CheckpointReply {
            offset: request.offset,
            total: total as u32,
            difference: ReconReply {
                epoch: self.epoch,
                source: self.source,
                target: self.target,
                removed,
                added,
            },
        })
    }
}

/// Partial pages never enter the decided ledger. The only successful output is
/// a complete ledger hashing to a quorum-verified commitment.
pub(crate) struct CheckpointTransfer {
    verified: VerifiedCheckpoint,
    previous: Arc<EpochLedger>,
    source: BlockHash,
    working: BTreeSet<Entry>,
    offset: u32,
    total: Option<u32>,
}
impl CheckpointTransfer {
    /// Admission bound, independent of attacker-supplied page totals (105 MB
    /// of wire edits). Larger checkpoints require a configured larger bound.
    pub const MAX_EDITS: usize = 1_000_000;
    pub fn new(verified: VerifiedCheckpoint, previous: Arc<EpochLedger>) -> Self {
        Self {
            verified,
            source: previous.state_hash(),
            working: previous.checkpoint_entries().iter().map(key).collect(),
            previous,
            offset: 0,
            total: None,
        }
    }
    pub fn request(&self) -> CheckpointReq {
        CheckpointReq {
            epoch: self.verified.epoch,
            source: self.source,
            target: self.verified.state,
            offset: self.offset,
        }
    }
    pub fn accept(&mut self, page: &CheckpointReply) -> Result<Option<Arc<EpochLedger>>, ()> {
        let d = &page.difference;
        let count = d.removed.len() + d.added.len();
        if d.epoch != self.verified.epoch
            || d.source != self.source
            || d.target != self.verified.state
            || page.offset != self.offset
            || page.total as usize > Self::MAX_EDITS
            || count > ReconReply::MAX_ENTRIES
            || self.total.is_some_and(|total| total != page.total)
            || self.offset as u64 + count as u64 > page.total as u64
            || (count == 0 && self.offset != page.total)
        {
            return Err(());
        }
        // Validate a page before mutation, including duplicate edits.
        let removed: BTreeSet<_> = d.removed.iter().map(key).collect();
        let added: BTreeSet<_> = d.added.iter().map(key).collect();
        if removed.len() != d.removed.len()
            || added.len() != d.added.len()
            || !removed.is_subset(&self.working)
            || added
                .iter()
                .any(|e| self.working.contains(e) && !removed.contains(e))
            || d.added.iter().chain(d.removed.iter()).any(|e| e.status > 1)
        {
            return Err(());
        }
        for e in removed {
            self.working.remove(&e);
        }
        self.working.extend(added);
        self.offset += count as u32;
        self.total = Some(page.total);
        if self.offset != page.total {
            return Ok(None);
        }
        let entries: Vec<_> = self.working.iter().copied().map(wire).collect();
        let rebuilt = EpochLedger::from_checkpoint_entries(&entries).ok_or(())?;
        if rebuilt.state_hash() != self.verified.state
            || self
                .previous
                .finalized_slots()
                .any(|(slot, b)| !rebuilt.is_finalized(slot, &b.hash))
        {
            return Err(());
        }
        Ok(Some(Arc::new(rebuilt)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::AccountSlot;
    #[test]
    fn multi_page_difference_reconstructs_finalized_and_retained_entries() {
        let previous = Arc::new(EpochLedger::new());
        let entries: Vec<_> = (1..=1300)
            .map(|i| CertifiedEntry {
                account: Account::from(i),
                height: 1,
                hash: BlockHash::from(i),
                previous: BlockHash::ZERO,
                status: (i % 2) as u8,
            })
            .collect();
        let target = EpochLedger::from_checkpoint_entries(&entries).unwrap();
        let difference = CheckpointDifference::new(ConsensusEpoch::ZERO, &previous, &target);
        let mut transfer = transfer(previous, &target);
        for _ in 0..2 {
            let page = difference.page(&transfer.request()).unwrap();
            assert_eq!(transfer.accept(&page), Ok(None));
        }
        let page = difference.page(&transfer.request()).unwrap();
        assert_eq!(*transfer.accept(&page).unwrap().unwrap(), target);
    }
    #[test]
    fn bad_hash_wrong_source_and_out_of_order_page_do_not_install() {
        let previous = Arc::new(EpochLedger::new());
        let mut target = EpochLedger::new();
        target.finalize_genesis(AccountSlot::new(Account::from(1), 1), BlockHash::from(1));
        let difference = CheckpointDifference::new(ConsensusEpoch::ZERO, &previous, &target);
        let mut transfer = transfer(previous, &target);
        let page = difference.page(&transfer.request()).unwrap();
        let mut bad = page.clone();
        bad.offset = 1;
        assert!(transfer.accept(&bad).is_err());
        bad = page.clone();
        bad.difference.source = BlockHash::ZERO;
        assert!(transfer.accept(&bad).is_err());
        bad = page;
        bad.difference.added[0].hash = BlockHash::from(2);
        assert!(transfer.accept(&bad).is_err());
    }
    #[test]
    fn retained_entries_can_be_removed_and_promoted() {
        let retained = CertifiedEntry {
            account: Account::from(1),
            height: 1,
            hash: BlockHash::from(1),
            previous: BlockHash::ZERO,
            status: 0,
        };
        let previous = Arc::new(EpochLedger::from_checkpoint_entries(&[retained]).unwrap());
        let target = EpochLedger::from_checkpoint_entries(&[CertifiedEntry {
            status: 1,
            ..retained
        }])
        .unwrap();
        let difference = CheckpointDifference::new(ConsensusEpoch::ZERO, &previous, &target);
        let mut transfer = transfer(previous, &target);
        let page = difference.page(&transfer.request()).unwrap();
        assert_eq!(page.difference.removed, vec![retained]);
        assert_eq!(*transfer.accept(&page).unwrap().unwrap(), target);
    }
    #[test]
    fn empty_difference_is_still_authenticated() {
        let previous = Arc::new(EpochLedger::new());
        let difference = CheckpointDifference::new(ConsensusEpoch::ZERO, &previous, &previous);
        let mut transfer = transfer(previous.clone(), &previous);
        let page = difference.page(&transfer.request()).unwrap();
        assert_eq!(transfer.accept(&page).unwrap(), Some(previous));
    }
    #[test]
    fn even_a_certified_target_cannot_replace_installed_finality() {
        let mut old = EpochLedger::new();
        old.finalize_genesis(AccountSlot::new(Account::from(1), 1), BlockHash::from(1));
        let previous = Arc::new(old);
        let mut target = EpochLedger::new();
        target.finalize_genesis(AccountSlot::new(Account::from(1), 1), BlockHash::from(2));
        let difference = CheckpointDifference::new(ConsensusEpoch::ZERO, &previous, &target);
        let mut transfer = transfer(previous, &target);
        let page = difference.page(&transfer.request()).unwrap();
        assert!(transfer.accept(&page).is_err());
    }
    #[test]
    fn transfer_rejects_unbounded_or_inconsistent_totals() {
        let previous = Arc::new(EpochLedger::new());
        let difference = CheckpointDifference::new(ConsensusEpoch::ZERO, &previous, &previous);
        let mut transfer = transfer(previous.clone(), &previous);
        let mut page = difference.page(&transfer.request()).unwrap();
        page.total = u32::MAX;
        assert!(transfer.accept(&page).is_err());
        assert_eq!(transfer.request().offset, 0);
    }
    /* Test helpers */
    fn transfer(previous: Arc<EpochLedger>, target: &EpochLedger) -> CheckpointTransfer {
        CheckpointTransfer::new(
            VerifiedCheckpoint {
                epoch: ConsensusEpoch::ZERO,
                state: target.state_hash(),
            },
            previous,
        )
    }
}
