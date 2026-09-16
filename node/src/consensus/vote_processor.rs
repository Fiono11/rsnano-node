use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use tracing::debug;

use rsnano_messages::NetworkFilter;
use rsnano_network::Channel;
use rsnano_types::{BlockHash, Vote, VoteDelivery, VoteError};
use rsnano_utils::{
    stats::{DetailType, StatType, Stats},
    sync::backpressure_channel::Sender,
};
use rustc_hash::FxHashMap;

use super::{
    AecFact, FilteredVote, ReceivedVote, VoteApplier, VoteProcessorQueue,
    vote_processor_queue::QueuedVote,
};

#[derive(Clone, Debug, PartialEq)]
pub struct VoteProcessorConfig {
    pub max_pr_queue: usize,
    pub max_non_pr_queue: usize,
    pub pr_priority: usize,
    pub threads: usize,
    pub batch_size: usize,
    pub max_triggered: usize,
}

impl VoteProcessorConfig {
    pub fn new(parallelism: usize) -> Self {
        Self {
            // RAI votes arrive in bursts from every representative; a brief
            // stall of vote application must not drop them.
            max_pr_queue: if cfg!(feature = "rai_protocol") {
                4096
            } else {
                256
            },
            max_non_pr_queue: 32,
            pr_priority: 3,
            threads: (parallelism / 2).clamp(1, 4),
            batch_size: 1024,
            max_triggered: 16384,
        }
    }
}

pub type VoteProcessedCallback2 =
    Box<dyn Fn(&Arc<Vote>, Option<&Arc<Channel>>, VoteDelivery, VoteError) + Send + Sync>;

pub struct VoteProcessor {
    threads: Mutex<Vec<JoinHandle<()>>>,
    queue: Arc<VoteProcessorQueue>,
    vote_applier: VoteApplier,
    stats: Arc<Stats>,
    network_filter: Arc<NetworkFilter>,
    pub total_processed: AtomicU64,
    cool_down: AtomicBool,
}

impl VoteProcessor {
    pub(crate) fn new(
        queue: Arc<VoteProcessorQueue>,
        vote_applier: VoteApplier,
        stats: Arc<Stats>,
        network_filter: Arc<NetworkFilter>,
    ) -> Self {
        Self {
            queue,
            vote_applier,
            stats,
            network_filter,
            threads: Mutex::new(Vec::new()),
            total_processed: AtomicU64::new(0),
            cool_down: AtomicBool::new(false),
        }
    }

    pub fn add_observer(&self, sink: Sender<AecFact>) {
        self.vote_applier.add_event_sink(sink);
    }

    pub fn cool_down(&self) {
        self.cool_down.store(true, Ordering::Relaxed);
    }

    pub fn recovered(&self) {
        self.cool_down.store(false, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.vote_applier.stop();
        self.queue.stop();

        let mut handles = Vec::new();
        {
            let mut guard = self.threads.lock().unwrap();
            std::mem::swap(&mut handles, &mut guard);
        }
        for handle in handles {
            handle.join().unwrap()
        }
    }

    pub fn run(&self) {
        loop {
            if self.cool_down.load(Ordering::Relaxed) {
                if self.queue.stopped() {
                    return;
                }

                std::thread::sleep(Duration::from_millis(25));
                continue;
            }

            self.stats.inc(StatType::VoteProcessor, DetailType::Loop);

            let batch = self.queue.wait_for_votes(self.queue.config.batch_size);
            if batch.is_empty() {
                break; //stopped
            }

            let start = Instant::now();

            for (_, queued) in &batch {
                self.process(queued);
            }

            self.total_processed
                .fetch_add(batch.len() as u64, Ordering::SeqCst);

            let elapsed_millis = start.elapsed().as_millis();
            if batch.len() == self.queue.config.batch_size && elapsed_millis > 100 {
                debug!(
                    "Processed {} votes in {} milliseconds (rate of {} votes per second)",
                    batch.len(),
                    elapsed_millis,
                    (batch.len() * 1000) / elapsed_millis as usize
                );
            }
        }
    }

    fn process(&self, queued: &QueuedVote) {
        let filter = queued.filter.unwrap_or_default();
        let received_vote = ReceivedVote::new(
            queued.vote.clone(),
            queued.source,
            queued.channel.as_ref().map(|c| c.channel_id()),
        );
        let filtered_vote = FilteredVote::new(received_vote, filter);
        let results = self.vote_results(&filtered_vote);
        // Opt-in evidence for drain stragglers: timeout votes that were
        // replayed from the cache or did not apply cleanly, with the result
        // for each hash. Every vote is far too much output under load.
        #[cfg(feature = "rai_protocol")]
        {
            static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *TRACE.get_or_init(|| std::env::var_os("RAI_VOTE_TRACE").is_some())
                && queued.vote.kind() == rsnano_types::VoteKind::Timeout
                && (queued.source == VoteDelivery::Replayed
                    || results
                        .values()
                        .any(|r| !matches!(r, Ok(()) | Err(VoteError::Replay))))
            {
                eprintln!(
                    "VOTE_TRACE {}",
                    serde_json::json!({
                        "pid":std::process::id(),"voter":queued.vote.voter,"epoch":queued.vote.epoch,
                        "kind":format!("{:?}",queued.vote.kind()),"source":format!("{:?}",queued.source),
                        "filter":queued.filter,
                        "hashes":queued.vote.hashes.iter().map(|h| (h, results.get(h).map(|r| format!("{:?}", r)))).collect::<Vec<_>>()
                    })
                );
            }
        }
        // RAI replays archived votes byte for byte when a peer asks for
        // evidence it lacks. A vote that found no election for one of its
        // hashes must stay acceptable, or the replay is discarded as a
        // duplicate until the filter entry ages out.
        #[cfg(feature = "rai_protocol")]
        if queued.digest != 0
            && self.vote_applier.is_principal(&queued.vote.voter)
            && results
                .values()
                .any(|r| matches!(r, Err(VoteError::Indeterminate)))
        {
            self.network_filter.clear(queued.digest);
        }
        #[cfg(not(feature = "rai_protocol"))]
        let _ = results;
    }

    pub fn vote_blocking(&self, vote: &FilteredVote) -> Result<(), VoteError> {
        aggregate_vote_results(&self.vote_results(vote))
    }

    fn vote_results(&self, vote: &FilteredVote) -> FxHashMap<BlockHash, Result<(), VoteError>> {
        if vote.validate().is_ok() {
            self.vote_applier.vote(vote)
        } else {
            FxHashMap::default()
        }
    }
}

impl Drop for VoteProcessor {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        debug_assert!(self.threads.lock().unwrap().is_empty());
    }
}

pub trait VoteProcessorExt {
    fn start(&self);
}

impl VoteProcessorExt for Arc<VoteProcessor> {
    fn start(&self) {
        let mut threads = self.threads.lock().unwrap();
        debug_assert!(threads.is_empty());
        for _ in 0..self.queue.config.threads {
            let self_l = Arc::clone(self);
            threads.push(
                std::thread::Builder::new()
                    .name("Vote processing".to_string())
                    .spawn(Box::new(move || {
                        self_l.run();
                    }))
                    .unwrap(),
            )
        }
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use crate::{
        consensus::{AecInsertRequest, AecService},
        representatives::RepresentativeTracker,
    };
    use rsnano_ledger::RepWeightCache;
    use rsnano_nullable_clock::SteadyClock;
    use rsnano_types::{Amount, BlockPriority, PrivateKey, SavedBlock, VoteKind};

    #[test]
    fn vote_without_election_leaves_the_duplicate_filter_for_replays() {
        let (processor, filter, aec) = processor();
        let unknown = SavedBlock::new_test_instance_with_key(1);
        let known = SavedBlock::new_test_instance_with_key(2);
        aec.insert(
            AecInsertRequest::new_priority(known.clone(), BlockPriority::new_test_instance()),
            SteadyClock::new_null().now(),
        )
        .unwrap();
        let vote = Arc::new(Vote::new_with_kind(
            &PrivateKey::from(1),
            vec![known.hash(), unknown.hash()],
            0,
            VoteKind::First,
        ));
        let (digest, _) = filter.apply(b"vote message");
        assert!(filter.check(digest));

        processor.process(&queued(vote, digest));

        assert!(
            !filter.check(digest),
            "a replay of the same message must reach the processor again"
        );
    }

    #[test]
    fn vote_applied_to_every_hash_stays_filtered() {
        let (processor, filter, aec) = processor();
        let block = SavedBlock::new_test_instance_with_key(1);
        aec.insert(
            AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
            SteadyClock::new_null().now(),
        )
        .unwrap();
        let vote = Arc::new(Vote::new_with_kind(
            &PrivateKey::from(1),
            vec![block.hash()],
            0,
            VoteKind::First,
        ));
        let (digest, _) = filter.apply(b"vote message");

        processor.process(&queued(vote, digest));

        assert!(filter.check(digest));
    }

    #[test]
    fn vote_of_a_non_principal_stays_filtered() {
        let (processor, filter, _) = processor();
        let vote = Arc::new(Vote::new_with_kind(
            &PrivateKey::from(2),
            vec![BlockHash::from(1)],
            0,
            VoteKind::First,
        ));
        let (digest, _) = filter.apply(b"vote message");

        processor.process(&queued(vote, digest));

        assert!(filter.check(digest));
    }

    /* Test helpers */

    fn processor() -> (VoteProcessor, Arc<NetworkFilter>, Arc<AecService>) {
        let rep_weights = Arc::new(RepWeightCache::default());
        rep_weights.put(PrivateKey::from(1).public_key(), Amount::nano(50_000_000));
        let aec = Arc::new(AecService::new_null());
        let rep_tracker = Arc::new(
            RepresentativeTracker::builder()
                .rep_weights(rep_weights.clone())
                .finish(),
        );
        let applier = VoteApplier::new(
            aec.clone(),
            rep_tracker,
            Arc::new(SteadyClock::new_null()),
            rep_weights,
        );
        let filter = Arc::new(NetworkFilter::new(1024));
        let processor = VoteProcessor::new(
            Arc::new(VoteProcessorQueue::new_null()),
            applier,
            Arc::new(Stats::default()),
            filter.clone(),
        );
        (processor, filter, aec)
    }

    fn queued(vote: Arc<Vote>, digest: u128) -> QueuedVote {
        QueuedVote {
            vote,
            source: VoteDelivery::Direct,
            channel: None,
            filter: None,
            digest,
        }
    }
}

// Aggregate results for individual hashes
pub fn aggregate_vote_results(
    results: &FxHashMap<BlockHash, Result<(), VoteError>>,
) -> Result<(), VoteError> {
    let mut ignored = false;
    let mut replay = false;
    let mut processed = false;
    let mut late = false;
    for res in results.values() {
        ignored |= matches!(res, Err(VoteError::Ignored));
        replay |= matches!(res, Err(VoteError::Replay));
        processed |= res.is_ok();
        late |= matches!(res, Err(VoteError::Late));
    }
    if ignored {
        Err(VoteError::Ignored)
    } else if replay {
        Err(VoteError::Replay)
    } else if processed {
        Ok(())
    } else if late {
        Err(VoteError::Late)
    } else {
        Err(VoteError::Indeterminate)
    }
}
