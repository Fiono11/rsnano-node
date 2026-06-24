use std::sync::Arc;
#[cfg(not(test))]
use std::time::Instant;

#[cfg(test)]
use mock_instant::thread_local::Instant;

use rsnano_types::{Amount, BlockHash, Vote};

use super::cached_vote_map::{CachedVote, CachedVoteMap};

/// Stores votes associated with a single block hash
#[derive(Clone)]
pub(crate) struct VotedBlock {
    pub id: usize,
    pub hash: BlockHash,
    pub votes: CachedVoteMap,
    pub last_vote: Instant,
    pub tally: Amount,
    pub final_tally: Amount,
}

impl VotedBlock {
    pub fn new(id: usize, hash: BlockHash, max_voters: usize) -> Self {
        VotedBlock {
            id,
            hash,
            votes: CachedVoteMap::new(max_voters),
            last_vote: Instant::now(),
            tally: Amount::ZERO,
            final_tally: Amount::ZERO,
        }
    }

    fn calculate_tally(&mut self) -> (Amount, Amount) {
        let mut tally = Amount::ZERO;
        let mut final_tally = Amount::ZERO;
        for voter in self.votes.iter() {
            tally = tally.wrapping_add(voter.weight);
            if voter.vote.is_final() {
                final_tally = final_tally.wrapping_add(voter.weight);
            }
        }
        (tally, final_tally)
    }

    pub fn tally(&self) -> Amount {
        self.tally
    }

    pub fn final_tally(&self) -> Amount {
        self.final_tally
    }

    pub fn votes(&self) -> Vec<Arc<Vote>> {
        self.votes.iter().map(|i| Arc::clone(&i.vote)).collect()
    }

    /// Adds a vote into a list, checks for duplicates and updates timestamp if new one is greater
    /// returns true if current tally changed, false otherwise
    pub fn vote(&mut self, vote: &Arc<Vote>, rep_weight: Amount) -> bool {
        let representative = vote.voter;
        if let Some(existing) = self.votes.find(&representative) {
            // We already have a vote from this rep
            // Update timestamp if newer but tally remains unchanged as we already counted this rep weight
            // It is not essential to keep tally up to date if rep voting weight changes, elections do tally calculations independently, so in the worst case scenario only our queue ordering will be a bit off
            if vote.timestamp() > existing.vote.timestamp() {
                let was_final = existing.vote.is_final();
                self.votes
                    .modify(&representative, Arc::clone(vote), rep_weight);
                return !was_final && vote.is_final(); // Tally changed only if the vote became final
            } else {
                return false;
            }
        }

        let inserted = self.votes.insert(CachedVote::new(vote.clone(), rep_weight));
        if inserted {
            (self.tally, self.final_tally) = self.calculate_tally();
            self.last_vote = Instant::now();
            true
        } else {
            false
        }
    }
}
