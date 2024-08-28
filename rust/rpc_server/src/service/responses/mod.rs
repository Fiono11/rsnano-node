mod ledger;
mod node;
mod utils;
mod wallets;

pub use ledger::*;
pub use node::*;
use serde_json::{json, to_string_pretty};
pub use utils::*;
pub use wallets::*;

fn format_error_message(error: &str) -> String {
    let json_value = json!({ "error": error });
    to_string_pretty(&json_value).unwrap()
}

#[cfg(test)]
mod test_helpers {
    use crate::{run_rpc_server, RpcServerConfig};
    use rand::{thread_rng, Rng};
    use reqwest::Url;
    use rsnano_core::{utils::get_cpu_count, Networks, WalletId};
    use rsnano_node::{node::Node, wallets::WalletsExt};
    use rsnano_rpc_client::NanoRpcClient;
    use std::{
        net::{IpAddr, SocketAddr},
        str::FromStr,
        sync::Arc,
    };
    use test_helpers::get_available_port;

    pub(crate) fn setup_rpc_client_and_server(
        node: Arc<Node>,
    ) -> (
        Arc<NanoRpcClient>,
        tokio::task::JoinHandle<Result<(), anyhow::Error>>,
    ) {
        let port = get_available_port();
        let rpc_server_config =
            RpcServerConfig::default_for(Networks::NanoBetaNetwork, get_cpu_count());
        let ip_addr = IpAddr::from_str(&rpc_server_config.address).unwrap();
        let socket_addr = SocketAddr::new(ip_addr, port);

        let server = node
            .tokio
            .spawn(run_rpc_server(node.clone(), socket_addr, true));

        let rpc_url = format!("http://[::1]:{}/", port);
        let rpc_client = Arc::new(NanoRpcClient::new(Url::parse(&rpc_url).unwrap()));

        (rpc_client, server)
    }

    pub(crate) fn create_wallet(node: Arc<Node>) -> WalletId {
        let wallet_id = WalletId::from_bytes(thread_rng().gen());
        node.wallets.create(wallet_id);
        wallet_id
    }
}