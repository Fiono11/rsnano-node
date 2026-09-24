use std::sync::{Arc, Mutex, mpsc::SyncSender};

use tracing::debug;

use rsnano_ledger::{AnySet, BlockSource, Ledger, LedgerSet, RollbackError};
use rsnano_messages::NetworkFilter;
use rsnano_network::ChannelId;
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Account, Block, BlockHash, ConsensusEpoch, SavedBlock, VoteDelivery};
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
        aggregate_vote_results,
        election::{ConfirmedElection, ElectionId},
        election_schedulers::ElectionSchedulers,
        vote_cache::VoteCache,
    },
    recently_cemented_inserter::RecentlyCementedInserter,
    utils::{BackpressureEventProcessor, ConfirmationStages, StageRecord, diagnostic, unix_ms},
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
    /// RAI: blocks a decided checkpoint finalized that this ledger lacked;
    /// force-inserted and cemented once present
    pub(crate) awaiting_cement: std::collections::HashSet<BlockHash>,
    pub(crate) events_since_cement_check: usize,
    /// Diagnostic: per-second stages of the blocks cemented
    pub(crate) confirmation_stages: ConfirmationStages,
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
        self.cement_awaited_checkpoint_blocks();
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
            AecFact::CheckpointFinalized { epoch, hashes } => {
                self.install_checkpoint_blocks(epoch, hashes)
            }
            AecFact::CheckpointRetained { epoch, retained } => {
                self.follow_retained_branches(epoch, retained)
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
                "{} block_queue={} vote_queue={} cementing={} aec={}",
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

    /// RAI: the blocks a decided checkpoint finalized are cemented. One the
    /// ledger already cemented is left alone; one it holds unconfirmed is
    /// handed to the confirming set, which cements it with its ancestors;
    /// one it does not hold at all is reported, and the finality stands in
    /// the decided state until the block arrives.
    fn install_checkpoint_blocks(&mut self, epoch: ConsensusEpoch, hashes: Vec<BlockHash>) {
        let mut cemented = 0;
        let mut queued = 0;
        let mut missing = 0;
        let mut fetched = 0;
        let mut absent = Vec::new();
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
                    absent.push(*hash);
                }
            }
        }
        // A finalized block this ledger lacks - its rival held here instead,
        // or never received - is inserted in place of the rival, parents
        // first (the hashes come in position order), and cemented once present
        for hash in absent {
            #[cfg(feature = "rai_protocol")]
            let block = self
                .aec_fork_inserter
                .fork_cache
                .read()
                .unwrap()
                .block(&hash)
                .or_else(|| self.active_elections.report_block(&hash));
            #[cfg(not(feature = "rai_protocol"))]
            let block: Option<Block> = None;
            if let Some(block) = block {
                self.block_processor_queue.push(BlockContext::new(
                    block,
                    BlockSource::Forced,
                    ChannelId::LOOPBACK,
                ));
                fetched += 1;
            }
            self.awaiting_cement.insert(hash);
            self.active_elections.await_checkpoint_blocks([hash]);
        }
        if missing > 0 {
            crate::utils::diagnostic!(
                "EPOCH_INSTALL_FETCH epoch={} missing={} fetched={}",
                epoch,
                missing,
                fetched
            );
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

    /// RAI: "Recovery through a fresh child": the owner extends a retained
    /// tip, so the ledger must hold the retained branch. A node whose ledger
    /// holds an omitted rival at a retained position rolls it back and
    /// installs the retained block, from the fork cache or the retained
    /// report data; a retained block this node does not hold yet is
    /// installed when it arrives as evidence (see the message processor).
    /// Cemented blocks are never rolled back.
    fn follow_retained_branches(
        &mut self,
        epoch: ConsensusEpoch,
        retained: Vec<(rsnano_types::Account, u64, BlockHash)>,
    ) {
        let mut held = 0;
        let mut forced = 0;
        let mut missing = 0;
        for (_, _, hash) in &retained {
            if self.ledger.any().block_exists(hash) {
                held += 1;
                continue;
            }
            #[cfg(feature = "rai_protocol")]
            let block = self
                .aec_fork_inserter
                .fork_cache
                .read()
                .unwrap()
                .block(hash)
                .or_else(|| self.active_elections.report_block(hash));
            #[cfg(not(feature = "rai_protocol"))]
            let block: Option<Block> = None;
            match block {
                Some(block) => {
                    self.block_processor_queue.push(BlockContext::new(
                        block,
                        BlockSource::Forced,
                        ChannelId::LOOPBACK,
                    ));
                    forced += 1;
                }
                None => missing += 1,
            }
        }
        crate::utils::diagnostic!(
            "EPOCH_RETAINED epoch={} retained={} held={} forced={} missing={}",
            epoch,
            retained.len(),
            held,
            forced,
            missing
        );
    }

    /// RAI: cement the checkpoint-finalized blocks that arrived since they
    /// were found missing; checked every few hundred events, not each one
    fn cement_awaited_checkpoint_blocks(&mut self) {
        if self.awaiting_cement.is_empty() {
            return;
        }
        self.events_since_cement_check += 1;
        if self.events_since_cement_check < 32 {
            return;
        }
        self.events_since_cement_check = 0;
        let arrived: Vec<BlockHash> = {
            let any = self.ledger.any();
            self.awaiting_cement
                .iter()
                .filter(|hash| any.block_exists(hash))
                .copied()
                .collect()
        };
        for hash in arrived {
            self.awaiting_cement.remove(&hash);
            self.active_elections.checkpoint_block_arrived(&hash);
            self.confirming_set.add_block(hash);
        }
    }

    fn clear_network_filter(&mut self, block: &Block) {
        let mut buffer = Vec::new();
        block
            .serialize_without_block_type(&mut buffer)
            .expect("Should serialize block successfully");
        self.network_filter.clear_bytes(&buffer);
    }
}
