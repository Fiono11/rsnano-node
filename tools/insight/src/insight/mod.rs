mod app;
mod block_processor;
mod bootstrap;
mod channels;
mod explorer;
mod frontier_scan;
mod gui;
mod message_collection;
mod message_rate_calculator;
mod message_recorder;
mod navigator;
mod node_callbacks;
mod node_runner;
mod nullable_runtime;
mod rep_names;
mod snapshot;

use eframe::egui;
use gui::MainView;
use rsnano_nullable_tracing_subscriber::TracingInitializer;

pub(crate) fn run_insight_app(app_name: &'static str) -> eframe::Result {
    TracingInitializer::default().init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1024.0, 768.0]),
        ..Default::default()
    };
    eframe::run_native(
        app_name,
        options,
        Box::new(|_| Ok(Box::new(MainView::new()))),
    )
}
