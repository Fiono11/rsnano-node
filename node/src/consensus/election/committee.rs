use std::{collections::HashMap, sync::Arc};

use rsnano_types::{Account, Amount, Blake2HashBuilder, BlockHash, PublicKey};
use rustc_hash::FxHashMap;

use super::KudzuThresholds;

/// Membership policy for RAI epochs. Setup before epochs remains stake weighted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommitteeModel {
    #[default]
    Weighted,
    EqualWeight {
        f: u32,
        p: u32,
    },
    /// The N = 3f + 2p + 1 members of the genesis committee throughout,
    /// each weighted by the balance delegated to it, but never further than
    /// `drift` permille from the equal share
    BoundedWeight {
        f: u32,
        p: u32,
        drift: u32,
    },
}

impl CommitteeModel {
    /// The drift a bounded-weight committee allows when none is configured.
    /// Below 1/7 (143 permille) with f = p = 1, the members that certify,
    /// finalize fast and select reports are the same as with equal weights.
    pub const DEFAULT_DRIFT: u32 = 100;

    pub fn expected_members(self) -> Option<u64> {
        match self {
            Self::Weighted => None,
            Self::EqualWeight { f, p } | Self::BoundedWeight { f, p, .. } => {
                Some(3 * u64::from(f) + 2 * u64::from(p) + 1)
            }
        }
    }
}

/// RAI: the voting weights the instances of one consensus epoch are counted
/// with, and the Kudzu thresholds derived from them. The committee's members
/// are the same throughout a run; the weight of each moves with the balances
/// of the accounts delegating to it, as of the state one epoch finalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committee {
    weights: FxHashMap<PublicKey, Amount>,
    thresholds: KudzuThresholds,
    model: CommitteeModel,
}

impl Committee {
    /// n is the weight of all members together. The members hold the whole
    /// supply between them, which is the largest amount there is, so the sum
    /// is held at the maximum rather than wrapped: see `CommitteeWeights::add`.
    pub fn new(weights: FxHashMap<PublicKey, Amount>) -> Self {
        let online = weights.values().fold(Amount::ZERO, |sum, weight| {
            sum.number()
                .checked_add(weight.number())
                .map(Amount::raw)
                .unwrap_or(Amount::MAX)
        });
        Self::with_online(weights, online)
    }

    /// n given explicitly: the online weight may exceed the weight known to
    /// vote, e.g. from the trended online weight estimate
    pub fn with_online(weights: FxHashMap<PublicKey, Amount>, online: Amount) -> Self {
        Self {
            weights,
            thresholds: KudzuThresholds::new(online),
            model: CommitteeModel::Weighted,
        }
    }

    /// Require exactly N distinct members; duplicates cannot inflate membership.
    pub fn equal_weight(
        members: impl IntoIterator<Item = PublicKey>,
        f: u32,
        p: u32,
    ) -> Option<Self> {
        let weights: FxHashMap<_, _> = members
            .into_iter()
            .map(|key| (key, Amount::raw(1)))
            .collect();
        if Some(weights.len() as u64) != (CommitteeModel::EqualWeight { f, p }).expected_members() {
            return None;
        }
        Some(Self {
            weights,
            thresholds: KudzuThresholds::equal_weight(f, p),
            model: CommitteeModel::EqualWeight { f, p },
        })
    }

    /// RAI: N = 3f + 2p + 1 members of unequal weight, see
    /// `KudzuThresholds::bounded`. None for the wrong number of members, or
    /// for weights the voting rules do not survive.
    pub fn bounded_weight(
        weights: FxHashMap<PublicKey, Amount>,
        f: u32,
        p: u32,
        drift: u32,
    ) -> Option<Self> {
        let model = CommitteeModel::BoundedWeight { f, p, drift };
        if Some(weights.len() as u64) != model.expected_members() {
            return None;
        }
        let thresholds = KudzuThresholds::bounded(weights.values().copied(), f, p)?;
        Some(Self {
            weights,
            thresholds,
            model,
        })
    }

    /// A committee nobody votes in: the thresholds of the model, no members,
    /// so neither account nor report certificates can form
    fn inert(model: CommitteeModel) -> Self {
        let thresholds = match model {
            CommitteeModel::Weighted => KudzuThresholds::new(Amount::ZERO),
            CommitteeModel::EqualWeight { f, p } => KudzuThresholds::equal_weight(f, p),
            CommitteeModel::BoundedWeight { f, p, .. } => {
                let nominal = (0..3 * f + 2 * p + 1).map(|_| Amount::raw(NOMINAL_WEIGHT));
                KudzuThresholds::bounded(nominal, f, p).expect("equal weights are bounded")
            }
        };
        Self {
            weights: FxHashMap::default(),
            thresholds,
            model,
        }
    }

    pub fn weight(&self, rep: &PublicKey) -> Amount {
        self.weights.get(rep).copied().unwrap_or_default()
    }

    pub fn weights(&self) -> &FxHashMap<PublicKey, Amount> {
        &self.weights
    }

    pub fn thresholds(&self) -> &KudzuThresholds {
        &self.thresholds
    }

    /// n: the weight the thresholds are derived from
    pub fn online(&self) -> Amount {
        self.thresholds.online
    }

    pub fn len(&self) -> usize {
        self.weights.len()
    }

    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }

    /// Order-independent digest of the weights: the same on every replica
    /// which derived the same committee, for the run's checker
    pub fn digest(&self) -> BlockHash {
        let mut entries: Vec<_> = self.weights.iter().collect();
        entries.sort();
        let mut builder = Blake2HashBuilder::new().update(b"RAI committee");
        // Bind the membership model and fault budgets as well as identities.
        // Keep the existing weighted digest unchanged for the baseline.
        match self.model {
            CommitteeModel::Weighted => {}
            CommitteeModel::EqualWeight { f, p } => {
                builder = builder
                    .update(b"equal_weight")
                    .update(f.to_le_bytes())
                    .update(p.to_le_bytes());
            }
            CommitteeModel::BoundedWeight { f, p, drift } => {
                builder = builder
                    .update(b"bounded_weight")
                    .update(f.to_le_bytes())
                    .update(p.to_le_bytes())
                    .update(drift.to_le_bytes());
            }
        }
        for (rep, weight) in entries {
            builder = builder
                .update(rep.as_bytes())
                .update(weight.number().to_le_bytes());
        }
        builder.build()
    }
}

/// RAI: the weight of one member's equal share in a bounded-weight committee
pub const NOMINAL_WEIGHT: u128 = 1000;

/// `balance · scale / total`, rounded to nearest. Both amounts are shifted
/// right until the total fits in 100 bits, so the product cannot overflow
/// for a scale below 2^28; every replica rounds the same way.
fn scaled_share(balance: u128, total: u128, scale: u128) -> u128 {
    let shift = (u128::BITS - total.leading_zeros()).saturating_sub(100);
    let (balance, total) = (balance >> shift, total >> shift);
    (balance * scale + total / 2) / total
}

/// RAI: the committees an instance is counted in. One as a rule: the
/// committee of the instance's epoch. Two while the epoch before the
/// instance's is still closing: its committee and the one before, and the
/// instance is joint. A joint instance notarizes or finalizes a block once
/// both committees do, and times out once one of them does: whatever the
/// single committee after the close notarizes, the joint count notarizes
/// no more than that, and the instance terminates no later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committees {
    committees: Vec<Arc<Committee>>,
}

impl Committees {
    pub fn single(committee: Arc<Committee>) -> Self {
        Self {
            committees: vec![committee],
        }
    }

    /// The instance's own committee first, the one of the epoch before after
    pub fn joint(own: Arc<Committee>, previous: Arc<Committee>) -> Self {
        Self {
            committees: vec![own, previous],
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<Committee>> {
        self.committees.iter()
    }

    pub fn len(&self) -> usize {
        self.committees.len()
    }

    pub fn is_empty(&self) -> bool {
        self.committees.is_empty()
    }

    pub fn is_joint(&self) -> bool {
        self.committees.len() > 1
    }

    /// The instance's own committee: the one its tallies are reported in
    pub fn primary(&self) -> &Committee {
        &self.committees[0]
    }
}

/// RAI: an account at its newest block finalized: what it delegates from
/// then on
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountFrontier {
    pub account: Account,
    pub height: u64,
    /// The block at the frontier: what a decided epoch state names, and what
    /// the delegation below is read from
    pub hash: BlockHash,
    pub representative: PublicKey,
    pub balance: Amount,
}

/// RAI: the voting weights as of the blocks finalized so far. An account
/// contributes its balance to its representative as of the newest block
/// counted for it; a newer block replaces that contribution, whatever the
/// blocks in between did. Every replica which counts the same frontiers
/// holds the same weights.
#[derive(Default, Debug, Clone)]
pub struct CommitteeWeights {
    counted: HashMap<Account, AccountFrontier>,
    weights: FxHashMap<PublicKey, Amount>,
}

impl CommitteeWeights {
    /// Counts an account at a frontier, replacing what was counted for it
    /// before if the frontier is newer
    pub fn count(&mut self, frontier: AccountFrontier) {
        if let Some(counted) = self.counted.get(&frontier.account) {
            if counted.height >= frontier.height {
                return;
            }
            self.subtract(counted.representative, counted.balance);
        }
        self.add(frontier.representative, frontier.balance);
        self.counted.insert(frontier.account, frontier);
    }

    pub fn count_all(&mut self, frontiers: impl IntoIterator<Item = AccountFrontier>) {
        for frontier in frontiers {
            self.count(frontier);
        }
    }

    /// The committee of these weights
    pub fn committee(&self) -> Committee {
        Committee::new(self.weights.clone())
    }

    /// Largest positive holders, with identity order breaking equal balances.
    /// An undersized committee is inert: retain configured thresholds but no
    /// voting members, so neither account nor report certificates can form.
    pub fn committee_under(&self, model: CommitteeModel) -> Committee {
        let Some(n) = model.expected_members() else {
            return self.committee();
        };
        let Some(members) = self.largest_holders(n as usize) else {
            tracing::error!(
                expected = n,
                actual = self.weights.values().filter(|w| !w.is_zero()).count(),
                "RAI committee has too few holders; voting disabled"
            );
            return Committee::inert(model);
        };
        match model {
            CommitteeModel::Weighted => unreachable!("a weighted committee has no fixed size"),
            CommitteeModel::EqualWeight { f, p } => {
                Committee::equal_weight(members, f, p).expect("selected exactly N distinct holders")
            }
            CommitteeModel::BoundedWeight { f, p, drift } => {
                self.bounded_committee(&members, f, p, drift)
            }
        }
    }

    /// RAI: the given members, each weighted by its share of what is
    /// delegated to them together, in units of `NOMINAL_WEIGHT` for the
    /// equal share and held within `drift` permille of it. The shares are
    /// of the members' total, not of the supply, so funds in flight between
    /// accounts move no member's weight. Inert for the wrong number of
    /// members, for members holding nothing, or for weights the voting rules
    /// do not survive.
    pub fn bounded_committee(
        &self,
        members: &[PublicKey],
        f: u32,
        p: u32,
        drift: u32,
    ) -> Committee {
        let model = CommitteeModel::BoundedWeight { f, p, drift };
        let balances: Vec<u128> = members.iter().map(|m| self.weight(m).number()).collect();
        let total = balances
            .iter()
            .fold(0u128, |sum, balance| sum.saturating_add(*balance));
        if total == 0 {
            tracing::error!("RAI bounded-weight committee members hold nothing; voting disabled");
            return Committee::inert(model);
        }
        let scale = NOMINAL_WEIGHT * members.len() as u128;
        let low = NOMINAL_WEIGHT.saturating_sub(u128::from(drift)).max(1);
        let high = NOMINAL_WEIGHT + u128::from(drift);
        let weights = members
            .iter()
            .zip(balances)
            .map(|(member, balance)| {
                let weight = scaled_share(balance, total, scale).clamp(low, high);
                (*member, Amount::raw(weight))
            })
            .collect();
        Committee::bounded_weight(weights, f, p, drift).unwrap_or_else(|| {
            tracing::error!(
                drift,
                "RAI bounded-weight committee outweighed by its heaviest members; voting disabled"
            );
            Committee::inert(model)
        })
    }

    /// The `n` largest positive holders, identity order breaking equal
    /// balances; None if there are fewer
    fn largest_holders(&self, n: usize) -> Option<Vec<PublicKey>> {
        let mut holders: Vec<_> = self.weights.iter().filter(|(_, w)| !w.is_zero()).collect();
        if holders.len() < n {
            return None;
        }
        holders.sort_by(|(a_key, a), (b_key, b)| b.cmp(a).then(a_key.cmp(b_key)));
        Some(holders.into_iter().take(n).map(|(key, _)| *key).collect())
    }

    fn weight(&self, rep: &PublicKey) -> Amount {
        self.weights.get(rep).copied().unwrap_or_default()
    }

    /// The height counted for an account, if any
    pub fn counted_height(&self, account: &Account) -> Option<u64> {
        self.counted.get(account).map(|frontier| frontier.height)
    }

    /// The accounts counted
    pub fn len(&self) -> usize {
        self.counted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counted.is_empty()
    }

    /// Adds an account's balance to its representative's weight.
    ///
    /// The weights of a committee sum to the whole supply when every account
    /// is counted once at a frontier that reflects its finalized sends, and
    /// the supply is the largest amount there is, so there is no headroom: a
    /// sum that overflows means some coins were counted twice, and it is the
    /// counting that is wrong, not the arithmetic. Wrapping would turn a
    /// total slightly over the supply into a tiny one, which is how a single
    /// representative came to outweigh every threshold before. The weight is
    /// held at the maximum instead and the fault is reported, so that a run
    /// shows it rather than deciding on nonsense.
    fn add(&mut self, rep: PublicKey, amount: Amount) {
        if amount.is_zero() {
            return;
        }
        let weight = self.weights.entry(rep).or_default();
        match weight.number().checked_add(amount.number()) {
            Some(sum) => *weight = Amount::raw(sum),
            None => {
                *weight = Amount::MAX;
                crate::utils::diagnostic!(
                    "COMMITTEE_OVERWEIGHT rep={} added={} : an account counted twice",
                    rep,
                    amount.number()
                );
            }
        }
    }

    fn subtract(&mut self, rep: PublicKey, amount: Amount) {
        let Some(weight) = self.weights.get_mut(&rep) else {
            return;
        };
        *weight = weight.checked_sub(amount).unwrap_or_default();
        if weight.is_zero() {
            self.weights.remove(&rep);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn equal_committee_thresholds_and_invalid_membership() {
        let committee = Committee::equal_weight((1..=6).map(rep), 1, 1).unwrap();
        let t = committee.thresholds();
        assert_eq!(
            (
                t.online.number(),
                t.certificate.number(),
                t.fast.number(),
                t.many.number(),
                t.report.number()
            ),
            (6, 4, 5, 3, 5)
        );
        assert_eq!(committee.weight(&rep(1)), Amount::raw(1));
        assert_eq!(committee.weight(&rep(7)), Amount::ZERO);
        assert!(Committee::equal_weight((1..=5).map(rep), 1, 1).is_none());
        assert!(Committee::equal_weight((1..=7).map(rep), 1, 1).is_none());
        assert!(Committee::equal_weight([rep(1); 6], 1, 1).is_none());
    }

    #[test]
    fn largest_holders_and_ties_are_selected_deterministically() {
        let model = CommitteeModel::EqualWeight { f: 1, p: 1 };
        let frontiers: Vec<_> = (1..=8)
            .map(|i| {
                frontier(
                    Account::from(i),
                    1,
                    i,
                    if i == 8 {
                        0
                    } else if i == 7 {
                        1
                    } else {
                        100
                    },
                )
            })
            .collect();
        let mut a = CommitteeWeights::default();
        let mut b = CommitteeWeights::default();
        a.count_all(frontiers.clone());
        b.count_all(frontiers.into_iter().rev());
        assert_eq!(a.committee_under(model), b.committee_under(model));
        let committee = a.committee_under(model);
        assert_eq!(committee.len(), 6);
        assert_eq!(committee.weight(&rep(7)), Amount::ZERO);
        assert_eq!(committee.weight(&rep(8)), Amount::ZERO);
        assert!(committee.weights().values().all(|w| *w == Amount::raw(1)));
        // With equal balances the smallest public key wins a one-member seat.
        let one = a.committee_under(CommitteeModel::EqualWeight { f: 0, p: 0 });
        assert_eq!(one.weight(&(1..=6).map(rep).min().unwrap()), Amount::raw(1));
        assert_eq!(a.committee().weight(&rep(1)), Amount::raw(100));
    }

    #[test]
    fn insufficient_holders_disable_voting_without_lowering_thresholds() {
        let mut weights = CommitteeWeights::default();
        weights.count(frontier(Account::from(1), 1, 1, 100));
        let committee = weights.committee_under(CommitteeModel::EqualWeight { f: 1, p: 1 });
        assert!(committee.is_empty());
        assert_eq!(committee.weight(&rep(1)), Amount::ZERO);
        assert_eq!(committee.thresholds().certificate, Amount::raw(4));
        assert_eq!(committee.thresholds().report, Amount::raw(5));
    }

    #[test]
    fn digest_binds_fault_budgets_and_membership_model() {
        // N=7 admits (f=2,p=0) and (f=0,p=3), with different quorums.
        let a = Committee::equal_weight((1..=7).map(rep), 2, 0).unwrap();
        let b = Committee::equal_weight((1..=7).map(rep), 0, 3).unwrap();
        let weighted = Committee::new(a.weights().clone());
        assert_ne!(a.digest(), b.digest());
        assert_ne!(a.digest(), weighted.digest());
    }

    #[test]
    fn bounded_committee_weights_follow_shares_within_the_drift() {
        let mut weights = CommitteeWeights::default();
        for (i, balance) in [130, 100, 100, 100, 104, 66].into_iter().enumerate() {
            let i = i as u64 + 1;
            weights.count(frontier(Account::from(i), 1, i, balance));
        }
        let members: Vec<_> = (1..=6).map(rep).collect();
        let committee = weights.bounded_committee(&members, 1, 1, 100);
        // Shares of 600 in units of 1000 for an equal share: 1300 and 660
        // are held at the drift, 1040 is not
        assert_eq!(committee.weight(&rep(1)), Amount::raw(1100));
        assert_eq!(committee.weight(&rep(2)), Amount::raw(1000));
        assert_eq!(committee.weight(&rep(5)), Amount::raw(1040));
        assert_eq!(committee.weight(&rep(6)), Amount::raw(900));
        assert_eq!(committee.online(), Amount::raw(6040));
        // The thresholds discount the heaviest members: 1100 and 1040
        let t = committee.thresholds();
        assert_eq!(t.certificate, Amount::raw(6040 - 2140));
        assert_eq!(t.report, Amount::raw(6040 - 1100));
        assert_eq!(
            weights.committee_under(CommitteeModel::BoundedWeight {
                f: 1,
                p: 1,
                drift: 100
            }),
            committee
        );
    }

    #[test]
    fn bounded_committee_counts_only_its_members() {
        let mut weights = CommitteeWeights::default();
        weights.count_all((1..=5).map(|i| frontier(Account::from(i), 1, i, 100)));
        weights.count(frontier(Account::from(7), 1, 7, 1000));
        // Member 6 holds nothing, outsider 7 holds the most
        let members: Vec<_> = (1..=6).map(rep).collect();
        let committee = weights.bounded_committee(&members, 1, 1, 100);
        assert_eq!(committee.len(), 6);
        assert_eq!(committee.weight(&rep(6)), Amount::raw(900));
        assert_eq!(committee.weight(&rep(7)), Amount::ZERO);
        assert_eq!(committee.weight(&rep(1)), Amount::raw(1100));
    }

    #[test]
    fn bounded_committee_is_inert_when_its_heaviest_members_outweigh_the_rest() {
        let mut weights = CommitteeWeights::default();
        for (i, balance) in [200, 200, 50, 50, 50, 50].into_iter().enumerate() {
            let i = i as u64 + 1;
            weights.count(frontier(Account::from(i), 1, i, balance));
        }
        let members: Vec<_> = (1..=6).map(rep).collect();
        let committee = weights.bounded_committee(&members, 1, 1, 500);
        assert!(committee.is_empty());
        assert_eq!(committee.thresholds().certificate, Amount::raw(4000));
        // Members holding nothing and the wrong number of members are inert too
        assert!(
            CommitteeWeights::default()
                .bounded_committee(&members, 1, 1, 100)
                .is_empty()
        );
        assert!(
            weights
                .bounded_committee(&members[..5], 1, 1, 100)
                .is_empty()
        );
    }

    #[test]
    fn bounded_committee_shares_do_not_overflow_for_the_whole_supply() {
        let mut weights = CommitteeWeights::default();
        let sixth = Amount::raw(u128::MAX / 6);
        for i in 1..=6 {
            weights.count(AccountFrontier {
                hash: BlockHash::from(i),
                account: Account::from(i),
                height: 1,
                representative: rep(i),
                balance: sixth,
            });
        }
        let members: Vec<_> = (1..=6).map(rep).collect();
        let committee = weights.bounded_committee(&members, 1, 1, 100);
        assert!(
            committee
                .weights()
                .values()
                .all(|w| *w == Amount::raw(NOMINAL_WEIGHT))
        );
    }

    #[test]
    fn bounded_digest_binds_the_drift_and_the_weights() {
        let members = |w: u128| (1..=6).map(|i| (rep(i), Amount::raw(w))).collect();
        let a = Committee::bounded_weight(members(1000), 1, 1, 100).unwrap();
        let b = Committee::bounded_weight(members(1000), 1, 1, 50).unwrap();
        let mut moved: FxHashMap<_, _> = members(1000);
        moved.insert(rep(1), Amount::raw(1050));
        let c = Committee::bounded_weight(moved, 1, 1, 100).unwrap();
        assert_ne!(a.digest(), b.digest());
        assert_ne!(a.digest(), c.digest());
        assert!(Committee::bounded_weight(members(1000), 2, 1, 100).is_none());
    }

    /// The members of a committee hold the whole supply between them, so an
    /// account counted twice has nowhere to go. The sum is held at the
    /// maximum rather than wrapping to a small number, which would have let
    /// one member outweigh every threshold.
    #[test]
    fn a_weight_over_the_supply_is_held_at_the_maximum() {
        let mut weights = CommitteeWeights::default();
        let rep = PrivateKey::from(1).public_key();
        weights.count(AccountFrontier {
            hash: BlockHash::from(1),
            account: Account::from(1),
            height: 1,
            representative: rep,
            balance: Amount::MAX,
        });
        weights.count(AccountFrontier {
            hash: BlockHash::from(1),
            account: Account::from(2),
            height: 1,
            representative: rep,
            balance: Amount::raw(1000),
        });
        let committee = weights.committee();
        assert_eq!(committee.weight(&rep), Amount::MAX);
        assert_eq!(committee.online(), Amount::MAX);
        // The thresholds stay ordered, so no single vote decides anything
        let thresholds = committee.thresholds();
        assert!(thresholds.certificate < thresholds.fast);
        assert!(thresholds.fast <= committee.online());
    }

    use super::*;
    use rsnano_types::PrivateKey;

    #[test]
    fn committee_derives_its_thresholds_from_the_sum_of_the_weights() {
        let committee = Committee::new(weights(&[(1, 40), (2, 27), (3, 33)]));
        assert_eq!(committee.online(), Amount::raw(100));
        assert_eq!(committee.thresholds().certificate, Amount::raw(62));
        assert_eq!(committee.weight(&rep(1)), Amount::raw(40));
        assert_eq!(committee.weight(&rep(4)), Amount::ZERO);
        assert_eq!(committee.len(), 3);

        let explicit = Committee::with_online(weights(&[(1, 40)]), Amount::raw(100));
        assert_eq!(explicit.online(), Amount::raw(100));
    }

    #[test]
    fn digest_does_not_depend_on_the_order_of_the_weights() {
        let a = Committee::new(weights(&[(1, 40), (2, 27), (3, 33)]));
        let b = Committee::new(weights(&[(3, 33), (1, 40), (2, 27)]));
        let c = Committee::new(weights(&[(1, 40), (2, 27), (3, 34)]));
        assert_eq!(a.digest(), b.digest());
        assert_ne!(a.digest(), c.digest());
    }

    #[test]
    fn committees_single_and_joint() {
        let own = Arc::new(Committee::new(weights(&[(1, 40)])));
        let previous = Arc::new(Committee::new(weights(&[(2, 40)])));
        let single = Committees::single(own.clone());
        assert!(!single.is_joint());
        assert_eq!(single.len(), 1);
        assert_eq!(single.primary(), own.as_ref());

        let joint = Committees::joint(own.clone(), previous.clone());
        assert!(joint.is_joint());
        assert_eq!(joint.len(), 2);
        assert_eq!(joint.primary(), own.as_ref());
        assert_eq!(joint.iter().nth(1).unwrap(), &previous);
    }

    #[test]
    fn an_account_contributes_its_balance_at_its_newest_frontier() {
        let mut weights = CommitteeWeights::default();
        let account = Account::from(1);
        weights.count(frontier(account, 1, 1, 100));
        assert_eq!(weights.committee().weight(&rep(1)), Amount::raw(100));
        assert_eq!(weights.counted_height(&account), Some(1));

        // A send: the balance drops, the representative keeps the rest
        weights.count(frontier(account, 2, 1, 60));
        assert_eq!(weights.committee().weight(&rep(1)), Amount::raw(60));

        // A change of representative moves the whole balance
        weights.count(frontier(account, 3, 2, 60));
        assert_eq!(weights.committee().weight(&rep(1)), Amount::ZERO);
        assert_eq!(weights.committee().weight(&rep(2)), Amount::raw(60));
        assert_eq!(weights.committee().len(), 1);

        // An older or the same frontier changes nothing
        weights.count(frontier(account, 2, 1, 60));
        weights.count(frontier(account, 3, 1, 999));
        assert_eq!(weights.committee().weight(&rep(2)), Amount::raw(60));
        assert_eq!(weights.committee().weight(&rep(1)), Amount::ZERO);
        assert_eq!(weights.len(), 1);
    }

    #[test]
    fn weights_sum_over_the_accounts_of_a_representative() {
        let mut weights = CommitteeWeights::default();
        weights.count_all([
            frontier(Account::from(1), 1, 1, 100),
            frontier(Account::from(2), 1, 1, 50),
            frontier(Account::from(3), 1, 2, 30),
        ]);
        let committee = weights.committee();
        assert_eq!(committee.weight(&rep(1)), Amount::raw(150));
        assert_eq!(committee.weight(&rep(2)), Amount::raw(30));
        assert_eq!(committee.online(), Amount::raw(180));

        // Account 2 moves to representative 2 in a later block
        weights.count(frontier(Account::from(2), 4, 2, 50));
        let committee = weights.committee();
        assert_eq!(committee.weight(&rep(1)), Amount::raw(100));
        assert_eq!(committee.weight(&rep(2)), Amount::raw(80));
        assert_eq!(committee.online(), Amount::raw(180));
    }

    /*
     * Test helpers
     */

    fn rep(i: u64) -> PublicKey {
        PrivateKey::from(i).public_key()
    }

    fn weights(entries: &[(u64, u128)]) -> FxHashMap<PublicKey, Amount> {
        entries
            .iter()
            .map(|(r, w)| (rep(*r), Amount::raw(*w)))
            .collect()
    }

    fn frontier(account: Account, height: u64, rep_id: u64, balance: u128) -> AccountFrontier {
        AccountFrontier {
            hash: BlockHash::from(height * 1000 + rep_id),
            account,
            height,
            representative: rep(rep_id),
            balance: Amount::raw(balance),
        }
    }
}
