use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_network::{Channel, ChannelId};
use rsnano_nullable_clock::SteadyClock;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{BlockHash, MaybeSavedBlock, NetworkType, QualifiedRoot};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, StatType, Stats},
};

use super::{LocalVoteHistory, vote_generator::VoteGenerator};
#[cfg(feature = "rai_protocol")]
use crate::consensus::epochs::VoteGate;
use crate::{
    config::{NetworkParams, NodeConfig},
    consensus::{VoteBroadcaster, election::VoteType},
    transport::MessageSender,
    wallets::WalletRepresentatives,
};

#[derive(Clone)]
pub struct VoteGenerationEvent {
    pub channel_id: ChannelId,
    pub blocks: Vec<MaybeSavedBlock>,
    pub final_vote: bool,
}

pub struct VoteGenerators {
    non_final_vote_generator: VoteGenerator,
    final_vote_generator: VoteGenerator,
    #[cfg(feature = "rai_protocol")]
    first_vote_generator: VoteGenerator,
    #[cfg(feature = "rai_protocol")]
    timeout_vote_generator: VoteGenerator,
    #[cfg(feature = "rai_protocol")]
    first_timeout_vote_generator: VoteGenerator,
    vote_listener: OutputListenerMt<VoteGenerationEvent>,
    voting_delay: Duration,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    stats: Arc<Stats>,
}

impl VoteGenerators {
    #[cfg(feature = "rai_protocol")]
    pub fn reply_earliest_election(&self, slot: rsnano_types::SlotRoot, channel: &Arc<Channel>) {
        self.final_vote_generator.reply_earliest_election(slot, channel);
        self.first_vote_generator.schedule_recovery(slot);
        self.non_final_vote_generator.schedule_recovery(slot);
        self.timeout_vote_generator.schedule_recovery(slot);
        self.first_timeout_vote_generator.schedule_recovery(slot);
    }

    #[cfg(feature = "rai_protocol")]
    pub(super) fn reply_election(&self, root: &rsnano_types::Root, epoch: u64, channel: &Arc<Channel>, kind: VoteType) -> bool {
        match kind {
            VoteType::First => self.first_vote_generator.reply_election(root, epoch, channel),
            VoteType::Final => self.final_vote_generator.reply_election(root, epoch, channel),
            _ => false,
        }
    }
    #[cfg(feature = "rai_protocol")]
    pub(super) fn reply_block(&self, block: &MaybeSavedBlock, channel: &Arc<Channel>) {
        self.first_vote_generator.reply_block(block, channel);
    }
    #[cfg(feature = "rai_protocol")]
    pub fn cut_generation(&self) -> u64 {
        self.first_vote_generator.cut_generation()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn voting_allowed(&self, root: &QualifiedRoot) -> bool {
        self.first_vote_generator.voting_allowed(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn clear_vote_spacing(&self) {
        self.non_final_vote_generator.clear_vote_spacing();
        self.final_vote_generator.clear_vote_spacing();
        self.first_vote_generator.clear_vote_spacing();
        self.timeout_vote_generator.clear_vote_spacing();
        #[cfg(feature = "rai_protocol")]
        self.first_timeout_vote_generator.clear_vote_spacing();
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn is_cut_recovery(&self, root: &QualifiedRoot) -> bool {
        self.non_final_vote_generator.is_cut_recovery(root)
    }

    fn voting_delay_for(network: NetworkType) -> Duration {
        match network {
            NetworkType::NanoDevNetwork => Duration::from_secs(1),
            _ => Duration::from_secs(15),
        }
    }

    pub(crate) fn new(
        ledger: Arc<Ledger>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        history: Arc<LocalVoteHistory>,
        stats: Arc<Stats>,
        config: &NodeConfig,
        network_params: &NetworkParams,
        vote_broadcaster: Arc<VoteBroadcaster>,
        message_sender: MessageSender,
        clock: Arc<SteadyClock>,
        #[cfg(feature = "rai_protocol")] vote_gate: Arc<VoteGate>,
        #[cfg(feature = "rai_protocol")] active_elections: Arc<crate::consensus::AecService>,
    ) -> Self {
        let voting_delay = Self::voting_delay_for(network_params.network.current_network);

        let non_final_vote_generator = VoteGenerator::new(
            ledger.clone(),
            wallet_reps.clone(),
            history.clone(),
            false, //none-final
            stats.clone(),
            message_sender.clone(),
            voting_delay,
            config.vote_generator_delay,
            vote_broadcaster.clone(),
            clock.clone(),
            #[cfg(feature = "rai_protocol")]
            VoteType::NonFinal,
            #[cfg(feature = "rai_protocol")]
            vote_gate.clone(),
            #[cfg(feature = "rai_protocol")]
            active_elections.clone(),
        );

        #[cfg(feature = "rai_protocol")]
        let first_vote_generator = VoteGenerator::new(
            ledger.clone(),
            wallet_reps.clone(),
            history.clone(),
            false,
            stats.clone(),
            message_sender.clone(),
            voting_delay,
            config.vote_generator_delay,
            vote_broadcaster.clone(),
            clock.clone(),
            VoteType::First,
            vote_gate.clone(),
            #[cfg(feature = "rai_protocol")]
            active_elections.clone(),
        );
        #[cfg(feature = "rai_protocol")]
        let timeout_vote_generator = VoteGenerator::new(
            ledger.clone(),
            wallet_reps.clone(),
            history.clone(),
            false,
            stats.clone(),
            message_sender.clone(),
            voting_delay,
            config.vote_generator_delay,
            vote_broadcaster.clone(),
            clock.clone(),
            VoteType::Timeout,
            vote_gate.clone(),
            #[cfg(feature = "rai_protocol")]
            active_elections.clone(),
        );

        #[cfg(feature = "rai_protocol")]
        let first_timeout_vote_generator = VoteGenerator::new(
            ledger.clone(),
            wallet_reps.clone(),
            history.clone(),
            false,
            stats.clone(),
            message_sender.clone(),
            voting_delay,
            config.vote_generator_delay,
            vote_broadcaster.clone(),
            clock.clone(),
            VoteType::FirstTimeout,
            vote_gate.clone(),
            #[cfg(feature = "rai_protocol")]
            active_elections.clone(),
        );

        let final_vote_generator = VoteGenerator::new(
            ledger,
            wallet_reps.clone(),
            history,
            true, //final
            stats.clone(),
            message_sender,
            voting_delay,
            config.vote_generator_delay,
            vote_broadcaster,
            clock,
            #[cfg(feature = "rai_protocol")]
            VoteType::Final,
            #[cfg(feature = "rai_protocol")]
            vote_gate,
            #[cfg(feature = "rai_protocol")]
            active_elections,
        );

        Self {
            non_final_vote_generator,
            final_vote_generator,
            vote_listener: OutputListenerMt::new(),
            voting_delay,
            wallet_reps,
            stats,
            #[cfg(feature = "rai_protocol")]
            first_vote_generator,
            #[cfg(feature = "rai_protocol")]
            timeout_vote_generator,
            #[cfg(feature = "rai_protocol")]
            first_timeout_vote_generator,
        }
    }

    pub fn new_null() -> Self {
        let ledger = Arc::new(Ledger::new_null());
        let wallet_reps = Arc::new(Mutex::new(WalletRepresentatives::new_null()));
        let history = Arc::new(LocalVoteHistory::new(NetworkType::NanoLiveNetwork));
        let stats = Arc::new(Stats::default());
        let config = NodeConfig::new_test_instance();
        let network_params = NetworkParams::new(NetworkType::NanoLiveNetwork);
        let vote_broadcaster = Arc::new(VoteBroadcaster::new_null());
        let message_sender = MessageSender::new_null();
        let clock = Arc::new(SteadyClock::new_null());
        Self::new(
            ledger,
            wallet_reps,
            history,
            stats,
            &config,
            &network_params,
            vote_broadcaster,
            message_sender,
            clock,
            #[cfg(feature = "rai_protocol")]
            Arc::new(VoteGate::default()),
            #[cfg(feature = "rai_protocol")]
            Arc::new(crate::consensus::AecService::new_null()),
        )
    }

    pub fn voting_delay(&self) -> Duration {
        self.voting_delay
    }

    pub fn start(&self) {
        self.non_final_vote_generator.start();
        self.final_vote_generator.start();
        #[cfg(feature = "rai_protocol")]
        self.first_vote_generator.start();
        #[cfg(feature = "rai_protocol")]
        self.timeout_vote_generator.start();
        #[cfg(feature = "rai_protocol")]
        self.first_timeout_vote_generator.start();
    }

    pub fn stop(&self) {
        self.non_final_vote_generator.stop();
        self.final_vote_generator.stop();
        #[cfg(feature = "rai_protocol")]
        self.first_vote_generator.stop();
        #[cfg(feature = "rai_protocol")]
        self.timeout_vote_generator.stop();
        #[cfg(feature = "rai_protocol")]
        self.first_timeout_vote_generator.stop();
    }

    pub fn track(&self) -> Arc<OutputTrackerMt<VoteGenerationEvent>> {
        self.vote_listener.track()
    }

    pub fn generate_vote(&self, root: &QualifiedRoot, hash: &BlockHash, vote_type: VoteType) {
        match vote_type {
            VoteType::NonFinal => {
                self.stats
                    .inc(StatType::Election, DetailType::GenerateVoteNormal);
                self.non_final_vote_generator.add(root, hash);
            }
            VoteType::Final => {
                self.stats
                    .inc(StatType::Election, DetailType::GenerateVoteFinal);
                self.final_vote_generator.add(root, hash);
            }
            #[cfg(feature = "rai_protocol")]
            VoteType::First => {
                self.stats
                    .inc(StatType::Election, DetailType::GenerateVoteFirst);
                self.first_vote_generator.add(root, hash)
            }
            #[cfg(feature = "rai_protocol")]
            VoteType::FirstTimeout => { self.first_timeout_vote_generator.add(root, hash); }
            #[cfg(feature = "rai_protocol")]
            VoteType::Timeout => {
                self.stats
                    .inc(StatType::Election, DetailType::GenerateVoteTimeout);
                self.timeout_vote_generator.add(root, hash)
            }
        }
    }

    pub(crate) fn generate_votes(
        &self,
        blocks: &[MaybeSavedBlock],
        channel: &Arc<Channel>,
        vote_type: VoteType,
        #[cfg(feature = "rai_protocol")] epoch: u64,
    ) -> usize {
        #[cfg(feature = "rai_protocol")]
        {
            // Epoch recovery is phase-complete. A requester cannot tell us which phase it
            // missed, and a later Final does not make the First/NonFinal evidence redundant for
            // a replica that is still deriving its election state. Replay every locally signed
            // phase, in protocol order, before optionally generating the phase requested by the
            // receiver's current state.
            match vote_type {
                VoteType::First => self
                    .first_vote_generator
                    .reply_cached_votes(blocks, channel, epoch),
                VoteType::NonFinal => {
                    self.non_final_vote_generator
                        .reply_cached_votes(blocks, channel, epoch);
                    // A Final vote is also notarization evidence. If this replica has already
                    // finalized and discarded the election, replaying its cached Final satisfies
                    // a requester's missing notarization without signing a conflicting phase.
                    self.final_vote_generator
                        .reply_cached_votes(blocks, channel, epoch);
                }
                VoteType::FirstTimeout => self.first_timeout_vote_generator.reply_cached_votes(blocks, channel, epoch),
                VoteType::Timeout => self
                    .timeout_vote_generator
                    .reply_cached_votes(blocks, channel, epoch),
                VoteType::Final => self
                    .final_vote_generator
                    .reply_cached_votes(blocks, channel, epoch),
            }
            // A reconstructed election may know that a second look is needed before this PR has
            // ever signed its First phase. Establish (or replay) that unique initial choice first;
            // the First generator's history guard rejects a conflicting second initial choice.
            if vote_type == VoteType::NonFinal {
                self.first_vote_generator.generate(blocks, channel, epoch);
            }
        }
        if self.vote_listener.is_tracked() {
            self.vote_listener.emit(VoteGenerationEvent {
                channel_id: channel.channel_id(),
                blocks: blocks.to_vec(),
                final_vote: vote_type == VoteType::Final,
            });
        }

        match vote_type {
            VoteType::NonFinal => self.non_final_vote_generator.generate(
                blocks,
                channel,
                #[cfg(feature = "rai_protocol")]
                epoch,
            ),
            VoteType::Final => self.final_vote_generator.generate(
                blocks,
                channel,
                #[cfg(feature = "rai_protocol")]
                epoch,
            ),
            #[cfg(feature = "rai_protocol")]
            VoteType::First => self.first_vote_generator.generate(blocks, channel, epoch),
            #[cfg(feature = "rai_protocol")]
            VoteType::Timeout => self.timeout_vote_generator.generate(blocks, channel, epoch),
            #[cfg(feature = "rai_protocol")]
            VoteType::FirstTimeout => self.first_timeout_vote_generator.generate(blocks, channel, epoch),
        }
    }

    pub fn voting_enabled(&self) -> bool {
        self.wallet_reps.lock().unwrap().voting_enabled()
    }
}

impl ContainerInfoProvider for VoteGenerators {
    #[cfg(not(feature = "rai_protocol"))]
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .node("non_final", self.non_final_vote_generator.container_info())
            .node("final", self.final_vote_generator.container_info())
            .finish()
    }

    #[cfg(feature = "rai_protocol")]
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .node("non_final", self.non_final_vote_generator.container_info())
            .node("final", self.final_vote_generator.container_info())
            .node("first", self.first_vote_generator.container_info())
            .node("timeout", self.timeout_vote_generator.container_info())
            .finish()
    }
}
