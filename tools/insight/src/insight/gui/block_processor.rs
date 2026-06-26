use crate::insight::block_processor::BlockProcessorViewModel;
use eframe::egui::{CentralPanel, Ui};

pub(crate) fn view_block_processor(ui: &mut Ui, model: &BlockProcessorViewModel) {
    CentralPanel::default().show_inside(ui, |ui| {
        ui.heading("Recently processed blocks");

        for block in &model.recently_processed {
            ui.label(block);
        }
    });
}
