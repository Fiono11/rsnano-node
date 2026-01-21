use crate::{ledger_snapshots::Aggregator, representatives::ConsensusParams};
use rsnano_messages::{Aggregatable, Preproposal, Proposal, ProposalHash, ProposalVote};
use rsnano_types::{Amount, PrivateKey};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct State {
    pub(crate) preproposal_aggregator: Aggregator<Preproposal>,
    pub(crate) proposal_aggregator: Aggregator<Proposal>,
    pub(crate) vote_aggregator: Aggregator<ProposalVote>,
    pub(crate) proposal_published: bool,
    pub(crate) proposal_voted: bool,
    pub(crate) current_snapshot_number: u32,
}

impl State {
    pub(crate) fn receive_preproposal(&mut self, preproposal: Preproposal) -> bool {
        if preproposal.snapshot_number != self.current_snapshot_number {
            return false;
        }
        self.preproposal_aggregator.add(preproposal);
        true
    }

    pub(crate) fn try_create_proposal(
        &self,
        consensus_params: &ConsensusParams,
        rep_key: &PrivateKey,
    ) -> Option<Proposal> {
        if self.preproposal_aggregator.has_quorum(consensus_params) && !self.proposal_published {
            let proposal = Proposal::new(
                self.preproposal_aggregator.values(),
                rep_key,
                self.current_snapshot_number,
            );
            Some(proposal)
        } else {
            None
        }
    }

    pub(crate) fn receive_proposal(&mut self, proposal: Proposal) -> bool {
        if proposal.snapshot_number != self.current_snapshot_number {
            return false;
        }

        self.proposal_aggregator.add(proposal);
        true
    }

    pub(crate) fn try_create_vote(
        &mut self,
        consensus_params: &ConsensusParams,
        rep_key: &PrivateKey,
    ) -> Option<ProposalVote> {
        let has_quorum = self.proposal_aggregator.has_quorum(&consensus_params);

        if has_quorum {
            let vote = self
                .create_vote(rep_key)
                .expect("Should always be able to create a vote when quorum reached");
            self.proposal_voted = true;
            Some(vote)
        } else {
            None
        }
    }

    pub(crate) fn receive_vote(&mut self, vote: ProposalVote) -> bool {
        if vote.snapshot_number != self.current_snapshot_number {
            return false;
        }

        self.vote_aggregator.add(vote);

        true
    }

    pub(crate) fn find_winner_proposal(&self, params: &ConsensusParams) -> Option<ProposalHash> {
        let mut tallies: HashMap<ProposalHash, Amount> = HashMap::new();

        for vote in self.vote_aggregator.values() {
            let weight = tallies.entry(vote.proposal_hash).or_default();
            *weight += params.rep_weights.weight(&vote.voter);
        }

        tallies
            .into_iter()
            .find(|(_, w)| *w >= params.quorum_weight)
            .map(|(p, _)| p)
    }

    pub(crate) fn advance_epoch(&mut self) {
        self.current_snapshot_number += 1;
        self.preproposal_aggregator.clear();
        self.proposal_aggregator.clear();
        self.vote_aggregator.clear();
        self.proposal_published = false;
        self.proposal_voted = false;
    }

    pub(crate) fn create_vote(&self, private_key: &PrivateKey) -> Option<ProposalVote> {
        Some(ProposalVote::new(
            self.proposal_aggregator.values().map(|p| p.hash()).max()?,
            private_key,
            self.current_snapshot_number,
        ))
    }

    pub(crate) fn set_proposal_published(&mut self, published: bool) {
        self.proposal_published = published;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_ledger::RepWeights;
    use rsnano_messages::ProposalHash;
    use rsnano_types::Amount;

    #[test]
    fn discard_preproposal_with_different_snapshot_number_than_current() {
        let mut state = State::default();
        state.current_snapshot_number = 10;

        let preproposal1 = Preproposal::new(
            vec![],
            &PrivateKey::from(1),
            state.current_snapshot_number - 1,
        );
        state.receive_preproposal(preproposal1.clone());

        assert!(state.preproposal_aggregator.is_empty());

        let preproposal2 = Preproposal::new(
            vec![],
            &PrivateKey::from(1),
            state.current_snapshot_number + 1,
        );
        state.receive_preproposal(preproposal2.clone());

        assert!(state.preproposal_aggregator.is_empty());
    }

    #[test]
    fn discard_proposal_with_different_snapshot_number_than_current() {
        let mut state = State::default();
        state.current_snapshot_number = 10;

        let proposal1 = Proposal::new(
            vec![],
            &PrivateKey::from(1),
            state.current_snapshot_number - 1,
        );
        state.receive_proposal(proposal1.clone());

        assert!(state.proposal_aggregator.is_empty());

        let proposal2 = Proposal::new(
            vec![],
            &PrivateKey::from(1),
            state.current_snapshot_number + 1,
        );
        state.receive_proposal(proposal2.clone());

        assert!(state.proposal_aggregator.is_empty());
    }

    #[test]
    fn discard_vote_with_different_snapshot_number_than_current() {
        let mut state = State::default();
        state.current_snapshot_number = 10;
        let snapshot_number = state.current_snapshot_number;

        let vote1 = ProposalVote::new(
            ProposalHash::from(1),
            &PrivateKey::from(1),
            snapshot_number - 1,
        );

        state.receive_vote(vote1);

        assert!(state.vote_aggregator.is_empty());

        let vote2 = ProposalVote::new(
            ProposalHash::from(1),
            &PrivateKey::from(1),
            snapshot_number + 1,
        );

        state.receive_vote(vote2);

        assert!(state.vote_aggregator.is_empty());
    }

    #[test]
    fn vote_for_proposal_with_highest_hash() {
        let snapshot_number = 0;
        let proposal1 = Proposal::new(vec![], &PrivateKey::from(1), snapshot_number);
        let proposal2 = Proposal::new(vec![], &PrivateKey::from(2), snapshot_number);
        let proposal3 = Proposal::new(vec![], &PrivateKey::from(3), snapshot_number);
        let proposal4 = Proposal::new(vec![], &PrivateKey::from(4), snapshot_number);

        let highest_hash = [
            proposal1.hash(),
            proposal2.hash(),
            proposal3.hash(),
            proposal4.hash(),
        ]
        .into_iter()
        .max()
        .unwrap();

        let mut state = State::default();
        state.proposal_aggregator.add(proposal1);
        state.proposal_aggregator.add(proposal2);
        state.proposal_aggregator.add(proposal3);
        state.proposal_aggregator.add(proposal4);

        let vote = state.create_vote(&PrivateKey::from(5));

        assert_eq!(vote.unwrap().proposal_hash, highest_hash);
    }

    #[test]
    fn a_winner_proposal_is_not_found_if_there_are_no_votes() {
        let state = State::default();

        assert_eq!(
            state.find_winner_proposal(&ConsensusParams::default()),
            None
        );
    }

    #[test]
    fn a_winner_proposal_is_not_found_if_quorum_is_not_reached() {
        let mut params = ConsensusParams::default();
        let rep_key = PrivateKey::from(1);
        let weight = Amount::nano(100_000);
        let mut rep_weights = RepWeights::default();
        rep_weights.set(rep_key.public_key(), weight);
        params.set_rep_weights(rep_weights, Amount::MAX);

        let proposal_hash = ProposalHash::from(1);
        let vote = ProposalVote::new(proposal_hash, &rep_key, 0);

        let mut state = State::default();
        state.vote_aggregator.add(vote);

        assert_eq!(state.find_winner_proposal(&params), None);
    }

    #[test]
    fn a_winner_proposal_is_found_if_quorum_is_reached() {
        let mut params = ConsensusParams::default();

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let weight = Amount::nano(100_000);

        let mut rep_weights = RepWeights::default();
        rep_weights.set(rep_key1.public_key(), weight);
        rep_weights.set(rep_key2.public_key(), weight);
        params.set_rep_weights(rep_weights, weight * 2);

        let proposal_hash = ProposalHash::from(1);
        let vote1 = ProposalVote::new(proposal_hash, &rep_key1, 0);
        let vote2 = ProposalVote::new(proposal_hash, &rep_key2, 0);

        let mut state = State::default();
        state.vote_aggregator.add(vote1);
        state.vote_aggregator.add(vote2);

        assert_eq!(state.find_winner_proposal(&params), Some(proposal_hash));
    }

    #[test]
    fn current_snapshot_number_is_increased_when_proposal_gets_confirmed() {
        let rep_key = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.set(rep_key.public_key(), Amount::MAX);

        let mut state = State::default();
        let snapshot_number = state.current_snapshot_number;

        let preproposal = Preproposal::new(vec![], &rep_key, snapshot_number);
        let proposal = Proposal::new([&preproposal], &rep_key, snapshot_number);
        let vote = ProposalVote::new(ProposalHash::from(123), &rep_key, snapshot_number);

        state.receive_preproposal(preproposal);
        state.receive_proposal(proposal);
        state.receive_vote(vote);
        state.advance_epoch();

        assert_eq!(state.current_snapshot_number, snapshot_number + 1);
        assert_eq!(
            state.preproposal_aggregator.len(),
            0,
            "preproposals not cleared"
        );
        assert_eq!(state.proposal_aggregator.len(), 0, "proposals not cleared");
        assert_eq!(state.vote_aggregator.len(), 0, "votes not cleared");
    }
}
