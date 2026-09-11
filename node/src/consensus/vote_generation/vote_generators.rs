use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_network::{Channel, ChannelId};
use rsnano_nullable_clock::SteadyClock;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{BlockHash, NetworkType, Root, SavedBlock};
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
    #[cfg(feature = "rai_protocol")]
    ledger: Arc<Ledger>,
    #[cfg(feature = "rai_protocol")]
    vote_state: Arc<Mutex<super::kudzu_vote_state::KudzuVoteState>>,
    #[cfg(feature = "rai_protocol")]
    elections: Arc<std::sync::RwLock<std::sync::Weak<crate::consensus::AecService>>>,
    #[cfg(feature = "rai_protocol")]
    vote_broadcaster: Arc<VoteBroadcaster>,
    non_final_vote_generator: VoteGenerator,
    final_vote_generator: VoteGenerator,
    vote_listener: OutputListenerMt<VoteGenerationEvent>,
    voting_delay: Duration,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    stats: Arc<Stats>,
}

impl VoteGenerators {
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn pause_epoch_report(
        &self,
        epoch: u64,
        aec: &crate::consensus::AecService,
    ) -> Vec<(rsnano_types::QualifiedRoot, BlockHash)> {
        let mut state = self.vote_state.lock().unwrap();
        state.pause_epoch(epoch);
        let report = aec.pending_cut_report(epoch);
        self.ledger
            .voting_epoch
            .store(epoch + 1, std::sync::atomic::Ordering::Release);
        report
    }
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn resume_epoch_cut(&self, epoch: u64, roots: Vec<rsnano_types::QualifiedRoot>) {
        self.vote_state.lock().unwrap().resume_cut(epoch, roots);
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn draining_complete(&self, epoch: u64, aec: &crate::consensus::AecService) -> bool {
        let ids = {
            let mut state = self.vote_state.lock().unwrap();
            state.drain_through(epoch);
            state.first_elections(epoch)
        };
        aec.elections_terminated(epoch, &ids)
    }
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn notify_notarizations(
        &self,
        notifications: Vec<(rsnano_types::ElectionId, BlockHash)>,
    ) {
        if notifications.is_empty() {
            return;
        }
        // Loading wallet keys decrypts them and derives their public keys. Do it
        // once per notification batch, not once for every election reaching the second-look threshold.
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        let needed: Vec<_> = {
            let state = self.vote_state.lock().unwrap();
            // Mixed local committees use periodic recovery: never accelerate a First replay.
            notifications
                .into_iter()
                .filter(|(id, hash)| {
                    state.voting_active(&id.root, id.epoch)
                        && !keys.is_empty()
                        && keys.iter().all(|key| {
                            state.needs_second_look(&id.root, key.public_key(), *hash, id.epoch)
                                && !state.has_notarization(
                                    &id.root,
                                    key.public_key(),
                                    *hash,
                                    id.epoch,
                                )
                        })
                })
                .collect()
        };
        for (id, hash) in needed {
            self.non_final_vote_generator
                .add_in_epoch(&id.root.root, &hash, id.epoch);
        }
    }

    /// Take the pause/cut filter without holding the signing mutex during AEC reads.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn solicitation_filter(&self) -> impl Fn(&rsnano_types::QualifiedRoot, u64) -> bool + use<> {
        self.vote_state.lock().unwrap().voting_filter()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn retain_signable_targets(
        &self,
        targets: &mut Vec<super::voting_scheduler::VoteTarget>,
    ) {
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        let state = self.vote_state.lock().unwrap();
        state.retain_voting_targets(
            targets,
            &keys.iter().map(|key| key.public_key()).collect::<Vec<_>>(),
        );
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
    ) -> Self {
        let voting_delay = Self::voting_delay_for(network_params.network.current_network);

        #[cfg(feature = "rai_protocol")]
        let elections = Arc::new(std::sync::RwLock::new(std::sync::Weak::new()));
        #[cfg(feature = "rai_protocol")]
        let vote_state = Arc::new(Mutex::new(
            super::kudzu_vote_state::KudzuVoteState::default(),
        ));

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
            elections.clone(),
            #[cfg(feature = "rai_protocol")]
            vote_state.clone(),
        );

        let final_vote_generator = VoteGenerator::new(
            ledger.clone(),
            wallet_reps.clone(),
            history,
            true, //final
            stats.clone(),
            message_sender.clone(),
            voting_delay,
            config.vote_generator_delay,
            vote_broadcaster.clone(),
            clock,
            #[cfg(feature = "rai_protocol")]
            elections.clone(),
            #[cfg(feature = "rai_protocol")]
            vote_state.clone(),
        );

        Self {
            #[cfg(feature = "rai_protocol")]
            ledger,
            #[cfg(feature = "rai_protocol")]
            vote_state,
            #[cfg(feature = "rai_protocol")]
            elections,
            #[cfg(feature = "rai_protocol")]
            vote_broadcaster,
            non_final_vote_generator,
            final_vote_generator,
            vote_listener: OutputListenerMt::new(),
            voting_delay,
            wallet_reps,
            stats,
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn set_elections(&self, elections: &Arc<crate::consensus::AecService>) {
        *self.elections.write().unwrap() = Arc::downgrade(elections);
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn recovery_entries(
        &self,
        requests: &[(BlockHash, rsnano_types::Root)],
        epoch: u64,
    ) -> Vec<rsnano_types::RaiBlockTreeEntry> {
        self.elections
            .read()
            .unwrap()
            .upgrade()
            .map(|e| e.recovery_entries(requests, epoch))
            .unwrap_or_default()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn recovery_reply(
        &self,
        requests: &[(BlockHash, Root)],
        epoch: u64,
    ) -> (Vec<rsnano_types::Block>, Vec<Arc<rsnano_types::Vote>>) {
        let entries = self.recovery_entries(requests, epoch);
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        let tx = self.ledger.store.begin_read();
        let mut state = self.vote_state.lock().unwrap();
        let mut final_locks = Vec::new();
        let mut blocks = Vec::new();
        let mut groups = std::collections::BTreeMap::<_, Vec<BlockHash>>::new();
        for entry in entries {
            let final_lock = self.ledger.store.final_vote.get(&tx, &entry.root);
            for (key_index, key) in keys.iter().enumerate() {
                let statement = if entry.block.is_some() {
                    state
                        .authorize(
                            &entry.root,
                            key.public_key(),
                            entry.hash(),
                            entry.epoch,
                            entry.finalized,
                            true,
                            true,
                            final_lock,
                        )
                        .map(|kind| (kind, entry.hash()))
                } else {
                    state.timeout_statement(&entry.root, key.public_key(), entry.epoch)
                };
                if let Some((kind, hash)) = statement {
                    if kind == rsnano_types::VoteKind::Final && final_lock.is_none() {
                        final_locks.push((entry.root.clone(), hash));
                    }
                    groups
                        .entry((key_index, entry.epoch, kind))
                        .or_default()
                        .push(hash);
                }
            }
            if let Some(block) = entry.block {
                blocks.push(block);
            }
        }
        drop(state);
        drop(tx);
        if !final_locks.is_empty() {
            let mut tx = self.ledger.store.begin_write();
            for (root, hash) in final_locks {
                assert!(self.ledger.store.final_vote.put(&mut tx, &root, &hash));
            }
            tx.commit();
        }
        // Existing vote messages route through a candidate hash. Supply those
        // payloads for timeout replies too, without marking them notarized.
        let timeout_hashes: Vec<_> = groups
            .iter()
            .filter(|((_, _, kind), _)| {
                matches!(
                    kind,
                    rsnano_types::VoteKind::Timeout | rsnano_types::VoteKind::FirstTimeout
                )
            })
            .flat_map(|(_, hashes)| hashes.iter().copied())
            .collect();
        if !timeout_hashes.is_empty() {
            if let Some(aec) = self.elections.read().unwrap().upgrade() {
                blocks.extend(aec.kudzu_candidates(&timeout_hashes).into_iter().flatten());
            }
        }
        // Sign batched replies after releasing the shared signing reservation.
        let mut votes = Vec::new();
        for ((key_index, epoch, kind), mut hashes) in groups {
            hashes.sort_unstable();
            hashes.dedup();
            for hashes in hashes.chunks(rsnano_messages::ConfirmAck::HASHES_MAX) {
                let vote = Arc::new(rsnano_types::Vote::new_with_kind(
                    &keys[key_index],
                    hashes.to_vec(),
                    epoch,
                    kind,
                ));
                if self.vote_broadcaster.enqueue_local(vote.clone()) {
                    votes.push(vote);
                }
            }
        }
        (blocks, votes)
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
        self.non_final_vote_generator.start();
        self.final_vote_generator.start();
    }

    pub fn stop(&self) {
        self.non_final_vote_generator.stop();
        self.final_vote_generator.stop();
    }

    pub fn track(&self) -> Arc<OutputTrackerMt<VoteGenerationEvent>> {
        self.vote_listener.track()
    }

    pub fn generate_vote(&self, root: &Root, hash: &BlockHash, vote_type: VoteType) {
        self.generate_vote_in_epoch(root, hash, vote_type, 0)
    }
    pub fn generate_vote_in_epoch(
        &self,
        root: &Root,
        hash: &BlockHash,
        vote_type: VoteType,
        epoch: u64,
    ) {
        match vote_type {
            VoteType::NonFinal => {
                self.stats
                    .inc(StatType::Election, DetailType::GenerateVoteNormal);
                self.non_final_vote_generator
                    .add_in_epoch(root, hash, epoch);
            }
            VoteType::Final => {
                self.stats
                    .inc(StatType::Election, DetailType::GenerateVoteFinal);
                self.final_vote_generator.add_in_epoch(root, hash, epoch);
            }
        }
    }

    pub(crate) fn generate_votes_in_epoch(
        &self,
        blocks: &[SavedBlock],
        channel: &Arc<Channel>,
        vote_type: VoteType,
        epoch: u64,
    ) -> usize {
        if self.vote_listener.is_tracked() {
            self.vote_listener.emit(VoteGenerationEvent {
                channel_id: channel.channel_id(),
                blocks: blocks.to_vec(),
                final_vote: vote_type == VoteType::Final,
            });
        }

        match vote_type {
            VoteType::NonFinal => self
                .non_final_vote_generator
                .generate_in_epoch(blocks, channel, epoch),
            VoteType::Final => self
                .final_vote_generator
                .generate_in_epoch(blocks, channel, epoch),
        }
    }

    pub fn voting_enabled(&self) -> bool {
        self.wallet_reps.lock().unwrap().voting_enabled()
    }
}

impl ContainerInfoProvider for VoteGenerators {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .node("non_final", self.non_final_vote_generator.container_info())
            .node("final", self.final_vote_generator.container_info())
            .finish()
    }
}
