mod cli;
use anyhow::Result;
use cli::run_cli;

#[cfg(not(feature = "banano"))]
compile_error!("The \"banano\" feature must be enabled to build rsban.");

fn main() -> Result<()> {
    run_cli()
}
