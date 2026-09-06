use std::{
    fs::File,
    process::{Command, Stdio},
    time::Duration,
};

use tokio::time::sleep;
use tracing::info;

use rsnano_rpc_client::NanoRpcClient;

use crate::{
    cli_args::CliArgs,
    setup::{GENESIS_BLOCK, GENESIS_PRV, peering_port, pr_balance_weights, pr_key},
};
use rsnano_types::Amount;

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
    let fixed_weights = pr_balance_weights(Amount::MAX, args.prs, args.fork_recipients)
        .into_iter()
        .enumerate()
        .map(|(i, weight)| {
            format!(
                "{}:{}",
                pr_key(i).public_key().encode_hex(),
                weight.number()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let node_keys = (0..args.prs)
        .map(|i| rsnano_types::PrivateKey::from(10_000 + i as u64))
        .collect::<Vec<_>>();
    let fixed_node_committee = node_keys
        .iter()
        .map(|key| key.public_key().encode_hex())
        .collect::<Vec<_>>()
        .join(",");
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        let mut node_dir = data_dir.clone();
        node_dir.push(format!("pr{i}"));
        std::fs::write(
            node_dir.join("node_id_private.key"),
            format!("{}\n", node_keys[i].raw_key().encode_hex()),
        )
        .unwrap();
        let stdout = File::create(node_dir.join("nanospam-node.log")).unwrap();
        let stderr = stdout.try_clone().unwrap();

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
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
            cmd
        } else {
            let mut cmd = Command::new("rsnano");
            cmd.env("NANO_TEST_GENESIS_BLOCK", GENESIS_BLOCK)
                .env("NANO_TEST_GENESIS_PRV ", GENESIS_PRV)
                .env(
                    "RUST_LOG",
                    "warn,rsnano_node::consensus::epochs::coordinator=info",
                )
                .arg("--network")
                .arg("test")
                .arg("--data-path")
                .arg(&node_dir)
                .arg("node")
                .arg("run")
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
            cmd
        };

        // Epoch reports need an explicit membership list, independent of ledger voting power.
        cmd.env("NANO_RAI_EPOCH_COMMITTEE", &fixed_committee);
        cmd.env("NANO_RAI_FIXED_WEIGHTS", &fixed_weights);
        cmd.env("NANO_RAI_NODE_COMMITTEE", &fixed_node_committee);

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
