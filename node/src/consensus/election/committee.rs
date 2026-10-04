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
}

impl CommitteeModel {
    pub fn expected_members(self) -> Option<u64> {
        match self {
            Self::Weighted => None,
            Self::EqualWeight { f, p } => Some(3 * u64::from(f) + 2 * u64::from(p) + 1),
        }
    }
}

/// Version 1.2 election thresholds, counted in distinct identities.
/// Kept separate from account-vote amounts and their recovery threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointThresholds {
    pub n: u64,
    pub q: u64,
    pub f_fast: u64,
    pub p_recovery: u64,
    pub w: u64,
    f: u64,
    p: u64,
}

impl CheckpointThresholds {
    pub fn new(n: u64, f: u32, p: u32) -> Option<Self> {
        let (f, p) = (u64::from(f), u64::from(p));
        if n != 3 * f + 2 * p + 1 {
            return None;
        }
        Some(Self {
            n,
            q: n - f - p,
            f_fast: n - p,
            p_recovery: n - f,
            w: f + 1,
            f,
            p,
        })
    }

    /// Candidate support for a valid distinct-identity recovery snapshot.
    pub fn recovery_support(self, m: u64) -> Option<u64> {
        (self.q..=self.n).contains(&m).then(|| m - self.f - self.p)
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
        CheckpointThresholds::new(weights.len() as u64, f, p)?;
        Some(Self {
            weights,
            thresholds: KudzuThresholds::equal_weight(f, p),
            model: CommitteeModel::EqualWeight { f, p },
        })
    }

    pub fn checkpoint_thresholds(&self) -> Option<CheckpointThresholds> {
        match self.model {
            CommitteeModel::Weighted => None,
            CommitteeModel::EqualWeight { f, p } => {
                CheckpointThresholds::new(self.len() as u64, f, p)
            }
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
        // Bind equal membership and fault budgets as well as identities. Keep
        // the existing weighted digest unchanged for the baseline.
        if let CommitteeModel::EqualWeight { f, p } = self.model {
            builder = builder
                .update(b"equal_weight")
                .update(f.to_le_bytes())
                .update(p.to_le_bytes());
        }
        for (rep, weight) in entries {
            builder = builder
                .update(rep.as_bytes())
                .update(weight.number().to_le_bytes());
        }
        builder.build()
    }
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
        match model {
            CommitteeModel::Weighted => self.committee(),
            CommitteeModel::EqualWeight { f, p } => {
                let mut holders: Vec<_> =
                    self.weights.iter().filter(|(_, w)| !w.is_zero()).collect();
                holders.sort_by(|(a_key, a), (b_key, b)| b.cmp(a).then(a_key.cmp(b_key)));
                let n = model.expected_members().unwrap();
                if (holders.len() as u64) < n {
                    tracing::error!(
                        expected = n,
                        actual = holders.len(),
                        "RAI equal-weight committee has too few holders; voting disabled"
                    );
                    return Committee {
                        weights: FxHashMap::default(),
                        thresholds: KudzuThresholds::equal_weight(f, p),
                        model,
                    };
                }
                Committee::equal_weight(
                    holders.into_iter().take(n as usize).map(|(key, _)| *key),
                    f,
                    p,
                )
                .expect("selected exactly N distinct holders")
            }
        }
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
    fn checkpoint_thresholds_cover_both_simulator_populations() {
        for (f, p, n, q, fast, recovery, w) in [
            (1, 1, 6, 4, 5, 5, 2),
            (2, 1, 9, 6, 8, 7, 3),
            (0, 0, 1, 1, 1, 1, 1),
        ] {
            let t = CheckpointThresholds::new(n, f, p).unwrap();
            assert_eq!((t.q, t.f_fast, t.p_recovery, t.w), (q, fast, recovery, w));
            assert_eq!(2 * t.q - n, u64::from(f) + 1);
            assert_eq!(2 * t.f_fast - n, 3 * u64::from(f) + 1);
            assert_eq!(t.f_fast + t.p_recovery - n, t.q);
            assert_eq!(t.recovery_support(t.p_recovery), Some(u64::from(f + p) + 1));
            assert!(t.recovery_support(q - 1).is_none());
            assert!(t.recovery_support(n + 1).is_none());
            assert!(CheckpointThresholds::new(n + 1, f, p).is_none());
        }
        let t = CheckpointThresholds::new(6, 1, 1).unwrap();
        assert_eq!(t.recovery_support(4), Some(2));
        assert_eq!(t.recovery_support(5), Some(3));
        let n = CommitteeModel::EqualWeight {
            f: u32::MAX,
            p: u32::MAX,
        }
        .expected_members()
        .unwrap();
        assert!(CheckpointThresholds::new(n, u32::MAX, u32::MAX).is_some());
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
        assert!(committee.checkpoint_thresholds().is_none());
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
