use std::{collections::HashMap, sync::Arc};

use rsnano_types::{Account, Amount, Blake2HashBuilder, BlockHash, PublicKey};
use rustc_hash::FxHashMap;

use super::KudzuThresholds;

/// RAI: how a committee counts. The weighted model of the baseline counts
/// each member with the balance delegated to it and derives the thresholds
/// as shares of the whole. The paper's model has N = 3f + 2p + 1 members of
/// equal weight with explicit integer thresholds (Section 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitteeModel {
    Weighted,
    EqualWeight { f: u32, p: u32 },
}

impl Default for CommitteeModel {
    fn default() -> Self {
        Self::Weighted
    }
}

impl CommitteeModel {
    /// N = 3f + 2p + 1 under the equal-weight model
    pub fn expected_members(&self) -> Option<usize> {
        match self {
            Self::Weighted => None,
            Self::EqualWeight { f, p } => Some(3 * *f as usize + 2 * *p as usize + 1),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Weighted => "weighted",
            Self::EqualWeight { .. } => "equal_weight",
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
        }
    }

    /// RAI, "Participants, faults, and membership": the members of equal
    /// weight one, with the paper's explicit thresholds. The members are the
    /// identities holding delegated weight; a member count other than
    /// N = 3f + 2p + 1 is reported by the caller, the thresholds still
    /// follow the configured f and p.
    pub fn equal_weight(members: impl IntoIterator<Item = PublicKey>, f: u32, p: u32) -> Self {
        let weights: FxHashMap<PublicKey, Amount> = members
            .into_iter()
            .map(|member| (member, Amount::raw(1)))
            .collect();
        let thresholds = KudzuThresholds::explicit(weights.len() as u128, f as u128, p as u128);
        Self {
            weights,
            thresholds,
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

    /// The committee of these weights under a model: weighted, or the
    /// identities holding weight as equal members
    pub fn committee_under(&self, model: CommitteeModel) -> Committee {
        match model {
            CommitteeModel::Weighted => self.committee(),
            CommitteeModel::EqualWeight { f, p } => Committee::equal_weight(
                self.weights
                    .iter()
                    .filter(|(_, weight)| !weight.is_zero())
                    .map(|(rep, _)| *rep),
                f,
                p,
            ),
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
    /// RAI, Table 1: f = p = 1, N = 6: four notarize or finalize, five
    /// finalize fast, three first votes recover, five reports close
    #[test]
    fn six_equal_members_have_the_papers_thresholds() {
        let members: Vec<PublicKey> = (1..=6).map(|i| PrivateKey::from(i).public_key()).collect();
        let committee = Committee::equal_weight(members.iter().copied(), 1, 1);
        let thresholds = committee.thresholds();
        assert_eq!(committee.len(), 6);
        assert_eq!(committee.online(), Amount::raw(6));
        assert_eq!(thresholds.certificate, Amount::raw(4));
        assert_eq!(thresholds.fast, Amount::raw(5));
        assert_eq!(thresholds.many, Amount::raw(3));
        assert_eq!(thresholds.report, Amount::raw(5));
        assert_eq!(thresholds.f, Amount::raw(1));
        assert_eq!(committee.weight(&members[0]), Amount::raw(1));
        assert_eq!(
            CommitteeModel::EqualWeight { f: 1, p: 1 }.expected_members(),
            Some(6)
        );
    }

    /// Under the equal-weight model an identity without delegated weight
    /// is not a member, and balances do not tilt the count
    #[test]
    fn equal_weight_members_are_the_identities_holding_weight() {
        let mut weights = CommitteeWeights::default();
        for (i, balance) in [(1u64, 1000u128), (2, 1), (3, 0)] {
            weights.count(AccountFrontier {
                hash: BlockHash::from(i),
                account: Account::from(i),
                height: 1,
                representative: PrivateKey::from(i).public_key(),
                balance: Amount::raw(balance),
            });
        }
        let committee = weights.committee_under(CommitteeModel::EqualWeight { f: 0, p: 0 });
        assert_eq!(committee.len(), 2);
        assert_eq!(
            committee.weight(&PrivateKey::from(1).public_key()),
            Amount::raw(1)
        );
        assert_eq!(
            committee.weight(&PrivateKey::from(2).public_key()),
            Amount::raw(1)
        );
        assert_eq!(
            committee.weight(&PrivateKey::from(3).public_key()),
            Amount::ZERO
        );
        let weighted = weights.committee_under(CommitteeModel::Weighted);
        assert_eq!(
            weighted.weight(&PrivateKey::from(1).public_key()),
            Amount::raw(1000)
        );
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
