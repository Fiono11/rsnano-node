use std::{
    collections::HashMap,
    sync::{Arc, RwLock, mpsc::Receiver},
    time::Duration,
};

use super::snapshot::{InsightSnapshot, take_snapshot};
use rsnano_network::ChannelId;
use rsnano_node::{NodeEvent, consensus::election::Election, representatives::QuorumSnapshot};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{Account, Amount, BlockHash, PublicKey, QualifiedRoot};

use crate::insight::{
    block_processor::BlockProcessorViewModel,
    bootstrap::{BootstrapInfo, BootstrapViewType},
    channels::Channels,
    explorer::{ExplorerViewModel, search_ledger},
    frontier_scan::FrontierScanInfo,
    message_collection::MessageCollection,
    message_recorder::MessageRecorder,
    navigator::{NavItem, Navigator},
    node_callbacks::NodeCallbackFactory,
    node_runner::NodeRunner,
    rep_names::well_known_rep_names,
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
}

pub(crate) struct InsightApp {
    pub explorer: ExplorerViewModel,
    pub block_processor: BlockProcessorViewModel,

    pub clock: Arc<SteadyClock>,
    pub messages: Arc<RwLock<MessageCollection>>,
    pub msg_recorder: Arc<MessageRecorder>,
    pub node_runner: NodeRunner,
    pub channels: Channels,
    pub navigator: Navigator,
    pub snapshot: InsightSnapshot,
    pub frontier_scan: FrontierScanInfo,
    pub bootstrap: BootstrapInfo,
    selected_election: Option<QualifiedRoot>,
    pub election_details: Option<Election>,
    bootstrap_view_type: BootstrapViewType,
    pub representatives: Vec<RepresentativeViewModel>,
    pub quorum: QuorumSnapshot,
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
        Self {
            explorer: Default::default(),
            block_processor: Default::default(),
            rx_cmd: rx,
            clock,
            messages,
            msg_recorder,
            node_runner: NodeRunner::new(callback_factory, tx_node_ev),
            channels,
            navigator: Navigator::new(),
            snapshot: InsightSnapshot::default(),
            frontier_scan: FrontierScanInfo::default(),
            last_update: None,
            bootstrap: Default::default(),
            selected_election: None,
            election_details: None,
            bootstrap_view_type: BootstrapViewType::BootstrapQueue,
            representatives: Vec::new(),
            rep_names,
            quorum: QuorumSnapshot::new_test_instance(),
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
        }
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
            self.navigator.current = NavItem::Explorer;
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

pub(crate) struct RepresentativeViewModel {
    pub account: Account,
    pub name: &'static str,
    pub weight: Amount,
    pub channel: Option<ChannelId>,
}
