use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use tokio::time::sleep;
use tracing::info;

use rsnano_rpc_client::NanoRpcClient;

use crate::{
    cli_args::CliArgs,
    setup::{GENESIS_BLOCK, GENESIS_PRV, peering_port},
};

pub(crate) async fn start_nodes(
    args: &CliArgs,
    data_dir: std::path::PathBuf,
    rpc_clients: &[NanoRpcClient],
) -> Vec<std::process::Child> {
    let mut children = Vec::new();
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
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
            let mut cmd = Command::new(rsnano_binary(args));
            cmd.env("NANO_TEST_GENESIS_BLOCK", GENESIS_BLOCK)
                .env("NANO_TEST_GENESIS_PRV ", GENESIS_PRV)
                .arg("--network")
                .arg("test")
                .arg("--data-path")
                .arg(&node_dir)
                .arg("node")
                .arg("run");
            cmd
        };

        if args.summary_only {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }

        info!("Starting node: {cmd:?}");
        children.push(cmd.spawn().unwrap());

        info!("Waiting for RPC...");
        while rpc_client.version().await.is_err() {
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
    children
}

fn rsnano_binary(args: &CliArgs) -> PathBuf {
    args.rsnano
        .clone()
        .or_else(sibling_rsnano_binary)
        .unwrap_or_else(|| PathBuf::from("rsnano"))
}

fn sibling_rsnano_binary() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    let binary_name = format!("rsnano{}", std::env::consts::EXE_SUFFIX);
    let candidate = current_exe.with_file_name(binary_name);
    candidate.is_file().then_some(candidate)
}
