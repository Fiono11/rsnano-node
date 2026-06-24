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
    votes: CachedVoteMap,
    pub last_vote: Instant,
    tally: Amount,
    final_tally: Amount,
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
    pub fn vote(&mut self, vote: Arc<Vote>, rep_weight: Amount) -> bool {
        let inserted = self.votes.insert(CachedVote::new(vote, rep_weight));
        if inserted {
            (self.tally, self.final_tally) = self.votes.calculate_tally();
            self.last_vote = Instant::now();
            true
        } else {
            false
        }
    }
}
