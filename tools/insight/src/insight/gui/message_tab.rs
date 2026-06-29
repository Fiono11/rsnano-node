use eframe::egui::{CentralPanel, Panel, Ui};

use super::{MessageTableView, MessageView, channels::ChannelsView};
use crate::insight::app::InsightApp;

pub(crate) fn view_message_tab(ui: &mut Ui, app: &mut InsightApp) {
    MessageTabView::new(app).show(ui);
}

struct MessageTabView<'a> {
    app: &'a mut InsightApp,
}

impl<'a> MessageTabView<'a> {
    fn new(app: &'a mut InsightApp) -> Self {
        Self { app }
    }

    fn show(&mut self, ui: &mut Ui) {
        self.show_channels(ui);
        self.show_message_overview(ui);
        self.show_message_details(ui);
    }

    fn show_channels(&mut self, ui: &mut Ui) {
        Panel::left("channels_panel")
            .min_size(350.0)
            .resizable(false)
            .show_inside(ui, |ui| {
                ChannelsView::new(self.app.channels_model()).view(ui);
            });
    }

    fn show_message_overview(&mut self, ui: &mut Ui) {
        Panel::left("messages_panel")
            .min_size(250.0)
            .resizable(true)
            .show_inside(ui, |ui| {
                MessageTableView::new(&mut self.app.message_table).view(ui);
            });
    }

    fn show_message_details(&mut self, ui: &mut Ui) {
        CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("Message details");
            if let Some(details) = self.app.message_table.selected_message() {
                MessageView::new(&details).view(ui);
            }
        });
    }
}
