use std::collections::{BTreeSet, HashMap};

use rsnano_types::{BlockHash, ConsensusEpoch};

use crate::consensus::election::{
    AccountSlot, CertificateKinds, CertifiedBlock, CertifiedState, CertifiedStatus, EpochLedger,
    RetainedKind,
};

/// RAI, "Reconstructing a report": "an entry tagged N must have a valid NC,
/// an entry tagged F must have an explicit valid finality proof for the
/// block or a selected descendant". Hash equality authenticates membership
/// and status; it does not prove a quorum. This checks every N and F entry
/// of a reconstructed `T_i` against certificates assembled locally from
/// retained signed votes, and against the verified predecessor for what it
/// already finalized or locked. The caller supplies the certificates; this
/// holds no votes and reads no network.
///
/// Returns the hashes whose status is not justified. An empty result makes
/// the state's memberships usable. A justified F entry justifies the F
/// entries of its selected ancestors within the state: finality for a
/// descendant finalizes the prefix.
pub(crate) fn unjustified_entries(
    epoch: ConsensusEpoch,
    state: &CertifiedState,
    predecessor: &EpochLedger,
    certificates: &dyn CertificateSource,
) -> Vec<BlockHash> {
    let mut justified: BTreeSet<CertifiedBlock> = BTreeSet::new();
    let mut anchors: Vec<CertifiedBlock> = Vec::new();
    let mut missing = Vec::new();
    for (block, entry) in state.entries() {
        let slot = AccountSlot::new(block.account, block.height);
        match entry.status {
            CertifiedStatus::Finalized => {
                if predecessor.is_finalized(&slot, &block.hash)
                    || finalized_by_votes(epoch, &block.hash, certificates)
                {
                    anchors.push(*block);
                }
            }
            CertifiedStatus::Notarized => {
                let inherited = predecessor.is_locked(&slot, &block.hash)
                    && predecessor.retained_kind(&block.hash) == RetainedKind::Notarized;
                if inherited || certificates.kinds(epoch, &block.hash).nc {
                    justified.insert(*block);
                } else {
                    missing.push(block.hash);
                }
            }
            // R is checked against the predecessor by the exchange
            CertifiedStatus::Recovery => {}
        }
    }
    // The selected prefix of every anchor: F entries below a justified F
    // are justified by it
    let by_hash: HashMap<BlockHash, CertifiedBlock> = state
        .entries()
        .map(|(block, _)| (block.hash, *block))
        .collect();
    for anchor in anchors {
        let mut current = anchor;
        loop {
            if !justified.insert(current) {
                break;
            }
            let Some(entry) = state.certification(&current) else {
                break;
            };
            if current.height <= 1 || entry.previous.is_zero() {
                break;
            }
            let Some(parent) = by_hash.get(&entry.previous) else {
                break;
            };
            if parent.height + 1 != current.height
                || parent.account != current.account
                || state.status(parent) != Some(CertifiedStatus::Finalized)
            {
                break;
            }
            current = *parent;
        }
    }
    for (block, entry) in state.entries() {
        if entry.status == CertifiedStatus::Finalized && !justified.contains(block) {
            missing.push(block.hash);
        }
    }
    missing.sort();
    missing.dedup();
    missing
}

/// Finality assembled from votes of the closing epoch or of a retained
/// earlier one: a certificate assembled after a predecessor was decided is
/// exposed by a later report as F for a block that predecessor carried
/// only as a lock
fn finalized_by_votes(
    epoch: ConsensusEpoch,
    hash: &BlockHash,
    certificates: &dyn CertificateSource,
) -> bool {
    (0..4)
        .filter_map(|back| epoch.as_u64().checked_sub(back).map(ConsensusEpoch::new))
        .any(|epoch| {
            let kinds = certificates.kinds(epoch, hash);
            kinds.fc || kinds.ff
        })
}

/// The certificates a validator has assembled locally from the signed
/// votes it retains, counted in the committee of the votes' epoch
pub(crate) trait CertificateSource {
    fn kinds(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> CertificateKinds;
}

impl CertificateSource for HashMap<(ConsensusEpoch, BlockHash), CertificateKinds> {
    fn kinds(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> CertificateKinds {
        self.get(&(epoch, *hash)).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::PlacedBlock;
    use rsnano_types::Account;

    #[test]
    fn inherited_finality_and_locks_justify_without_votes() {
        let epoch = ConsensusEpoch::new(1);
        let mut predecessor = EpochLedger::new();
        predecessor.finalize_genesis_block(slot(1, 1), PlacedBlock::new(hash(1), BlockHash::ZERO));
        predecessor.finalize_genesis_block(slot(1, 2), PlacedBlock::new(hash(2), hash(1)));
        let mut state = CertifiedState::new();
        state.certify(block(1, 1, 1), BlockHash::ZERO, CertifiedStatus::Finalized);
        state.certify(block(1, 2, 2), hash(1), CertifiedStatus::Finalized);
        assert!(unjustified_entries(epoch, &state, &predecessor, &no_votes()).is_empty());

        // An N entry needs the predecessor's represented lock, not just
        // retention
        let retained =
            EpochLedger::from_checkpoint_entries(&[entry(2, 1, 5, BlockHash::ZERO, 2)]).unwrap();
        let recovery =
            EpochLedger::from_checkpoint_entries(&[entry(2, 1, 5, BlockHash::ZERO, 3)]).unwrap();
        let mut state = CertifiedState::new();
        state.certify(block(2, 1, 5), BlockHash::ZERO, CertifiedStatus::Notarized);
        assert!(unjustified_entries(epoch, &state, &retained, &no_votes()).is_empty());
        assert_eq!(
            unjustified_entries(epoch, &state, &recovery, &no_votes()),
            vec![hash(5)]
        );
    }

    #[test]
    fn n_and_f_need_their_own_certificates() {
        let epoch = ConsensusEpoch::new(1);
        let predecessor = EpochLedger::new();
        let mut state = CertifiedState::new();
        state.certify(block(1, 1, 1), BlockHash::ZERO, CertifiedStatus::Notarized);
        state.certify(block(2, 1, 2), BlockHash::ZERO, CertifiedStatus::Finalized);
        state.certify(block(3, 1, 3), BlockHash::ZERO, CertifiedStatus::Recovery);
        assert_eq!(
            unjustified_entries(epoch, &state, &predecessor, &no_votes()),
            vec![hash(1), hash(2)]
        );
        let mut votes = HashMap::new();
        votes.insert((epoch, hash(1)), kinds(true, false, false));
        // An NC alone does not justify F
        votes.insert((epoch, hash(2)), kinds(true, false, false));
        assert_eq!(
            unjustified_entries(epoch, &state, &predecessor, &votes),
            vec![hash(2)]
        );
        votes.insert((epoch, hash(2)), kinds(true, false, true));
        assert!(unjustified_entries(epoch, &state, &predecessor, &votes).is_empty());
        // A certificate of the epoch before justifies late finality
        votes.insert((epoch, hash(2)), kinds(false, false, false));
        votes.insert((ConsensusEpoch::ZERO, hash(2)), kinds(true, true, false));
        assert!(unjustified_entries(epoch, &state, &predecessor, &votes).is_empty());
    }

    #[test]
    fn a_descendant_certificate_justifies_its_selected_prefix_only() {
        let epoch = ConsensusEpoch::new(1);
        let predecessor = EpochLedger::new();
        let mut state = CertifiedState::new();
        state.certify(block(1, 1, 1), BlockHash::ZERO, CertifiedStatus::Finalized);
        state.certify(block(1, 2, 2), hash(1), CertifiedStatus::Finalized);
        state.certify(block(1, 3, 3), hash(2), CertifiedStatus::Finalized);
        // A sibling of 2 is not on the selected path
        state.certify(block(1, 2, 4), hash(1), CertifiedStatus::Finalized);
        let mut votes = HashMap::new();
        votes.insert((epoch, hash(3)), kinds(true, true, false));
        assert_eq!(
            unjustified_entries(epoch, &state, &predecessor, &votes),
            vec![hash(4)]
        );
        // The walk stops at a gap in the prefix
        let mut gap = CertifiedState::new();
        gap.certify(block(1, 1, 1), BlockHash::ZERO, CertifiedStatus::Finalized);
        gap.certify(block(1, 3, 3), hash(2), CertifiedStatus::Finalized);
        assert_eq!(
            unjustified_entries(epoch, &gap, &predecessor, &votes),
            vec![hash(1)]
        );
    }

    /* Test helpers */

    fn hash(i: u64) -> BlockHash {
        BlockHash::from(i)
    }

    fn slot(account: u64, height: u64) -> AccountSlot {
        AccountSlot::new(Account::from(account), height)
    }

    fn block(account: u64, height: u64, id: u64) -> CertifiedBlock {
        CertifiedBlock::new(Account::from(account), height, hash(id))
    }

    fn entry(
        account: u64,
        height: u64,
        id: u64,
        previous: BlockHash,
        status: u8,
    ) -> rsnano_messages::CertifiedEntry {
        rsnano_messages::CertifiedEntry {
            account: Account::from(account),
            height,
            hash: hash(id),
            previous,
            status,
        }
    }

    fn kinds(nc: bool, fc: bool, ff: bool) -> CertificateKinds {
        CertificateKinds { nc, fc, ff }
    }

    fn no_votes() -> HashMap<(ConsensusEpoch, BlockHash), CertificateKinds> {
        HashMap::new()
    }
}
