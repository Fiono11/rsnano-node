use eframe::egui::{TextEdit, Ui};

use crate::insight::app::InsightCommand;
use std::sync::mpsc::Sender;

pub(crate) fn view_search_bar(ui: &mut Ui, input: &mut String, tx: &Sender<InsightCommand>) {
    let response = ui.add(
        TextEdit::singleline(input)
            .hint_text("account / block hash ...")
            .desired_width(450.0),
    );
    if response.changed() {
        let _ = tx.send(InsightCommand::Search(input.clone()));
    }
}
