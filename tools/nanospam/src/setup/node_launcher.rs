use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use tokio::time::sleep;
use tracing::info;

use rsnano_rpc_client::NanoRpcClient;

use crate::{
    cli_args::CliArgs,
    node_lifetime::NodeLifetime,
    setup::{GENESIS_BLOCK, GENESIS_PRV, peering_port},
};

const RPC_START_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) async fn start_nodes(
    args: &CliArgs,
    data_dir: std::path::PathBuf,
    rpc_clients: &[NanoRpcClient],
    node_lifetime: &mut NodeLifetime,
) -> anyhow::Result<()> {
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        if rpc_client.version().await.is_ok() {
            bail!(
                "PR{i} RPC is already responding before startup; another nanospam network is likely using the fixed test ports"
            );
        }
    }

    // Spawn the complete committee before waiting for RPC so their epoch-zero
    // clocks begin as close together as process startup permits.
    for i in 0..rpc_clients.len() {
        let mut node_dir = data_dir.clone();
        node_dir.push(format!("pr{i}"));

        let mut cmd = if args.cpp {
            let mut cmd = Command::new("nano_node");
            cmd.env("NANO_TEST_GENESIS_BLOCK", GENESIS_BLOCK)
                .env("NANO_TEST_GENESIS_PRV ", GENESIS_PRV)
                .env("NANO_TEST_EPOCH_1", "0")
                .env("NANO_TEST_EPOCH_2", "0")
                .env("NANO_TEST_EPOCH_2_RECV", "0")
                .arg("--network")
                .arg("test")
                .arg("--data_path")
                .arg(&node_dir)
                .arg("--daemon")
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd
        } else {
            let mut cmd = Command::new("rsnano");
            cmd.env("NANO_TEST_GENESIS_BLOCK", GENESIS_BLOCK)
                .env("NANO_TEST_GENESIS_PRV ", GENESIS_PRV)
                .arg("--network")
                .arg("test")
                .arg("--data-path")
                .arg(&node_dir)
                .arg("node")
                .arg("run")
                .stdout(Stdio::null());
            cmd
        };

        info!("Starting node: {cmd:?}");
        let child = cmd
            .spawn()
            .with_context(|| format!("could not start node PR{i}"))?;
        node_lifetime.track(child);
    }

    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        info!("Waiting for PR{i} RPC...");
        let started = Instant::now();
        while rpc_client.version().await.is_err() {
            if let Some(status) = node_lifetime.child_status(i)? {
                bail!("node PR{i} exited before its RPC started: {status}");
            }
            if started.elapsed() >= RPC_START_TIMEOUT {
                bail!("node PR{i} RPC did not start within {RPC_START_TIMEOUT:?}");
            }
            sleep(Duration::from_millis(100)).await;
        }
    }

    if args.cpp {
        // Send keepalives so that nano_node connects (their preconfigured peers don't allow ports)!
        info!("Sending keepalives...");
        for (i, rpc_client) in rpc_clients.iter().enumerate() {
            for k in 0..args.prs {
                if k != i {
                    rpc_client.keepalive("::1", peering_port(k)).await?;
                }
            }
        }
        // Give time to connect
        sleep(Duration::from_secs(5)).await;
    }
    Ok(())
}
