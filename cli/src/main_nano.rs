mod cli;

use anyhow::Result;
use cli::run_cli;

#[cfg(feature = "banano")]
compile_error!("The \"banano\" feature must not be enabled to build rsnano.");

fn main() -> Result<()> {
    run_cli()
}
