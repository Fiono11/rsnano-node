mod cli;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, CliInfrastructure, CommandLineArgs};

pub fn run_cli() -> Result<()> {
    let args = CommandLineArgs::parse();
    let mut infra = CliInfrastructure::default();
    Cli {}.run(&mut infra, args)?;
    Ok(())
}
