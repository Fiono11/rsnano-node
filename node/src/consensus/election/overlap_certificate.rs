use std::collections::BTreeSet;

use rsnano_types::{BlockHash, ConsensusEpoch};

use super::{AccountSlot, CertifiedStatus, EpochLedger, LockStrength, SelectedReport};

/// RAI, "Commit admission witnesses": a block of the epoch being closed that
/// a selected report names finalized and the predecessor checkpoint does
/// not hold. Such a block may have been finalized under the core overlap
/// exception, before the predecessor checkpoint was known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FreshFinalized {
    pub slot: AccountSlot,
    pub hash: BlockHash,
    pub previous: BlockHash,
}

/// The fresh finalized blocks the selected reports name, each once
pub fn fresh_finalized(
    selection: &[SelectedReport],
    previous: &EpochLedger,
) -> Vec<FreshFinalized> {
    let mut fresh = BTreeSet::new();
    for report in selection {
        for (block, entry) in report.certified.entries() {
            if entry.status != CertifiedStatus::Finalized {
                continue;
            }
            let slot = AccountSlot::new(block.account, block.height);
            if previous.is_finalized(&slot, &block.hash) {
                continue;
            }
            fresh.insert(FreshFinalized {
                slot,
                hash: block.hash,
                previous: entry.previous,
            });
        }
    }
    fresh.into_iter().collect()
}

/// The exclusion witnesses an overlap certificate needs beyond the
/// closing-epoch witness: for every recovery record of the last closed
/// state (`S_{e-2}`, the anchor the overlap admission was checked against)
/// a fresh finalized block bypasses, the record's origin and the block's
/// branch at the divergent position. A manifest names them so that the
/// certificate can be checked from the manifest alone.
pub fn overlap_claims(
    anchor: &EpochLedger,
    fresh: &[FreshFinalized],
) -> Vec<(ConsensusEpoch, BlockHash)> {
    let mut claims = BTreeSet::new();
    for block in fresh {
        if anchor.retained_depth(block.slot.account).is_none() {
            continue;
        }
        let branch = anchor.branch_of(block.slot, block.hash, block.previous);
        for bypassed in anchor.bypassed_records(block.slot.account, &branch) {
            if bypassed.record.strength == LockStrength::Recovery {
                claims.insert((bypassed.record.origin, bypassed.target));
            }
        }
    }
    claims.into_iter().collect()
}

/// RAI, overlap certificate `OC_e(B)`: the fresh finalized blocks of epoch
/// `e` whose overlap admission the evidence proves. For each it holds
/// - the closing-epoch exclusion witness `XW_{e-1}(B)`,
/// - the notarization certificate `NC_e(B)`,
/// - a witness of the matching origin for every record of the anchor
///   `S_{e-2}` the block bypasses, and
/// - a parent finalized in the predecessor checkpoint `S_{e-1}` or itself
///   fresh finalized: the rest of the prefix.
///
/// The finalization certificate itself is what the report check requires
/// of every fresh F entry already. The result is a function of the
/// evidence, the selection and the two closed states alone, known before
/// the state of the epoch is built, so every validator that holds the same
/// evidence marks the same certificates.
pub fn overlap_certified(
    fresh: &[FreshFinalized],
    previous: &EpochLedger,
    anchor: &EpochLedger,
    closing_witness: &dyn Fn(&BlockHash) -> bool,
    notarized: &dyn Fn(&BlockHash) -> bool,
    witness: &dyn Fn(ConsensusEpoch, &BlockHash) -> bool,
) -> BTreeSet<BlockHash> {
    let named: BTreeSet<BlockHash> = fresh.iter().map(|block| block.hash).collect();
    fresh
        .iter()
        .filter(|block| {
            let opens = block.slot.height <= 1 && block.previous.is_zero();
            opens
                || named.contains(&block.previous)
                || previous.is_finalized(
                    &AccountSlot::new(block.slot.account, block.slot.height - 1),
                    &block.previous,
                )
        })
        .filter(|block| closing_witness(&block.hash) && notarized(&block.hash))
        .filter(|block| anchor.admits(block.slot, block.hash, block.previous, witness))
        .map(|block| block.hash)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::{CertifiedBlock, CertifiedState, ResidualVotes};
    use rsnano_types::{Account, Amount, PrivateKey};

    #[test]
    fn fresh_finalized_blocks_are_those_the_predecessor_does_not_hold() {
        let mut previous = EpochLedger::new();
        previous.finalize_genesis(slot(1), BlockHash::from(1));
        let mut certified = CertifiedState::new();
        certify(&mut certified, 1, BlockHash::from(1), BlockHash::ZERO);
        certify(&mut certified, 2, BlockHash::from(2), BlockHash::from(1));
        certified.certify(
            CertifiedBlock::new(account(), 3, BlockHash::from(3)),
            BlockHash::from(2),
            CertifiedStatus::Notarized,
        );
        let residual = ResidualVotes::new();

        let fresh = fresh_finalized(&[report(&certified, &residual)], &previous);

        assert_eq!(
            fresh,
            vec![FreshFinalized {
                slot: slot(2),
                hash: BlockHash::from(2),
                previous: BlockHash::from(1),
            }]
        );
    }

    #[test]
    fn a_complete_certificate_is_marked() {
        let (previous, anchor, fresh) = finalized_child();

        let certified =
            overlap_certified(&fresh, &previous, &anchor, &|_| true, &|_| true, &|_, _| {
                false
            });

        assert_eq!(certified, BTreeSet::from([BlockHash::from(2)]));
    }

    #[test]
    fn no_certificate_without_the_closing_witness_or_the_notarization() {
        let (previous, anchor, fresh) = finalized_child();

        let no_witness = overlap_certified(
            &fresh,
            &previous,
            &anchor,
            &|_| false,
            &|_| true,
            &|_, _| false,
        );
        let no_nc = overlap_certified(
            &fresh,
            &previous,
            &anchor,
            &|_| true,
            &|_| false,
            &|_, _| false,
        );

        assert!(no_witness.is_empty());
        assert!(no_nc.is_empty());
    }

    #[test]
    fn the_parent_is_finalized_before_or_fresh_finalized_itself() {
        let (previous, anchor, fresh) = finalized_child();
        let child = FreshFinalized {
            slot: slot(3),
            hash: BlockHash::from(3),
            previous: BlockHash::from(2),
        };
        let orphan = FreshFinalized {
            slot: slot(3),
            hash: BlockHash::from(4),
            previous: BlockHash::from(8),
        };
        let blocks = [fresh[0], child, orphan];

        let certified = overlap_certified(
            &blocks,
            &previous,
            &anchor,
            &|_| true,
            &|_| true,
            &|_, _| false,
        );

        assert_eq!(
            certified,
            BTreeSet::from([BlockHash::from(2), BlockHash::from(3)])
        );
    }

    /// A recovery record of the anchor on a rival is discharged only by a
    /// witness of its own origin naming the block's branch; the manifest
    /// is asked to name that witness
    #[test]
    fn a_bypassed_anchor_record_needs_its_matching_origin_witness() {
        let (previous, mut anchor, fresh) = finalized_child();
        let rival = BlockHash::from(9);
        anchor.retain_recovery_for_test(slot(2), rival, BlockHash::from(1));

        assert_eq!(
            overlap_claims(&anchor, &fresh),
            vec![(ConsensusEpoch::ZERO, BlockHash::from(2))]
        );
        let without =
            overlap_certified(&fresh, &previous, &anchor, &|_| true, &|_| true, &|_, _| {
                false
            });
        let wrong_origin = overlap_certified(
            &fresh,
            &previous,
            &anchor,
            &|_| true,
            &|_| true,
            &|origin, _| origin == ConsensusEpoch::new(1),
        );
        let matching = overlap_certified(
            &fresh,
            &previous,
            &anchor,
            &|_| true,
            &|_| true,
            &|origin, hash| origin == ConsensusEpoch::ZERO && *hash == BlockHash::from(2),
        );

        assert!(without.is_empty());
        assert!(wrong_origin.is_empty());
        assert_eq!(matching, BTreeSet::from([BlockHash::from(2)]));
    }

    /* Test helpers */

    /// Block 2 at height 2 named finalized on block 1, which the closed
    /// states hold finalized
    fn finalized_child() -> (EpochLedger, EpochLedger, Vec<FreshFinalized>) {
        let mut anchor = EpochLedger::new();
        anchor.finalize_genesis(slot(1), BlockHash::from(1));
        let previous = anchor.clone();
        let fresh = vec![FreshFinalized {
            slot: slot(2),
            hash: BlockHash::from(2),
            previous: BlockHash::from(1),
        }];
        (previous, anchor, fresh)
    }

    fn account() -> Account {
        PrivateKey::from(1).account()
    }

    fn slot(height: u64) -> AccountSlot {
        AccountSlot::new(account(), height)
    }

    fn certify(state: &mut CertifiedState, height: u64, hash: BlockHash, previous: BlockHash) {
        state.certify(
            CertifiedBlock::new(account(), height, hash),
            previous,
            CertifiedStatus::Finalized,
        );
    }

    fn report<'a>(
        certified: &'a CertifiedState,
        residual: &'a ResidualVotes,
    ) -> SelectedReport<'a> {
        SelectedReport {
            reporter: PrivateKey::from(2).public_key(),
            weight: Amount::raw(10),
            certified,
            residual,
        }
    }
}
