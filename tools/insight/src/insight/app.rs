use std::{
    collections::HashMap,
    sync::{Arc, RwLock, atomic::Ordering, mpsc::Receiver},
    time::Duration,
};

use rsnano_node::{NodeEvent, consensus::election::Election};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{Account, BlockHash, PublicKey, QualifiedRoot};

use super::snapshot::{InsightSnapshot, take_snapshot};
use crate::insight::{
    block_processor::BlockProcessorViewModel,
    bootstrap::{BootstrapInfo, BootstrapViewType},
    channels::Channels,
    explorer::{ExplorerViewModel, search_ledger},
    frontier_scan::FrontierScanInfo,
    message_collection::MessageCollection,
    message_recorder::MessageRecorder,
    message_stats::MessageStatsViewModel,
    navigator::{NAV_ORDER, NavItem, TabViewModel},
    node_callbacks::NodeCallbackFactory,
    node_runner::NodeRunner,
    queues::{QueueGroupViewModel, QueueViewModel},
    rep_names::well_known_rep_names,
    representatives::{RepresentativeViewModel, RepresentativesViewModel},
    vote_cache::{VoteCacheViewModel, VoteViewModel},
};

pub(crate) enum InsightCommand {
    AddAccountToBootstrapQueue,
    ClearBlockedBootstrapAccounts,
    VerifyBlockedBootstrapAccounts,
    PrintProcessingBootstrapBlocks,
    Search(String),
    NavigateBootstrap(BootstrapViewType),
    CloseElection,
    RollBack,
    ShowElection(QualifiedRoot),
    Navigate(NavItem),
}

pub(crate) struct InsightApp {
    pub explorer: ExplorerViewModel,
    pub block_processor: BlockProcessorViewModel,
    pub representatives: RepresentativesViewModel,
    pub tabs: Vec<TabViewModel>,
    pub current_tab: NavItem,
    pub vote_cache: VoteCacheViewModel,
    pub message_stats: MessageStatsViewModel,
    pub queue_groups: Vec<QueueGroupViewModel>,

    pub clock: Arc<SteadyClock>,
    pub messages: Arc<RwLock<MessageCollection>>,
    pub msg_recorder: Arc<MessageRecorder>,
    pub node_runner: NodeRunner,
    pub channels: Channels,
    pub snapshot: InsightSnapshot,
    pub frontier_scan: FrontierScanInfo,
    pub bootstrap: BootstrapInfo,
    selected_election: Option<QualifiedRoot>,
    pub election_details: Option<Election>,
    bootstrap_view_type: BootstrapViewType,
    rep_names: HashMap<PublicKey, &'static str>,
    last_update: Option<Timestamp>,
    rx_cmd: Receiver<InsightCommand>,
    rx_node_ev: Receiver<NodeEvent>,
}

impl InsightApp {
    pub fn new(rx: Receiver<InsightCommand>) -> Self {
        let clock = Arc::new(SteadyClock::default());
        let messages = Arc::new(RwLock::new(MessageCollection::default()));
        let msg_recorder = Arc::new(MessageRecorder::new(messages.clone()));
        let callback_factory = NodeCallbackFactory::new(msg_recorder.clone(), clock.clone());
        let rep_names = well_known_rep_names();
        let channels = Channels::new(messages.clone(), rep_names.clone());
        let (tx_node_ev, rx_node_ev) = std::sync::mpsc::channel::<NodeEvent>();

        let tabs = NAV_ORDER
            .iter()
            .map(|i| TabViewModel {
                selected: false,
                label: i.name(),
                value: *i,
            })
            .collect();

        Self {
            explorer: Default::default(),
            block_processor: Default::default(),
            current_tab: NavItem::Peers,
            vote_cache: Default::default(),
            message_stats: Default::default(),
            queue_groups: Vec::new(),
            tabs,
            rx_cmd: rx,
            clock,
            messages,
            msg_recorder,
            node_runner: NodeRunner::new(callback_factory, tx_node_ev),
            channels,
            snapshot: InsightSnapshot::default(),
            frontier_scan: FrontierScanInfo::default(),
            last_update: None,
            bootstrap: Default::default(),
            selected_election: None,
            election_details: None,
            bootstrap_view_type: BootstrapViewType::BootstrapQueue,
            representatives: Default::default(),
            rep_names,
            rx_node_ev,
        }
    }

    pub(crate) fn update(&mut self) -> bool {
        while let Ok(cmd) = self.rx_cmd.try_recv() {
            self.process_command(cmd);
        }

        while let Ok(e) = self.rx_node_ev.try_recv() {
            self.process_node_event(e);
        }

        if !self.should_update() {
            return false;
        }

        if let Some(node) = self.node_runner.node() {
            let snapshot = take_snapshot(&node);
            self.message_stats.send_rate =
                self.msg_recorder.rates.send_rate.load(Ordering::Relaxed);
            self.message_stats.receive_rate =
                self.msg_recorder.rates.receive_rate.load(Ordering::Relaxed);
            self.vote_cache.cached_blocks = node.vote_cache.len();
            self.vote_cache.block_votes.clear();

            self.queue_groups.clear();
            self.queue_groups.push(QueueGroupViewModel {
                heading: "Active Elections".to_string(),
                queues: vec![
                    QueueViewModel::new(
                        "Priority",
                        snapshot.aec_info.priority,
                        snapshot.aec_info.max_elections,
                    ),
                    QueueViewModel::new("Hinted", snapshot.aec_info.hinted, snapshot.max_hinted),
                    QueueViewModel::new(
                        "Optimistic",
                        snapshot.aec_info.optimistic,
                        snapshot.max_optimistic,
                    ),
                    QueueViewModel::new(
                        "Total",
                        snapshot.aec_info.total,
                        snapshot.aec_info.max_elections,
                    ),
                ],
            });
            self.queue_groups.push(QueueGroupViewModel::for_fair_queue(
                "Block Processor",
                &snapshot.block_processor_info,
            ));
            self.queue_groups.push(QueueGroupViewModel::for_fair_queue(
                "Vote Processor",
                &snapshot.vote_processor_info,
            ));
            self.queue_groups.push(QueueGroupViewModel {
                heading: "Miscellaneous".to_string(),
                queues: vec![QueueViewModel::new(
                    "Confirming",
                    snapshot.confirming_set.size,
                    snapshot.confirming_set.max_size,
                )],
            });

            if let Some(block_hash) = BlockHash::decode_hex(&self.vote_cache.search) {
                let votes = node.vote_cache.get(&block_hash);
                let rep_weights = node.ledger.rep_weights.read();
                self.vote_cache
                    .block_votes
                    .extend(votes.iter().map(|v| VoteViewModel {
                        rep_key: v.voter,
                        is_final: v.is_final(),
                        weight: rep_weights.weight(&v.voter),
                    }));
            }

            let channels = node.network.read().unwrap().sorted_channels();
            let telemetries = node.telemetry.get_all_telemetries();
            node.rep_tracker.with_snapshot(|s| {
                let min_rep_weight = s.quorum().minimum_principal_weight;
                self.channels
                    .update(channels, telemetries, s, min_rep_weight);
            });
            self.snapshot = snapshot;
            self.frontier_scan
                .update(&node.bootstrapper, self.clock.now());
            self.bootstrap.update(&node.bootstrapper);
            self.election_details = self.selected_election.as_ref().and_then(|root| {
                let node = self.node_runner.node()?;
                node.aec.election_for_root(root)
            });
            node.rep_tracker.with_snapshot(|s| {
                self.representatives.quorum = s.quorum().clone();
                self.representatives.reps = s
                    .iter()
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
                .reps
                .sort_unstable_by(|a, b| b.weight.cmp(&a.weight));
        }

        true
    }

    fn should_update(&mut self) -> bool {
        let now = self.clock.now();
        if let Some(last_update) = self.last_update
            && now - last_update < Duration::from_millis(500)
        {
            false
        } else {
            self.last_update = Some(now);
            true
        }
    }

    pub fn bootstrap_view(&self) -> BootstrapViewType {
        self.bootstrap_view_type
    }

    fn process_command(&mut self, cmd: InsightCommand) {
        match cmd {
            InsightCommand::AddAccountToBootstrapQueue => self.add_priority_account(),
            InsightCommand::ClearBlockedBootstrapAccounts => self.clear_blocked_accounts(),
            InsightCommand::VerifyBlockedBootstrapAccounts => self.verify_blocked_accounts(),
            InsightCommand::PrintProcessingBootstrapBlocks => self.print_processing(),
            InsightCommand::Search(s) => self.search(&s),
            InsightCommand::NavigateBootstrap(view_type) => self.bootstrap_view_type = view_type,
            InsightCommand::CloseElection => {
                self.selected_election = None;
                self.election_details = None;
            }
            InsightCommand::RollBack => self.roll_back(),
            InsightCommand::ShowElection(root) => self.selected_election = Some(root),
            InsightCommand::Navigate(target) => self.navigate(target),
        }
    }

    fn navigate(&mut self, target: NavItem) {
        for t in &mut self.tabs {
            t.selected = t.value == target;
        }
        self.current_tab = target;
    }

    fn add_priority_account(&mut self) {
        if let Some(account) = Account::parse(&self.bootstrap.add_account) {
            self.bootstrap.add_account.clear();
            if let Some(node) = self.node_runner.node() {
                node.bootstrapper.enqueue(account);
            }
        }
    }

    fn clear_blocked_accounts(&self) {
        if let Some(node) = self.node_runner.node() {
            node.bootstrapper.clear_blocked_accounts();
        }
    }

    fn verify_blocked_accounts(&self) {
        if let Some(node) = self.node_runner.node() {
            node.bootstrapper.verify_blocked_accounts();
        }
    }

    fn print_processing(&self) {
        if let Some(node) = self.node_runner.node() {
            node.bootstrapper.print_processing();
        }
    }

    fn search(&mut self, input: &str) {
        if let Some(node) = self.node_runner.node() {
            search_ledger(&node.ledger, input, &mut self.explorer);
            self.navigate(NavItem::Explorer);
        }
    }

    fn roll_back(&self) {
        if let Some(hash) = BlockHash::decode_hex(&self.explorer.rollback_hash)
            && let Some(node) = self.node_runner.node()
        {
            let _ = node.ledger.roll_back(&hash);
        }
    }

    fn process_node_event(&mut self, ev: NodeEvent) {
        match ev {
            NodeEvent::BlocksProcessed(results) => {
                for r in results {
                    if r.status.is_ok() {
                        self.block_processor
                            .recently_processed
                            .push_back(r.block.hash().to_string());
                        if self.block_processor.recently_processed.len() > 20 {
                            self.block_processor.recently_processed.pop_front();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
