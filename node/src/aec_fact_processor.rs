use std::sync::{Arc, Mutex, mpsc::SyncSender};

use tracing::debug;

use rsnano_ledger::{BlockSource, Ledger, LedgerSet, RollbackError};
use rsnano_messages::NetworkFilter;
use rsnano_network::ChannelId;
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Block, BlockHash, ConsensusEpoch, VoteDelivery};
use rsnano_utils::{
    EventHandlerMut, EventHandlerRegistry,
    stats::{Sample, Stats},
};

use crate::{
    NodeEvent,
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::Bootstrapper,
    cementation::ConfirmingSet,
    consensus::{
        AecCooldownReason, AecFact, AecForkInserter, AecService, BootstrapElectionActivator,
        LocalVotesRemover, VoteProcessor, VoteRebroadcastQueue, WinnerBlockBroadcaster,
        aggregate_vote_results, election_schedulers::ElectionSchedulers, vote_cache::VoteCache,
    },
    recently_cemented_inserter::RecentlyCementedInserter,
    utils::BackpressureEventProcessor,
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

    fn process(&mut self, event: AecFact) {
        self.plugins.handle(&event);
        match event {
            AecFact::ElectionStarted(hash, root) => {
                self.aec_fork_inserter.try_add_cached_forks(&root);
                self.bootstrap_election_activator.election_started(hash);
                if let Some(tx) = &self.node_observer {
                    tx.send(NodeEvent::ElectionStarted(hash)).unwrap();
                }
            }
            AecFact::ElectionConfirmed(election) => {
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
            AecFact::ElectionTerminated(_) => self.election_schedulers.notify(),
            // RAI: the blocks held back while the epoch drained start now
            AecFact::EpochAdvanced(_) => self.election_schedulers.notify(),
            AecFact::LateBlocksDiscarded { epoch, hashes } => {
                self.discard_late_blocks(epoch, hashes)
            }
            AecFact::CheckpointFinalized { epoch, hashes } => {
                self.install_checkpoint_blocks(epoch, hashes)
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

    /// RAI: the blocks a decided checkpoint finalized are cemented. One the
    /// ledger already cemented is left alone; one it holds unconfirmed is
    /// handed to the confirming set, which cements it with its ancestors;
    /// one it does not hold at all is reported, and the finality stands in
    /// the decided state until the block arrives.
    fn install_checkpoint_blocks(&mut self, epoch: ConsensusEpoch, hashes: Vec<BlockHash>) {
        let mut cemented = 0;
        let mut queued = 0;
        let mut missing = 0;
        {
            let any = self.ledger.any();
            let confirmed = self.ledger.confirmed();
            for hash in &hashes {
                if confirmed.block_exists(hash) {
                    cemented += 1;
                } else if any.block_exists(hash) {
                    self.confirming_set.add_block(*hash);
                    queued += 1;
                } else {
                    missing += 1;
                }
            }
        }
        crate::utils::diagnostic!(
            "EPOCH_INSTALLED epoch={} finalized={} cemented={} queued={} missing={}",
            epoch,
            hashes.len(),
            cemented,
            queued,
            missing
        );
    }

    fn clear_network_filter(&mut self, block: &Block) {
        let mut buffer = Vec::new();
        block
            .serialize_without_block_type(&mut buffer)
            .expect("Should serialize block successfully");
        self.network_filter.clear_bytes(&buffer);
    }
}
