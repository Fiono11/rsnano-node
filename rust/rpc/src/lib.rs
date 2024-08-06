use async_trait::async_trait;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::{Server, ServerHandle};
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::RpcModule;
use rsnano_node::node::Node;
use std::net::SocketAddr;
use std::sync::Arc;

#[rpc(server)]
pub trait Rpc {
    #[method(name = "say_hello")]
    async fn say_hello(&self) -> Result<String, ErrorObjectOwned>;
}

struct MyRpc {
    node: Arc<Node>,
}

#[async_trait]
impl RpcServer for MyRpc {
    async fn say_hello(&self) -> Result<String, ErrorObjectOwned> {
        // Here you can access self.node
        let response = self.node.config.allow_local_peers;
        Ok(response.to_string())
    }
}

pub async fn run_server(node: Arc<Node>) -> anyhow::Result<ServerHandle> {
    let port = 9944;
    let server = Server::builder()
        .build(format!("127.0.0.1:{}", port).parse::<SocketAddr>()?)
        .await?;
    let mut module = RpcModule::new(());

    let my_rpc = MyRpc { node };

    module.merge(RpcServer::into_rpc(my_rpc))?;

    let addr = server.local_addr()?;
    println!("Server listening on {}", addr);

    let handle = server.start(module);

    Ok(handle)
}
