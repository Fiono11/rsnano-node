use std::{
    sync::{Arc, Mutex, mpsc::SyncSender},
    time::{Duration, Instant, SystemTime},
};

use tracing::debug;

use rsnano_ledger::{AnySet, BlockSource, Ledger, LedgerSet, RollbackError};
use rsnano_messages::NetworkFilter;
use rsnano_network::ChannelId;
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Account, Block, BlockHash, ConsensusEpoch, SavedBlock, VoteDelivery};
use rsnano_utils::{
    EventHandlerRegistry,
    stats::{Sample, Stats},
    thread_pool::ThreadPool,
};

use crate::{
    NodeEvent,
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::Bootstrapper,
    cementation::ConfirmingSet,
    checkpoint_installer::CheckpointInstaller,
    consensus::{
        AecCooldownReason, AecFact, AecForkInserter, AecService, BootstrapElectionActivator,
        LocalVotesRemover, VoteProcessor, VoteRebroadcastQueue, WinnerBlockBroadcaster,
        aggregate_vote_results,
        election::{ConfirmedElection, ElectionId},
        election_schedulers::ElectionSchedulers,
        vote_cache::VoteCache,
    },
    recently_cemented_inserter::RecentlyCementedInserter,
    utils::{
        BackpressureEventProcessor, ConfirmationStages, FactTimings, StageRecord, cpu_times,
        diagnostic, unix_ms,
    },
};

/// Processes facts from the active election container (AEC)
pub(crate) struct AecFactProcessor {
    pub(crate) vote_processor: Arc<VoteProcessor>,
    pub(crate) node_observer: Option<SyncSender<NodeEvent>>,
    pub(crate) election_schedulers: Arc<ElectionSchedulers>,
    pub(crate) network_filter: Arc<NetworkFilter>,
    pub(crate) bootstrap_election_activator: BootstrapElectionActivator,
    pub(crate) recently_cemented_inserter: RecentlyCementedInserter,
    pub(crate) vote_rebroadcast_queue: Arc<VoteRebroadcastQueue>,
    pub(crate) block_processor_queue: Arc<BlockProcessorQueue>,
    pub(crate) confirming_set: Arc<ConfirmingSet>,
    pub(crate) active_elections: Arc<AecService>,
    pub(crate) clock: Arc<SteadyClock>,
    pub(crate) local_votes_remover: LocalVotesRemover,
    pub(crate) stats: Arc<Stats>,
    pub(crate) aec_fork_inserter: Arc<AecForkInserter>,
    pub(crate) winner_block_broadcaster: Arc<Mutex<WinnerBlockBroadcaster>>,
    pub(crate) bootstrapper: Arc<Bootstrapper>,
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) vote_cache: Arc<VoteCache>,
    pub(crate) plugins: EventHandlerRegistry<AecFact>,
    /// RAI: follows decided checkpoints in the ledger, on its own worker
    pub(crate) checkpoint_installer: Arc<CheckpointInstaller>,
    /// One thread: the checkpoints are installed in the order decided
    pub(crate) checkpoint_worker: Arc<ThreadPool>,
    pub(crate) events_since_cement_check: usize,
    /// Diagnostic: per-second stages of the blocks cemented
    pub(crate) confirmation_stages: ConfirmationStages,
    /// Diagnostic: per-second time spent on each kind of fact
    pub(crate) fact_timings: FactTimings,
    /// Diagnostic: thread and process CPU time at the last fact summary
    pub(crate) last_cpu_times: Option<(Duration, Duration)>,
    /// Elections started whose activation is batched
    pub(crate) started_elections: Vec<BlockHash>,
}

impl BackpressureEventProcessor<AecFact> for AecFactProcessor {
    fn cool_down(&mut self) {
        self.active_elections
            .set_cooldown(true, AecCooldownReason::AecFactQueueFull);
        self.vote_processor.cool_down();
    }

    fn recovered(&mut self) {
        self.active_elections
            .set_cooldown(false, AecCooldownReason::AecFactQueueFull);
        self.vote_processor.recovered();
    }

    fn idle(&mut self) {
        self.activate_started_elections();
    }

    fn process(&mut self, event: AecFact) {
        let kind = event.kind();
        let started = Instant::now();
        let mut per_event: Vec<(&'static str, Duration)> = Vec::with_capacity(8);
        let mut mark = started;
        self.plugins.handle_each(&event, |name| {
            let now = Instant::now();
            per_event.push((short_type_name(name), now - mark));
            mark = now;
        });
        let plugins = started.elapsed();
        self.cement_awaited_checkpoint_blocks();
        let awaited = started.elapsed() - plugins;
        if !matches!(event, AecFact::ElectionStarted(..)) {
            self.activate_started_elections();
        }
        let activation = started.elapsed() - plugins - awaited;
        self.handle_fact(event);
        if cfg!(feature = "rai_protocol") {
            let handling = started.elapsed() - plugins - awaited - activation;
            per_event.push(("awaited_cement", awaited));
            per_event.push(("activation", activation));
            if let Some(line) =
                self.fact_timings
                    .record(unix_ms() as u64, kind, handling, &per_event)
            {
                // CPU spent since the last summary, by this thread and by
                // the whole node
                let cpu = cpu_times();
                let (thread_ms, process_ms) = match (cpu, self.last_cpu_times) {
                    (Some((thread, process)), Some((last_thread, last_process))) => (
                        thread.saturating_sub(last_thread).as_millis(),
                        process.saturating_sub(last_process).as_millis(),
                    ),
                    _ => (0, 0),
                };
                self.last_cpu_times = cpu;
                diagnostic!(
                    "{} thread_cpu_ms={} process_cpu_ms={}",
                    line,
                    thread_ms,
                    process_ms
                );
            }
        }
    }
}

impl AecFactProcessor {
    const ACTIVATION_BATCH: usize = 64;

    /// The elections started since the last call skip their passive phase,
    /// under one AEC lock. Called for a full batch, before any other kind of
    /// fact and when the queue is drained, so an activation waits at most
    /// for a run of consecutive starts.
    fn activate_started_elections(&mut self) {
        if self.started_elections.is_empty() {
            return;
        }
        self.bootstrap_election_activator
            .elections_started(&self.started_elections);
        self.started_elections.clear();
    }

    fn handle_fact(&mut self, event: AecFact) {
        match event {
            AecFact::ElectionStarted(hash, root) => {
                self.aec_fork_inserter.try_add_cached_forks(&root);
                self.started_elections.push(hash);
                if self.started_elections.len() >= Self::ACTIVATION_BATCH {
                    self.activate_started_elections();
                }
                if let Some(tx) = &self.node_observer {
                    tx.send(NodeEvent::ElectionStarted(hash)).unwrap();
                }
            }
            AecFact::ElectionConfirmed(mut election) => {
                election.handed_to_cementing = Some(SystemTime::now());
                self.confirming_set.add(election.clone());
                // We don't rebroadcast winners during bootstrap, because it would just
                // spam the network with blocks that the other nodes already have
                if !self.bootstrapper.is_bootstrapping() {
                    // Ensure election winner is broadcasted
                    self.winner_block_broadcaster
                        .lock()
                        .unwrap()
                        .try_broadcast_winner(&election.winner, &election.votes);
                }
            }
            AecFact::ElectionTerminated(id) => {
                // RAI: the notarized block is a complete parent now; its
                // child is started without waiting for it to be cemented
                if cfg!(feature = "rai_protocol") {
                    if let Some(account) = self.account_of_instance(&id) {
                        self.election_schedulers
                            .activate_after_notarization(account);
                    }
                }
                self.election_schedulers.notify()
            }
            // RAI: the blocks held back while the epoch drained start now
            AecFact::EpochAdvanced(_, _) => self.election_schedulers.notify(),
            AecFact::LateBlocksDiscarded { epoch, hashes } => {
                self.discard_late_blocks(epoch, hashes)
            }
            // Off this thread: checking every finalized block against the
            // ledger held up the cementations queued behind it
            AecFact::CheckpointFinalized { epoch, hashes } => {
                let installer = self.checkpoint_installer.clone();
                self.checkpoint_worker
                    .execute(move || installer.install(epoch, hashes));
            }
            AecFact::CheckpointRetained { epoch, retained } => {
                let installer = self.checkpoint_installer.clone();
                self.checkpoint_worker
                    .execute(move || installer.follow_retained_branches(epoch, retained));
            }
            AecFact::ElectionEnded(election) => {
                self.election_schedulers.notify();

                let now = self.clock.now();
                let elapsed = election.start().elapsed(now);
                // Track election duration
                self.stats.sample(
                    Sample::ActiveElectionDuration,
                    elapsed.as_millis() as i64,
                    (0, 1000 * 60 * 10),
                ); // 0-10 minutes range

                for (hash, block) in election.candidate_blocks() {
                    // Notify observers about dropped elections & blocks lost confirmed elections
                    if (!election.is_confirmed() || *hash != election.winner().hash())
                        && let Some(tx) = &self.node_observer
                    {
                        tx.send(NodeEvent::ElectionStopped(*hash)).unwrap();
                    }

                    if !election.is_confirmed() {
                        self.clear_network_filter(block);
                    }
                }
            }
            AecFact::BlockAddedToElection(_) => {}
            AecFact::BlockDiscarded(block) => {
                self.clear_network_filter(&block);
            }
            AecFact::WinnerChanged(previous_winner, new_winner) => {
                debug!(from = ?previous_winner, to = ?new_winner.hash(), "Winning fork changed");
                // Kudzu: our statements are immutable and stay in the election;
                // legacy withdraws its votes for the previous winner to vote again
                if !cfg!(feature = "rai_protocol") {
                    self.local_votes_remover
                        .remove_local_votes(&previous_winner, &new_winner.qualified_root());
                }

                // Roll back the previous winner and add the new winner to the ledger
                self.block_processor_queue.push(BlockContext::new(
                    new_winner.clone(),
                    BlockSource::Forced,
                    ChannelId::LOOPBACK,
                ));
            }
            AecFact::VoteProcessed(vote, _weight, results) => {
                // Certificate evidence was requested by this node, it is not gossip
                if vote.delivery != VoteDelivery::Evidence {
                    self.vote_rebroadcast_queue
                        .try_enqueue(&vote.vote, &results);
                }

                let result = aggregate_vote_results(&results);

                if let Some(tx) = &self.node_observer {
                    tx.send(NodeEvent::VoteProcessed(vote.vote, result))
                        .unwrap();
                }
            }
            AecFact::BlockConfirmed(block, election) => {
                if cfg!(feature = "rai_protocol") {
                    self.record_confirmation_stages(&block, &election);
                }
                if let Some(tx) = &self.node_observer {
                    tx.send(NodeEvent::BlockConfirmed(block, election.clone()))
                        .unwrap();
                }
                self.recently_cemented_inserter.insert(election);
            }
            AecFact::Recovered => self.election_schedulers.notify(),
        }
    }
}

impl AecFactProcessor {
    /// Diagnostic: once a second, the stages of the blocks cemented in the
    /// second before, with the queue depths at that moment
    fn record_confirmation_stages(&mut self, block: &SavedBlock, election: &ConfirmedElection) {
        let now_ms = unix_ms() as u64;
        let record = StageRecord::new(&block.hash(), block.timestamp(), election, now_ms);
        if let Some(line) = self.confirmation_stages.record(now_ms, record) {
            diagnostic!(
                "{} block_queue={} vote_queue={} cement_queue={} aec={}",
                line,
                self.block_processor_queue.total_queue_len(),
                self.vote_processor.queue_len(),
                self.confirming_set.len(),
                self.active_elections.len()
            );
        }
    }

    /// The account whose position an instance decides: its parent's, or
    /// the root's for an open block
    fn account_of_instance(&self, id: &ElectionId) -> Option<Account> {
        if id.root.previous.is_zero() {
            return Some(Account::from(id.root.root));
        }
        self.ledger
            .any()
            .get_block(&id.root.previous)
            .map(|parent| parent.account())
    }

    const ROLLBACK_BATCH: usize = 16;

    /// RAI: blocks notarized in a closed epoch after its certificate was
    /// seen are not in the value finalized: rolled back from the ledger,
    /// together with what was built on them
    fn discard_late_blocks(&mut self, epoch: ConsensusEpoch, hashes: Vec<BlockHash>) {
        // What is cemented is finalized: never rolled back, whatever a late
        // instance of an earlier epoch notarized
        let confirmed = self.ledger.confirmed();
        let (cemented, hashes): (Vec<BlockHash>, Vec<BlockHash>) = hashes
            .into_iter()
            .partition(|hash| confirmed.block_exists(hash));
        drop(confirmed);
        // The backlog scan re-queues an unconfirmed block without an election
        // for a new election: taken out before it is proposed again
        for hash in &hashes {
            self.vote_cache.remove(hash);
            self.election_schedulers.remove(hash);
        }
        // In small transactions: the block processor keeps its turns in between
        let mut rolled_back = 0;
        // The other candidate of a fork was never in this ledger
        let mut not_held = 0;
        let mut failed: Vec<String> = Vec::new();
        for chunk in hashes.chunks(Self::ROLLBACK_BATCH) {
            // Unchecked: the votes for the block still arriving fill the vote
            // cache again in between, and the ledger would refuse
            let results = self.ledger.roll_back_batch_unchecked(chunk, usize::MAX);
            for result in results.iter() {
                rolled_back += result.rolled_back.len();
                match &result.error {
                    None => {}
                    Some(RollbackError::BlockNotFound) => not_held += 1,
                    Some(error) => failed.push(format!("{:?}", error)),
                }
            }
        }
        // The hashes are listed: the run's safety check reads them to prove
        // that nothing discarded was ever finalized
        crate::utils::diagnostic!(
            "EPOCH_DISCARDED epoch={} candidates={} rolled_back={} not_held={} cemented_kept={} failed={} {:?} hashes={:?}",
            epoch,
            hashes.len(),
            rolled_back,
            not_held,
            cemented.len(),
            failed.len(),
            failed.iter().take(3).collect::<Vec<_>>(),
            hashes.iter().map(|h| h.to_string()).collect::<Vec<_>>()
        );
    }

    /// RAI: cement the checkpoint-finalized blocks that arrived since they
    /// were found missing; checked every few hundred events, not each one
    fn cement_awaited_checkpoint_blocks(&mut self) {
        if !self.checkpoint_installer.is_awaiting() {
            return;
        }
        self.events_since_cement_check += 1;
        if self.events_since_cement_check < 32 {
            return;
        }
        self.events_since_cement_check = 0;
        self.checkpoint_installer.cement_arrived();
    }

    fn clear_network_filter(&mut self, block: &Block) {
        let mut buffer = Vec::new();
        block
            .serialize_without_block_type(&mut buffer)
            .expect("Should serialize block successfully");
        self.network_filter.clear_bytes(&buffer);
    }
}

/// Diagnostic: `rsnano_node::consensus::VoteCache` or
/// `alloc::sync::Arc<rsnano_node::consensus::VoteCache>` as `VoteCache`
fn short_type_name(name: &'static str) -> &'static str {
    let name = name.trim_end_matches('>');
    name.rsplit("::").next().unwrap_or(name)
}
