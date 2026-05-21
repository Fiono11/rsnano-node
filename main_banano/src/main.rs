use anyhow::Result;
use rsnano_cli::run_cli;

#[cfg(not(feature = "banano"))]
compile_error!("The \"banano\" feature must be enabled to build rsban.");

fn main() -> Result<()> {
    run_cli()
}
