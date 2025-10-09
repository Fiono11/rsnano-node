use std::collections::HashMap;

use crate::ledger_snapshots::Aggregator;
use rsnano_ledger::RepWeights;
use rsnano_messages::{Aggregatable, ProposalHash, ProposalVote};
use rsnano_types::Amount;

pub(crate) struct ConsensusParams {
    pub(crate) rep_weights: RepWeights,
    pub(crate) quorum_weight: Amount,
}

impl Default for ConsensusParams {
    fn default() -> Self {
        Self {
            rep_weights: Default::default(),
            quorum_weight: Amount::MAX,
        }
    }
}

impl ConsensusParams {
    pub(crate) fn set_rep_weights(&mut self, rep_weights: RepWeights, quorum_weight: Amount) {
        self.rep_weights = rep_weights;
        self.quorum_weight = quorum_weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_messages::Preproposal;
    use rsnano_types::{Account, BlockHash, PrivateKey};

    #[test]
    fn default_quorum_weight_is_max() {
        let params = ConsensusParams::default();
        assert_eq!(params.quorum_weight, Amount::MAX);
    }

    #[test]
    fn no_quorum_if_value_doesnt_have_enough_vote_weight() {
        let mut consensus_params = ConsensusParams::default();

        let rep_key = PrivateKey::from(1);
        let weight = Amount::nano(10_000);
        let mut rep_weights = RepWeights::new();
        rep_weights.insert(rep_key.public_key(), weight);
        consensus_params.set_rep_weights(rep_weights, Amount::MAX);

        let mut aggregator = Aggregator::default();
        aggregator.add(Preproposal::new(Vec::new(), &rep_key));

        assert_eq!(
            has_quantitative_quorum(&consensus_params, &aggregator),
            false
        );
    }

    #[test]
    fn reach_quantitative_quorum() {
        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);

        let mut rep_weights = RepWeights::new();
        rep_weights.insert(rep_key1.public_key(), Amount::nano(100_000));
        rep_weights.insert(rep_key2.public_key(), Amount::nano(200_000));

        let mut aggregator = Aggregator::default();
        let mut consensus_params = ConsensusParams::default();
        consensus_params.set_rep_weights(rep_weights, Amount::nano(300_000));

        let preproposal1 = Preproposal::new(test_frontiers(), &rep_key1);
        aggregator.add(preproposal1.clone());
        let preproposal2 = Preproposal::new(test_frontiers(), &rep_key2);
        aggregator.add(preproposal2.clone());

        assert_eq!(
            has_quantitative_quorum(&consensus_params, &aggregator),
            true
        );
    }

    #[test]
    fn a_winner_proposal_is_not_found_if_there_are_no_votes() {
        assert_eq!(
            find_winner_proposal(&ConsensusParams::default(), vec![]),
            None
        );
    }

    #[test]
    fn a_winner_proposal_is_not_found_if_quorum_is_not_reached() {
        let mut params = ConsensusParams::default();
        let rep_key = PrivateKey::from(1);
        let weight = Amount::nano(100_000);
        let mut rep_weights = RepWeights::new();
        rep_weights.insert(rep_key.public_key(), weight);
        params.set_rep_weights(rep_weights, Amount::MAX);

        let proposal_hash = ProposalHash::from(1);
        let proposal_vote = ProposalVote::new(proposal_hash, &rep_key);

        assert_eq!(find_winner_proposal(&params, &[proposal_vote]), None);
    }

    #[test]
    fn a_winner_proposal_is_found_if_quorum_is_reached() {
        let mut params = ConsensusParams::default();

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let weight = Amount::nano(100_000);

        let mut rep_weights = RepWeights::new();
        rep_weights.insert(rep_key1.public_key(), weight);
        rep_weights.insert(rep_key2.public_key(), weight);
        params.set_rep_weights(rep_weights, weight * 2);

        let proposal_hash = ProposalHash::from(1);
        let proposal_vote1 = ProposalVote::new(proposal_hash, &rep_key1);
        let proposal_vote2 = ProposalVote::new(proposal_hash, &rep_key2);

        assert_eq!(
            find_winner_proposal(&params, &[proposal_vote1, proposal_vote2]),
            Some(proposal_hash)
        );
    }

    fn test_frontiers() -> Vec<(Account, BlockHash)> {
        vec![(Account::from(1), BlockHash::from(10))]
    }
}
