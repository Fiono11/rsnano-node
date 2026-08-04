mod app;
pub(crate) mod cli_args;
mod confirmation_receiver;
mod confirmation_tracker;
mod domain;
mod frontiers_sync;
mod handshake;
mod high_prio_check;
pub(crate) mod node_lifetime;
mod setup;
mod wallets_factory;

use crate::cli_args::CommandLine;
use app::NanoSpamApp;
use clap::Parser;
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    let args = CommandLine::parse().into_args();
    args.validate()?;

    let shutdown = CancellationToken::new();
    let signal_shutdown = shutdown.clone();
    let signal_task = tokio::spawn(async move {
        shutdown_signal().await?;
        signal_shutdown.cancel();
        Ok::<_, std::io::Error>(())
    });

    let result = NanoSpamApp::new(args).run(shutdown.clone()).await;
    signal_task.abort();

    if shutdown.is_cancelled() {
        Err(anyhow::anyhow!(
            "interrupted; spawned nodes were terminated"
        ))
    } else {
        result
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}
