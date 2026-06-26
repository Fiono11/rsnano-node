use std::sync::mpsc::Sender;

use eframe::egui::{CentralPanel, Grid, TextEdit, Ui};

use crate::insight::{app::InsightCommand, explorer::ExplorerViewModel};

pub(crate) struct ExplorerView<'a> {
    model: &'a mut ExplorerViewModel,
    tx: &'a Sender<InsightCommand>,
}

impl<'a> ExplorerView<'a> {
    pub(crate) fn new(model: &'a mut ExplorerViewModel, tx: &'a Sender<InsightCommand>) -> Self {
        Self { model, tx }
    }

    pub fn show(&mut self, ui: &mut Ui) {
        CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut self.model.rollback_hash)
                        .desired_width(400.0)
                        .hint_text("hash..."),
                );
                if ui.button("roll back block!").clicked() {
                    let _ = self.tx.send(InsightCommand::RollBack);
                }
            });
            ui.heading(format!("Block {}", self.model.hash));
            ui.add_space(20.0);
            Grid::new("block_grid").num_columns(2).show(ui, |ui| {
                ui.label("Destination: ");
                if ui.link(&self.model.destination).clicked() {
                    let _ = self
                        .tx
                        .send(InsightCommand::Search(self.model.destination.clone()));
                }
                ui.end_row();

                ui.label("Raw data: ");
                ui.label(&self.model.block);
                ui.end_row();

                ui.label("Subtype: ");
                ui.label(self.model.subtype);
                ui.end_row();

                ui.label("Amount: ");
                ui.label(&self.model.amount);
                ui.end_row();

                ui.label("Balance: ");
                ui.label(&self.model.balance);
                ui.end_row();

                ui.label("Height: ");
                ui.label(&self.model.height);
                ui.end_row();

                ui.label("Timestamp: ");
                ui.label(&self.model.timestamp);
                ui.end_row();

                ui.label("confirmed: ");
                ui.label(&self.model.confirmed);
                ui.end_row();
            });
        });
    }
}
