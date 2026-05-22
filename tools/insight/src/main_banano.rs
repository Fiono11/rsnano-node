mod insight;

use crate::insight::run_insight_app;

#[cfg(not(feature = "banano"))]
compile_error!("The \"banano\" feature must be enabled to build rsban-insight.");

fn main() -> eframe::Result {
    run_insight_app("RsBan Insight")
}
