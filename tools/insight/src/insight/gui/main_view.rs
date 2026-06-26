use eframe::egui::{
    self, global_theme_preference_switch, warn_if_debug_build, CentralPanel, Panel, Ui,
};

use rsnano_node::consensus::BucketSnapshot;
use rsnano_types::Amount;

use super::{
    block_processor::view_block_processor,
    bootstrap::{view_bootstrap, AccountViewModel, BootstrapQueueViewModel},
    formatted_number, view_ledger_stats, view_message_recorder_controls, view_message_tab,
    view_node_runner, view_peers, view_queue_group, view_search_bar, view_tabs, BlockViewModel,
    ChannelsViewModel, ExplorerView, FrontierScanViewModel, MessageStatsView,
    MessageStatsViewModel, MessageTableViewModel, QueueGroupViewModel, TabViewModel,
};
use crate::insight::{
    app::InsightApp,
    bootstrap::BootstrapViewType,
    explorer::ExplorerState,
    gui::{
        bootstrap::{BootstrapViewModel, PeerScoresViewModel},
        elections::{
            view_election_details, view_elections, BucketViewModel, ElectionDetailsViewModel,
            ElectionViewModel, ElectionsViewModel, RepVoteViewModel,
        },
        representatives::{view_representatives, RepresentativesViewModel},
        QueueViewModel,
    },
    navigator::NavItem,
};

pub(crate) struct MainView {
    model: MainViewModel,
}

impl MainView {
    pub(crate) fn new() -> Self {
        let model = MainViewModel::new();
        Self { model }
    }
}

impl MainView {
    fn view_controls_panel(&mut self, ui: &mut Ui) {
        Panel::top("controls_panel").show_inside(ui, |ui| {
            ui.add_space(1.0);
            ui.horizontal(|ui| {
                view_node_runner(ui, &mut self.model.app.node_runner);
                ui.separator();
                view_message_recorder_controls(ui, &self.model.app.msg_recorder);
                ui.separator();
                view_search_bar(ui, &mut self.model.search_input, &mut self.model.app);
            });
            ui.add_space(1.0);
        });
    }

    fn view_tabs(&mut self, ui: &mut Ui) {
        Panel::top("tabs_panel").show_inside(ui, |ui| {
            view_tabs(ui, &self.model.tabs(), &mut self.model.app.navigator);
        });
    }

    fn view_stats(&mut self, ui: &mut Ui) {
        Panel::bottom("bottom_panel").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                global_theme_preference_switch(ui);
                ui.separator();
                MessageStatsView::new(self.model.message_stats()).view(ui);
                ui.separator();
                view_ledger_stats(ui, &self.model.app.ledger_stats);
                warn_if_debug_build(ui);
            });
        });
    }
}

impl eframe::App for MainView {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.model.update();
        self.view_controls_panel(ui);
        self.view_tabs(ui);
        self.view_stats(ui);

        match self.model.app.navigator.current {
            NavItem::Peers => view_peers(ui, self.model.channels()),
            NavItem::Messages => view_message_tab(ui, &mut self.model),
            NavItem::Queues => view_queues(ui, self.model.queue_groups()),
            NavItem::Representatives => view_representatives(ui, self.model.representatives()),
            NavItem::BlockProcessor => view_block_processor(ui),
            NavItem::Elections => {
                if let Some(details) = self.model.election_details() {
                    view_election_details(ui, details, &mut self.model.app)
                } else {
                    view_elections(ui, self.model.elections(), &mut self.model.app)
                }
            }
            NavItem::Bootstrap => view_bootstrap(ui, self.model.bootstrap(), &mut self.model.app),
            NavItem::Explorer => {
                ExplorerView::new(&self.model.explorer(), &mut self.model.app).show(ui)
            }
        }

        // Repaint to show the continuously increasing current block and message counters
        ui.request_repaint();
    }
}

fn view_queues(ui: &mut Ui, groups: Vec<QueueGroupViewModel>) {
    CentralPanel::default().show_inside(ui, |ui| {
        for group in groups {
            view_queue_group(ui, group);
            ui.add_space(10.0);
        }
    });
}

pub(crate) struct MainViewModel {
    pub app: InsightApp,
    pub message_table: MessageTableViewModel,
    pub search_input: String,
}

impl MainViewModel {
    pub(crate) fn new() -> Self {
        let app = InsightApp::new();
        Self::for_app(app)
    }

    pub(crate) fn for_app(app: InsightApp) -> Self {
        let message_table = MessageTableViewModel::new(app.messages.clone());

        Self {
            app,
            message_table,
            search_input: String::new(),
        }
    }

    pub(crate) fn update(&mut self) {
        if !self.app.update() {
            return;
        }

        self.message_table.update_message_counts();
    }

    pub(crate) fn tabs(&self) -> Vec<TabViewModel> {
        self.app
            .navigator
            .all
            .iter()
            .map(|i| TabViewModel {
                selected: *i == self.app.navigator.current,
                label: i.name(),
                value: *i,
            })
            .collect()
    }

    pub(crate) fn message_stats(&self) -> MessageStatsViewModel<'_> {
        MessageStatsViewModel::new(&self.app.msg_recorder)
    }

    pub(crate) fn channels(&mut self) -> ChannelsViewModel<'_> {
        ChannelsViewModel::new(&mut self.app.channels)
    }

    pub(crate) fn queue_groups(&self) -> Vec<QueueGroupViewModel> {
        vec![
            QueueGroupViewModel {
                heading: "Active Elections".to_string(),
                queues: vec![
                    QueueViewModel::new(
                        "Priority",
                        self.app.snapshot.aec_info.priority,
                        self.app.snapshot.aec_info.max_elections,
                    ),
                    QueueViewModel::new("Hinted", self.app.snapshot.aec_info.hinted, self.app.snapshot.max_hinted),
                    QueueViewModel::new(
                        "Optimistic",
                        self.app.snapshot.aec_info.optimistic,
                        self.app.snapshot.max_optimistic,
                    ),
                    QueueViewModel::new(
                        "Total",
                        self.app.snapshot.aec_info.total,
                        self.app.snapshot.aec_info.max_elections,
                    ),
                ],
            },
            QueueGroupViewModel::for_fair_queue("Block Processor", &self.app.snapshot.block_processor_info),
            QueueGroupViewModel::for_fair_queue("Vote Processor", &self.app.snapshot.vote_processor_info),
            QueueGroupViewModel {
                heading: "Miscellaneous".to_string(),
                queues: vec![QueueViewModel::new(
                    "Confirming",
                    self.app.snapshot.confirming_set.size,
                    self.app.snapshot.confirming_set.max_size,
                )],
            },
        ]
    }

    pub fn representatives(&self) -> RepresentativesViewModel<'_> {
        RepresentativesViewModel {
            quorum: &self.app.quorum,
            reps: &self.app.representatives,
        }
    }

    pub fn bootstrap(&self) -> BootstrapViewModel {
        match self.app.bootstrap_view() {
            BootstrapViewType::BootstrapQueue => {
                let download_queue = self
                    .app
                    .bootstrap
                    .snapshot
                    .download_queue
                    .iter()
                    .map(|e| AccountViewModel::from(e))
                    .collect();

                let blocked = self
                    .app
                    .bootstrap
                    .snapshot
                    .blocked
                    .iter()
                    .map(|e| AccountViewModel::from(e))
                    .collect();

                let downloading = self
                    .app
                    .bootstrap
                    .snapshot
                    .downloading
                    .iter()
                    .map(|e| AccountViewModel::from(e))
                    .collect();

                let info = &self.app.bootstrap.snapshot.info;
                BootstrapViewModel::BootstrapQueue(BootstrapQueueViewModel {
                    download_queue_len: formatted_number(info.download_queue),
                    downloading_count: formatted_number(info.downloading),
                    blocked_accounts: formatted_number(info.blocked),
                    unblocked_accounts: formatted_number(info.unblocked),
                    process_queue: formatted_number(info.ready_to_process),
                    processing: formatted_number(info.processing),
                    unique_blocking_accounts: info.unique_blocking_accounts,
                    unknown_dependencies: info.unknown_dependencies,
                    cached_blocks: formatted_number(info.cached_blocks),
                    discarded_blocks: formatted_number(info.discarded_blocks),
                    download_queue,
                    downloading,
                    blocked,
                })
            }
            BootstrapViewType::PeerScores => BootstrapViewModel::PeerScores(PeerScoresViewModel {
                peers: self.app.peer_scores.clone(),
            }),
            BootstrapViewType::FrontierScan => {
                BootstrapViewModel::FrontierScan(self.frontier_scan())
            }
        }
    }

    pub fn elections(&self) -> ElectionsViewModel {
        if self.app.elections.buckets.len() < 33 {
            return Default::default();
        }
        let (col1, col2) = self.app.elections.buckets.split_at(33);
        ElectionsViewModel {
            bucket_col1: create_bucket_column(col1),
            bucket_col2: create_bucket_column(col2),
        }
    }

    pub fn election_details(&self) -> Option<ElectionDetailsViewModel> {
        self.app
            .election_details
            .as_ref()
            .map(|e| ElectionDetailsViewModel {
                winner_hash: e.winner().hash().encode_hex(),
                non_final_tally: e.winner_tally(),
                final_tally: e.winner_final_tally(),
                root: e.qualified_root().encode_hex(),
                behavior: e.behavior().as_str(),
                account: e.account().encode_account(),
                state: e.state().as_str(),
                candidate_blocks: e
                    .candidate_blocks()
                    .keys()
                    .map(|h| h.encode_hex())
                    .collect(),
                vote_count: e.vote_count().to_string(),
                phase: if e.is_final() {
                    "final voting"
                } else {
                    "non-final voting"
                },
                elapsed: format!(
                    "{} seconds",
                    e.start().elapsed(self.app.clock.now()).as_secs()
                ),
                non_final_votes: self
                    .app
                    .representatives
                    .iter()
                    .map(|r| RepVoteViewModel {
                        rep: if r.name.is_empty() {
                            r.account.encode_account()
                        } else {
                            r.name.to_string()
                        },
                        weight: r.weight,
                        voted: e.votes().contains_key(&r.account.as_key()),
                        is_final: e
                            .votes()
                            .get(&r.account.as_key())
                            .map(|i| i.is_final_vote())
                            .unwrap_or(false),
                    })
                    .collect(),
            })
    }

    pub fn explorer(&self) -> BlockViewModel {
        let mut view_model = BlockViewModel::default();
        if let ExplorerState::Block(b) = self.app.explorer.state() {
            view_model.show(b);
        }
        view_model
    }

    pub fn frontier_scan(&self) -> FrontierScanViewModel {
        FrontierScanViewModel::new(&self.app.frontier_scan)
    }
}

fn create_bucket_column(buckets: &[BucketSnapshot]) -> Vec<BucketViewModel> {
    buckets
        .iter()
        .map(|i| BucketViewModel {
            name: format!("Bucket {:02}", i.bucket_index),
            election_count: i.election_count,
            elections: i
                .elections
                .iter()
                .map(|election| {
                    let mut hash = election.winner_hash.encode_hex();
                    hash.truncate(6);
                    ElectionViewModel {
                        hash,
                        non_final_tally: to_short_tally(election.non_final_tally),
                        final_tally: to_short_tally(election.final_tally),
                        root: election.root.clone(),
                    }
                })
                .collect(),
        })
        .collect()
}

fn to_short_tally(tally: Amount) -> u16 {
    (tally.number() / Amount::nano(1_000_000).number()) as u16
}

pub(super) fn truncate_text(s: &mut String, len: usize) {
    if s.len() > len {
        s.replace_range(len.., "...");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_tally() {
        assert_eq!(
            108,
            to_short_tally(Amount::decode_dec("108902282988839324247169685594164037852").unwrap())
        );
    }
}
