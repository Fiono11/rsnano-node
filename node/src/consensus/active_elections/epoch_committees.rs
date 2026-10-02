use std::{cmp::max, collections::BTreeMap, sync::Arc};

use rsnano_ledger::RepWeights;
use rsnano_types::{Account, Amount, BlockHash, ConsensusEpoch, PublicKey};

use crate::{
    consensus::election::{AccountFrontier, Committee, CommitteeWeights, Committees},
    representatives::QuorumSnapshot,
};

/// RAI: the committees of the consensus epochs. The setup of a run leaves
/// the genesis committee; every epoch closed since derives one from the
/// state it finalized, and the epoch two after uses it: the epoch after
/// runs while the close is still on. An epoch's instances are joint with
/// the committee of the epoch before while that epoch is still closing
/// (see `Committees`).
#[derive(Default)]
pub(crate) struct EpochCommittees {
    /// The committee of the setup, used by the first two epochs
    genesis: Option<Arc<Committee>>,
    /// The committee each closed epoch derived
    derived: BTreeMap<ConsensusEpoch, Arc<Committee>>,
    /// The weights as of the frontiers counted so far
    weights: CommitteeWeights,
    /// The frontiers of epochs agreed on before the epoch before them: the
    /// weights are cumulative, so they wait for their turn
    pending: BTreeMap<ConsensusEpoch, Vec<AccountFrontier>>,
}

impl EpochCommittees {
    /// The frontiers of every account at the end of the setup: the genesis
    /// committee, and the base the epochs' frontiers are counted on
    pub fn start(&mut self, frontiers: Vec<AccountFrontier>) -> Arc<Committee> {
        self.weights.count_all(frontiers);
        let genesis = Arc::new(self.weights.committee());
        self.genesis = Some(genesis.clone());
        genesis
    }

    pub fn started(&self) -> bool {
        self.genesis.is_some()
    }

    /// The frontiers the epoch finalized: the committee it derives, and
    /// those of the epochs after it which waited for it. None without a
    /// genesis committee, if the epoch derived one already, or while the
    /// epoch before it has not: the weights are cumulative, so a replica
    /// which agrees on an epoch before the one before it derives nothing
    /// until then, rather than a committee the others do not hold.
    pub fn derive(
        &mut self,
        epoch: ConsensusEpoch,
        frontiers: Vec<AccountFrontier>,
    ) -> Vec<(ConsensusEpoch, Arc<Committee>)> {
        if !self.started() || self.derived.contains_key(&epoch) {
            return Vec::new();
        }
        self.pending.insert(epoch, frontiers);
        let mut derived = Vec::new();
        loop {
            let next = self
                .derived
                .keys()
                .next_back()
                .map_or(ConsensusEpoch::ZERO, |last| last.next());
            let Some(frontiers) = self.pending.remove(&next) else {
                break;
            };
            self.weights.count_all(frontiers);
            let committee = Arc::new(self.weights.committee());
            self.derived.insert(next, committee.clone());
            derived.push((next, committee));
        }
        derived
    }

    /// The committee the instances of an epoch are counted in: the one
    /// derived two epochs before, the genesis committee for the first two
    /// epochs. None while that epoch's derivation is still missing here.
    pub fn committee(&self, epoch: ConsensusEpoch) -> Option<Arc<Committee>> {
        match epoch.as_u64().checked_sub(2) {
            Some(derived_by) => self.derived.get(&ConsensusEpoch::new(derived_by)).cloned(),
            None => self.genesis.clone(),
        }
    }

    /// The committee the instances of an epoch are counted in: the one
    /// derived two epochs before (Section 5.2, K_e = C(e−2)), alone. The
    /// lag is what makes it known before the epoch opens, so the epoch
    /// after can open while this one is still closing.
    pub fn for_epoch(&self, epoch: ConsensusEpoch) -> Option<Committees> {
        self.committee(epoch).map(Committees::single)
    }

    /// The accounts counted so far
    pub fn counted(&self) -> usize {
        self.weights.len()
    }

    /// The height already counted for an account: a frontier at or below it
    /// changes nothing, because the weights are cumulative
    pub fn counted_height(&self, account: &Account) -> Option<u64> {
        self.weights.counted_height(account)
    }

    /// Every committee known here, the genesis one first, as seen from outside
    pub fn infos(&self) -> Vec<CommitteeInfo> {
        let info = |derived_by: Option<ConsensusEpoch>, committee: &Committee| CommitteeInfo {
            derived_by,
            digest: committee.digest(),
            online: committee.online(),
            members: committee.len(),
            weights: committee.weights().iter().map(|(k, v)| (*k, *v)).collect(),
        };
        self.genesis
            .iter()
            .map(|genesis| info(None, genesis))
            .chain(
                self.derived
                    .iter()
                    .map(|(epoch, committee)| info(Some(*epoch), committee)),
            )
            .collect()
    }
}

/// RAI: a committee as seen from outside
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitteeInfo {
    /// The epoch which derived it; None for the genesis committee
    pub derived_by: Option<ConsensusEpoch>,
    pub digest: BlockHash,
    pub online: Amount,
    pub members: usize,
    pub weights: Vec<(PublicKey, Amount)>,
}

/// Without a genesis committee (a run without epochs, the setup before
/// them): the ledger's current weights, n the larger of the online weight
/// and its trended estimate, as the legacy quorum
pub(crate) fn live_committees(rep_weights: &RepWeights, quorum: &QuorumSnapshot) -> Committees {
    let online = max(quorum.online_weight, quorum.trended_or_min_weight);
    Committees::single(Arc::new(Committee::with_online(
        (**rep_weights).clone(),
        online,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{Account, Amount, PrivateKey, PublicKey};

    /// The weights are cumulative: an epoch agreed on before the epoch
    /// before it waits, and both derive once the earlier one is in
    #[test]
    fn an_epoch_agreed_out_of_order_waits_for_the_one_before() {
        let mut committees = EpochCommittees::default();
        committees.start(vec![frontier(1, 1, 1, 100), frontier(2, 1, 2, 100)]);
        // Epoch 1 agreed first: account 2 moves to representative 1
        assert!(
            committees
                .derive(ConsensusEpoch::new(1), vec![frontier(2, 2, 1, 100)])
                .is_empty()
        );
        assert!(committees.committee(ConsensusEpoch::new(3)).is_none());
        // Epoch 0 then: account 1 moves to representative 2, and both derive
        let derived = committees.derive(ConsensusEpoch::ZERO, vec![frontier(1, 2, 2, 100)]);
        assert_eq!(
            derived.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
            vec![ConsensusEpoch::ZERO, ConsensusEpoch::new(1)]
        );
        let c0 = committees.committee(ConsensusEpoch::new(2)).unwrap();
        let c1 = committees.committee(ConsensusEpoch::new(3)).unwrap();
        assert_eq!(c0.weight(&rep(1)), Amount::ZERO);
        assert_eq!(c0.weight(&rep(2)), Amount::raw(200));
        // C(1) counts epoch 0's move too: what a replica deriving in order holds
        assert_eq!(c1.weight(&rep(1)), Amount::raw(100));
        assert_eq!(c1.weight(&rep(2)), Amount::raw(100));
    }

    #[test]
    fn live_committees_follow_the_ledger_and_the_quorum() {
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep(1), Amount::nano(30_000_000));
        let mut quorum = QuorumSnapshot::new_test_instance();
        quorum.online_weight = Amount::nano(50_000_000);
        quorum.trended_or_min_weight = Amount::nano(100_000_000);
        let live = live_committees(&rep_weights, &quorum);
        assert!(!live.is_joint());
        assert_eq!(live.primary().online(), Amount::nano(100_000_000));
        assert_eq!(live.primary().weight(&rep(1)), Amount::nano(30_000_000));
    }

    /*
     * Test helpers
     */

    fn rep(i: u64) -> PublicKey {
        PrivateKey::from(i).public_key()
    }

    fn frontier(account: u64, height: u64, rep_id: u64, balance: u128) -> AccountFrontier {
        AccountFrontier {
            hash: BlockHash::from(height * 1000 + rep_id),
            account: Account::from(account),
            height,
            representative: rep(rep_id),
            balance: Amount::raw(balance),
        }
    }
}
