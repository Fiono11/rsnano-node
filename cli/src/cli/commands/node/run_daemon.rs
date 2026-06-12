use crate::cli::GlobalArgs;
use clap::Parser;
use rsnano_daemon::DaemonBuilder;
use rsnano_node::config::NodeFlags;
use rsnano_nullable_tracing_subscriber::TracingInitializer;

#[derive(Parser, PartialEq, Debug)]
pub(crate) struct RunDaemonArgs {
    /// Turn off automatic wallet backup process
    #[arg(long)]
    disable_backup: bool,
    /// Turn off the request loop
    #[arg(long)]
    disable_request_loop: bool,
    /// Turn off the rep crawler process
    #[arg(long)]
    disable_rep_crawler: bool,
    /// Do not provide any telemetry data to nodes requesting it. Responses are still made to requests, but they will have an empty payload.
    #[arg(long)]
    disable_providing_telemetry_metrics: bool,
    /// Disables block republishing by disabling the local_block_broadcaster component
    #[arg(long)]
    disable_block_processor_republishing: bool,
    /// Enable voting
    #[arg(long)]
    enable_voting: bool,
    /// Skip ledger consistency check on startup, this is not recommended and should only be used for testing or recovery purposes
    #[arg(long)]
    skip_consistency_check: bool,
}

impl RunDaemonArgs {
    pub(crate) fn run_daemon(&self, global_args: GlobalArgs) -> anyhow::Result<()> {
        TracingInitializer::default().init();
        let network = global_args.network;
        let flags = self.get_flags();
        DaemonBuilder::new(network)
            .flags(flags)
            .data_path(&global_args.data_path)
            .run(shutdown_signal())
    }

    pub(crate) fn get_flags(&self) -> NodeFlags {
        let mut flags = NodeFlags::new();
        flags.disable_backup = self.disable_backup;
        flags.disable_rep_crawler = self.disable_rep_crawler;
        flags.disable_request_loop = self.disable_request_loop;
        flags.disable_providing_telemetry_metrics = self.disable_providing_telemetry_metrics;
        flags.disable_block_processor_republishing = self.disable_block_processor_republishing;
        flags.enable_voting = self.enable_voting;
        flags.skip_consistency_check = self.skip_consistency_check;
        flags
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
