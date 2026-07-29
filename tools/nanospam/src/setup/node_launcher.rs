use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use tokio::time::sleep;
use tracing::info;

use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::PeersDto;

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

            // Epoch-close diagnostics are logged at `info`, while nodes normally
            // run at `warn`. Keep unrelated output quiet but expose the complete
            // RAI close path for nanospam runs. Preserve the caller's filter,
            // adding the diagnostic directive only when it was not specified.
            let mut rust_log = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned());
            for directive in [
                "rsnano_node::consensus::rai=info",
                "rsnano_node::bootstrap::bootstrapper::rai_epoch_bootstrap=debug",
            ] {
                let target = directive.split_once('=').unwrap().0;
                if !rust_log.contains(target) {
                    rust_log.push(',');
                    rust_log.push_str(directive);
                }
            }
            cmd.env("RUST_LOG", rust_log);
            cmd.env("NANOSPAM_PR_INDEX", i.to_string());
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

    wait_for_pr_mesh(rpc_clients).await;

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

async fn wait_for_pr_mesh(rpc_clients: &[NanoRpcClient]) {
    let expected_peers = rpc_clients.len().saturating_sub(1);
    if expected_peers == 0 {
        return;
    }

    info!(
        expected_peers,
        "Waiting for all PRs to form the initial peer mesh..."
    );
    let mut consecutive_ready_checks = 0;
    loop {
        let mut all_ready = true;
        for (index, rpc_client) in rpc_clients.iter().enumerate() {
            let peer_count = match rpc_client.peers(None).await {
                Ok(PeersDto::Simple(peers)) => peers.peers.len(),
                Ok(PeersDto::Detailed(peers)) => peers.peers.len(),
                Err(_) => 0,
            };
            if peer_count < expected_peers {
                all_ready = false;
                info!(
                    pr = index,
                    peer_count, expected_peers, "PR is not ready yet"
                );
            }
        }

        if all_ready {
            consecutive_ready_checks += 1;
            if consecutive_ready_checks >= 3 {
                info!("All PRs are connected and ready");
                return;
            }
        } else {
            consecutive_ready_checks = 0;
        }

        sleep(Duration::from_millis(250)).await;
    }
}

pub(crate) async fn wait_for_pr_ledgers(rpc_clients: &[NanoRpcClient]) {
    info!("Waiting for every PR to finalize the common setup ledger...");
    let mut consecutive_ready_checks = 0;
    loop {
        let mut counts = Vec::with_capacity(rpc_clients.len());
        #[cfg(not(feature = "rai_protocol"))]
        let mut all_finalized = true;
        #[cfg(feature = "rai_protocol")]
        let mut all_finalized = true;
        for rpc_client in rpc_clients {
            match rpc_client.block_count().await {
                Ok(count) => {
                    let total = count.count.inner();
                    let cemented = count.cemented.inner();
                    #[cfg(not(feature = "rai_protocol"))]
                    {
                        all_finalized &= total == cemented;
                    }
                    counts.push((total, cemented));
                }
                Err(_) => {
                    all_finalized = false;
                    counts.push((0, 0));
                }
            }
        }

        // RAI finality is represented by certified close records, while the
        // legacy cemented counter is not its readiness signal. Every setup
        // block has already been finalized on PR0 and explicitly published to
        // each follower, so equal stable block counts are the required restart
        // barrier. Non-RAI builds additionally require legacy cementation.
        let same_ledger = counts
            .first()
            .is_some_and(|first| counts.iter().all(|count| count.0 == first.0));
        if all_finalized && same_ledger {
            consecutive_ready_checks += 1;
            if consecutive_ready_checks >= 3 {
                info!(?counts, "Every PR has finalized the common setup ledger");
                return;
            }
        } else {
            consecutive_ready_checks = 0;
            info!(?counts, "PR setup ledgers have not converged yet");
        }

        sleep(Duration::from_millis(250)).await;
    }
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
