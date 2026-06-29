use std::sync::mpsc::Sender;

use eframe::egui::{
    self, CentralPanel, Panel, Ui, global_theme_preference_switch, warn_if_debug_build,
};

use super::{
    ExplorerView, block_processor::view_block_processor, bootstrap::view_bootstrap,
    view_ledger_stats, view_message_recorder_controls, view_message_tab, view_node_runner,
    view_peers, view_queue_group, view_search_bar, view_tabs,
};
use crate::insight::{
    app::{InsightApp, InsightCommand},
    gui::{
        elections::{view_election_details, view_elections},
        representatives::view_representatives,
        view_message_stats,
        vote_cache::view_vote_cache,
    },
    navigator::NavItem,
    queues::QueueGroupViewModel,
};

pub(crate) struct MainView {
    app: InsightApp,
    tx: Sender<InsightCommand>,
}

impl MainView {
    pub(crate) fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let app = InsightApp::new(rx);
        Self { app, tx }
    }
}

impl MainView {
    fn view_controls_panel(&mut self, ui: &mut Ui) {
        Panel::top("controls_panel").show_inside(ui, |ui| {
            ui.add_space(1.0);
            ui.horizontal(|ui| {
                view_node_runner(ui, &mut self.app.node_runner);
                ui.separator();
                view_message_recorder_controls(ui, &self.app.msg_recorder);
                ui.separator();
                view_search_bar(ui, &mut self.app.search_input, &self.tx);
            });
            ui.add_space(1.0);
        });
    }

    fn view_tabs(&mut self, ui: &mut Ui) {
        Panel::top("tabs_panel").show_inside(ui, |ui| {
            view_tabs(ui, &self.app.tabs, &self.tx);
        });
    }

    fn view_stats(&mut self, ui: &mut Ui) {
        Panel::bottom("bottom_panel").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                global_theme_preference_switch(ui);
                ui.separator();
                view_message_stats(ui, &self.app.message_stats);
                ui.separator();
                view_ledger_stats(ui, &self.app.snapshot.ledger_stats);
                warn_if_debug_build(ui);
            });
        });
    }
}

impl eframe::App for MainView {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.update();
        self.view_controls_panel(ui);
        self.view_tabs(ui);
        self.view_stats(ui);

        match self.app.current_tab {
            NavItem::Peers => view_peers(ui, self.app.channels_model()),
            NavItem::Messages => view_message_tab(ui, &mut self.app),
            NavItem::Queues => view_queues(ui, &self.app.queue_groups),
            NavItem::Representatives => view_representatives(ui, &self.app.representatives),
            NavItem::BlockProcessor => view_block_processor(ui, &self.app.block_processor),
            NavItem::Elections => {
                if let Some(details) = self.app.election_details() {
                    view_election_details(ui, details, &self.tx)
                } else {
                    view_elections(ui, self.app.elections(), &self.tx)
                }
            }
            NavItem::VoteCache => view_vote_cache(ui, &mut self.app.vote_cache),
            NavItem::Bootstrap => view_bootstrap(
                ui,
                &self.app.bootstrap_details,
                &mut self.app.bootstrap,
                &self.tx,
            ),
            NavItem::Explorer => ExplorerView::new(&mut self.app.explorer, &self.tx).show(ui),
        }

        // Repaint to show the continuously increasing current block and message counters
        ui.request_repaint();
    }
}

fn view_queues(ui: &mut Ui, groups: &[QueueGroupViewModel]) {
    CentralPanel::default().show_inside(ui, |ui| {
        for group in groups {
            view_queue_group(ui, group);
            ui.add_space(10.0);
        }
    });
}
