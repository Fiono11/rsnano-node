use std::{fs::remove_dir_all, path::Path};

use tracing::info;

use crate::cli_args::CliArgs;
use rsnano_types::{Amount, Block, BlockHash, PrivateKey};

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
                .replace("CPS_LIMIT", &args.cps_limit.to_string());
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

fn preconfigured_representatives(prs: usize) -> String {
    let representatives = (0..prs)
        .map(|i| format!("\"{}\"", pr_key(i).account().encode_account()))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{representatives}]")
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

pub(crate) fn pr_balance_weights(total: Amount, prs: usize, fork_recipients: usize) -> Vec<Amount> {
    pr_weights(total, prs, fork_recipients)
}

fn pr_weights(total: Amount, prs: usize, fork_recipients: usize) -> Vec<Amount> {
    let mut weights = vec![Amount::ZERO; prs];
    if prs == 0 {
        return weights;
    }
    if fork_recipients == 0 || fork_recipients >= prs {
        let base = total / prs as u128;
        weights.fill(base);
        weights[0] += total - base * prs as u128;
        return weights;
    }

    let split = |percent: u128| {
        Amount::raw(total.number() / 100 * percent + total.number() % 100 * percent / 100)
    };
    let first_total = split(51);
    let second_total = total - first_total;
    let first_base = first_total / fork_recipients as u128;
    let second_count = prs - fork_recipients;
    let second_base = second_total / second_count as u128;
    weights[..fork_recipients].fill(first_base);
    weights[fork_recipients..].fill(second_base);
    weights[0] += first_total - first_base * fork_recipients as u128;
    weights[fork_recipients] += second_total - second_base * second_count as u128;
    weights
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
