use std::{
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_network::{Channel, ChannelId};
use rsnano_nullable_clock::SteadyClock;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{BlockHash, ConsensusEpoch, NetworkType, PrivateKey, Root, SavedBlock};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, StatType, Stats},
};

use super::{LocalVoteHistory, SignedVoteLog, vote_generator::VoteGenerator};
use crate::{
    config::{NetworkParams, NodeConfig},
    consensus::{CheckpointBases, SigningRecords, VoteBroadcaster, election::VoteType},
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
    /// RAI: the checkpoints first votes name as their base
    bases: Arc<CheckpointBases>,
    /// RAI: what a restarted node signs for the first time when it starts
    #[cfg(feature = "rai_protocol")]
    recorded: Mutex<Vec<super::RecordedVotes>>,
    /// RAI: the votes a restarted node released before it stopped, served
    /// again as stored
    #[cfg(feature = "rai_protocol")]
    stored: Mutex<Vec<Arc<rsnano_types::Vote>>>,
    /// RAI, stored signatures: shared with every generator, set once the
    /// node has opened its signing records
    #[cfg_attr(not(feature = "rai_protocol"), allow(dead_code))]
    signing: Arc<OnceLock<Arc<SigningRecords>>>,
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
        signed_log: Arc<SignedVoteLog>,
        bases: Arc<CheckpointBases>,
    ) -> Self {
        let signing = Arc::new(OnceLock::new());
        let voting_delay = Self::voting_delay_for(network_params.network.current_network);

        // RAI: the close election's notarization, timeout and abstain votes
        // have generators of their own
        #[cfg(feature = "rai_protocol")]
        let vote_types = vec![
            VoteType::NonFinal,
            VoteType::Final,
            VoteType::Notar,
            VoteType::Timeout,
            VoteType::Abstain,
            VoteType::LateNotar,
            VoteType::EarlyFirst,
        ];
        #[cfg(not(feature = "rai_protocol"))]
        let vote_types = vec![VoteType::NonFinal, VoteType::Final];

        let generators = vote_types
            .into_iter()
            .map(|vote_type| {
                let generator = VoteGenerator::new(
                    ledger.clone(),
                    wallet_reps.clone(),
                    history.clone(),
                    vote_type,
                    stats.clone(),
                    message_sender.clone(),
                    voting_delay,
                    config.vote_generator_delay,
                    vote_broadcaster.clone(),
                    clock.clone(),
                    signed_log.clone(),
                    bases.clone(),
                    signing.clone(),
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
            bases,
            #[cfg(feature = "rai_protocol")]
            recorded: Mutex::new(Vec::new()),
            #[cfg(feature = "rai_protocol")]
            stored: Mutex::new(Vec::new()),
            signing,
        }
    }

    /// RAI, stored signatures: the records this node's own account votes
    /// are written to before they leave. Set once, when the node has opened
    /// them; until then nothing is stored.
    #[cfg_attr(not(feature = "rai_protocol"), allow(dead_code))]
    pub(crate) fn set_signing(&self, signing: Arc<SigningRecords>) {
        let _ = self.signing.set(signing);
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
            Arc::new(SignedVoteLog::new_null()),
            Arc::new(CheckpointBases::default()),
        )
    }

    pub fn voting_delay(&self) -> Duration {
        self.voting_delay
    }

    pub fn start(&self) {
        for (_, generator) in &self.generators {
            generator.start();
        }
        #[cfg(feature = "rai_protocol")]
        self.sign_recorded();
    }

    /// RAI, durable signing records: the votes a restarted node recorded
    /// before it stopped, and the signed votes it stored. The stored ones
    /// are served again when it starts; it signs only the recorded votes
    /// none of them covers, which it never released.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn restore_recorded(
        &self,
        slots: &[crate::consensus::SlotRecord],
        reports: &[crate::consensus::ReportRecord],
        stored: &[Arc<rsnano_types::Vote>],
    ) {
        let recorded = super::recorded_votes(slots, reports, VoteGenerator::MAX_HASHES);
        *self.recorded.lock().unwrap() = super::unreleased(recorded, stored);
        *self.stored.lock().unwrap() = stored.to_vec();
    }

    /// The stored votes are kept as they were signed. The rest are the
    /// statements the node's records and reports hold but whose signature
    /// never left the process: signed now for the first time, also in epochs
    /// the node left and signs nothing new in - one identity, block, kind
    /// and epoch, so the signature cannot conflict with any other
    #[cfg(feature = "rai_protocol")]
    fn sign_recorded(&self) {
        let stored = std::mem::take(&mut *self.stored.lock().unwrap());
        let recorded = std::mem::take(&mut *self.recorded.lock().unwrap());
        if stored.is_empty() && recorded.is_empty() {
            return;
        }
        let served = stored.len();
        if let Some((_, generator)) = self.generators.first() {
            for vote in stored {
                generator.keep_stored(vote);
            }
        }
        let mut signed = 0;
        for votes in &recorded {
            self.generator(votes.vote_type)
                .sign_recorded(&votes.roots, &votes.hashes, votes.epoch);
            signed += votes.hashes.len();
        }
        crate::utils::diagnostic!(
            "EPOCH_VOTES_RESTORED stored={} signed_unreleased={}",
            served,
            signed
        );
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

    pub fn generate_vote(
        &self,
        root: &Root,
        hash: &BlockHash,
        epoch: ConsensusEpoch,
        vote_type: VoteType,
    ) {
        let detail = match vote_type {
            VoteType::NonFinal => DetailType::GenerateVoteNormal,
            VoteType::Final => DetailType::GenerateVoteFinal,
            VoteType::Notar => DetailType::GenerateVoteNotar,
            VoteType::Timeout => DetailType::GenerateVoteTimeout,
            VoteType::Abstain => DetailType::GenerateVoteAbstain,
            VoteType::LateNotar => DetailType::GenerateVoteNotar,
            VoteType::EarlyFirst => DetailType::GenerateVoteNormal,
        };
        self.stats.inc(StatType::Election, detail);
        self.generator(vote_type).add(root, hash, epoch);
    }

    /// RAI evaluation, a Byzantine member: see `VoteGenerator::sign_now`
    #[cfg(feature = "rai_byzantine")]
    pub(crate) fn sign_now(
        &self,
        root: &Root,
        hash: &BlockHash,
        epoch: ConsensusEpoch,
        vote_type: VoteType,
    ) {
        self.generator(vote_type).sign_now(root, hash, epoch);
    }

    /// Reply to a request for votes in the given epoch
    pub(crate) fn generate_votes(
        &self,
        blocks: &[SavedBlock],
        channel: &Arc<Channel>,
        epoch: ConsensusEpoch,
        vote_type: VoteType,
    ) -> usize {
        if self.vote_listener.is_tracked() {
            self.vote_listener.emit(VoteGenerationEvent {
                channel_id: channel.channel_id(),
                blocks: blocks.to_vec(),
                final_vote: vote_type == VoteType::Final,
            });
        }

        self.generator(vote_type).generate(blocks, channel, epoch)
    }

    pub fn voting_enabled(&self) -> bool {
        self.wallet_reps.lock().unwrap().voting_enabled()
    }

    /// RAI: the base a vote of this type signed now names: the checkpoint
    /// a first vote is cast on, zero for any other kind
    pub fn vote_base(&self, vote_type: VoteType, epoch: ConsensusEpoch) -> BlockHash {
        if vote_type.is_first() {
            self.bases
                .first_vote_base(epoch, vote_type == VoteType::EarlyFirst)
        } else {
            BlockHash::ZERO
        }
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
                VoteType::Abstain => "abstain",
                VoteType::LateNotar => "late_notar",
                VoteType::EarlyFirst => "early_first",
            };
            builder = builder.node(name, generator.container_info());
        }
        builder.finish()
    }
}
