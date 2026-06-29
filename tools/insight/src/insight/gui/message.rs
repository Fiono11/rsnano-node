use eframe::egui::{Grid, ScrollArea, Ui};

use crate::insight::messages::MessageViewModel;

pub(crate) fn view_message(ui: &mut Ui, model: &MessageViewModel) {
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        Grid::new("details_grid").num_columns(2).show(ui, |ui| {
            ui.label("Date:");
            ui.label(model.date.clone());
            ui.end_row();

            ui.label("Channel:");
            ui.label(model.channel_id.clone());
            ui.end_row();

            ui.label("Direction:");
            ui.label(model.direction.clone());
            ui.end_row();

            ui.label("Type:");
            ui.label(model.message_type.clone());
            ui.end_row();
        });

        ui.add_space(20.0);
        ui.label(&model.message);
    });
}
