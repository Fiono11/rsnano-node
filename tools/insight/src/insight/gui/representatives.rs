use crate::insight::{app::RepresentativeViewModel, gui::formatted_number};
use eframe::egui::{Align, CentralPanel, Layout, Ui};
use egui_extras::{Column, TableBuilder};
use rsnano_types::Amount;

pub(crate) fn view_representatives(ui: &mut Ui, model: RepresentativesViewModel) {
    CentralPanel::default().show_inside(ui, |ui| {
        ui.heading("Representatives");
        TableBuilder::new(ui)
            .striped(true)
            .resizable(false)
            .cell_layout(Layout::left_to_right(Align::Center))
            .auto_shrink(false)
            .column(Column::exact(350.0)) // account
            .column(Column::exact(150.0)) // name
            .column(Column::exact(80.0)) //rep weight
            .column(Column::auto())
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Account");
                });
                header.col(|ui| {
                    ui.strong("Name");
                });
                header.col(|ui| {
                    ui.strong("Weight");
                });
            })
            .body(|body| {
                body.rows(20.0, model.reps.len(), |mut row| {
                    let Some(row_model) = model.reps.get(row.index()) else {
                        return;
                    };
                    row.col(|ui| {
                        ui.label(row_model.account.encode_account());
                    });
                    row.col(|ui| {
                        ui.label(row_model.name);
                    });
                    row.col(|ui| {
                        ui.label(formatted_number(
                            row_model.weight.number() / Amount::nano(1).number(),
                        ));
                    });
                })
            });
    });
}

pub(crate) struct RepresentativesViewModel<'a> {
    pub reps: &'a [RepresentativeViewModel],
}
