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
        use primitive_types::U256;
        use rsnano_messages::ProposalHash;

        // Can't determine a winner if quorum hasn't been reached
        if !self.vote_aggregator.has_quorum(params) {
            return None;
        }

        // Calculate tallies for each proposal hash
        let mut tallies: HashMap<ProposalHash, Amount> = HashMap::new();
        let mut total_vote_weight = Amount::ZERO;

        for vote in self.vote_aggregator.values() {
            let weight = params.rep_weights.weight(&vote.voter);
            let entry = tallies.entry(vote.proposal_hash).or_default();
            *entry += weight;
            total_vote_weight += weight;
        }

        // Calculate online weight from quorum_weight (quorum_weight is 67% of online weight)
        // online_weight = quorum_weight * 100 / 67
        let online_weight = if params.quorum_weight == Amount::MAX {
            // If quorum_weight is MAX, we can't calculate online weight
            // In this case, use total_vote_weight as a proxy
            total_vote_weight
        } else {
            // Calculate: online_weight = quorum_weight * 100 / 67
            // Using U256 for precision
            let quorum_u256 = U256::from(params.quorum_weight.number());
            let online_u256 = quorum_u256 * U256::from(100) / U256::from(67);
            Amount::raw(online_u256.as_u128())
        };

        // Check if we have 4f+1 votes (where f < 20%, so 4f+1 = 80%+1)
        // 4f+1 = 80% + 1 = 0.8 * online_weight + 1
        // For simplicity, we'll use 80% threshold (4f+1 ≈ 80% when f is close to 20%)
        let four_f_plus_one_threshold = if online_weight > Amount::ZERO {
            // Calculate 80% of online weight
            let threshold_u256 =
                U256::from(online_weight.number()) * U256::from(80) / U256::from(100);
            Amount::raw(threshold_u256.as_u128())
        } else {
            Amount::ZERO
        };

        // Only proceed if we have at least 4f+1 votes (80% of online weight)
        if total_vote_weight < four_f_plus_one_threshold {
            return None;
        }

        // Find the proposal with the most votes
        let (most_popular_hash, most_popular_weight) = tallies
            .iter()
            .max_by_key(|(_, weight)| **weight)
            .map(|(hash, weight)| (*hash, *weight))?;

        // Decision rules:
        // 1. If 4f+1 vote for the same proposal hash, confirm that one
        if most_popular_weight >= four_f_plus_one_threshold {
            return Some(most_popular_hash);
        }

        // 2. If (total votes - votes for most popular) >= 40%, confirm nil
        let votes_not_for_most_popular = total_vote_weight - most_popular_weight;
        let forty_percent_threshold = if online_weight > Amount::ZERO {
            let threshold_u256 =
                U256::from(online_weight.number()) * U256::from(40) / U256::from(100);
            Amount::raw(threshold_u256.as_u128())
        } else {
            Amount::ZERO
        };

        if votes_not_for_most_popular >= forty_percent_threshold {
            // Confirm nil (return None, which represents nil)
            return None;
        }

        // 3. Else (<40%), if some proposal has at least 41%, confirm that one
        let forty_one_percent_threshold = if online_weight > Amount::ZERO {
            let threshold_u256 =
                U256::from(online_weight.number()) * U256::from(41) / U256::from(100);
            Amount::raw(threshold_u256.as_u128())
        } else {
            Amount::ZERO
        };

        if most_popular_weight >= forty_one_percent_threshold {
            return Some(most_popular_hash);
        }

        // 4. Else confirm nil
        None
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
        rep_weights.put(rep_key.public_key(), weight);
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
        let weight = Amount::nano(120_000_000);

        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key1.public_key(), weight);
        rep_weights.put(rep_key2.public_key(), weight);
        // quorum_weight must be <= 1.675 * weight for 2w total votes to reach 80% threshold
        // Using 1.67 * weight to be safely below the limit
        params.set_rep_weights(rep_weights, Amount::nano(200_400_000));

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
        weights.put(rep_key.public_key(), Amount::MAX);

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

    // Helper function to create consensus params with specific weights
    fn create_consensus_params(
        rep_weights: &[(PrivateKey, Amount)],
        quorum_weight: Amount,
    ) -> ConsensusParams {
        let mut weights = RepWeights::default();
        for (key, weight) in rep_weights {
            weights.put(key.public_key(), *weight);
        }
        let mut params = ConsensusParams::default();
        params.set_rep_weights(weights, quorum_weight);
        params
    }

    // Helper function to add votes to state
    fn add_votes(state: &mut State, votes: &[(ProposalHash, &PrivateKey)]) {
        let snapshot_number = state.current_snapshot_number;
        for (hash, key) in votes {
            let vote = ProposalVote::new(*hash, key, snapshot_number);
            state.vote_aggregator.add(vote);
        }
    }

    #[test]
    fn no_winner_if_less_than_4f_plus_one_votes() {
        // Setup: online_weight = 100, quorum_weight = 67 (67% of 100)
        // 4f+1 threshold = 80% of 100 = 80
        // We'll add votes totaling 79 (less than 80)
        let _online_weight = Amount::nano(100_000_000);
        let quorum_weight = Amount::nano(67_000_000); // 67% of online_weight

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let rep_key3 = PrivateKey::from(3);
        let weight_per_rep = Amount::nano(26_333_333); // ~26.33 each, total ~79

        let rep_weights = vec![
            (rep_key1.clone(), weight_per_rep),
            (rep_key2.clone(), weight_per_rep),
            (rep_key3.clone(), weight_per_rep),
        ];
        let params = create_consensus_params(&rep_weights, quorum_weight);

        let mut state = State::default();
        let proposal_hash = ProposalHash::from(1);
        add_votes(
            &mut state,
            &[
                (proposal_hash, &rep_key1),
                (proposal_hash, &rep_key2),
                (proposal_hash, &rep_key3),
            ],
        );

        // Should return None because we don't have 4f+1 votes yet
        assert_eq!(state.find_winner_proposal(&params), None);
    }

    #[test]
    fn confirm_proposal_if_4f_plus_one_vote_for_same_hash() {
        // Setup: online_weight = 100, quorum_weight = 67
        // 4f+1 threshold = 80% of 100 = 80
        // We'll add votes totaling 80+ all for the same hash
        let _online_weight = Amount::nano(100_000_000);
        let quorum_weight = Amount::nano(67_000_000);

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let rep_key3 = PrivateKey::from(3);
        let rep_key4 = PrivateKey::from(4);
        // Each rep has 20, total = 80 (exactly 80%)
        let weight_per_rep = Amount::nano(20_000_000);

        let rep_weights = vec![
            (rep_key1.clone(), weight_per_rep),
            (rep_key2.clone(), weight_per_rep),
            (rep_key3.clone(), weight_per_rep),
            (rep_key4.clone(), weight_per_rep),
        ];
        let params = create_consensus_params(&rep_weights, quorum_weight);

        let mut state = State::default();
        let proposal_hash = ProposalHash::from(1);
        add_votes(
            &mut state,
            &[
                (proposal_hash, &rep_key1),
                (proposal_hash, &rep_key2),
                (proposal_hash, &rep_key3),
                (proposal_hash, &rep_key4),
            ],
        );

        // Should confirm the proposal because 4f+1 (80%) voted for it
        assert_eq!(state.find_winner_proposal(&params), Some(proposal_hash));
    }

    #[test]
    fn confirm_nil_if_votes_not_for_most_popular_ge_40_percent() {
        // Setup: online_weight = 100, quorum_weight = 67
        // 4f+1 threshold = 80
        // Scenario: Most popular has 50%, others have 30% (total 80%)
        // votes_not_for_most_popular = 30% >= 40%? No, but let's test with 40%+
        // Actually: if most popular = 50%, others = 30%, then others = 30% < 40%
        // Let's do: most popular = 45%, others = 35% (total 80%)
        // votes_not_for_most_popular = 35% < 40%, so this won't trigger nil
        // Let's do: most popular = 40%, others = 40% (total 80%)
        // votes_not_for_most_popular = 40% >= 40%, so this should trigger nil
        let _online_weight = Amount::nano(100_000_000);
        let quorum_weight = Amount::nano(67_000_000);

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let rep_key3 = PrivateKey::from(3);
        let rep_key4 = PrivateKey::from(4);
        // Each rep has 20, total = 80
        let weight_per_rep = Amount::nano(20_000_000);

        let rep_weights = vec![
            (rep_key1.clone(), weight_per_rep),
            (rep_key2.clone(), weight_per_rep),
            (rep_key3.clone(), weight_per_rep),
            (rep_key4.clone(), weight_per_rep),
        ];
        let params = create_consensus_params(&rep_weights, quorum_weight);

        let mut state = State::default();
        let proposal_hash1 = ProposalHash::from(1);
        let proposal_hash2 = ProposalHash::from(2);
        // 2 votes for hash1 (40%), 2 votes for hash2 (40%)
        add_votes(
            &mut state,
            &[
                (proposal_hash1, &rep_key1),
                (proposal_hash1, &rep_key2),
                (proposal_hash2, &rep_key3),
                (proposal_hash2, &rep_key4),
            ],
        );

        // Should confirm nil because votes_not_for_most_popular (40%) >= 40%
        assert_eq!(state.find_winner_proposal(&params), None);
    }

    #[test]
    fn confirm_proposal_if_most_popular_ge_41_percent_and_others_lt_40_percent() {
        // Setup: online_weight = 100, quorum_weight = 67
        // 4f+1 threshold = 80
        // Scenario: Most popular has 41%, others have 39% (total 80%)
        // votes_not_for_most_popular = 39% < 40%, so check if most popular >= 41%
        let _online_weight = Amount::nano(100_000_000);
        let quorum_weight = Amount::nano(67_000_000);

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let rep_key3 = PrivateKey::from(3);
        let rep_key4 = PrivateKey::from(4);
        // rep1 and rep2: 20.5 each = 41% total
        // rep3 and rep4: 19.5 each = 39% total
        let weight_high = Amount::nano(20_500_000);
        let weight_low = Amount::nano(19_500_000);

        let rep_weights = vec![
            (rep_key1.clone(), weight_high),
            (rep_key2.clone(), weight_high),
            (rep_key3.clone(), weight_low),
            (rep_key4.clone(), weight_low),
        ];
        let params = create_consensus_params(&rep_weights, quorum_weight);

        let mut state = State::default();
        let proposal_hash1 = ProposalHash::from(1);
        let proposal_hash2 = ProposalHash::from(2);
        // 2 votes for hash1 (41%), 2 votes for hash2 (39%)
        add_votes(
            &mut state,
            &[
                (proposal_hash1, &rep_key1),
                (proposal_hash1, &rep_key2),
                (proposal_hash2, &rep_key3),
                (proposal_hash2, &rep_key4),
            ],
        );

        // Should confirm hash1 because it has 41% and others have <40%
        assert_eq!(state.find_winner_proposal(&params), Some(proposal_hash1));
    }

    #[test]
    fn confirm_nil_if_most_popular_lt_41_percent_and_others_lt_40_percent() {
        // Setup: online_weight = 100, quorum_weight = 67
        // 4f+1 threshold = 80
        // Scenario: Most popular has 40%, others have 40% (total 80%)
        // votes_not_for_most_popular = 40% >= 40%, so this should trigger nil
        // Actually wait, let me re-read: if others >= 40%, confirm nil
        // So if most popular = 40%, others = 40%, then others = 40% >= 40%, confirm nil
        // But if most popular = 40.5%, others = 39.5%, then others = 39.5% < 40%
        // and most popular = 40.5% < 41%, so confirm nil
        let _online_weight = Amount::nano(100_000_000);
        let quorum_weight = Amount::nano(67_000_000);

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let rep_key3 = PrivateKey::from(3);
        let rep_key4 = PrivateKey::from(4);
        // rep1 and rep2: 20.25 each = 40.5% total
        // rep3 and rep4: 19.75 each = 39.5% total
        let weight_high = Amount::nano(20_250_000);
        let weight_low = Amount::nano(19_750_000);

        let rep_weights = vec![
            (rep_key1.clone(), weight_high),
            (rep_key2.clone(), weight_high),
            (rep_key3.clone(), weight_low),
            (rep_key4.clone(), weight_low),
        ];
        let params = create_consensus_params(&rep_weights, quorum_weight);

        let mut state = State::default();
        let proposal_hash1 = ProposalHash::from(1);
        let proposal_hash2 = ProposalHash::from(2);
        // 2 votes for hash1 (40.5%), 2 votes for hash2 (39.5%)
        add_votes(
            &mut state,
            &[
                (proposal_hash1, &rep_key1),
                (proposal_hash1, &rep_key2),
                (proposal_hash2, &rep_key3),
                (proposal_hash2, &rep_key4),
            ],
        );

        // Should confirm nil because most popular (40.5%) < 41% and others (39.5%) < 40%
        assert_eq!(state.find_winner_proposal(&params), None);
    }

    #[test]
    fn confirm_proposal_with_multiple_proposals_and_one_has_4f_plus_one() {
        // Setup: online_weight = 100, quorum_weight = 67
        // 4f+1 threshold = 80
        // Scenario: hash1 has 80% (4f+1), hash2 has 20%
        let _online_weight = Amount::nano(100_000_000);
        let quorum_weight = Amount::nano(67_000_000);

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let rep_key3 = PrivateKey::from(3);
        let rep_key4 = PrivateKey::from(4);
        let rep_key5 = PrivateKey::from(5);
        // reps 1-4: 20 each = 80% total
        // rep 5: 20 = 20% (but we only count up to 80% for 4f+1 threshold)
        let weight_per_rep = Amount::nano(20_000_000);

        let rep_weights = vec![
            (rep_key1.clone(), weight_per_rep),
            (rep_key2.clone(), weight_per_rep),
            (rep_key3.clone(), weight_per_rep),
            (rep_key4.clone(), weight_per_rep),
            (rep_key5.clone(), weight_per_rep),
        ];
        let params = create_consensus_params(&rep_weights, quorum_weight);

        let mut state = State::default();
        let proposal_hash1 = ProposalHash::from(1);
        let proposal_hash2 = ProposalHash::from(2);
        // 4 votes for hash1 (80%), 1 vote for hash2 (20%)
        add_votes(
            &mut state,
            &[
                (proposal_hash1, &rep_key1),
                (proposal_hash1, &rep_key2),
                (proposal_hash1, &rep_key3),
                (proposal_hash1, &rep_key4),
                (proposal_hash2, &rep_key5),
            ],
        );

        // Should confirm hash1 because it has 4f+1 (80%)
        assert_eq!(state.find_winner_proposal(&params), Some(proposal_hash1));
    }

    #[test]
    fn confirm_nil_with_three_way_split() {
        // Setup: online_weight = 100, quorum_weight = 67
        // 4f+1 threshold = 80
        // Scenario: Three proposals, each with roughly equal votes
        // hash1: 27%, hash2: 27%, hash3: 26% (total 80%)
        // votes_not_for_most_popular = 27% + 26% = 53% >= 40%, so confirm nil
        let _online_weight = Amount::nano(100_000_000);
        let quorum_weight = Amount::nano(67_000_000);

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let rep_key3 = PrivateKey::from(3);
        let rep_key4 = PrivateKey::from(4);
        let rep_key5 = PrivateKey::from(5);
        let rep_key6 = PrivateKey::from(6);
        // Each rep has ~13.33, so 2 reps = ~26.67%
        let weight_per_rep = Amount::nano(13_333_333);

        let rep_weights = vec![
            (rep_key1.clone(), weight_per_rep),
            (rep_key2.clone(), weight_per_rep),
            (rep_key3.clone(), weight_per_rep),
            (rep_key4.clone(), weight_per_rep),
            (rep_key5.clone(), weight_per_rep),
            (rep_key6.clone(), weight_per_rep),
        ];
        let params = create_consensus_params(&rep_weights, quorum_weight);

        let mut state = State::default();
        let proposal_hash1 = ProposalHash::from(1);
        let proposal_hash2 = ProposalHash::from(2);
        let proposal_hash3 = ProposalHash::from(3);
        // 2 votes for hash1, 2 votes for hash2, 2 votes for hash3
        add_votes(
            &mut state,
            &[
                (proposal_hash1, &rep_key1),
                (proposal_hash1, &rep_key2),
                (proposal_hash2, &rep_key3),
                (proposal_hash2, &rep_key4),
                (proposal_hash3, &rep_key5),
                (proposal_hash3, &rep_key6),
            ],
        );

        // Should confirm nil because votes_not_for_most_popular >= 40%
        assert_eq!(state.find_winner_proposal(&params), None);
    }

    #[test]
    fn edge_case_exactly_40_percent_for_others() {
        // Setup: online_weight = 100, quorum_weight = 67
        // 4f+1 threshold = 80
        // Scenario: Most popular has 40%, others have exactly 40%
        // votes_not_for_most_popular = 40% >= 40%, so confirm nil
        let _online_weight = Amount::nano(100_000_000);
        let quorum_weight = Amount::nano(67_000_000);

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let rep_key3 = PrivateKey::from(3);
        let rep_key4 = PrivateKey::from(4);
        let weight_per_rep = Amount::nano(20_000_000); // 20 each

        let rep_weights = vec![
            (rep_key1.clone(), weight_per_rep),
            (rep_key2.clone(), weight_per_rep),
            (rep_key3.clone(), weight_per_rep),
            (rep_key4.clone(), weight_per_rep),
        ];
        let params = create_consensus_params(&rep_weights, quorum_weight);

        let mut state = State::default();
        let proposal_hash1 = ProposalHash::from(1);
        let proposal_hash2 = ProposalHash::from(2);
        // 2 votes for hash1 (40%), 2 votes for hash2 (40%)
        add_votes(
            &mut state,
            &[
                (proposal_hash1, &rep_key1),
                (proposal_hash1, &rep_key2),
                (proposal_hash2, &rep_key3),
                (proposal_hash2, &rep_key4),
            ],
        );

        // Should confirm nil because votes_not_for_most_popular (40%) >= 40%
        assert_eq!(state.find_winner_proposal(&params), None);
    }

    #[test]
    fn edge_case_exactly_41_percent_for_most_popular() {
        // Setup: online_weight = 100, quorum_weight = 67
        // 4f+1 threshold = 80
        // Scenario: Most popular has exactly 41%, others have 39%
        // votes_not_for_most_popular = 39% < 40%, and most popular = 41% >= 41%, so confirm it
        let _online_weight = Amount::nano(100_000_000);
        let quorum_weight = Amount::nano(67_000_000);

        let rep_key1 = PrivateKey::from(1);
        let rep_key2 = PrivateKey::from(2);
        let rep_key3 = PrivateKey::from(3);
        let rep_key4 = PrivateKey::from(4);
        // rep1 and rep2: 20.5 each = 41% total
        // rep3 and rep4: 19.5 each = 39% total
        let weight_high = Amount::nano(20_500_000);
        let weight_low = Amount::nano(19_500_000);

        let rep_weights = vec![
            (rep_key1.clone(), weight_high),
            (rep_key2.clone(), weight_high),
            (rep_key3.clone(), weight_low),
            (rep_key4.clone(), weight_low),
        ];
        let params = create_consensus_params(&rep_weights, quorum_weight);

        let mut state = State::default();
        let proposal_hash1 = ProposalHash::from(1);
        let proposal_hash2 = ProposalHash::from(2);
        // 2 votes for hash1 (41%), 2 votes for hash2 (39%)
        add_votes(
            &mut state,
            &[
                (proposal_hash1, &rep_key1),
                (proposal_hash1, &rep_key2),
                (proposal_hash2, &rep_key3),
                (proposal_hash2, &rep_key4),
            ],
        );

        // Should confirm hash1 because it has exactly 41% and others have <40%
        assert_eq!(state.find_winner_proposal(&params), Some(proposal_hash1));
    }
}
