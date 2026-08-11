use std::{fs::remove_dir_all, path::Path};

use tracing::info;

use crate::cli_args::CliArgs;
use rsnano_types::{Block, BlockHash, PrivateKey};

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
    preconfigured_representatives = ["nano_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo"]
    database_backend = "DB_BACKEND"
    cps_limit = CPS_LIMIT

[node.lmdb]
    sync = "nosync_unsafe"

[node.network]
    max_peers_per_ip = 256

[node.bounded_backlog]
    enable = false

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

RAI_CONFIG

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

pub(crate) fn configure_nodes(args: &CliArgs, data_dir: &Path) {
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

    configure_run_nodes(args, data_dir);
}

/// Rewrites configuration without touching the prepared ledgers.
pub(crate) fn configure_run_nodes(args: &CliArgs, data_dir: &Path) {
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
        info!("Writing node config file: {node_config_path:?}");
        let node_config = NODE_CONFIG
            .replace("PEERING_PORT", &peering_port(i).to_string())
            .replace("WS_PORT", &websocket_port(i).to_string())
            .replace("PRECONF_PEERS", &preconfigured_peers(args.prs, i))
            .replace("DB_BACKEND", if args.rocksdb { "rocksdb" } else { "lmdb" })
            .replace("CPS_LIMIT", &args.cps_limit.to_string())
            .replace("RAI_CONFIG", &rai_config(args));
        std::fs::write(node_config_path, node_config).unwrap();

        let mut rpc_config_path = node_dir.clone();
        rpc_config_path.push("config-rpc.toml");
        if !rpc_config_path.exists() {
            info!("Creating rpc config file: {rpc_config_path:?}");
            let rpc_config = RPC_CONFIG.replace("RPC_PORT", &rpc_port(i).to_string());
            std::fs::write(rpc_config_path, rpc_config).unwrap();
        }
    }
}

fn rai_config(args: &CliArgs) -> String {
    if args.setup_only() {
        // Keep all setup elections in epoch zero. `run` rewrites this config
        // before restarting the prepared ledgers and enables timed boundaries.
        return "[node.rai]\n    enable_epoch_ticker = false".to_string();
    }
    let mut config = String::from("[node.rai]\n    enable_epoch_ticker = true");
    if let Some(duration) = args.rai_epoch_duration_ms {
        config.push_str(&format!("\n    epoch_duration = {duration}"));
    }
    if let Some(interval) = args.rai_tick_interval_ms {
        config.push_str(&format!("\n    tick_interval = {interval}"));
    }
    let committee = (0..args.prs)
        .map(|i| format!("\"{}\"", pr_key(i).account().encode_account()))
        .collect::<Vec<_>>()
        .join(", ");
    // On run, the node reads these representatives' weights from the
    // cemented distribution prepared by `nanospam setup`.
    config.push_str(&format!("\n    genesis_committee = [{committee}]"));
    config.push_str("\n    reset_finalization_on_start = true");
    // A block published on an epoch boundary can be assigned to the old
    // epoch by one PR and the successor epoch by another. Once the certified
    // close releases the old slot, let every PR retry that same starting slot
    // in the successor epoch so the benchmark measures continuous protocol
    // throughput instead of waiting for the next close to settle it.
    config.push_str("\n    retry_released_slots = true");
    config
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
    use crate::cli_args::CommandLine;
    use clap::Parser;

    #[test]
    fn writes_requested_rai_timing() {
        let args = CommandLine::parse_from([
            "nanospam",
            "run",
            "--data-dir",
            "/tmp/rai",
            "--blocks",
            "1",
            "--rai-epoch-duration-ms",
            "5000",
            "--rai-tick-interval-ms",
            "100",
        ])
        .into_args();

        assert_eq!(
            rai_config(&args),
            format!(
                "[node.rai]\n    enable_epoch_ticker = true\n    epoch_duration = 5000\n    tick_interval = 100\n    genesis_committee = [\"{}\"]\n    reset_finalization_on_start = true\n    retry_released_slots = true",
                pr_key(0).account().encode_account()
            )
        );
    }

    #[test]
    fn setup_disables_epoch_boundaries() {
        let args = CommandLine::parse_from([
            "nanospam",
            "setup",
            "--data-dir",
            "/tmp/rai",
            "--rai-epoch-duration-ms",
            "1",
            "--rai-tick-interval-ms",
            "1",
        ])
        .into_args();

        assert_eq!(
            rai_config(&args),
            "[node.rai]\n    enable_epoch_ticker = false"
        );
    }

    #[test]
    fn run_config_defines_one_genesis_committee_member_per_pr() {
        let args = CommandLine::parse_from([
            "nanospam",
            "run",
            "--data-dir",
            "/tmp/rai",
            "--prs",
            "6",
            "--accounts",
            "6",
            "--blocks",
            "0",
        ])
        .into_args();

        let config = rai_config(&args);
        for i in 0..6 {
            assert!(config.contains(&pr_key(i).account().encode_account()));
        }
        assert_eq!(config.matches('"').count() / 2, 6);
    }
}
