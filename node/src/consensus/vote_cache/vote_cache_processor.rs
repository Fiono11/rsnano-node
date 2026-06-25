use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Condvar, Mutex, MutexGuard, atomic::Ordering},
    thread::JoinHandle,
};

use rsnano_types::{BlockHash, Vote, VoteDelivery};

use super::{stats::VoteCacheStats, voted_block_map::VotedBlockMap};
use crate::consensus::VoteProcessorQueue;

pub(crate) struct VoteCacheProcessor {
    state: Arc<Mutex<State>>,
    condition: Arc<Condvar>,
    stats: Arc<VoteCacheStats>,
    cache: Arc<Mutex<VotedBlockMap>>,
    vote_queue: Arc<VoteProcessorQueue>,
    max_triggered: usize,
}

impl VoteCacheProcessor {
    pub(crate) fn new(
        cache: Arc<Mutex<VotedBlockMap>>,
        vote_queue: Arc<VoteProcessorQueue>,
        stats: Arc<VoteCacheStats>,
        max_triggered: usize,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                thread: None,
                stopped: false,
                triggered: VecDeque::new(),
            })),
            condition: Arc::new(Condvar::new()),
            stats,
            vote_queue,
            cache,
            max_triggered,
        }
    }
}

impl VoteCacheProcessor {
    pub fn start(&self) {
        debug_assert!(self.state.lock().unwrap().thread.is_none());
        let cache_loop = VoteCacheLoop {
            state: self.state.clone(),
            condition: self.condition.clone(),
            stats: self.stats.clone(),
            cache: self.cache.clone(),
            vote_queue: self.vote_queue.clone(),
        };

        self.state.lock().unwrap().thread = Some(
            std::thread::Builder::new()
                .name("Vote cache proc".to_owned())
                .spawn(move || cache_loop.run())
                .unwrap(),
        );
    }

    pub fn stop(&self) {
        let thread = {
            let mut state = self.state.lock().unwrap();
            state.stopped = true;
            state.thread.take()
        };

        self.condition.notify_all();

        if let Some(handle) = thread {
            handle.join().unwrap();
        }
    }

    pub fn trigger(&self, hash: BlockHash) {
        {
            let mut state = self.state.lock().unwrap();
            if state.triggered.len() > self.max_triggered {
                state.triggered.pop_front();
                self.stats
                    .processor_overfill
                    .fetch_add(1, Ordering::Relaxed);
            }
            state.triggered.push_back(hash);
        }
        self.condition.notify_all();
        self.stats.triggered.fetch_add(1, Ordering::Relaxed);
    }

    pub fn len(&self) -> usize {
        self.state.lock().unwrap().triggered.len()
    }
}

impl Drop for VoteCacheProcessor {
    fn drop(&mut self) {
        self.stop();
    }
}

struct State {
    thread: Option<JoinHandle<()>>,
    stopped: bool,
    triggered: VecDeque<BlockHash>,
}

struct VoteCacheLoop {
    state: Arc<Mutex<State>>,
    condition: Arc<Condvar>,
    stats: Arc<VoteCacheStats>,
    cache: Arc<Mutex<VotedBlockMap>>,
    vote_queue: Arc<VoteProcessorQueue>,
}

impl VoteCacheLoop {
    fn run(&self) {
        let mut vote_buffer: Vec<Arc<Vote>> = Vec::new();
        let mut guard = self.state.lock().unwrap();
        while !guard.stopped {
            if !guard.triggered.is_empty() {
                self.run_batch(guard, &mut vote_buffer);
                guard = self.state.lock().unwrap();
            } else {
                guard = self
                    .condition
                    .wait_while(guard, |i| !i.stopped && i.triggered.is_empty())
                    .unwrap();
            }
        }
    }

    fn run_batch(&self, mut state: MutexGuard<'_, State>, vote_buffer: &mut Vec<Arc<Vote>>) {
        let mut triggered = VecDeque::new();
        std::mem::swap(&mut triggered, &mut state.triggered);
        drop(state);

        //deduplicate
        let hashes: HashSet<BlockHash> = triggered.drain(..).collect();

        self.stats
            .processed
            .fetch_add(hashes.len() as u64, Ordering::Relaxed);

        for hash in hashes {
            self.cache.lock().unwrap().collect_votes(vote_buffer, &hash);

            for vote in vote_buffer.drain(..) {
                self.vote_queue
                    .enqueue(vote, None, VoteDelivery::Replayed, Some(hash));
            }
        }
    }
}
