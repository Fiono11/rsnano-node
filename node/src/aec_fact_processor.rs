use std::sync::{Arc, Mutex, mpsc::SyncSender};

use tracing::debug;

use rsnano_ledger::BlockSource;
use rsnano_messages::NetworkFilter;
use rsnano_network::ChannelId;
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::Block;
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
        aggregate_vote_results, election_schedulers::ElectionSchedulers,
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
                self.local_votes_remover
                    .remove_local_votes(&previous_winner, &new_winner.qualified_root());

                // Roll back the previous winner and add the new winner to the ledger
                self.block_processor_queue.push(BlockContext::new(
                    new_winner.clone(),
                    BlockSource::Forced,
                    ChannelId::LOOPBACK,
                ));
            }
            AecFact::VoteProcessed(vote, _weight, results) => {
                self.vote_rebroadcast_queue
                    .try_enqueue(&vote.vote, &results);

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
    fn clear_network_filter(&mut self, block: &Block) {
        let mut buffer = Vec::new();
        block
            .serialize_without_block_type(&mut buffer)
            .expect("Should serialize block successfully");
        self.network_filter.clear_bytes(&buffer);
    }
}
