use eframe::egui::Ui;
use egui_extras::{Size, StripBuilder};

use crate::insight::{gui::formatted_number, message_stats::MessageStatsViewModel};

pub fn view_message_stats(ui: &mut Ui, model: &MessageStatsViewModel) {
    ui.label("Messages");
    ui.label("out/s:");
    StripBuilder::new(ui)
        .size(Size::exact(35.0))
        .horizontal(|mut strip| {
            strip.cell(|ui| {
                ui.label(formatted_number(model.send_rate));
            })
        });

    ui.label("in/s:");
    StripBuilder::new(ui)
        .size(Size::exact(35.0))
        .horizontal(|mut strip| {
            strip.cell(|ui| {
                ui.label(formatted_number(model.receive_rate));
            })
        });
}
