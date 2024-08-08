use crate::server::Service;

impl Service {
    pub async fn version(&self) -> String {
        let mut txn = self.node.store.env.tx_begin_read();
        let version = self.node.store.version.get(&mut txn);
        format!("store_version: {}", version.unwrap()).to_string()
    }
}
