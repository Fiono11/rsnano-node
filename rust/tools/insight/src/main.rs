mod app;
mod app_model;
mod node_factory;
mod nullable_runtime;

use app::InsightApp;
use app_model::AppModel;
use eframe::egui;
use tokio::runtime::Runtime;

fn main() -> eframe::Result {
    let runtime = Runtime::new().unwrap();
    let model = AppModel::with_runtime(runtime.handle().clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1024.0, 768.0]),
        ..Default::default()
    };
    eframe::run_native(
        "RsNano Insight",
        options,
        Box::new(|cc| Ok(Box::new(InsightApp::new(model)))),
    )
}
