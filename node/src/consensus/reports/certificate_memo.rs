use std::collections::HashMap;

use rsnano_types::{BlockHash, ConsensusEpoch};

use crate::consensus::election::CertificateKinds;

/// RAI: the certificates the signed votes held here form for blocks, as
/// looked up. Votes are only ever added while an epoch is undecided, so a
/// certificate once formed stays formed, and kinds that already hold a
/// notarization and finality answer every question a report check asks of
/// them: such a block need not be looked up again. The epoch's finalized
/// blocks are looked up while the epoch runs, so that checking the reports
/// at its close looks up little.
#[derive(Default)]
pub(crate) struct CertificateMemo {
    kinds: HashMap<(ConsensusEpoch, BlockHash), CertificateKinds>,
}

impl CertificateMemo {
    /// The kinds held for a block if no lookup can change what a report
    /// check concludes from them
    pub fn settled(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> Option<CertificateKinds> {
        self.kinds
            .get(&(epoch, *hash))
            .filter(|kinds| kinds.notarization && (kinds.finalization || kinds.fast))
            .copied()
    }

    /// Records what a lookup found
    pub fn record(&mut self, epoch: ConsensusEpoch, hash: BlockHash, kinds: CertificateKinds) {
        let held = self.kinds.entry((epoch, hash)).or_default();
        held.notarization |= kinds.notarization;
        held.finalization |= kinds.finalization;
        held.fast |= kinds.fast;
    }

    /// Forgets the epochs before `epoch`
    pub fn forget_before(&mut self, epoch: ConsensusEpoch) {
        self.kinds.retain(|(held, _), _| *held >= epoch);
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// `lookup` through the memo: the blocks whose kinds are settled are
    /// answered from it, the others looked up and recorded. None if the
    /// lookup is: the committee of the epoch is not known here.
    pub fn kinds_of(
        memo: &std::sync::Mutex<CertificateMemo>,
        epoch: ConsensusEpoch,
        hashes: &[BlockHash],
        lookup: &dyn Fn(ConsensusEpoch, &[BlockHash]) -> Option<Vec<CertificateKinds>>,
    ) -> Option<Vec<CertificateKinds>> {
        let mut found: Vec<Option<CertificateKinds>> = {
            let memo = memo.lock().unwrap();
            hashes
                .iter()
                .map(|hash| memo.settled(epoch, hash))
                .collect()
        };
        let unknown: Vec<BlockHash> = hashes
            .iter()
            .zip(&found)
            .filter(|(_, kinds)| kinds.is_none())
            .map(|(hash, _)| *hash)
            .collect();
        if !unknown.is_empty() || hashes.is_empty() {
            let looked_up = lookup(epoch, &unknown)?;
            let mut memo = memo.lock().unwrap();
            let mut looked_up = unknown.iter().zip(looked_up);
            for kinds in found.iter_mut().filter(|kinds| kinds.is_none()) {
                let (hash, held) = looked_up.next()?;
                memo.record(epoch, *hash, held);
                *kinds = Some(held);
            }
        }
        found.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn settled_kinds_are_answered_without_a_lookup() {
        let memo = Mutex::new(CertificateMemo::default());
        let epoch = ConsensusEpoch::new(2);
        let (final_, open) = (BlockHash::from(1), BlockHash::from(2));
        let strong = CertificateKinds {
            notarization: true,
            finalization: true,
            fast: false,
        };
        let weak = CertificateKinds {
            notarization: true,
            finalization: false,
            fast: false,
        };
        let lookup = |_: ConsensusEpoch, hashes: &[BlockHash]| {
            Some(
                hashes
                    .iter()
                    .map(|hash| if *hash == final_ { strong } else { weak })
                    .collect(),
            )
        };
        assert_eq!(
            CertificateMemo::kinds_of(&memo, epoch, &[final_, open], &lookup),
            Some(vec![strong, weak])
        );

        let asked = Mutex::new(Vec::new());
        let counting = |epoch: ConsensusEpoch, hashes: &[BlockHash]| {
            asked.lock().unwrap().extend_from_slice(hashes);
            lookup(epoch, hashes)
        };
        assert_eq!(
            CertificateMemo::kinds_of(&memo, epoch, &[final_, open], &counting),
            Some(vec![strong, weak])
        );
        assert_eq!(*asked.lock().unwrap(), vec![open]);
    }

    #[test]
    fn an_epoch_whose_committee_is_unknown_is_not_answered() {
        let memo = Mutex::new(CertificateMemo::default());
        let unknown = |_: ConsensusEpoch, _: &[BlockHash]| None;
        assert_eq!(
            CertificateMemo::kinds_of(
                &memo,
                ConsensusEpoch::new(1),
                &[BlockHash::from(1)],
                &unknown
            ),
            None
        );
        assert_eq!(memo.lock().unwrap().len(), 0);
    }
}
