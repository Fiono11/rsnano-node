use std::sync::mpsc::Sender;

use eframe::egui::{
    Align, CentralPanel, Color32, Label, Layout, Panel, RichText, ScrollArea, TextEdit, Ui,
};
use egui_extras::{Column, Size, StripBuilder, TableBuilder};

use crate::insight::{
    app::InsightCommand,
    bootstrap::{
        BootstrapDetails, BootstrapInfo, BootstrapQueueViewModel, BootstrapViewType,
        PeerScoresViewModel,
    },
    gui::view_frontier_scan,
};

pub(crate) fn view_bootstrap(
    ui: &mut Ui,
    details: &BootstrapDetails,
    info: &mut BootstrapInfo,
    tx: &Sender<InsightCommand>,
) {
    Panel::top("bootstr_menu").show_inside(ui, |ui| {
        ui.horizontal(|ui| {
            for i in BootstrapViewType::all() {
                let selected = i == details.view_type();
                if ui.selectable_label(selected, i.as_str()).clicked() {
                    let _ = tx.send(InsightCommand::NavigateBootstrap(i));
                };
            }
        });
    });

    CentralPanel::default().show_inside(ui, |ui| match details {
        BootstrapDetails::BootstrapQueue(view_model) => {
            view_bootstrap_queue(ui, view_model, info, tx);
        }
        BootstrapDetails::PeerScores(view_model) => view_peer_scores(ui, view_model),
        BootstrapDetails::FrontierScan(view_model) => view_frontier_scan(ui, view_model, tx),
    });
}

pub(crate) fn view_bootstrap_queue(
    ui: &mut Ui,
    model: &BootstrapQueueViewModel,
    info: &mut BootstrapInfo,
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
                    TextEdit::singleline(&mut info.search)
                        .hint_text("filter account...")
                        .desired_width(300.0),
                );
                ui.separator();
                ui.add(
                    TextEdit::singleline(&mut info.add_account)
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

                        for account in &model.download_queue {
                            ui.horizontal(|ui| {
                                StripBuilder::new(ui)
                                    .size(Size::exact(40.0))
                                    .size(Size::remainder())
                                    .horizontal(|mut strip| {
                                        strip.cell(|ui| {
                                            ui.label(&account.priority);
                                        });
                                        strip.cell(|ui| {
                                            ui.label(&account.account);
                                        });
                                    });
                            });
                        }
                    });

                    strip.cell(|ui| {
                        ui.heading(format!("Downloading: {}", model.downloading_count));

                        for account in &model.downloading {
                            ui.horizontal(|ui| {
                                StripBuilder::new(ui)
                                    .size(Size::exact(40.0))
                                    .size(Size::remainder())
                                    .horizontal(|mut strip| {
                                        strip.cell(|ui| {
                                            ui.label(&account.priority);
                                        });
                                        strip.cell(|ui| {
                                            ui.label(&account.account);
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
                        for item in &model.blocked {
                            ui.horizontal(|ui| {
                                StripBuilder::new(ui)
                                    .size(Size::exact(170.0))
                                    .size(Size::exact(170.0))
                                    .size(Size::exact(170.0))
                                    .horizontal(|mut strip| {
                                        strip.cell(|ui| {
                                            if ui.link(&item.account).clicked() {
                                                info.search = item.account_val.encode_account();
                                            }
                                        });
                                        strip.cell(|ui| {
                                            ui.label(&item.dependency);
                                        });
                                        strip.cell(|ui| {
                                            if ui.link(&item.dependency_account).clicked() {
                                                info.search =
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

pub(crate) fn view_peer_scores(ui: &mut Ui, model: &PeerScoresViewModel) {
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
