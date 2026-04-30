use crate::app::InsightApp;
use eframe::egui::{self, CentralPanel, FontId, RichText, Ui};
use egui_extras::{Size, StripBuilder};
use rsnano_types::QualifiedRoot;

pub(crate) fn view_elections(ctx: &egui::Context, model: ElectionsViewModel, app: &mut InsightApp) {
    CentralPanel::default().show(ctx, |ui| {
        StripBuilder::new(ui)
            .size(Size::remainder())
            .size(Size::remainder())
            .horizontal(|mut strip| {
                strip.cell(|ui| view_bucket_column(ui, model.bucket_col1, app));
                strip.cell(|ui| view_bucket_column(ui, model.bucket_col2, app));
            });
    });
}

fn view_bucket_column(ui: &mut Ui, buckets: Vec<BucketViewModel>, app: &mut InsightApp) {
    for bucket in buckets {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} ({:>3}): ", bucket.name, bucket.election_count))
                    .font(FontId::monospace(11.0)),
            );
            for election in bucket.elections {
                if ui
                    .link(format!(
                        "[{} {:03}/{:03}] ",
                        election.hash, election.non_final_tally, election.final_tally
                    ))
                    .clicked()
                {
                    app.show_election(election.root);
                }
            }
        });
    }
}

pub(crate) fn view_election_details(ctx: &egui::Context, model: ElectionDetailsViewModel) {
    CentralPanel::default().show(ctx, |ui| {
        ui.heading(model.winner_hash);
        ui.separator();
        ui.label("qualified root:");
        ui.label(model.root);
        ui.separator();
        ui.label("non final tally:");
        ui.label(model.non_final_tally);
        ui.separator();
        ui.label("final tally:");
        ui.label(model.final_tally);
    });
}

#[derive(Default)]
pub(crate) struct ElectionsViewModel {
    pub bucket_col1: Vec<BucketViewModel>,
    pub bucket_col2: Vec<BucketViewModel>,
}

pub(crate) struct BucketViewModel {
    pub name: String,
    pub election_count: usize,
    pub elections: Vec<ElectionViewModel>,
}

pub(crate) struct ElectionViewModel {
    pub hash: String,
    pub non_final_tally: u16,
    pub final_tally: u16,
    pub root: QualifiedRoot,
}

pub(crate) struct ElectionDetailsViewModel {
    pub winner_hash: String,
    pub non_final_tally: String,
    pub final_tally: String,
    pub root: String,
}
