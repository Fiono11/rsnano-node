use crate::insight::{gui::nano_amount_string, representatives::RepresentativesViewModel};
use eframe::egui::{Align, CentralPanel, Layout, Ui};
use egui_extras::{Column, TableBuilder};

pub(crate) fn view_representatives(ui: &mut Ui, model: &RepresentativesViewModel) {
    CentralPanel::default().show_inside(ui, |ui| {
        ui.heading("Representatives");
        ui.label(format!(
            "trended: {}",
            nano_amount_string(model.quorum.trended_or_min_weight)
        ));
        ui.label(format!(
            "online: {}",
            nano_amount_string(model.quorum.online_weight)
        ));
        ui.label(format!(
            "peered: {}",
            nano_amount_string(model.quorum.peered_weight)
        ));
        ui.label(format!(
            "quorum: {}",
            nano_amount_string(model.quorum.quorum_delta)
        ));
        ui.add_space(6.0);
        TableBuilder::new(ui)
            .striped(true)
            .resizable(false)
            .cell_layout(Layout::left_to_right(Align::Center))
            .auto_shrink(false)
            .column(Column::exact(350.0)) // account
            .column(Column::exact(150.0)) // name
            .column(Column::exact(100.0)) //rep weight
            .column(Column::auto())
            .column(Column::remainder())
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
                header.col(|ui| {
                    ui.strong("Channel");
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
                        ui.label(nano_amount_string(row_model.weight));
                    });
                    row.col(|ui| {
                        if let Some(id) = row_model.channel {
                            ui.label(id.to_string());
                        }
                    });
                })
            });
    });
}
