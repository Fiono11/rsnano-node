use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_network::{Channel, ChannelId};
use rsnano_nullable_clock::SteadyClock;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{BlockHash, NetworkType, PrivateKey, Root, SavedBlock, VoteKind};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, StatType, Stats},
};

use super::{LocalVoteHistory, vote_generator::VoteGenerator};
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
    /// One generator per vote type. Legacy needs non-final and final votes,
    /// the Kudzu rules additionally need notarization and timeout votes.
    generators: Vec<(VoteType, VoteGenerator)>,
    vote_listener: OutputListenerMt<VoteGenerationEvent>,
    voting_delay: Duration,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    stats: Arc<Stats>,
}

impl VoteGenerators {
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
    ) -> Self {
        let voting_delay = Self::voting_delay_for(network_params.network.current_network);

        let vote_types = if cfg!(feature = "rai_protocol") {
            vec![
                VoteType::NonFinal,
                VoteType::Final,
                VoteType::Notar,
                VoteType::Timeout,
            ]
        } else {
            vec![VoteType::NonFinal, VoteType::Final]
        };

        let generators = vote_types
            .into_iter()
            .map(|vote_type| {
                let generator = VoteGenerator::new(
                    ledger.clone(),
                    wallet_reps.clone(),
                    history.clone(),
                    VoteKind::from(vote_type),
                    stats.clone(),
                    message_sender.clone(),
                    voting_delay,
                    config.vote_generator_delay,
                    vote_broadcaster.clone(),
                    clock.clone(),
                );
                (vote_type, generator)
            })
            .collect();

        Self {
            generators,
            vote_listener: OutputListenerMt::new(),
            voting_delay,
            wallet_reps,
            stats,
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
        )
    }

    pub fn voting_delay(&self) -> Duration {
        self.voting_delay
    }

    pub fn start(&self) {
        for (_, generator) in &self.generators {
            generator.start();
        }
    }

    pub fn stop(&self) {
        for (_, generator) in &self.generators {
            generator.stop();
        }
    }

    fn generator(&self, vote_type: VoteType) -> &VoteGenerator {
        self.generators
            .iter()
            .find(|(t, _)| *t == vote_type)
            .map(|(_, g)| g)
            .unwrap_or_else(|| panic!("no vote generator for {:?}", vote_type))
    }

    pub fn track(&self) -> Arc<OutputTrackerMt<VoteGenerationEvent>> {
        self.vote_listener.track()
    }

    pub fn generate_vote(&self, root: &Root, hash: &BlockHash, vote_type: VoteType) {
        let detail = match vote_type {
            VoteType::NonFinal => DetailType::GenerateVoteNormal,
            VoteType::Final => DetailType::GenerateVoteFinal,
            VoteType::Notar => DetailType::GenerateVoteNotar,
            VoteType::Timeout => DetailType::GenerateVoteTimeout,
        };
        self.stats.inc(StatType::Election, detail);
        self.generator(vote_type).add(root, hash);
    }

    pub(crate) fn generate_votes(
        &self,
        blocks: &[SavedBlock],
        channel: &Arc<Channel>,
        vote_type: VoteType,
    ) -> usize {
        if self.vote_listener.is_tracked() {
            self.vote_listener.emit(VoteGenerationEvent {
                channel_id: channel.channel_id(),
                blocks: blocks.to_vec(),
                final_vote: vote_type == VoteType::Final,
            });
        }

        self.generator(vote_type).generate(blocks, channel)
    }

    pub fn voting_enabled(&self) -> bool {
        self.wallet_reps.lock().unwrap().voting_enabled()
    }

    /// The private keys of this node's voting representatives
    pub fn rep_priv_keys(&self) -> Vec<PrivateKey> {
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        keys
    }
}

impl ContainerInfoProvider for VoteGenerators {
    fn container_info(&self) -> ContainerInfo {
        let mut builder = ContainerInfo::builder();
        for (vote_type, generator) in &self.generators {
            let name = match vote_type {
                VoteType::NonFinal => "non_final",
                VoteType::Final => "final",
                VoteType::Notar => "notar",
                VoteType::Timeout => "timeout",
            };
            builder = builder.node(name, generator.container_info());
        }
        builder.finish()
    }
}
