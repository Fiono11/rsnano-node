mod insight;

use crate::insight::run_insight_app;

#[cfg(feature = "banano")]
compile_error!("The \"banano\" feature must not be enabled to build rsnano-insight.");

fn main() -> eframe::Result {
    run_insight_app("RsNano Insight")
}
