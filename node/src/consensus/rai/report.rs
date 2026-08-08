use std::collections::{BTreeMap, BTreeSet};

use blake2::{
    Blake2bVar,
    digest::{Update, VariableOutput},
};
use rsnano_ledger::RepWeights;
use rsnano_types::{
    Amount, BlockHash, PrivateKey, PublicKey, QualifiedRoot, RaiEpoch, RaiSlotId, Signature,
};

use super::rai_fault_allowance;

const REPORT_DOMAIN: &[u8] = b"RAI/Report/v2";
pub const MAX_REPORT_CHUNK_OBLIGATIONS: usize = 900;
const CLOSE_CUT_DOMAIN: &[u8] = b"RAI/CloseCut/v1";

/// A signed assertion of the slot obligations visible to a representative.
/// `BTreeSet` makes both the signed message and the wire preimage canonical.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiReport {
    pub reporter: PublicKey,
    pub epoch: RaiEpoch,
    pub chunk_index: u16,
    pub chunk_count: u16,
    pub visible_obligations: BTreeSet<RaiSlotId>,
    pub signature: Signature,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiReportProof {
    pub epoch: RaiEpoch,
    pub reports: Vec<RaiReport>,
}

impl RaiReport {
    pub fn new(
        key: &PrivateKey,
        epoch: RaiEpoch,
        obligations: impl IntoIterator<Item = RaiSlotId>,
    ) -> Self {
        let reporter = key.public_key();
        let visible_obligations = obligations.into_iter().collect();
        let mut result = Self {
            reporter,
            epoch,
            chunk_index: 0,
            chunk_count: 1,
            visible_obligations,
            signature: Signature::new(),
        };
        result.signature = key.sign(&result.signing_bytes());
        result
    }

    pub fn new_chunks(
        key: &PrivateKey,
        epoch: RaiEpoch,
        obligations: impl IntoIterator<Item = RaiSlotId>,
    ) -> Vec<Self> {
        let obligations = obligations.into_iter().collect::<BTreeSet<_>>();
        let chunks = obligations
            .into_iter()
            .collect::<Vec<_>>()
            .chunks(MAX_REPORT_CHUNK_OBLIGATIONS)
            .map(|chunk| chunk.iter().cloned().collect::<BTreeSet<_>>())
            .collect::<Vec<_>>();
        let chunks = if chunks.is_empty() {
            vec![BTreeSet::new()]
        } else {
            chunks
        };
        let chunk_count = u16::try_from(chunks.len()).expect("too many RAI report chunks");
        chunks
            .into_iter()
            .enumerate()
            .map(|(chunk_index, visible_obligations)| {
                let mut report = Self {
                    reporter: key.public_key(),
                    epoch,
                    chunk_index: chunk_index as u16,
                    chunk_count,
                    visible_obligations,
                    signature: Signature::new(),
                };
                report.signature = key.sign(&report.signing_bytes());
                report
            })
            .collect()
    }

    pub fn validate(&self) -> bool {
        self.reporter
            .verify(&self.signing_bytes(), &self.signature)
            .is_ok()
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = canonical_obligations(REPORT_DOMAIN, self.epoch, &self.visible_obligations);
        bytes.extend_from_slice(&self.chunk_index.to_be_bytes());
        bytes.extend_from_slice(&self.chunk_count.to_be_bytes());
        bytes
    }
}

impl From<RaiReport> for rsnano_messages::RaiReportMessage {
    fn from(report: RaiReport) -> Self {
        Self {
            reporter: report.reporter,
            epoch: report.epoch,
            chunk_index: report.chunk_index,
            chunk_count: report.chunk_count,
            visible_obligations: report.visible_obligations.into_iter().collect(),
            signature: report.signature,
        }
    }
}

impl From<rsnano_messages::RaiReportMessage> for RaiReport {
    fn from(report: rsnano_messages::RaiReportMessage) -> Self {
        Self {
            reporter: report.reporter,
            epoch: report.epoch,
            chunk_index: report.chunk_index,
            chunk_count: report.chunk_count,
            visible_obligations: report.visible_obligations.into_iter().collect(),
            signature: report.signature,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportInsert {
    Added,
    Duplicate,
    Equivocation,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReportError {
    InvalidSignature,
    WrongEpoch,
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidSignature => "invalid report signature",
            Self::WrongEpoch => "report contains an obligation from a different epoch",
        })
    }
}
impl std::error::Error for ReportError {}

/// Deduplicates reports and permanently excludes an epoch/reporter pair after
/// two different signed payloads are observed.
#[derive(Default)]
pub struct RaiReportStore {
    reports: BTreeMap<(RaiEpoch, PublicKey, u16), RaiReport>,
    equivocators: BTreeSet<(RaiEpoch, PublicKey)>,
}

impl RaiReportStore {
    pub fn all(&self) -> Vec<RaiReport> {
        self.reports.values().cloned().collect()
    }

    pub fn insert(&mut self, report: RaiReport) -> Result<ReportInsert, ReportError> {
        if report
            .visible_obligations
            .iter()
            .any(|obligation| obligation.epoch != report.epoch)
        {
            return Err(ReportError::WrongEpoch);
        }
        if !report.validate() {
            return Err(ReportError::InvalidSignature);
        }
        let reporter_key = (report.epoch, report.reporter);
        if report.chunk_count == 0 || report.chunk_index >= report.chunk_count {
            return Err(ReportError::WrongEpoch);
        }
        if self.equivocators.contains(&reporter_key) {
            return Ok(ReportInsert::Equivocation);
        }
        if self.reports.iter().any(|((epoch, reporter, _), existing)| {
            *epoch == report.epoch
                && *reporter == report.reporter
                && existing.chunk_count != report.chunk_count
        }) {
            self.reports
                .retain(|(epoch, reporter, _), _| (*epoch, *reporter) != reporter_key);
            self.equivocators.insert(reporter_key);
            return Ok(ReportInsert::Equivocation);
        }
        let key = (report.epoch, report.reporter, report.chunk_index);
        match self.reports.get(&key) {
            None => {
                self.reports.insert(key, report);
                Ok(ReportInsert::Added)
            }
            Some(old) if old.visible_obligations == report.visible_obligations => {
                Ok(ReportInsert::Duplicate)
            }
            Some(_) => {
                self.reports.remove(&key);
                self.reports
                    .retain(|(epoch, reporter, _), _| (*epoch, *reporter) != reporter_key);
                self.equivocators.insert(reporter_key);
                Ok(ReportInsert::Equivocation)
            }
        }
    }

    pub fn is_equivocator(&self, epoch: RaiEpoch, reporter: &PublicKey) -> bool {
        self.equivocators.contains(&(epoch, *reporter))
    }

    pub fn report_weight(
        &self,
        epoch: RaiEpoch,
        reporter: &PublicKey,
        committee: &RepWeights,
    ) -> Amount {
        if self.is_equivocator(epoch, reporter) || !self.has_complete_report(epoch, reporter) {
            Amount::ZERO
        } else {
            committee.weight(reporter)
        }
    }

    /// Returns a canonical proof containing only non-equivocating committee
    /// members. Reports have already passed signature validation on insertion.
    pub fn proof(&self, epoch: RaiEpoch, committee: &RepWeights) -> RaiReportProof {
        RaiReportProof {
            epoch,
            reports: self
                .reports
                .iter()
                .filter(|((e, reporter, _), _)| {
                    *e == epoch
                        && !self.is_equivocator(epoch, reporter)
                        && self.has_complete_report(epoch, reporter)
                        && !committee.weight(reporter).is_zero()
                })
                .map(|(_, report)| report.clone())
                .collect(),
        }
    }

    pub fn has_quorum(&self, epoch: RaiEpoch, committee: &RepWeights) -> bool {
        let total = total_weight(committee);
        if total == 0 {
            return false;
        }
        let faulty = rai_fault_allowance(total);
        self.proof(epoch, committee)
            .reports
            .iter()
            .map(|report| report.reporter)
            .collect::<BTreeSet<_>>()
            .iter()
            .fold(0u128, |sum, reporter| {
                sum.saturating_add(raw(committee.weight(reporter)))
            })
            >= total.saturating_sub(faulty)
    }

    pub fn has_full_coverage(&self, epoch: RaiEpoch, committee: &RepWeights) -> bool {
        let total = total_weight(committee);
        total != 0
            && self
                .proof(epoch, committee)
                .reports
                .iter()
                .map(|report| report.reporter)
                .collect::<BTreeSet<_>>()
                .iter()
                .fold(0u128, |sum, reporter| {
                    sum.saturating_add(raw(committee.weight(reporter)))
                })
                == total
    }

    fn has_complete_report(&self, epoch: RaiEpoch, reporter: &PublicKey) -> bool {
        let chunks = self
            .reports
            .iter()
            .filter(|((e, r, _), _)| *e == epoch && r == reporter)
            .map(|(_, report)| report)
            .collect::<Vec<_>>();
        chunks.first().is_some_and(|first| {
            chunks.len() == first.chunk_count as usize
                && chunks
                    .iter()
                    .enumerate()
                    .all(|(index, report)| report.chunk_index as usize == index)
        })
    }

    /// An obligation is report-visible when its support from valid,
    /// non-equivocating close-committee reports is strictly greater than F.
    pub fn visible_from_reports(
        &self,
        epoch: RaiEpoch,
        committee: &RepWeights,
    ) -> BTreeSet<RaiSlotId> {
        let faulty = rai_fault_allowance(total_weight(committee));
        let mut support = BTreeMap::<RaiSlotId, u128>::new();
        for report in self.proof(epoch, committee).reports {
            let weight = raw(committee.weight(&report.reporter));
            for obligation in report.visible_obligations {
                support
                    .entry(obligation)
                    .and_modify(|total| *total = total.saturating_add(weight))
                    .or_insert(weight);
            }
        }
        support
            .into_iter()
            .filter(|(_, weight)| *weight > faulty)
            .map(|(obligation, _)| obligation)
            .collect()
    }
}

fn total_weight(committee: &RepWeights) -> u128 {
    committee
        .iter()
        .fold(0u128, |sum, (_, weight)| sum.saturating_add(raw(*weight)))
}

fn raw(amount: Amount) -> u128 {
    u128::from_be_bytes(amount.to_be_bytes())
}

/// Returns obligations supported by a voter in any applicable slot committee.
pub fn visible_from_slot_votes<'a>(
    votes: impl IntoIterator<Item = (&'a PublicKey, &'a RaiSlotId)>,
    slot_committees: &[std::sync::Arc<RepWeights>],
) -> BTreeSet<RaiSlotId> {
    votes
        .into_iter()
        .filter(|(voter, _)| slot_committees.iter().any(|c| !c.weight(voter).is_zero()))
        .map(|(_, root)| root.clone())
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseCut {
    pub epoch: RaiEpoch,
    pub obligations: BTreeSet<RaiSlotId>,
}

impl RaiCloseCut {
    pub fn new(epoch: RaiEpoch, obligations: impl IntoIterator<Item = RaiSlotId>) -> Self {
        Self {
            epoch,
            obligations: obligations.into_iter().collect(),
        }
    }
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_obligations(CLOSE_CUT_DOMAIN, self.epoch, &self.obligations)
    }
    pub fn hash(&self) -> BlockHash {
        hash_bytes(&self.canonical_bytes())
    }
}

/// Hash-to-preimage cache. A hash can only name its canonical, validated cut.
#[derive(Clone, Debug, Default)]
pub struct RaiCloseCutStore {
    cuts: BTreeMap<BlockHash, RaiCloseCut>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CloseCutDecisionError {
    WrongPhase,
    MissingPreimage,
    InvalidCut,
    ImmutableDecision,
}

impl std::fmt::Display for CloseCutDecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongPhase => "epoch is not closing its cut",
            Self::MissingPreimage => "canonical cut preimage is unavailable",
            Self::InvalidCut => "cut does not match currently visible obligations",
            Self::ImmutableDecision => "the epoch already has a different decided cut",
        })
    }
}
impl std::error::Error for CloseCutDecisionError {}

impl RaiCloseCutStore {
    pub fn insert(&mut self, cut: RaiCloseCut) -> BlockHash {
        let hash = cut.hash();
        self.cuts.entry(hash).or_insert(cut);
        hash
    }
    pub fn get(&self, hash: &BlockHash) -> Option<&RaiCloseCut> {
        self.cuts.get(hash)
    }

    pub fn all(&self) -> Vec<RaiCloseCut> {
        self.cuts.values().cloned().collect()
    }
}

fn canonical_obligations(
    domain: &[u8],
    epoch: RaiEpoch,
    obligations: &BTreeSet<RaiSlotId>,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(
        domain.len() + 12 + obligations.len() * (8 + QualifiedRoot::SERIALIZED_SIZE),
    );
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(&epoch.number().to_be_bytes());
    bytes.extend_from_slice(&(obligations.len() as u32).to_be_bytes());
    for slot in obligations {
        bytes.extend_from_slice(&slot.epoch.number().to_be_bytes());
        bytes.extend_from_slice(&slot.root.to_bytes());
    }
    bytes
}

fn hash_bytes(bytes: &[u8]) -> BlockHash {
    let mut out = [0; 32];
    let mut hasher = Blake2bVar::new(out.len()).expect("valid hash length");
    hasher.update(bytes);
    hasher
        .finalize_variable(&mut out)
        .expect("configured output length");
    BlockHash::from_bytes(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn root(n: u64) -> QualifiedRoot {
        QualifiedRoot::new(n.into(), (n + 100).into())
    }
    fn slot(epoch: u64, n: u64) -> RaiSlotId {
        RaiSlotId {
            epoch: epoch.into(),
            root: root(n),
        }
    }
    fn weights(items: &[(PublicKey, u128)]) -> Arc<RepWeights> {
        let mut result = RepWeights::default();
        for (key, weight) in items {
            result.put(*key, Amount::raw(*weight));
        }
        Arc::new(result)
    }

    #[test]
    fn report_signatures_are_checked() {
        let key = PrivateKey::from(1);
        let mut report = RaiReport::new(&key, 4.into(), [slot(4, 1)]);
        assert!(report.validate());
        report.visible_obligations.insert(slot(4, 2));
        assert!(!report.validate());
    }

    #[test]
    fn report_rejects_an_obligation_from_another_epoch() {
        let key = PrivateKey::from(1);
        let mut store = RaiReportStore::default();

        assert_eq!(
            store.insert(RaiReport::new(&key, 4.into(), [slot(3, 1)])),
            Err(ReportError::WrongEpoch)
        );
    }

    #[test]
    fn chunked_report_counts_weight_only_after_all_chunks_arrive() {
        let key = PrivateKey::from(1);
        let committee = weights(&[(key.public_key(), 1)]);
        let reports = RaiReport::new_chunks(
            &key,
            RaiEpoch::new(4),
            (0..=MAX_REPORT_CHUNK_OBLIGATIONS as u64).map(|n| slot(4, n)),
        );
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(RaiReport::validate));

        let mut store = RaiReportStore::default();
        store.insert(reports[0].clone()).unwrap();
        assert!(!store.has_quorum(RaiEpoch::new(4), &committee));
        store.insert(reports[1].clone()).unwrap();
        assert!(store.has_quorum(RaiEpoch::new(4), &committee));
        assert_eq!(
            store
                .visible_from_reports(RaiEpoch::new(4), &committee)
                .len(),
            MAX_REPORT_CHUNK_OBLIGATIONS + 1
        );
    }

    #[test]
    fn same_root_in_adjacent_epochs_is_not_merged() {
        let shared_root = root(1);
        let obligations = BTreeSet::from([
            RaiSlotId {
                epoch: 3.into(),
                root: shared_root.clone(),
            },
            RaiSlotId {
                epoch: 4.into(),
                root: shared_root,
            },
        ]);

        assert_eq!(obligations.len(), 2);
    }

    #[test]
    fn equivocation_removes_report_and_weight() {
        let key = PrivateKey::from(1);
        let committee = weights(&[(key.public_key(), 50)]);
        let mut store = RaiReportStore::default();
        store
            .insert(RaiReport::new(&key, 4.into(), [slot(4, 1)]))
            .unwrap();
        assert_eq!(
            store
                .insert(RaiReport::new(&key, 4.into(), [slot(4, 2)]))
                .unwrap(),
            ReportInsert::Equivocation
        );
        assert_eq!(
            store.report_weight(4.into(), &key.public_key(), &committee),
            Amount::ZERO
        );
        assert!(store.visible_from_reports(4.into(), &committee).is_empty());
    }

    #[test]
    fn report_and_vote_visibility_use_the_applicable_committees() {
        let member = PrivateKey::from(1);
        let outsider = PrivateKey::from(2);
        let committee = weights(&[(member.public_key(), 50)]);
        let mut reports = RaiReportStore::default();
        reports
            .insert(RaiReport::new(&member, 3.into(), [slot(3, 2)]))
            .unwrap();
        reports
            .insert(RaiReport::new(&outsider, 3.into(), [slot(3, 3)]))
            .unwrap();
        assert_eq!(
            reports.visible_from_reports(3.into(), &committee),
            BTreeSet::from([slot(3, 2)])
        );
        let member_key = member.public_key();
        let outsider_key = outsider.public_key();
        let member_root = slot(3, 4);
        let outsider_root = slot(3, 5);
        let votes = [(&member_key, &member_root), (&outsider_key, &outsider_root)];
        assert_eq!(
            visible_from_slot_votes(votes, &[committee]),
            BTreeSet::from([slot(3, 4)])
        );
    }

    #[test]
    fn report_quorum_requires_at_least_w_minus_f() {
        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
        ];
        let committee = weights(&[
            (keys[0].public_key(), 8),
            (keys[1].public_key(), 9),
            (keys[2].public_key(), 3),
        ]);
        let mut reports = RaiReportStore::default();
        reports
            .insert(RaiReport::new(&keys[0], 3.into(), []))
            .unwrap();
        assert!(!reports.has_quorum(3.into(), &committee));

        reports
            .insert(RaiReport::new(&keys[1], 3.into(), []))
            .unwrap();
        assert!(reports.has_quorum(3.into(), &committee));
    }

    #[test]
    fn report_visibility_is_strictly_greater_than_f() {
        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
        ];
        let committee = weights(&[
            (keys[0].public_key(), 3),
            (keys[1].public_key(), 4),
            (keys[2].public_key(), 6),
            (keys[3].public_key(), 7),
        ]);
        let exactly_f = slot(3, 1);
        let above_f = slot(3, 2);
        let mut reports = RaiReportStore::default();
        for (key, obligations) in [
            (&keys[0], vec![exactly_f, above_f.clone()]),
            (&keys[1], vec![above_f.clone()]),
            (&keys[2], Vec::new()),
            (&keys[3], Vec::new()),
        ] {
            reports
                .insert(RaiReport::new(key, 3.into(), obligations))
                .unwrap();
        }

        assert_eq!(
            reports.visible_from_reports(3.into(), &committee),
            BTreeSet::from([above_f])
        );
    }

    #[test]
    fn four_honest_reports_derive_same_cut_in_any_arrival_order() {
        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
        ];
        let committee = weights(
            &keys
                .iter()
                .map(|key| (key.public_key(), 5))
                .collect::<Vec<_>>(),
        );
        let reports = [
            RaiReport::new(&keys[0], 3.into(), [slot(3, 1)]),
            RaiReport::new(&keys[1], 3.into(), [slot(3, 2)]),
            RaiReport::new(&keys[2], 3.into(), [slot(3, 1), slot(3, 2)]),
            RaiReport::new(&keys[3], 3.into(), []),
        ];
        let mut forward = RaiReportStore::default();
        let mut reverse = RaiReportStore::default();
        for report in reports.iter().cloned() {
            forward.insert(report).unwrap();
        }
        for report in reports.iter().rev().cloned() {
            reverse.insert(report).unwrap();
        }

        assert!(forward.has_quorum(3.into(), &committee));
        assert!(reverse.has_quorum(3.into(), &committee));
        let forward_cut =
            RaiCloseCut::new(3.into(), forward.visible_from_reports(3.into(), &committee));
        let reverse_cut =
            RaiCloseCut::new(3.into(), reverse.visible_from_reports(3.into(), &committee));
        assert_eq!(forward_cut, reverse_cut);
        assert_eq!(forward_cut.hash(), reverse_cut.hash());
    }

    #[test]
    fn cut_is_canonical_and_hash_validates_preimage() {
        let a = RaiCloseCut::new(7.into(), [slot(7, 2), slot(7, 1), slot(7, 2)]);
        let b = RaiCloseCut::new(7.into(), [slot(7, 1), slot(7, 2)]);
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        assert_eq!(a.hash(), b.hash());
        assert_ne!(a.hash(), RaiCloseCut::new(7.into(), [slot(7, 1)]).hash());
        let mut store = RaiCloseCutStore::default();
        let hash = store.insert(a.clone());
        assert_eq!(store.get(&hash), Some(&a));
    }

    #[test]
    fn cut_hash_commits_to_obligation_epochs() {
        let shared_root = root(1);
        let epoch_seven = RaiCloseCut::new(
            7.into(),
            [RaiSlotId {
                epoch: 7.into(),
                root: shared_root.clone(),
            }],
        );
        let epoch_eight_slot = RaiCloseCut::new(
            7.into(),
            [RaiSlotId {
                epoch: 8.into(),
                root: shared_root,
            }],
        );

        assert_ne!(epoch_seven.hash(), epoch_eight_slot.hash());
    }
}
