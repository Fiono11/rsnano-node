use std::{cmp::max, collections::BTreeMap, sync::Arc};

use rsnano_ledger::RepWeights;
use rsnano_types::{Account, Amount, BlockHash, ConsensusEpoch, PublicKey};

use crate::{
    consensus::election::{
        AccountFrontier, Committee, CommitteeModel, CommitteeWeights, Committees,
    },
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
    /// RAI: how the committees count (see `CommitteeModel`)
    model: CommitteeModel,
}

impl EpochCommittees {
    pub fn with_model(model: CommitteeModel) -> Self {
        Self {
            model,
            ..Default::default()
        }
    }

    pub fn model(&self) -> CommitteeModel {
        self.model
    }

    /// The frontiers of every account at the end of the setup: the genesis
    /// committee, and the base the epochs' frontiers are counted on
    pub fn start(&mut self, frontiers: Vec<AccountFrontier>) -> Arc<Committee> {
        self.weights.count_all(frontiers);
        let genesis = Arc::new(self.weights.committee_under(self.model));
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
            let committee = Arc::new(self.weights.committee_under(self.model));
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

    /// The committees the close election of an epoch is counted in
    /// (Section 9.2): the old committee O_e = C(e−2), which issued the
    /// epoch's ordinary votes, and the new committee N_e = C(e−1), which is
    /// already running the epoch after. The two Kudzu instances of the paper
    /// are one joint instance here: a close block is elected once both
    /// committees certify it, and a round times out once either does.
    pub fn for_close(&self, epoch: ConsensusEpoch) -> Option<Committees> {
        let old = self.committee(epoch)?;
        // RAI, "Lagged committees and lifecycle": under the paper's model
        // "handoff for e is decided by C_{e-2} and verified and installed by
        // the already known successor committee C_{e-1}": the closing
        // committee decides alone; the joint count is the baseline's
        if matches!(self.model, CommitteeModel::EqualWeight { .. }) {
            return Some(Committees::single(old));
        }
        let new = self.committee(epoch.next())?;
        if Arc::ptr_eq(&old, &new) {
            Some(Committees::single(old))
        } else {
            Some(Committees::joint(old, new))
        }
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

    /// RAI: under the paper's model the closing committee decides alone,
    /// whatever the successor committee is
    #[test]
    fn the_closing_committee_decides_alone_under_the_equal_weight_model() {
        let mut committees =
            EpochCommittees::with_model(CommitteeModel::EqualWeight { f: 0, p: 0 });
        committees.start(vec![frontier(1, 1, 1, 10)]);
        // Epoch 0 derives a different committee for epoch 2
        committees.derive(ConsensusEpoch::ZERO, vec![frontier(2, 1, 2, 10)]);
        let close = committees.for_close(ConsensusEpoch::ZERO).unwrap();
        assert!(!close.is_joint());
        assert_eq!(close.primary().len(), 1);
        assert_eq!(close.primary().online(), Amount::raw(1));
        let weighted = EpochCommittees::default();
        assert_eq!(weighted.model(), CommitteeModel::Weighted);
    }
    use rsnano_types::{Account, Amount, PrivateKey, PublicKey};

    #[test]
    fn first_two_epochs_use_the_genesis_committee() {
        let mut committees = EpochCommittees::default();
        assert!(!committees.started());
        assert!(committees.committee(ConsensusEpoch::ZERO).is_none());
        assert!(committees.for_epoch(ConsensusEpoch::ZERO).is_none());

        let genesis = committees.start(vec![frontier(1, 1, 1, 100), frontier(2, 1, 2, 100)]);
        assert!(committees.started());
        assert_eq!(genesis.online(), Amount::raw(200));
        assert_eq!(committees.counted(), 2);
        let infos = committees.infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].derived_by, None);
        assert_eq!(infos[0].digest, genesis.digest());
        assert_eq!(infos[0].members, 2);
        for epoch in [ConsensusEpoch::ZERO, ConsensusEpoch::new(1)] {
            let single = committees.for_epoch(epoch).unwrap();
            assert!(!single.is_joint());
            assert_eq!(single.primary(), genesis.as_ref());
        }
        // Both committees of the close of epoch 0 are the genesis committee,
        // and joint with itself is not joint
        let close = committees.for_close(ConsensusEpoch::ZERO).unwrap();
        assert!(!close.is_joint());
        assert_eq!(close.primary(), genesis.as_ref());
        assert!(committees.committee(ConsensusEpoch::new(2)).is_none());
    }

    #[test]
    fn a_closed_epoch_derives_the_committee_of_the_epoch_two_after() {
        let mut committees = EpochCommittees::default();
        // Nothing to derive on before the genesis committee
        assert!(
            committees
                .derive(ConsensusEpoch::ZERO, vec![frontier(1, 2, 2, 100)])
                .is_empty()
        );
        let genesis = committees.start(vec![frontier(1, 1, 1, 100), frontier(2, 1, 2, 100)]);

        // Epoch 0 finalized a change of account 1 to representative 2
        let (_, derived) = committees
            .derive(ConsensusEpoch::ZERO, vec![frontier(1, 2, 2, 100)])
            .pop()
            .unwrap();
        assert_eq!(derived.weight(&rep(1)), Amount::ZERO);
        assert_eq!(derived.weight(&rep(2)), Amount::raw(200));
        // Derived once
        assert!(
            committees
                .derive(ConsensusEpoch::ZERO, vec![frontier(1, 3, 1, 100)])
                .is_empty()
        );

        // Epoch 2 counts in it alone: the lag makes it known before the
        // epoch opens, whatever epoch 1's close is doing
        let epoch2 = ConsensusEpoch::new(2);
        let single = committees.for_epoch(epoch2).unwrap();
        assert!(!single.is_joint());
        assert_eq!(single.primary(), derived.as_ref());

        // The close of epoch 0 counts in the old committee, which voted in
        // epoch 0, and the new one, which runs epoch 1: both the genesis one
        assert!(
            !committees
                .for_close(ConsensusEpoch::ZERO)
                .unwrap()
                .is_joint()
        );
        // The close of epoch 1: the genesis committee and C(0)
        let close1 = committees.for_close(ConsensusEpoch::new(1)).unwrap();
        assert!(close1.is_joint());
        assert_eq!(close1.primary(), genesis.as_ref());
        assert_eq!(close1.iter().nth(1).unwrap(), &derived);

        // Epoch 3 and the close of epoch 2 need the committee of epoch 1
        let epoch3 = ConsensusEpoch::new(3);
        assert!(committees.for_epoch(epoch3).is_none());
        assert!(committees.for_close(epoch2).is_none());
        assert_eq!(
            committees
                .derive(ConsensusEpoch::new(1), vec![frontier(2, 2, 1, 50)])
                .len(),
            1
        );
        let single = committees.for_epoch(epoch3).unwrap();
        assert!(!single.is_joint());
        assert_eq!(single.primary().weight(&rep(1)), Amount::raw(50));
        assert_eq!(single.primary().weight(&rep(2)), Amount::raw(100));
        let close2 = committees.for_close(epoch2).unwrap();
        assert!(close2.is_joint());
        assert_eq!(close2.primary(), derived.as_ref());
        assert_eq!(close2.iter().nth(1).unwrap().as_ref(), single.primary());
    }

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
