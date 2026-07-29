use std::{fs::remove_dir_all, path::Path};

use tracing::info;

use crate::cli_args::CliArgs;
use rsnano_types::{Block, BlockHash, PrivateKey};

#[cfg(feature = "rai_protocol")]
const DEFAULT_RAI_EPOCH_DURATION_MS: u64 = 30_000;
#[cfg(feature = "rai_protocol")]
const DEFAULT_RAI_CLOSE_ATTEMPT_DURATION_MS: u64 = 3_000;
#[cfg(feature = "rai_protocol")]
const DEFAULT_RAI_TICK_INTERVAL_MS: u64 = 250;
#[cfg(feature = "rai_protocol")]
const RAI_SETUP_EPOCH_DURATION_MS: u64 = 30 * 1000;

pub(crate) const GENESIS_BLOCK: &str = r#"{
    "type": "open",
    "account": "nano_3nroioygg54nusrmyun4woimqex36sp3drnctdt5955uqu47fxbkrxk7n7ne",
    "source": "D315857CE70C54DE713F6E82E5613BB3A1266C15E28AD2F4338C7BBEC456F532",
    "representative": "nano_3nroioygg54nusrmyun4woimqex36sp3drnctdt5955uqu47fxbkrxk7n7ne",
    "signature": "3F6792C2DC623DF2E8643777160AB983B66B337E2478E13D2C3448126A8F4CD8DCCD19803C158A057FA44060AE0EFC09B1C311CB4FBF42F8D240610B38F56E08",
    "work": "70FEF01F7EC45DEC"
    }"#;

pub(crate) const GENESIS_PRV: &str =
    "49643F9B10CA1AA34F9AF8ED4AABD29F436104CCC375974B108534A48EAE3FE1";

pub(crate) const NODE_CONFIG: &str = r#"
[node]
    peering_port = PEERING_PORT
    allow_local_peers = true
    bandwidth_limit = 0
    enable_voting = true
    preconfigured_peers = PRECONF_PEERS
    preconfigured_representatives = PRECONF_REPS
    database_backend = "DB_BACKEND"
    cps_limit = CPS_LIMIT

[node.lmdb]
    sync = "nosync_unsafe"

[node.network]
    max_peers_per_ip = 256

[node.bounded_backlog]
    enable = false

RAI_CONFIG

[node.bootstrap_server]
    # default 500
    limiter = 500

[node.bootstrap]
    # default 500
    rate_limit = 500

    # default 16
    channel_limit = 64

[node.monitor]
    interval = 10

[node.websocket]
    enable = true
    address = "::"
    port = WS_PORT

[rpc]
    enable = true
"#;

pub(crate) const RPC_CONFIG: &str = r#"
address = "::"
enable_control = true
port = RPC_PORT
"#;

#[cfg(not(feature = "rai_protocol"))]
pub(crate) fn configure_nodes(args: &CliArgs, data_dir: &Path) {
    configure_nodes_with_epoch_duration(args, data_dir, None);
}

#[cfg(feature = "rai_protocol")]
pub(crate) fn configure_nodes_for_setup(args: &CliArgs, data_dir: &Path) {
    configure_nodes_with_epoch_duration(args, data_dir, Some(RAI_SETUP_EPOCH_DURATION_MS));
}

fn configure_nodes_with_epoch_duration(
    args: &CliArgs,
    data_dir: &Path,
    epoch_duration_override: Option<u64>,
) {
    for i in 0..100 {
        let mut pr_dir = data_dir.to_path_buf();
        pr_dir.push(format!("pr{i}"));

        if pr_dir.exists() {
            info!("Deleting data from previous run: {pr_dir:?}...");
            remove_dir_all(&pr_dir).unwrap();
        } else {
            break;
        }
    }

    for i in 0..args.prs {
        info!("********************************************************************************");
        info!("Setting up node PR{i}...");

        let mut node_dir = data_dir.to_path_buf();
        node_dir.push(format!("pr{i}"));

        info!("Creating directory {node_dir:?}");
        std::fs::create_dir_all(&node_dir).unwrap();

        let mut ledger_path = node_dir.clone();
        ledger_path.push("data.ldb");

        let mut node_config_path = node_dir.clone();
        node_config_path.push("config-node.toml");
        if !node_config_path.exists() {
            info!("Creating node config file: {node_config_path:?}");
            let node_config = NODE_CONFIG
                .replace("PEERING_PORT", &peering_port(i).to_string())
                .replace("WS_PORT", &websocket_port(i).to_string())
                .replace("PRECONF_PEERS", &preconfigured_peers(args.prs, i))
                .replace("PRECONF_REPS", &preconfigured_representatives(args.prs))
                .replace("DB_BACKEND", if args.rocksdb { "rocksdb" } else { "lmdb" })
                .replace("CPS_LIMIT", &args.cps_limit.to_string())
                .replace("RAI_CONFIG", &rai_config(args, epoch_duration_override));
            std::fs::write(node_config_path, node_config).unwrap();
        }

        let mut rpc_config_path = node_dir.clone();
        rpc_config_path.push("config-rpc.toml");
        if !rpc_config_path.exists() {
            info!("Creating rpc config file: {rpc_config_path:?}");
            let rpc_config = RPC_CONFIG.replace("RPC_PORT", &rpc_port(i).to_string());
            std::fs::write(rpc_config_path, rpc_config).unwrap();
        }
    }
}

#[cfg(feature = "rai_protocol")]
pub(crate) fn configure_nodes_for_workload(args: &CliArgs, data_dir: &Path) {
    for i in 0..args.prs {
        let path = data_dir.join(format!("pr{i}/config-node.toml"));
        let config = std::fs::read_to_string(&path).unwrap();
        let duration = args
            .rai_epoch_duration_ms
            .unwrap_or(DEFAULT_RAI_EPOCH_DURATION_MS);
        let updated = config
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("epoch_duration =") {
                    format!("    epoch_duration = {duration}")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, updated).unwrap();
    }
}

fn rai_config(args: &CliArgs, epoch_duration_override: Option<u64>) -> String {
    if args.cpp {
        return String::new();
    }

    #[cfg(feature = "rai_protocol")]
    let (epoch_duration, close_attempt_duration, tick_interval) = (
        Some(
            epoch_duration_override.or(args.rai_epoch_duration_ms)
                .unwrap_or(DEFAULT_RAI_EPOCH_DURATION_MS),
        ),
        Some(
            args.rai_close_attempt_duration_ms
                .unwrap_or(DEFAULT_RAI_CLOSE_ATTEMPT_DURATION_MS),
        ),
        Some(
            args.rai_tick_interval_ms
                .unwrap_or(DEFAULT_RAI_TICK_INTERVAL_MS),
        ),
    );
    #[cfg(not(feature = "rai_protocol"))]
    let (epoch_duration, close_attempt_duration, tick_interval) = (
        args.rai_epoch_duration_ms,
        args.rai_close_attempt_duration_ms,
        args.rai_tick_interval_ms,
    );

    if epoch_duration.is_none() && close_attempt_duration.is_none() && tick_interval.is_none() {
        return String::new();
    }

    let mut result = String::from("[node.rai]\n");
    result.push_str(&format!(
        "    genesis_committee = {}\n",
        preconfigured_representatives(args.prs)
    ));
    if let Some(duration) = epoch_duration {
        result.push_str(&format!("    epoch_duration = {duration}\n"));
    }
    if let Some(duration) = close_attempt_duration {
        result.push_str(&format!("    close_attempt_duration = {duration}\n"));
    }
    if let Some(interval) = tick_interval {
        result.push_str(&format!("    tick_interval = {interval}\n"));
    }
    result
}

fn preconfigured_peers(prs: usize, current_pr: usize) -> String {
    let mut result = String::new();
    result.push('[');
    for i in 0..prs {
        if i == current_pr {
            continue;
        }

        result.push_str(&format!("\"[::1]:{}\",", peering_port(i)));
    }
    result.push(']');
    result
}

fn preconfigured_representatives(prs: usize) -> String {
    let representatives = (0..prs)
        .map(|i| format!("\"{}\"", pr_key(i).account().encode_account()))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{representatives}]")
}

pub(crate) fn peering_port(node_id: usize) -> u16 {
    17075 + (node_id as u16) * 10
}

pub(crate) fn rpc_port(node_id: usize) -> u16 {
    17076 + (node_id as u16) * 10
}

pub(crate) fn websocket_port(node_id: usize) -> u16 {
    17078 + (node_id as u16) * 10
}

pub(crate) fn pr_key(node_id: usize) -> PrivateKey {
    if node_id == 0 {
        genesis_key()
    } else {
        PrivateKey::from(node_id as u64)
    }
}

pub(crate) fn genesis_key() -> PrivateKey {
    PrivateKey::from_hex_str(GENESIS_PRV).expect("Genesis key should be valid")
}

pub(crate) fn get_genesis_hash() -> BlockHash {
    let genesis_block: Block = serde_json::from_str(GENESIS_BLOCK).unwrap();
    genesis_block.hash()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "rai_protocol")]
    use clap::Parser;

    #[test]
    fn preconfigures_every_generated_representative() {
        let reps = preconfigured_representatives(4);

        for i in 0..4 {
            assert!(reps.contains(&pr_key(i).account().encode_account()));
        }
        assert_eq!(reps.matches("nano_").count(), 4);
    }

    #[test]
    fn custom_genesis_is_the_first_preconfigured_representative() {
        let reps = preconfigured_representatives(1);

        assert_eq!(
            reps,
            format!("[\"{}\"]", genesis_key().account().encode_account())
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_builds_use_short_test_network_timings_by_default() {
        let args = CliArgs::parse_from(["nanospam"]);

        assert_eq!(
            rai_config(&args, None),
            format!(
                "[node.rai]\n    genesis_committee = {}\n    epoch_duration = 30000\n    close_attempt_duration = 3000\n    tick_interval = 250\n",
                preconfigured_representatives(1)
            )
        );
    }

}
