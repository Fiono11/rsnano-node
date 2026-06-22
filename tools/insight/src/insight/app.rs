use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

use rsnano_ledger::BlockSource;
use rsnano_node::{
    bootstrap::bootstrapper::PeerScoreSnapshot,
    cementation::ConfirmingSetInfo,
    consensus::{ActiveElectionsInfo, AecSnapshot, RepTier, election::Election},
    representatives::QuorumSnapshot,
};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{Account, Amount, BlockHash, PublicKey, QualifiedRoot};
use rsnano_utils::fair_queue::FairQueueInfo;

use crate::insight::{
    bootstrap::{BootstrapInfo, BootstrapViewType},
    channels::Channels,
    explorer::Explorer,
    frontier_scan::FrontierScanInfo,
    ledger_stats::LedgerStats,
    message_collection::MessageCollection,
    message_recorder::MessageRecorder,
    navigator::{NavItem, Navigator},
    node_callbacks::NodeCallbackFactory,
    node_runner::NodeRunner,
    rep_names::well_known_rep_names,
};
use rsnano_network::ChannelId;

pub(crate) struct InsightApp {
    last_update: Option<Timestamp>,
    pub clock: Arc<SteadyClock>,
    pub messages: Arc<RwLock<MessageCollection>>,
    pub msg_recorder: Arc<MessageRecorder>,
    pub node_runner: NodeRunner,
    pub channels: Channels,
    pub explorer: Explorer,
    pub navigator: Navigator,
    pub ledger_stats: LedgerStats,
    pub aec_info: ActiveElectionsInfo,
    pub max_hinted: usize,
    pub max_optimistic: usize,
    pub confirming_set: ConfirmingSetInfo,
    pub block_processor_info: FairQueueInfo<BlockSource>,
    pub vote_processor_info: FairQueueInfo<RepTier>,
    pub frontier_scan: FrontierScanInfo,
    pub bootstrap: BootstrapInfo,
    pub elections: AecSnapshot,
    pub rollback_hash: String,
    selected_election: Option<QualifiedRoot>,
    pub election_details: Option<Election>,
    bootstrap_view_type: BootstrapViewType,
    pub peer_scores: Vec<PeerScoreSnapshot>,
    pub representatives: Vec<RepresentativeViewModel>,
    pub quorum: QuorumSnapshot,
    rep_names: HashMap<PublicKey, &'static str>,
}

impl InsightApp {
    pub fn new() -> Self {
        let clock = Arc::new(SteadyClock::default());
        let messages = Arc::new(RwLock::new(MessageCollection::default()));
        let msg_recorder = Arc::new(MessageRecorder::new(messages.clone()));
        let callback_factory = NodeCallbackFactory::new(msg_recorder.clone(), clock.clone());
        let rep_names = well_known_rep_names();
        let channels = Channels::new(messages.clone(), rep_names.clone());
        Self {
            clock,
            messages,
            msg_recorder,
            node_runner: NodeRunner::new(callback_factory),
            channels,
            explorer: Explorer::new(),
            navigator: Navigator::new(),
            ledger_stats: LedgerStats::new(),
            aec_info: Default::default(),
            max_hinted: 1,
            max_optimistic: 1,
            confirming_set: Default::default(),
            block_processor_info: Default::default(),
            vote_processor_info: Default::default(),
            frontier_scan: FrontierScanInfo::default(),
            last_update: None,
            bootstrap: Default::default(),
            rollback_hash: String::new(),
            elections: AecSnapshot::default(),
            selected_election: None,
            election_details: None,
            bootstrap_view_type: BootstrapViewType::BootstrapQueue,
            peer_scores: Vec::new(),
            representatives: Vec::new(),
            rep_names,
            quorum: QuorumSnapshot::new_test_instance(),
        }
    }

    pub fn search(&mut self, input: &str) {
        if let Some(node) = self.node_runner.node() {
            let has_result = self.explorer.search(&node.ledger, input);
            if has_result {
                self.navigator.current = NavItem::Explorer;
            }
        }
    }

    pub(crate) fn update(&mut self) -> bool {
        let now = self.clock.now();
        if let Some(last_update) = self.last_update
            && now - last_update < Duration::from_millis(500)
        {
            return false;
        }

        if let Some(node) = self.node_runner.node() {
            self.ledger_stats.update(&node);
            let channels = node.network.read().unwrap().sorted_channels();
            let telemetries = node.telemetry.get_all_telemetries();
            node.rep_tracker.with_snapshot(|s| {
                let min_rep_weight = s.quorum().minimum_principal_weight;
                self.channels
                    .update(channels, telemetries, s, min_rep_weight);
            });
            self.aec_info = node.aec.info();
            self.max_optimistic = node.election_schedulers.optimistic.max_elections();
            self.max_hinted = node.election_schedulers.hinted.max_elections;
            self.confirming_set = node.confirming_set.info();
            self.block_processor_info = node.block_processor_queue.info();
            self.vote_processor_info = node.vote_processor_queue.info();
            self.frontier_scan.update(&node.bootstrapper, now);
            self.bootstrap.update(&node.bootstrapper);
            self.elections = node.aec.snapshot();
            self.election_details = self.selected_election.as_ref().and_then(|root| {
                let node = self.node_runner.node()?;
                node.aec.election_for_root(root)
            });
            self.peer_scores = node.bootstrapper.peer_score_snapshot();
            self.representatives = node.rep_tracker.with_snapshot(|s| {
                self.quorum = s.quorum().clone();
                s.iter()
                    .map(|rep| {
                        let name = self.rep_names.get(&rep.public_key).unwrap_or(&"");
                        RepresentativeViewModel {
                            account: rep.public_key.as_account(),
                            name: *name,
                            weight: rep.weight,
                            channel: rep.channel_id.clone(),
                        }
                    })
                    .collect()
            });
            self.representatives
                .sort_unstable_by(|a, b| b.weight.cmp(&a.weight));
        }

        self.last_update = Some(now);
        true
    }

    pub(crate) fn add_priority_account(&mut self) {
        if let Some(account) = Account::parse(&self.bootstrap.add_account) {
            self.bootstrap.add_account.clear();
            if let Some(node) = self.node_runner.node() {
                node.bootstrapper.enqueue(account);
            }
        }
    }

    pub(crate) fn roll_back(&self) {
        if let Some(hash) = BlockHash::decode_hex(&self.rollback_hash)
            && let Some(node) = self.node_runner.node()
        {
            let _ = node.ledger.roll_back(&hash);
        }
    }

    pub fn clear_blocked_accounts(&self) {
        if let Some(node) = self.node_runner.node() {
            node.bootstrapper.clear_blocked_accounts();
        }
    }

    pub fn verify_blocked_accounts(&self) {
        if let Some(node) = self.node_runner.node() {
            node.bootstrapper.verify_blocked_accounts();
        }
    }

    pub fn print_processing(&self) {
        if let Some(node) = self.node_runner.node() {
            node.bootstrapper.print_processing();
        }
    }

    pub fn show_election(&mut self, root: QualifiedRoot) {
        self.selected_election = Some(root);
    }

    pub fn close_election(&mut self) {
        self.selected_election = None;
        self.election_details = None;
    }

    pub fn select_bootstrap_view(&mut self, view_type: BootstrapViewType) {
        self.bootstrap_view_type = view_type;
    }

    pub fn bootstrap_view(&self) -> BootstrapViewType {
        self.bootstrap_view_type
    }
}

pub(crate) struct RepresentativeViewModel {
    pub account: Account,
    pub name: &'static str,
    pub weight: Amount,
    pub channel: Option<ChannelId>,
}
