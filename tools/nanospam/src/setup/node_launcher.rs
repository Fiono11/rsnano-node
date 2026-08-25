use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use tokio::time::sleep;
use tracing::info;

use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::PeersDto;

use crate::{
    cli_args::CliArgs,
    setup::{GENESIS_BLOCK, GENESIS_PRV, peering_port, pr_key},
};

#[cfg(feature = "rai_protocol")]
pub(crate) const EPOCH_START_MARKER: &str = "rai_epoch_start";
#[cfg(feature = "rai_protocol")]
pub(crate) const CLOSE_METRICS_FILE: &str = "rai_close_metrics";

pub(crate) async fn start_nodes(
    args: &CliArgs,
    data_dir: std::path::PathBuf,
    rpc_clients: &[NanoRpcClient],
) -> Vec<std::process::Child> {
    let mut children = Vec::new();
    let fixed_committee = (0..args.prs)
        .map(|i| pr_key(i).public_key().encode_hex())
        .collect::<Vec<_>>()
        .join(",");
    let fixed_committee_ports = (0..args.prs)
        .map(|i| peering_port(i).to_string())
        .collect::<Vec<_>>()
        .join(",");
    #[cfg(feature = "rai_protocol")]
    let epoch_start_marker = data_dir.join(EPOCH_START_MARKER);
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        let mut node_dir = data_dir.clone();
        node_dir.push(format!("pr{i}"));

        let mut cmd = if args.cpp {
            let mut cmd = Command::new("nano_node");
            cmd.env("NANO_TEST_GENESIS_BLOCK", GENESIS_BLOCK)
                .env("NANO_TEST_GENESIS_PRV ", GENESIS_PRV)
                .env("NANO_RAI_FIXED_COMMITTEE", &fixed_committee)
                .env(
                    "NANO_RAI_FIXED_COMMITTEE_PEERING_PORTS",
                    &fixed_committee_ports,
                )
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
                .env(
                    "NANO_RAI_LOCAL_REPRESENTATIVE",
                    pr_key(i).public_key().encode_hex(),
                )
                .env("NANO_RAI_NODE_INDEX", i.to_string())
                .arg("--network")
                .arg("test")
                .arg("--data-path")
                .arg(&node_dir)
                .arg("node")
                .arg("run")
                .stdout(Stdio::null());
            #[cfg(feature = "rai_protocol")]
            if args.epoch_duration > 0 {
                cmd.env(
                    "NANO_RAI_EPOCH_DURATION_SECONDS",
                    args.epoch_duration.to_string(),
                )
                .env("NANO_RAI_EPOCH_START_DELAY_SECONDS", "5")
                .env("NANO_RAI_EPOCH_START_FILE", &epoch_start_marker)
                .stdout(Stdio::inherit());
                cmd.env(
                    "NANO_RAI_CLOSE_METRICS_FILE",
                    data_dir.join(format!("{CLOSE_METRICS_FILE}_pr{i}")),
                );
            }
            cmd
        };

        info!("Starting node: {cmd:?}");
        children.push(cmd.spawn().unwrap());

        info!("Waiting for RPC...");
        while rpc_client.version().await.is_err() {
            sleep(Duration::from_millis(100)).await;
        }
    }

    // Establish the PR mesh explicitly. Preconfigured-peer discovery is
    // asynchronous and RPC readiness alone does not mean the PRs are connected.
    info!("Connecting all PRs...");
    let started = Instant::now();
    loop {
        for (i, rpc_client) in rpc_clients.iter().enumerate() {
            for k in 0..args.prs {
                if k != i {
                    rpc_client.keepalive("::1", peering_port(k)).await.unwrap();
                }
            }
        }

        let mut peer_counts = Vec::with_capacity(rpc_clients.len());
        for rpc_client in rpc_clients {
            let count = match rpc_client.peers(Some(false)).await {
                Ok(PeersDto::Simple(response)) => response.peers.len(),
                Ok(PeersDto::Detailed(response)) => response.peers.len(),
                Err(_) => 0,
            };
            peer_counts.push(count);
        }
        let expected_peers = args.prs.saturating_sub(1);
        if peer_counts.iter().all(|count| *count >= expected_peers) {
            info!(?peer_counts, "All PRs connected");
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "PR mesh did not form: peer counts {peer_counts:?}, expected at least {}",
            expected_peers
        );
        sleep(Duration::from_millis(100)).await;
    }

    children
}
