use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use tokio::time::sleep;
use tracing::info;

use rsnano_rpc_client::NanoRpcClient;

use crate::{
    cli_args::CliArgs,
    setup::{GENESIS_BLOCK, GENESIS_PRV, RemoteNode, launcher_script, peering_port},
};

pub(crate) async fn start_nodes(
    args: &CliArgs,
    data_dir: &Path,
    rpc_clients: &[NanoRpcClient],
) -> Vec<Child> {
    let mut children = Vec::new();
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        if let Some(remote) = remote_node(args, data_dir, i) {
            info!("Copying PR{i}'s data directory to its machine...");
            remote
                .push(&remote_launcher_script(args, data_dir, i))
                .unwrap();
        }
        let mut cmd = node_command(args, data_dir, i);

        info!("Starting node: {cmd:?}");
        children.push(cmd.spawn().unwrap());
        write_node_pids(data_dir, &children).unwrap();

        info!("Waiting for RPC...");
        while rpc_client.version().await.is_err() {
            sleep(Duration::from_millis(100)).await;
        }
    }

    if args.cpp {
        // Send keepalives so that nano_node connects (their preconfigured peers don't allow ports)!
        info!("Sending keepalives...");
        for (i, rpc_client) in rpc_clients.iter().enumerate() {
            for k in 0..args.honest_prs() {
                if k != i {
                    rpc_client
                        .keepalive(&args.hosts().address(k).to_string(), peering_port(k))
                        .await
                        .unwrap();
                }
            }
        }
        // Give time to connect
        sleep(Duration::from_secs(5)).await;
    }
    children
}

/// The command that starts node PR`i` on its data directory: at the start
/// and again on a restart. Its diagnostics are tagged `pr<i>`.
pub(crate) fn node_command(args: &CliArgs, data_dir: &Path, i: usize) -> Command {
    let node_dir = node_dir(data_dir, i);
    if args.cpp {
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
        return cmd;
    }
    let mut cmd = match remote_node(args, data_dir, i) {
        // The script pushed by start_nodes holds what the local branch sets
        Some(remote) => remote.start_command(),
        None => {
            let mut cmd = Command::new(rsnano_program(args, i));
            cmd.envs(rsnano_env(&node_dir, i))
                .args(rsnano_args(&node_dir));
            cmd
        }
    };
    cmd.stdout(Stdio::null());
    cmd
}

/// PR`i` when `--hosts` places it on another machine
pub(crate) fn remote_node(args: &CliArgs, data_dir: &Path, i: usize) -> Option<RemoteNode> {
    args.hosts()
        .ssh_target(i)
        .map(|target| RemoteNode::new(target, node_dir(data_dir, i)))
}

/// Every running representative's remote handle, None for a local one
pub(crate) fn remote_nodes(args: &CliArgs, data_dir: &Path) -> Vec<Option<RemoteNode>> {
    (0..args.honest_prs())
        .map(|i| remote_node(args, data_dir, i))
        .collect()
}

/// The launch script of a remote PR`i`: the same environment and arguments
/// as a local start, the binary from `--remote-bin-dir`
fn remote_launcher_script(args: &CliArgs, data_dir: &Path, i: usize) -> String {
    let node_dir = node_dir(data_dir, i);
    let program = rsnano_program(args, i);
    let program = match (&args.remote_bin_dir, program.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => program,
    };
    let mut env = rsnano_env(&node_dir, i);
    env.extend(forwarded_switches(std::env::vars()));
    launcher_script(&program, &env, &rsnano_args(&node_dir), &node_dir)
}

/// The node's `RAI_*` switches (such as `RAI_FAST_PATH`) set for this
/// process: a local node inherits them, a remote one gets them through its
/// launch script, so that a switch holds on every machine of a run
fn forwarded_switches(
    vars: impl IntoIterator<Item = (String, String)>,
) -> Vec<(&'static str, String)> {
    let mut switches: Vec<_> = vars
        .into_iter()
        .filter_map(|(name, value)| {
            let known = NODE_SWITCHES.iter().find(|known| **known == name)?;
            Some((*known, value))
        })
        .collect();
    switches.sort();
    switches
}

fn node_dir(data_dir: &Path, i: usize) -> PathBuf {
    data_dir.join(format!("pr{i}"))
}

fn rsnano_program(args: &CliArgs, i: usize) -> PathBuf {
    match (args.byzantine_node_index(), &args.byzantine_node_bin) {
        (Some(index), Some(binary)) if index == i => binary.clone(),
        _ => "rsnano".into(),
    }
}

fn rsnano_env(node_dir: &Path, i: usize) -> Vec<(&'static str, String)> {
    vec![
        ("NANO_TEST_GENESIS_BLOCK", GENESIS_BLOCK.to_owned()),
        ("NANO_TEST_GENESIS_PRV ", GENESIS_PRV.to_owned()),
        (NODE_TAG_VAR, format!("pr{i}")),
        (
            DIAG_FILE_VAR,
            node_dir.join("diagnostics.log").display().to_string(),
        ),
    ]
}

fn rsnano_args(node_dir: &Path) -> Vec<String> {
    vec![
        "--network".to_owned(),
        "test".to_owned(),
        "--data-path".to_owned(),
        node_dir.display().to_string(),
        "node".to_owned(),
        "run".to_owned(),
    ]
}

/// Lets the harness stop precisely the children of this run; line i is PR`i`
pub(crate) fn write_node_pids(data_dir: &Path, children: &[Child]) -> std::io::Result<()> {
    std::fs::write(
        data_dir.join("node-pids"),
        children
            .iter()
            .map(|child| format!("{}\n", child.id()))
            .collect::<String>(),
    )
}

/// Names the node in its diagnostic lines (read by the rsnano node)
const NODE_TAG_VAR: &str = "RSNANO_NODE_TAG";
/// Where the node appends its diagnostic lines too, in its data directory:
/// they outlive nanospam's own log (read by the rsnano node)
const DIAG_FILE_VAR: &str = "RSNANO_DIAG_FILE";
/// The node's A/B switches that must reach every machine of a run (read by
/// the rsnano node)
const NODE_SWITCHES: [&str; 3] = ["RAI_FAST_PATH", "RAI_CLOSE_MEMO", "RAI_CLOSE_MEMO_CHECK"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_only_the_node_switches() {
        let vars = [
            ("PATH".to_owned(), "/bin".to_owned()),
            ("RAI_FAST_PATH".to_owned(), "0".to_owned()),
            ("RAI_BENCH".to_owned(), "1".to_owned()),
            ("RAI_CLOSE_MEMO".to_owned(), "1".to_owned()),
        ];

        assert_eq!(
            forwarded_switches(vars),
            vec![
                ("RAI_CLOSE_MEMO", "1".to_owned()),
                ("RAI_FAST_PATH", "0".to_owned())
            ]
        );
    }

    #[test]
    fn forwards_nothing_when_no_switch_is_set() {
        assert_eq!(
            forwarded_switches([("HOME".to_owned(), "/root".to_owned())]),
            vec![]
        );
    }
}
