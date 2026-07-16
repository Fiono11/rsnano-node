mod app;
pub(crate) mod cli_args;
mod confirmation_receiver;
mod domain;
mod frontiers_sync;
mod handshake;
mod high_prio_check;
pub(crate) mod node_lifetime;
#[cfg(feature = "rai_protocol")]
mod rai_logging;
mod setup;
mod wallets_factory;

use crate::cli_args::CliArgs;
use app::NanoSpamApp;
use clap::Parser;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    let args = CliArgs::parse();
    NanoSpamApp::new(args).run().await
}
