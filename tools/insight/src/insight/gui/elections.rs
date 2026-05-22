use eframe::egui::{self, CentralPanel, FontId, Grid, RichText, Ui};
use egui_extras::{Size, StripBuilder};

use rsnano_types::QualifiedRoot;

use crate::insight::app::InsightApp;

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

pub(crate) fn view_election_details(
    ctx: &egui::Context,
    model: ElectionDetailsViewModel,
    app: &mut InsightApp,
) {
    CentralPanel::default().show(ctx, |ui| {
        ui.horizontal(|ui| {
            if ui.link("Elections").clicked() {
                app.close_election();
            };
            ui.label(" > ");
            ui.label(&model.winner_hash);
        });
        ui.add_space(16.0);
        ui.heading(format!("Block {}", model.winner_hash));

        Grid::new("election_grid").num_columns(2).show(ui, |ui| {
            ui.label("Account ");
            ui.label(model.account);
            ui.end_row();

            ui.label("Qualified root ");
            ui.label(model.root);
            ui.end_row();

            ui.label("Behavior ");
            ui.label(model.behavior);
            ui.end_row();

            ui.label("State ");
            ui.label(model.state);
            ui.end_row();

            ui.label("Vote count ");
            ui.label(model.vote_count);
            ui.end_row();

            ui.label("Phase ");
            ui.label(model.phase);
            ui.end_row();

            ui.label("Elapsed ");
            ui.label(model.elapsed);
            ui.end_row();

            ui.label("Non final tally ");
            ui.label(model.non_final_tally);
            ui.end_row();

            ui.label("Final tally ");
            ui.label(model.final_tally);
            ui.end_row();

            for (i, block) in model.candidate_blocks.iter().enumerate() {
                ui.label(format!("Candidate {} ", i));
                ui.label(block);
                ui.end_row();
            }
        });
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
    pub behavior: &'static str,
    pub account: String,
    pub state: &'static str,
    pub candidate_blocks: Vec<String>,
    pub vote_count: String,
    pub phase: &'static str,
    pub elapsed: String,
}
