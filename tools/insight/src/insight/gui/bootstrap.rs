use eframe::egui::{
    Align, CentralPanel, Color32, Label, Layout, Panel, RichText, ScrollArea, TextEdit, Ui,
};
use egui_extras::{Column, Size, StripBuilder, TableBuilder};

use rsnano_node::bootstrap::bootstrapper::{BootstrappingAccountInfo, PeerScoreSnapshot};
use rsnano_types::Account;

use crate::insight::{
    app::{InsightApp, InsightCommand},
    bootstrap::BootstrapViewType,
    gui::{FrontierScanViewModel, view_frontier_scan},
};

use super::main_view::truncate_text;
use std::sync::mpsc::Sender;

pub(crate) fn view_bootstrap(
    ui: &mut Ui,
    view_model: BootstrapViewModel,
    app: &mut InsightApp,
    tx: &Sender<InsightCommand>,
) {
    Panel::top("bootstr_menu").show_inside(ui, |ui| {
        ui.horizontal(|ui| {
            for i in BootstrapViewType::all() {
                let selected = i == view_model.view_type();
                if ui.selectable_label(selected, i.as_str()).clicked() {
                    app.select_bootstrap_view(i);
                };
            }
        });
    });

    CentralPanel::default().show_inside(ui, |ui| match view_model {
        BootstrapViewModel::BootstrapQueue(view_model) => {
            view_bootstrap_queue(ui, view_model, app, tx);
        }
        BootstrapViewModel::PeerScores(view_model) => view_peer_scores(ui, view_model),
        BootstrapViewModel::FrontierScan(view_model) => view_frontier_scan(ui, view_model, app),
    });
}

pub(crate) fn view_bootstrap_queue(
    ui: &mut Ui,
    model: BootstrapQueueViewModel,
    app: &mut InsightApp,
    tx: &Sender<InsightCommand>,
) {
    CentralPanel::default().show_inside(ui, |ui| {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.heading("Bootstrap Queue");
            ui.add_space(16.0);

            ui.horizontal(|ui| {
                StripBuilder::new(ui)
                    .size(Size::exact(160.0))
                    .size(Size::exact(160.0))
                    .size(Size::exact(160.0))
                    .size(Size::exact(160.0))
                    .size(Size::exact(160.0))
                    .horizontal(|mut strip| {
                        strip.cell(|ui| {
                            ui.label(format!("Unblocked: {}", model.unblocked_accounts));
                        });
                        strip.cell(|ui| {
                            ui.label(format!("Ready to process: {}", model.process_queue));
                        });
                        strip.cell(|ui| {
                            ui.label(format!("Processing: {}", model.processing));
                        });
                        strip.cell(|ui| {
                            ui.label(format!("Cached blocks: {}", model.cached_blocks));
                        });
                        strip.cell(|ui| {
                            ui.label(format!("Discarded blocks: {}", model.discarded_blocks));
                        });
                    });
            });

            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut app.bootstrap.search)
                        .hint_text("filter account...")
                        .desired_width(300.0),
                );
                ui.separator();
                ui.add(
                    TextEdit::singleline(&mut app.bootstrap.add_account)
                        .hint_text("account...")
                        .desired_width(300.0),
                );
                if ui.button("add account").clicked() {
                    let _ = tx.send(InsightCommand::AddAccountToBootstrapQueue);
                }
            });

            ui.add_space(16.0);

            StripBuilder::new(ui)
                .size(Size::relative(0.28))
                .size(Size::relative(0.28))
                .size(Size::remainder())
                .horizontal(|mut strip| {
                    strip.cell(|ui| {
                        ui.heading(format!("Download queue: {}", model.download_queue_len));

                        ui.horizontal(|ui| {
                            StripBuilder::new(ui)
                                .size(Size::exact(40.0))
                                .size(Size::remainder())
                                .horizontal(|mut strip| {
                                    strip.cell(|ui| {
                                        ui.strong("Priority");
                                    });
                                    strip.cell(|ui| {
                                        ui.strong("Account");
                                    });
                                });
                        });

                        for account in model.download_queue {
                            ui.horizontal(|ui| {
                                StripBuilder::new(ui)
                                    .size(Size::exact(40.0))
                                    .size(Size::remainder())
                                    .horizontal(|mut strip| {
                                        strip.cell(|ui| {
                                            ui.label(account.priority);
                                        });
                                        strip.cell(|ui| {
                                            ui.label(account.account);
                                        });
                                    });
                            });
                        }
                    });

                    strip.cell(|ui| {
                        ui.heading(format!("Downloading: {}", model.downloading_count));

                        for account in model.downloading {
                            ui.horizontal(|ui| {
                                StripBuilder::new(ui)
                                    .size(Size::exact(40.0))
                                    .size(Size::remainder())
                                    .horizontal(|mut strip| {
                                        strip.cell(|ui| {
                                            ui.label(account.priority);
                                        });
                                        strip.cell(|ui| {
                                            ui.label(account.account);
                                        });
                                    });
                            });
                        }
                    });

                    strip.cell(|ui| {
                        ui.horizontal(|ui| {
                            ui.heading(format!("Blocked accounts: {}", model.blocked_accounts));
                        });
                        ui.horizontal(|ui| {
                            ui.label(format!("unknown senders: {}", model.unknown_dependencies));
                            ui.add_space(50.0);
                            ui.label(format!(
                                "unique senders: {}",
                                model.unique_blocking_accounts
                            ));
                        });
                        ui.horizontal(|ui| {
                            if ui.button("clear blocked accounts").clicked() {
                                let _ = tx.send(InsightCommand::ClearBlockedBootstrapAccounts);
                            }
                            if ui.button("verify").clicked() {
                                let _ = tx.send(InsightCommand::VerifyBlockedBootstrapAccounts);
                            }
                            if ui.button("print processing").clicked() {
                                let _ = tx.send(InsightCommand::PrintProcessingBootstrapBlocks);
                            }
                        });

                        ui.horizontal(|ui| {
                            StripBuilder::new(ui)
                                .size(Size::exact(170.0))
                                .size(Size::exact(170.0))
                                .size(Size::exact(170.0))
                                .horizontal(|mut strip| {
                                    strip.cell(|ui| {
                                        ui.strong("Blocked account");
                                    });
                                    strip.cell(|ui| {
                                        ui.strong("Missing send");
                                    });
                                    strip.cell(|ui| {
                                        ui.strong("Sender");
                                    });
                                });
                        });
                        for item in model.blocked {
                            ui.horizontal(|ui| {
                                StripBuilder::new(ui)
                                    .size(Size::exact(170.0))
                                    .size(Size::exact(170.0))
                                    .size(Size::exact(170.0))
                                    .horizontal(|mut strip| {
                                        strip.cell(|ui| {
                                            if ui.link(item.account).clicked() {
                                                app.bootstrap.search =
                                                    item.account_val.encode_account();
                                            }
                                        });
                                        strip.cell(|ui| {
                                            ui.label(item.dependency);
                                        });
                                        strip.cell(|ui| {
                                            if ui.link(item.dependency_account).clicked() {
                                                app.bootstrap.search =
                                                    item.dependency_account_val.encode_account();
                                            }
                                        });
                                    });
                            });
                        }
                    });
                });
        });
    });
}

pub enum BootstrapViewModel {
    BootstrapQueue(BootstrapQueueViewModel),
    PeerScores(PeerScoresViewModel),
    FrontierScan(FrontierScanViewModel),
}
impl BootstrapViewModel {
    fn view_type(&self) -> BootstrapViewType {
        match self {
            BootstrapViewModel::BootstrapQueue(_) => BootstrapViewType::BootstrapQueue,
            BootstrapViewModel::PeerScores(_) => BootstrapViewType::PeerScores,
            BootstrapViewModel::FrontierScan(_) => BootstrapViewType::FrontierScan,
        }
    }
}

pub(crate) struct BootstrapQueueViewModel {
    pub download_queue_len: String,
    pub blocked_accounts: String,
    pub unblocked_accounts: String,
    pub process_queue: String,
    pub processing: String,
    pub downloading_count: String,
    pub unique_blocking_accounts: usize,
    pub unknown_dependencies: usize,
    pub cached_blocks: String,
    pub discarded_blocks: String,
    pub download_queue: Vec<AccountViewModel>,
    pub downloading: Vec<AccountViewModel>,
    pub blocked: Vec<AccountViewModel>,
}

pub(crate) struct AccountViewModel {
    pub account: String,
    pub priority: String,
    pub dependency: String,
    pub dependency_account: String,
    pub account_val: Account,
    pub dependency_account_val: Account,
}

impl From<&BootstrappingAccountInfo> for AccountViewModel {
    fn from(e: &BootstrappingAccountInfo) -> Self {
        let mut account = e.account.encode_account();
        let mut dependency = e.dependency_block.to_string();
        let mut dependency_account = e.dependency_account.encode_account();
        truncate_text(&mut account, 20);
        truncate_text(&mut dependency, 15);
        truncate_text(&mut dependency_account, 20);
        Self {
            account,
            priority: format!("{:.2}", e.priority.as_f64()),
            dependency,
            dependency_account,
            account_val: e.account,
            dependency_account_val: e.dependency_account,
        }
    }
}

pub(crate) fn view_peer_scores(ui: &mut Ui, model: PeerScoresViewModel) {
    ui.heading("Peer Scores");
    TableBuilder::new(ui)
        .striped(true)
        .resizable(false)
        .cell_layout(Layout::left_to_right(Align::Center))
        .auto_shrink(false)
        .column(Column::auto())
        .column(Column::auto())
        .column(Column::auto())
        .column(Column::auto())
        .column(Column::auto())
        .column(Column::auto())
        .column(Column::auto())
        .column(Column::auto())
        .column(Column::remainder())
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.strong("Channel");
            });
            header.col(|ui| {
                ui.strong("Running Queries");
            });
            header.col(|ui| {
                ui.strong("Priority");
            });
            header.col(|ui| {
                ui.strong("Timeouts");
            });
            header.col(|ui| {
                ui.strong("Requests");
            });
            header.col(|ui| {
                ui.strong("Responses");
            });
            header.col(|ui| {
                ui.strong("Channel Full");
            });
            header.col(|ui| {
                ui.strong("Blocks received");
            });
            header.col(|ui| {
                ui.strong("Out of date");
            });
        })
        .body(|body| {
            body.rows(20.0, model.peers.len(), |mut row| {
                let peer = &model.peers[row.index()];
                row.col(|ui| {
                    ui.add(Label::new(peer.channel_id.to_string()).selectable(false));
                });
                row.col(|ui| {
                    ui.add(Label::new(&peer.running_queries.to_string()).selectable(false));
                });
                row.col(|ui| {
                    if peer.priority < 0.0 {
                        ui.add(
                            Label::new(
                                RichText::new(format!("{:.1}", peer.priority)).color(Color32::RED),
                            )
                            .selectable(false),
                        );
                    } else {
                        ui.add(Label::new(format!("{:.1}", peer.priority)).selectable(false));
                    }
                });
                row.col(|ui| {
                    ui.add(Label::new(peer.timeouts.to_string()).selectable(false));
                });
                row.col(|ui| {
                    ui.add(Label::new(peer.requests.to_string()).selectable(false));
                });
                row.col(|ui| {
                    ui.add(Label::new(peer.responses.to_string()).selectable(false));
                });
                row.col(|ui| {
                    ui.add(Label::new(peer.channel_full.to_string()).selectable(false));
                });
                row.col(|ui| {
                    ui.add(Label::new(peer.blocks_received.to_string()).selectable(false));
                });
                row.col(|ui| {
                    ui.add(Label::new(peer.out_of_date.to_string()).selectable(false));
                });
            })
        });
}

pub(crate) struct PeerScoresViewModel {
    pub peers: Vec<PeerScoreSnapshot>,
}
