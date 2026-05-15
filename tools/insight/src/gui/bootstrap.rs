use eframe::egui::{self, CentralPanel, ScrollArea, TextEdit};
use egui_extras::{Size, StripBuilder};

use rsnano_node::bootstrap::bootstrapper::BootstrappingAccountInfo;
use rsnano_types::Account;

use crate::app::InsightApp;

use super::main_view::truncate_text;

pub(crate) fn view_bootstrap(ctx: &egui::Context, model: BootstrapViewModel, app: &mut InsightApp) {
    CentralPanel::default().show(ctx, |ui| {
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
                                app.clear_blocked_accounts();
                            }
                            if ui.button("verify").clicked() {
                                app.verify_blocked_accounts();
                            }
                            if ui.button("print processing").clicked() {
                                app.print_processing();
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
