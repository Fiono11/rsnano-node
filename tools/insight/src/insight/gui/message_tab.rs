use eframe::egui::{CentralPanel, Panel, Ui};

use super::MessageTableView;
use crate::insight::{
    app::InsightApp,
    gui::{channels::view_channels, view_message},
    messages::MessageTableViewModel,
};

pub(crate) fn view_message_tab(ui: &mut Ui, app: &mut InsightApp) {
    channels_left_panel(ui, app);
    messages_panel(ui, &mut app.message_table);
    message_details_panel(ui, &app.message_table);
}

fn channels_left_panel(ui: &mut Ui, app: &mut InsightApp) {
    Panel::left("channels_panel")
        .min_size(350.0)
        .resizable(false)
        .show_inside(ui, |ui| {
            view_channels(ui, app.channels_model());
        });
}

fn messages_panel(ui: &mut Ui, messages: &mut MessageTableViewModel) {
    Panel::left("messages_panel")
        .min_size(250.0)
        .resizable(true)
        .show_inside(ui, |ui| {
            MessageTableView::new(messages).view(ui);
        });
}

fn message_details_panel(ui: &mut Ui, model: &MessageTableViewModel) {
    CentralPanel::default().show_inside(ui, |ui| {
        ui.heading("Message details");
        if let Some(details) = model.selected_message() {
            view_message(ui, &details);
        }
    });
}
