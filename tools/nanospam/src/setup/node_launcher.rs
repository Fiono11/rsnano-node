use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use tokio::time::sleep;
use tracing::info;

use rsnano_rpc_client::NanoRpcClient;

use crate::{
    cli_args::CliArgs,
    node_lifetime::NodeLifetime,
    setup::{GENESIS_BLOCK, GENESIS_PRV, peering_port, pr_key},
};

const RPC_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) async fn start_nodes(
    args: &CliArgs,
    data_dir: std::path::PathBuf,
    rpc_clients: &[NanoRpcClient],
) -> Result<NodeLifetime> {
    // Keep every successfully spawned process guarded while later nodes start.
    // If this future is cancelled or returns an error, Drop cleans them all up.
    let mut nodes = NodeLifetime::default();
    let fixed_committee = (0..args.prs)
        .map(|i| pr_key(i).public_key().encode_hex())
        .collect::<Vec<_>>()
        .join(",");
    let epoch_start_file = data_dir.join("rai_epoch_start");
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        let mut node_dir = data_dir.clone();
        node_dir.push(format!("pr{i}"));

        let mut cmd = if args.cpp {
            let mut cmd = Command::new("nano_node");
            cmd.env("NANO_TEST_GENESIS_BLOCK", GENESIS_BLOCK)
                .env("NANO_TEST_GENESIS_PRV ", GENESIS_PRV)
                .env("NANO_RAI_FIXED_COMMITTEE", &fixed_committee)
                .env("NANO_RAI_EPOCH_START_FILE", &epoch_start_file)
                .env("NANO_RAI_EPOCH_DURATION_MS", "5000")
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
                .env("NANO_RAI_FIXED_COMMITTEE", &fixed_committee)
                .env("NANO_RAI_EPOCH_START_FILE", &epoch_start_file)
                .env("NANO_RAI_EPOCH_DURATION_MS", "5000")
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
        nodes.push(child);

        info!("Waiting for RPC...");
        let deadline = Instant::now() + RPC_STARTUP_TIMEOUT;
        while rpc_client.version().await.is_err() {
            if let Some(status) = nodes
                .last_mut()
                .expect("the node was just added")
                .try_wait()
                .context("could not query node process status")?
            {
                bail!("node PR{i} exited before RPC became ready: {status}");
            }
            if Instant::now() >= deadline {
                bail!(
                    "node PR{i} RPC did not become ready within {} seconds",
                    RPC_STARTUP_TIMEOUT.as_secs()
                );
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
                    rpc_client.keepalive("::1", peering_port(k)).await.unwrap();
                }
            }
        }
        // Give time to connect
        sleep(Duration::from_secs(5)).await;
    }
    Ok(nodes)
}
