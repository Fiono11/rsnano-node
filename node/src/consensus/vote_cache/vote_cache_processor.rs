use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, atomic::Ordering},
};

#[cfg(test)]
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::BlockHash;
use rsnano_utils::thread_factory::{JoinHandle, ThreadFactory};

use super::{enqueuer::CachedVotesEnqueuer, stats::VoteCacheStats, voted_block_map::VotedBlockMap};
use crate::consensus::VoteProcessorQueue;

pub(crate) struct VoteCacheProcessor {
    state: Arc<Mutex<State>>,
    condition: Arc<Condvar>,
    stats: Arc<VoteCacheStats>,
    cache: Arc<Mutex<VotedBlockMap>>,
    vote_queue: Arc<VoteProcessorQueue>,
    max_triggered: usize,
    thread_factory: ThreadFactory,
    #[cfg(test)]
    trigger_listener: OutputListenerMt<BlockHash>,
}

impl VoteCacheProcessor {
    pub(crate) fn new(
        cache: Arc<Mutex<VotedBlockMap>>,
        vote_queue: Arc<VoteProcessorQueue>,
        stats: Arc<VoteCacheStats>,
        max_triggered: usize,
        thread_factory: ThreadFactory,
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
            thread_factory,
            #[cfg(test)]
            trigger_listener: OutputListenerMt::new(),
        }
    }

    pub fn start(&self) {
        debug_assert!(self.state.lock().unwrap().thread.is_none());
        let cache_loop = VoteCacheLoop {
            state: self.state.clone(),
            condition: self.condition.clone(),
            vote_enqueuer: CachedVotesEnqueuer::new(
                self.cache.clone(),
                self.vote_queue.clone(),
                self.stats.clone(),
            ),
        };

        self.state.lock().unwrap().thread = Some(
            self.thread_factory
                .spawn("Vote cache proc", move || cache_loop.run()),
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

    #[cfg(test)]
    pub fn track_trigger(&self) -> Arc<OutputTrackerMt<BlockHash>> {
        self.trigger_listener.track()
    }

    pub fn trigger(&self, block_hash: BlockHash) {
        #[cfg(test)]
        {
            self.trigger_listener.emit(block_hash);
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.triggered.len() > self.max_triggered {
                state.triggered.pop_front();
                self.stats
                    .processor_overfill
                    .fetch_add(1, Ordering::Relaxed);
            }
            state.triggered.push_back(block_hash);
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
    thread: Option<JoinHandle>,
    stopped: bool,
    triggered: VecDeque<BlockHash>,
}

struct VoteCacheLoop {
    state: Arc<Mutex<State>>,
    condition: Arc<Condvar>,
    vote_enqueuer: CachedVotesEnqueuer,
}

impl VoteCacheLoop {
    fn run(mut self) {
        let mut guard = self.state.lock().unwrap();
        while !guard.stopped {
            if !guard.triggered.is_empty() {
                let mut triggered = VecDeque::new();
                std::mem::swap(&mut triggered, &mut guard.triggered);
                drop(guard);
                self.vote_enqueuer.enqueue(&triggered);
                triggered.clear();
                guard = self.state.lock().unwrap();
            } else {
                guard = self
                    .condition
                    .wait_while(guard, |i| !i.stopped && i.triggered.is_empty())
                    .unwrap();
            }
        }
    }
}
