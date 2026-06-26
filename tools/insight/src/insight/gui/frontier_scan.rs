use std::sync::mpsc::Sender;

use eframe::egui::{CentralPanel, ProgressBar, ScrollArea, Ui};
use egui_extras::{Size, StripBuilder};

use rsnano_types::Account;

use super::formatted_number;
use crate::insight::{app::InsightCommand, frontier_scan::FrontierScanInfo};

pub(crate) fn view_frontier_scan(
    ui: &mut Ui,
    model: &FrontierScanViewModel,
    tx: &Sender<InsightCommand>,
) {
    FrontierScanView::new(model, tx).show(ui)
}

struct FrontierScanView<'a> {
    model: &'a FrontierScanViewModel,
    tx: &'a Sender<InsightCommand>,
}

impl<'a> FrontierScanView<'a> {
    fn new(model: &'a FrontierScanViewModel, tx: &'a Sender<InsightCommand>) -> Self {
        Self { model, tx }
    }

    fn show(self, ui: &mut Ui) {
        CentralPanel::default().show_inside(ui, |ui| {
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(&self.model.frontiers_rate);
                    ui.add_space(100.0);
                    ui.heading(&self.model.outdated_rate);
                    ui.add_space(50.0);
                    ui.label(&self.model.frontiers_total);
                    ui.add_space(50.0);
                    ui.label(&self.model.outdated_total);
                });

                for heads in self.model.frontier_heads.chunks(4) {
                    ui.horizontal(|ui| {
                        for head in heads {
                            StripBuilder::new(ui)
                                .size(Size::exact(50.0))
                                .size(Size::exact(120.0))
                                .size(Size::exact(100.0))
                                .horizontal(|mut strip| {
                                    strip.cell(|ui| {
                                        ui.label(&head.start);
                                    });
                                    strip.cell(|ui| {
                                        ui.add(
                                            ProgressBar::new(head.done_normalized)
                                                .text(&head.current),
                                        );
                                    });
                                    strip.cell(|ui| {
                                        ui.label(&head.end);
                                    });
                                });
                        }
                    });
                }
                ui.add_space(20.0);

                ui.heading("Outdated accounts found:");
                for account in &self.model.outdated_accounts {
                    if ui.link(account.clone()).clicked() {
                        let _ = self.tx.send(InsightCommand::Search(account.clone()));
                    }
                }
            });
        });
    }
}

pub(crate) struct FrontierScanViewModel {
    pub frontiers_rate: String,
    pub outdated_rate: String,
    pub frontiers_total: String,
    pub outdated_total: String,
    pub frontier_heads: Vec<FrontierHeadViewModel>,
    pub outdated_accounts: Vec<String>,
}

impl FrontierScanViewModel {
    pub(crate) fn new(info: &FrontierScanInfo) -> Self {
        let frontier_heads = info
            .frontier_heads
            .iter()
            .map(|head| FrontierHeadViewModel {
                start: abbrev(&head.start),
                end: abbrev(&head.end),
                current: format!("{:.1}%", head.done_normalized() * 100.0),
                done_normalized: head.done_normalized(),
            })
            .collect();

        let outdated_accounts = info
            .outdated_accounts
            .iter()
            .map(|a| a.encode_account())
            .collect();

        Self {
            frontiers_rate: format!("{} frontiers/s", formatted_number(info.frontiers_rate())),
            outdated_rate: format!("{} outdated/s", formatted_number(info.outdated_rate())),
            frontiers_total: format!("{} frontiers total", formatted_number(info.frontiers_total)),
            outdated_total: format!("{} outdated total", formatted_number(info.outdated_total)),
            frontier_heads,
            outdated_accounts,
        }
    }
}

pub(crate) struct FrontierHeadViewModel {
    pub start: String,
    pub end: String,
    pub current: String,
    pub done_normalized: f32,
}

fn abbrev(account: &Account) -> String {
    let mut result = account.encode_hex();
    result.truncate(4);
    result.push_str("...");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviate_acccount() {
        assert_eq!(abbrev(&Account::from(0)), "0000...");
    }
}
