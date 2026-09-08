use rustc_hash::FxHashMap as HashMap;
use std::sync::Arc;

use rsnano_types::{Amount, BlockHash, PublicKey, Vote, VoteError, VoteKind};
use rustc_hash::FxHashMap;

/// Weighted election adaptation: f = 19%, p = 19% of the legacy quorum base.
#[derive(Clone, Copy, Debug)]
pub struct KudzuThresholds {
    pub total: Amount,
    pub certificate: Amount,
    pub fast: Amount,
    pub second_look: Amount,
}

impl KudzuThresholds {
    pub const F_PERCENT: u128 = 19;
    pub const P_PERCENT: u128 = 19;
    pub fn new(total: Amount) -> Self {
        // Quotient/remainder arithmetic avoids overflowing even for Amount::MAX.
        let percent = |p: u128, ceil: bool| {
            let n = total.number();
            Amount::raw((n / 100) * p + ((n % 100) * p + if ceil { 99 } else { 0 }) / 100)
        };
        Self {
            total,
            certificate: percent(100 - Self::F_PERCENT - Self::P_PERCENT, true),
            fast: percent(100 - Self::P_PERCENT, true),
            second_look: Amount::raw(
                percent(Self::F_PERCENT + Self::P_PERCENT, false).number() + 1,
            ),
        }
    }
}

/// A local certificate consists of distinct representatives' authenticated statements.
/// These are collected from ordinary vote gossip, not a new certificate wire message.
#[derive(Clone, Debug)]
pub struct KudzuCertificate {
    pub hash: BlockHash,
    pub kind: VoteKind,
    pub total_weight: Amount,
    pub votes: Vec<Arc<Vote>>,
}

#[derive(Clone, Default)]
pub(super) struct KudzuVotes {
    first: HashMap<PublicKey, Arc<Vote>>,
    notar: HashMap<(PublicKey, BlockHash), Arc<Vote>>,
    final_votes: HashMap<PublicKey, Arc<Vote>>,
    pub thresholds: Option<KudzuThresholds>,
    pub first_tallies: HashMap<BlockHash, Amount>,
    pub notar_tallies: HashMap<BlockHash, Amount>,
    pub final_tallies: HashMap<BlockHash, Amount>,
    // A batched vote can contain several election candidates. Remember the specific
    // candidate admitted for this instance, rather than searching the signed batch.
    first_hashes: HashMap<PublicKey, BlockHash>,
    final_hashes: HashMap<PublicKey, BlockHash>,
}

impl KudzuVotes {
    pub fn insert(&mut self, vote: Arc<Vote>, hash: BlockHash) -> Result<(), VoteError> {
        let rep = vote.voter;
        match vote.kind() {
            VoteKind::First => {
                if self.first.contains_key(&rep) {
                    return Err(VoteError::Replay);
                }
                if !self.notar.contains_key(&(rep, hash))
                    && self.notar.keys().filter(|(r, _)| *r == rep).count() >= 3
                {
                    return Err(VoteError::Ignored);
                }
                self.first_hashes.insert(rep, hash);
                self.first.insert(rep, vote.clone());
                self.notar.insert((rep, hash), vote);
            }
            VoteKind::Notarize => {
                if self.notar.contains_key(&(rep, hash)) {
                    return Err(VoteError::Replay);
                }
                if self.notar.keys().filter(|(r, _)| *r == rep).count() >= 3 {
                    return Err(VoteError::Ignored);
                }
                self.notar.insert((rep, hash), vote);
            }
            VoteKind::Final => {
                if self.final_votes.contains_key(&rep) {
                    return Err(VoteError::Replay);
                }
                self.final_hashes.insert(rep, hash);
                self.final_votes.insert(rep, vote);
            }
        }
        Ok(())
    }

    pub fn tally(&mut self, weights: &FxHashMap<PublicKey, Amount>, total: Amount) {
        self.thresholds = Some(KudzuThresholds::new(total));
        self.first_tallies.clear();
        self.notar_tallies.clear();
        self.final_tallies.clear();
        let add = |tallies: &mut HashMap<BlockHash, Amount>, rep, hash| {
            let weight = weights.get(&rep).copied().unwrap_or_default();
            let entry = tallies.entry(hash).or_default();
            *entry = Amount::raw(entry.number().saturating_add(weight.number()));
        };
        for (&rep, &hash) in &self.first_hashes {
            add(&mut self.first_tallies, rep, hash);
        }
        for &(rep, hash) in self.notar.keys() {
            add(&mut self.notar_tallies, rep, hash);
        }
        for (&rep, &hash) in &self.final_hashes {
            add(&mut self.final_tallies, rep, hash);
        }
    }

    pub fn needs_vote(&self, rep: &PublicKey, hash: BlockHash, quorum: bool) -> bool {
        !self.notar.contains_key(&(*rep, hash))
            || (quorum && self.final_hashes.get(rep) != Some(&hash))
    }

    pub fn has_certificate(&self, hash: BlockHash, kind: VoteKind) -> bool {
        let Some(t) = self.thresholds else {
            return false;
        };
        if t.total.is_zero() {
            return false;
        }
        let (tally, required) = match kind {
            VoteKind::First => (self.first_tallies.get(&hash), t.fast),
            VoteKind::Notarize => (self.notar_tallies.get(&hash), t.certificate),
            VoteKind::Final => (self.final_tallies.get(&hash), t.certificate),
        };
        tally.copied().unwrap_or_default() >= required
    }

    pub fn certificate(&self, hash: BlockHash, kind: VoteKind) -> Option<KudzuCertificate> {
        if !self.has_certificate(hash, kind) {
            return None;
        }
        let t = self.thresholds?;
        let votes = match kind {
            VoteKind::First => self
                .first
                .iter()
                .filter(|(r, _)| self.first_hashes[r] == hash)
                .map(|(_, v)| v.clone())
                .collect(),
            VoteKind::Notarize => self
                .notar
                .iter()
                .filter(|((_, h), _)| *h == hash)
                .map(|(_, v)| v.clone())
                .collect(),
            VoteKind::Final => self
                .final_votes
                .iter()
                .filter(|(r, _)| self.final_hashes[r] == hash)
                .map(|(_, v)| v.clone())
                .collect(),
        };
        Some(KudzuCertificate {
            hash,
            kind,
            total_weight: t.total,
            votes,
        })
    }

    pub fn second_look(&self, hash: &BlockHash) -> bool {
        self.thresholds.is_some_and(|t| {
            !t.total.is_zero()
                && self.first_tallies.get(hash).copied().unwrap_or_default() >= t.second_look
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::Election;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{Block, PrivateKey, SavedBlock, StateBlockArgs};

    fn election() -> (Election, BlockHash, BlockHash) {
        let args = StateBlockArgs::new_test_instance();
        let a = SavedBlock::new_test_instance_with(args.clone().into());
        let b: Block = StateBlockArgs {
            representative: 999.into(),
            ..args
        }
        .into();
        let mut e = Election::new_test_instance_with(a.clone());
        e.try_add_fork(&b, Amount::ZERO);
        (e, a.hash(), b.hash())
    }

    fn vote(e: &mut Election, rep: u64, hash: BlockHash, kind: VoteKind) -> Result<(), VoteError> {
        e.add_kudzu_vote(
            Arc::new(Vote::new_with_kind(
                &PrivateKey::from(rep),
                vec![hash],
                e.epoch,
                kind,
            )),
            hash,
            Timestamp::new_test_instance(),
        )
    }

    fn weights(entries: &[(u64, u128)]) -> FxHashMap<PublicKey, Amount> {
        entries
            .iter()
            .map(|(r, w)| (PrivateKey::from(*r).public_key(), Amount::raw(*w)))
            .collect()
    }

    #[test]
    fn kudzu_threshold_rounding_and_full_supply() {
        let t = KudzuThresholds::new(Amount::raw(100));
        assert_eq!(
            (t.certificate, t.fast, t.second_look),
            (Amount::raw(62), Amount::raw(81), Amount::raw(39))
        );
        let t = KudzuThresholds::new(Amount::raw(101));
        assert_eq!(
            (t.certificate, t.fast, t.second_look),
            (Amount::raw(63), Amount::raw(82), Amount::raw(39))
        );
        let t = KudzuThresholds::new(Amount::MAX);
        assert!(t.fast < Amount::MAX && t.fast > t.certificate);
    }

    #[test]
    fn kudzu_fast_certificate_at_81_percent_only() {
        let (mut e, a, _) = election();
        let weights = weights(&[(1, 80), (2, 1)]);
        vote(&mut e, 1, a, VoteKind::First).unwrap();
        e.update_kudzu_tallies(&weights, Amount::raw(100));
        assert!(e.has_quorum());
        assert!(!e.is_confirmed());
        vote(&mut e, 2, a, VoteKind::First).unwrap();
        e.update_kudzu_tallies(&weights, Amount::raw(100));
        assert!(e.is_confirmed());
        assert_eq!(
            e.kudzu_certificate(a, VoteKind::First).unwrap().votes.len(),
            2
        );
    }

    #[test]
    fn kudzu_final_votes_do_not_manufacture_first_or_notarization_votes() {
        let (mut e, a, _) = election();
        let weights = weights(&[(1, 61), (2, 1)]);
        vote(&mut e, 1, a, VoteKind::Final).unwrap();
        vote(&mut e, 2, a, VoteKind::Final).unwrap();
        e.update_kudzu_tallies(&weights, Amount::raw(100));
        assert!(!e.is_confirmed());
        assert!(!e.has_quorum());
        // Reordered first votes must still be accepted after final votes.
        vote(&mut e, 1, a, VoteKind::First).unwrap();
        e.update_kudzu_tallies(&weights, Amount::raw(100));
        assert!(!e.is_confirmed());
        vote(&mut e, 2, a, VoteKind::First).unwrap();
        e.update_kudzu_tallies(&weights, Amount::raw(100));
        assert!(e.is_confirmed());
        assert!(e.kudzu_certificate(a, VoteKind::First).is_none());
    }

    #[test]
    fn kudzu_first_vote_already_notarizes_and_cannot_switch() {
        let (mut e, a, b) = election();
        vote(&mut e, 1, a, VoteKind::First).unwrap();
        assert_eq!(
            vote(&mut e, 1, a, VoteKind::Notarize),
            Err(VoteError::Replay)
        );
        assert_eq!(vote(&mut e, 1, b, VoteKind::First), Err(VoteError::Replay));
        vote(&mut e, 1, b, VoteKind::Notarize).unwrap();
        e.update_kudzu_tallies(&weights(&[(1, 40)]), Amount::raw(100));
        assert_eq!(e.kudzu.first_tallies.get(&a), Some(&Amount::raw(40)));
        assert_eq!(e.kudzu.notar_tallies.get(&a), Some(&Amount::raw(40)));
        assert_eq!(e.kudzu.notar_tallies.get(&b), Some(&Amount::raw(40)));
        assert!(!e.is_confirmed());
    }

    #[test]
    fn kudzu_second_look_is_strictly_above_38_percent() {
        let (mut e, _, b) = election();
        vote(&mut e, 1, b, VoteKind::First).unwrap();
        let weights = weights(&[(1, 38), (2, 1)]);
        e.update_kudzu_tallies(&weights, Amount::raw(100));
        assert!(!e.can_notarize(&b));
        vote(&mut e, 2, b, VoteKind::First).unwrap();
        e.update_kudzu_tallies(&weights, Amount::raw(100));
        assert!(e.can_notarize(&b));
        assert_eq!(e.winner().hash(), b);
    }

    #[test]
    fn kudzu_zero_denominator_and_wrong_epoch_cannot_confirm() {
        let (mut e, a, _) = election();
        vote(&mut e, 1, a, VoteKind::First).unwrap();
        e.update_kudzu_tallies(&weights(&[(1, 100)]), Amount::ZERO);
        assert!(!e.is_confirmed());
        let wrong_epoch = Arc::new(Vote::new_with_kind(
            &PrivateKey::from(2),
            vec![a],
            99,
            VoteKind::First,
        ));
        assert_eq!(
            e.add_kudzu_vote(wrong_epoch, a, Timestamp::new_test_instance()),
            Err(VoteError::Invalid)
        );
    }
}
