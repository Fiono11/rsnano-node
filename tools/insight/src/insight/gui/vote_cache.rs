use eframe::egui::{Align, CentralPanel, Layout, Ui};

use crate::insight::{gui::nano_amount_string, vote_cache::VoteCacheViewModel};
use egui_extras::{Column, TableBuilder};

pub(crate) fn view_vote_cache(ui: &mut Ui, model: &mut VoteCacheViewModel) {
    CentralPanel::default().show_inside(ui, |ui| {
        ui.heading("Vote Cache");
        ui.label(&format!("Cached blocks: {}", model.cached_blocks));
        ui.text_edit_singleline(&mut model.search);
        ui.add_space(6.0);
        if !model.block_votes.is_empty() {
            ui.heading("Votes");

            TableBuilder::new(ui)
                .striped(true)
                .resizable(false)
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink(false)
                .column(Column::exact(350.0)) // account
                .column(Column::exact(100.0)) //rep weight
                .column(Column::auto()) // final
                .column(Column::remainder())
                .header(20.0, |mut header| {
                    header.col(|ui| {
                        ui.strong("Representative");
                    });
                    header.col(|ui| {
                        ui.strong("Weight");
                    });
                    header.col(|ui| {
                        ui.strong("Final");
                    });
                })
                .body(|body| {
                    body.rows(20.0, model.block_votes.len(), |mut row| {
                        let Some(row_model) = model.block_votes.get(row.index()) else {
                            return;
                        };
                        row.col(|ui| {
                            ui.label(&row_model.rep_key.as_account().encode_account());
                        });
                        row.col(|ui| {
                            ui.label(nano_amount_string(row_model.weight));
                        });
                        row.col(|ui| {
                            if row_model.is_final {
                                ui.label("✔");
                            }
                        });
                    })
                });
        }
    });
}
