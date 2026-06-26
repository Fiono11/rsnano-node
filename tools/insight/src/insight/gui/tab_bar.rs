use std::sync::mpsc::Sender;

use eframe::egui::Ui;

use crate::insight::{app::InsightCommand, navigator::TabViewModel};

pub(crate) fn view_tabs(ui: &mut Ui, tabs: &[TabViewModel], tx: &Sender<InsightCommand>) {
    ui.horizontal(|ui| {
        for tab in tabs {
            if ui.selectable_label(tab.selected, tab.label).clicked() {
                let _ = tx.send(InsightCommand::Navigate(tab.value));
            }
        }
    });
}
