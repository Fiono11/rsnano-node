use std::cmp::Ordering;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::{
    http, serve_with_graceful_shutdown, stop_channel, ConnectionState, RpcServiceBuilder, Server,
    ServerConfig, ServerHandle, StopHandle,
};
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::{tracing, Methods, RpcModule};
use rsnano_node::node::Node;
use tokio::net::TcpListener;

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

pub async fn run_server(node: Arc<Node>) -> anyhow::Result<SocketAddr> {
    let port = 9944;
    let server = Server::builder()
        .build(format!("127.0.0.1:{}", port).parse::<SocketAddr>()?)
        .await?;
    let mut module = RpcModule::new(());
    //module.register_method("say_hello", |_, _, _| "lo")?;

    let my_rpc = MyRpc { node };

    // Create the RpcModule
    //let mut module = RpcModule::new(my_rpc.clone());

    module.merge(RpcServer::into_rpc(my_rpc))?;

    let addr = server.local_addr()?;
    println!("Server listening on {}", addr); // Ensure we print the correct address and port

    let handle = server.start(module);

    // In this example we don't care about doing shutdown so let's it run forever.
    // You may use the `ServerHandle` to shut it down or manage it yourself.
    tokio::spawn(handle.stopped());

    Ok(addr)
}
