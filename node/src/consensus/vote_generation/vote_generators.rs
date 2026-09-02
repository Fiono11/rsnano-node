use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_network::{Channel, ChannelId};
use rsnano_nullable_clock::SteadyClock;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{BlockHash, NetworkType, QualifiedRoot, SavedBlock};
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
    pub blocks: Vec<SavedBlock>,
    pub final_vote: bool,
}

pub struct VoteGenerators {
    non_final_vote_generator: VoteGenerator,
    final_vote_generator: VoteGenerator,
    #[cfg(feature = "rai_protocol")]
    first_vote_generator: VoteGenerator,
    #[cfg(feature = "rai_protocol")]
    timeout_vote_generator: VoteGenerator,
    vote_listener: OutputListenerMt<VoteGenerationEvent>,
    voting_delay: Duration,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    stats: Arc<Stats>,
}

impl VoteGenerators {
    #[cfg(feature = "rai_protocol")]
    pub fn cut_generation(&self) -> u64 {
        self.first_vote_generator.cut_generation()
    }

    pub fn voting_allowed(&self, root: &QualifiedRoot) -> bool {
        self.first_vote_generator.voting_allowed(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn clear_vote_spacing(&self) {
        self.non_final_vote_generator.clear_vote_spacing();
        self.final_vote_generator.clear_vote_spacing();
        self.first_vote_generator.clear_vote_spacing();
        self.timeout_vote_generator.clear_vote_spacing();
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
    }

    pub fn stop(&self) {
        self.non_final_vote_generator.stop();
        self.final_vote_generator.stop();
        #[cfg(feature = "rai_protocol")]
        self.first_vote_generator.stop();
        #[cfg(feature = "rai_protocol")]
        self.timeout_vote_generator.stop();
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
            VoteType::Timeout => {
                self.stats
                    .inc(StatType::Election, DetailType::GenerateVoteTimeout);
                self.timeout_vote_generator.add(root, hash)
            }
        }
    }

    pub(crate) fn generate_votes(
        &self,
        blocks: &[SavedBlock],
        channel: &Arc<Channel>,
        vote_type: VoteType,
        #[cfg(feature = "rai_protocol")] epoch: u64,
    ) -> usize {
        #[cfg(feature = "rai_protocol")]
        {
            let without_final: Vec<_> = blocks
                .iter()
                .filter(|block| !self.final_vote_generator.has_cached_vote(block, epoch))
                .cloned()
                .collect();
            self.first_vote_generator
                .reply_cached_votes(&without_final, channel, epoch);
            self.non_final_vote_generator
                .reply_cached_votes(&without_final, channel, epoch);
            self.timeout_vote_generator
                .reply_cached_votes(&without_final, channel, epoch);
            self.final_vote_generator
                .reply_cached_votes(blocks, channel, epoch);
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
