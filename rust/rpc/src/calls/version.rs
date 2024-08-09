use crate::server::Service;
use rsnano_node::{BUILD_INFO, VERSION_STRING};
use serde::Serialize;
use serde_json::to_string_pretty;

#[derive(Serialize)]
struct Version {
    rpc_version: String,
    store_version: String,
    protocol_version: String,
    node_vendor: String,
    store_vendor: String,
    network: String,
    network_identifier: String,
    build_info: String,
}

impl Version {
    fn new(
        rpc_version: String,
        store_version: String,
        protocol_version: String,
        node_vendor: String,
        store_vendor: String,
        network: String,
        network_identifier: String,
        build_info: String,
    ) -> Self {
        Self {
            rpc_version,
            store_version,
            protocol_version,
            node_vendor,
            store_vendor,
            network,
            network_identifier,
            build_info,
        }
    }
}

impl Service {
    pub(crate) async fn version(&self) -> String {
        let mut txn = self.node.store.env.tx_begin_read();
        let rpc_version = String::from("1");
        let store_version = self.node.store.version.get(&mut txn).unwrap().to_string();
        let protocol_version = self
            .node
            .network_params
            .network
            .protocol_version
            .to_string();
        let node_vendor = format!("RsNano {}", VERSION_STRING);
        let store_vendor = self.node.store.vendor();
        let network = self
            .node
            .network_params
            .network
            .get_current_network_as_string()
            .to_string();
        let network_identifier = self.node.network_params.ledger.genesis.hash().to_string();
        let build_info = BUILD_INFO.to_string();

        let version = Version::new(
            rpc_version,
            store_version,
            protocol_version,
            node_vendor,
            store_vendor,
            network,
            network_identifier,
            build_info,
        );

        to_string_pretty(&version).unwrap()
    }
}
