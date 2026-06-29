use eframe::egui::{Align, Label, Layout, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use super::view_rep_state;
use crate::insight::channels::ChannelsViewModel;

pub(crate) fn view_channels<'a>(ui: &mut Ui, mut model: ChannelsViewModel<'a>) {
    ui.add_space(5.0);
    ui.heading(model.heading());
    TableBuilder::new(ui)
        .striped(true)
        .resizable(false)
        .auto_shrink(false)
        .cell_layout(Layout::left_to_right(Align::Center))
        .sense(Sense::click())
        .column(Column::auto())
        .column(Column::auto()) // rep state
        .column(Column::remainder())
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.strong("Channel");
            });
            header.col(|ui| {
                ui.strong("Rep");
            });
            header.col(|ui| {
                ui.strong("Remote Addr");
            });
        })
        .body(|body| {
            body.rows(20.0, model.channel_count(), |mut row| {
                let Some(row_model) = model.get_row(row.index()) else {
                    return;
                };
                if row_model.is_selected {
                    row.set_selected(true);
                }
                row.col(|ui| {
                    ui.add(Label::new(row_model.channel_id).selectable(false));
                });
                row.col(|ui| {
                    view_rep_state(ui, row_model.rep_state);
                });
                row.col(|ui| {
                    ui.add(Label::new(row_model.remote_addr).selectable(false));
                });
                if row.response().clicked() {
                    model.select(row.index());
                }
            })
        });
}
