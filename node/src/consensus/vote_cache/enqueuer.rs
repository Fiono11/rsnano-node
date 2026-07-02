use std::sync::{Arc, Mutex, atomic::Ordering};

use rsnano_types::{BlockHash, Vote, VoteDelivery};

use super::{stats::VoteCacheStats, voted_block_map::VotedBlockMap};
use crate::consensus::VoteProcessorQueue;

/// Enqueues cached votes for the vote processor
pub(super) struct CachedVotesEnqueuer {
    cache: Arc<Mutex<VotedBlockMap>>,
    vote_queue: Arc<VoteProcessorQueue>,
    stats: Arc<VoteCacheStats>,
    vote_buffer: Vec<Arc<Vote>>,
}

impl CachedVotesEnqueuer {
    pub fn new(
        cache: Arc<Mutex<VotedBlockMap>>,
        vote_queue: Arc<VoteProcessorQueue>,
        stats: Arc<VoteCacheStats>,
    ) -> Self {
        Self {
            cache,
            vote_queue,
            stats,
            vote_buffer: Vec::new(),
        }
    }

    pub fn enqueue<'a>(&mut self, block_hashes: impl IntoIterator<Item = &'a BlockHash>) {
        let mut hash_count = 0;
        for hash in block_hashes {
            hash_count += 1;
            self.cache
                .lock()
                .unwrap()
                .collect_votes(&mut self.vote_buffer, &hash);

            for vote in self.vote_buffer.drain(..) {
                self.vote_queue
                    .enqueue(vote, None, VoteDelivery::Replayed, Some(*hash));
            }
        }
        self.stats
            .processed
            .fetch_add(hash_count, Ordering::Relaxed);
    }
}
