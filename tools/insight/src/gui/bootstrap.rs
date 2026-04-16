use eframe::egui::{self, CentralPanel, ScrollArea, TextEdit};
use egui_extras::{Size, StripBuilder};

use rsnano_types::Account;

use crate::app::InsightApp;

pub(crate) fn view_bootstrap(ctx: &egui::Context, model: BootstrapViewModel, app: &mut InsightApp) {
    CentralPanel::default().show(ctx, |ui| {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.heading("Bootstrap Queue");
            ui.add_space(16.0);

            ui.horizontal(|ui| {
                ui.label(format!("Unblocked accounts: {}", model.unblocked_accounts));
                ui.label(format!("Process queue: {}", model.process_queue));
                ui.label(format!("Processing: {}", model.processing));
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
                    app.add_priority_account();
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

                        for item in model.download_queue {
                            ui.horizontal(|ui| {
                                StripBuilder::new(ui)
                                    .size(Size::exact(40.0))
                                    .size(Size::remainder())
                                    .horizontal(|mut strip| {
                                        strip.cell(|ui| {
                                            ui.label(item.priority);
                                        });
                                        strip.cell(|ui| {
                                            ui.label(item.account);
                                        });
                                    });
                            });
                        }
                    });

                    strip.cell(|ui| {
                        ui.heading(format!("Downloading: {}", model.downloading_count));

                        for item in model.downloading {}
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
                            if ui.button("Clear blocked accounts").clicked() {
                                app.clear_blocked_accounts();
                            }
                            if ui.button("Consistency check").clicked() {
                                app.blocked_consistency_check();
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

pub(crate) struct BootstrapViewModel {
    pub download_queue_len: String,
    pub blocked_accounts: String,
    pub unblocked_accounts: String,
    pub process_queue: String,
    pub processing: String,
    pub downloading_count: String,
    pub unique_blocking_accounts: usize,
    pub unknown_dependencies: usize,
    pub download_queue: Vec<AccountViewModel>,
    pub downloading: Vec<AccountViewModel>,
    pub blocked: Vec<BlockedViewModel>,
}

pub(crate) struct AccountViewModel {
    pub account: String,
    pub priority: String,
    pub dependency: String,
    pub dependency_account: String,
    pub account_val: Account,
    pub dependency_account_val: Account,
}

pub(crate) struct BlockedViewModel {
    pub account: String,
    pub dependency: String,
    pub dependency_account: String,
    pub account_val: Account,
    pub dependency_account_val: Account,
}
