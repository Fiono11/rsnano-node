use std::collections::{BTreeMap, BTreeSet};

use rsnano_ledger::RepWeights;
use rsnano_messages::EpochClose;
use rsnano_types::{Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, QualifiedRoot, Signature};

use super::election::kudzu::KudzuThresholds;

#[derive(Default)]
struct Report {
    digest: BlockHash,
    pages: u16,
    received: BTreeMap<u16, EpochClose>,
    entries: Option<Vec<(QualifiedRoot, BlockHash)>>,
}

/// One immutable report per weighted committee member. Deliberately waits for
/// every member, as specified by the experimental pause/report protocol.
#[derive(Default)]
pub(super) struct EpochCut {
    reports: BTreeMap<PublicKey, Report>,
    pub local: Vec<EpochClose>,
    pub started: bool,
    pub roots: Option<Vec<QualifiedRoot>>,
    pub recovery_cursor: usize,
}

fn digest(hashes: &[BlockHash]) -> BlockHash {
    let mut builder = Blake2HashBuilder::new().update(b"rai-pending-report-v1");
    for hash in hashes {
        builder = builder.update(hash.as_bytes());
    }
    builder.build()
}

impl EpochCut {
    pub fn packets(
        epoch: u64,
        entries: &[(QualifiedRoot, BlockHash)],
        key: &PrivateKey,
    ) -> Vec<EpochClose> {
        let mut entries = entries.to_vec();
        entries.sort();
        entries.dedup_by(|a, b| a.0 == b.0);
        let hashes: Vec<BlockHash> = entries
            .iter()
            .flat_map(|(root, hash)| [root.root.into(), root.previous, *hash])
            .collect();
        let page_size = EpochClose::PAGE_SIZE / 3 * 3;
        let pages = hashes.len().div_ceil(page_size).max(1);
        assert!(
            pages <= EpochClose::MAX_PAGES as usize,
            "pending report exceeds wire limit"
        );
        let state = digest(&hashes);
        (0..pages)
            .map(|page| {
                let start = (page * page_size).min(hashes.len());
                let end = (start + page_size).min(hashes.len());
                let mut packet = EpochClose {
                    epoch,
                    round: 0,
                    parent: BlockHash::ZERO,
                    state,
                    kind: 8,
                    voter: PublicKey::ZERO,
                    signature: Signature::new(),
                    page: page as u16,
                    pages: pages as u16,
                    hashes: hashes[start..end].to_vec(),
                    base: BlockHash::ZERO,
                    removed: vec![],
                };
                packet.sign(key);
                packet
            })
            .collect()
    }

    pub fn receive(&mut self, packet: EpochClose, epoch: u64, weights: &RepWeights) {
        if packet.epoch != epoch
            || !packet.valid_cut_report()
            || weights.weight(&packet.voter).is_zero()
        {
            return;
        }
        let report = self.reports.entry(packet.voter).or_insert_with(|| Report {
            digest: packet.state,
            pages: packet.pages,
            ..Default::default()
        });
        if report.digest != packet.state || report.pages != packet.pages || report.entries.is_some()
        {
            return;
        }
        report.received.entry(packet.page).or_insert(packet);
        if report.received.len() != report.pages as usize {
            return;
        }
        let hashes: Vec<_> = report
            .received
            .values()
            .flat_map(|p| p.hashes.iter().copied())
            .collect();
        if digest(&hashes) != report.digest {
            return;
        }
        let entries: Vec<_> = hashes
            .chunks_exact(3)
            .map(|h| (QualifiedRoot::new(h[0].into(), h[1]), h[2]))
            .collect();
        if entries.windows(2).any(|w| w[0].0 >= w[1].0) {
            return;
        }
        report.entries = Some(entries);
    }

    pub fn finish(&mut self, weights: &RepWeights) -> bool {
        if self.roots.is_some()
            || weights.is_empty()
            || !weights
                .keys()
                .all(|rep| self.reports.get(rep).is_some_and(|r| r.entries.is_some()))
        {
            return false;
        }
        let total = weights
            .values()
            .fold(0u128, |sum, w| sum.saturating_add(w.number()));
        let f = total / 100 * KudzuThresholds::F_PERCENT
            + total % 100 * KudzuThresholds::F_PERCENT / 100;
        let mut reported = BTreeMap::<QualifiedRoot, u128>::new();
        for (rep, report) in &self.reports {
            for (root, _) in report.entries.as_ref().unwrap() {
                let weight = reported.entry(root.clone()).or_default();
                *weight = weight.saturating_add(weights.weight(rep).number());
            }
        }
        self.roots = Some(
            reported
                .into_iter()
                .filter_map(|(root, weight)| (weight > f).then_some(root))
                .collect(),
        );
        true
    }

    pub fn recovery_targets(
        &self,
        pending: &[QualifiedRoot],
    ) -> Vec<(BlockHash, rsnano_types::Root)> {
        let pending: BTreeSet<_> = pending.iter().collect();
        self.reports
            .values()
            .filter_map(|r| r.entries.as_ref())
            .flatten()
            .filter(|(root, _)| pending.contains(root))
            .map(|(root, hash)| (*hash, root.root))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::Amount;

    #[test]
    fn all_reports_and_strict_f_threshold_with_duplicate_replay() {
        let keys: Vec<_> = (1..=3).map(PrivateKey::from).collect();
        let weights: RepWeights = [
            (keys[0].public_key(), Amount::raw(19)),
            (keys[1].public_key(), Amount::raw(1)),
            (keys[2].public_key(), Amount::raw(80)),
        ]
        .into();
        let root = QualifiedRoot::new_test_instance();
        let mut cut = EpochCut::default();
        let p = EpochCut::packets(0, &[(root.clone(), 1.into())], &keys[0]);
        for _ in 0..3 {
            cut.receive(p[0].clone(), 0, &weights);
        }
        assert!(!cut.finish(&weights));
        for p in EpochCut::packets(0, &[(root.clone(), 2.into())], &keys[1]) {
            cut.receive(p, 0, &weights);
        }
        assert!(!cut.finish(&weights));
        for p in EpochCut::packets(0, &[], &keys[2]) {
            cut.receive(p, 0, &weights);
        }
        assert!(cut.finish(&weights));
        assert_eq!(cut.roots, Some(vec![root]));
        let mut cut = EpochCut::default();
        for p in p {
            cut.receive(p, 0, &weights);
        }
        for key in &keys[1..] {
            for p in EpochCut::packets(0, &[], key) {
                cut.receive(p, 0, &weights);
            }
        }
        assert!(cut.finish(&weights));
        assert_eq!(cut.roots, Some(vec![]));
    }

    #[test]
    fn report_pages_are_authenticated_and_reassembled_out_of_order() {
        let key = PrivateKey::from(1);
        let weights: RepWeights = [(key.public_key(), Amount::raw(100))].into();
        let entries: Vec<_> = (1..400)
            .map(|n| (QualifiedRoot::new(n.into(), 0.into()), n.into()))
            .collect();
        let packets = EpochCut::packets(7, &entries, &key);
        let mut changed = packets[0].clone();
        changed.hashes[0] = 1000.into();
        assert!(!changed.valid_cut_report());
        changed = packets[0].clone();
        changed.pages += 1;
        assert!(!changed.valid_cut_report());
        let mut cut = EpochCut::default();
        for p in packets.into_iter().rev() {
            assert!(!p.valid_vote());
            cut.receive(p, 7, &weights);
        }
        assert!(cut.finish(&weights));
        assert_eq!(cut.roots.unwrap().len(), entries.len());
    }
}
