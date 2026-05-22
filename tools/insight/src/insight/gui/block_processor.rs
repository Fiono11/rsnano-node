use eframe::egui::{CentralPanel, Ui};

pub(crate) fn view_block_processor(ui: &mut Ui) {
    CentralPanel::default().show_inside(ui, |ui| {
        ui.label("TODO");
    });
}
