use rsnano_node::node::Node;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::signal;
use warp::Filter;

pub async fn run_server(node: Arc<Node>) -> anyhow::Result<()> {
    let service = Service(node);

    let rpc_route = warp::path::end()
        .and(warp::post())
        .and(warp::body::json())
        .and_then(move |rpc_request: RpcRequest| {
            let service = service.clone();
            async move {
                if rpc_request.action == "version" {
                    let response = service.version().await;
                    let json_response = warp::reply::json(&RpcResponse { message: response });
                    Ok::<_, warp::Rejection>(json_response)
                } else {
                    let error_response = warp::reply::json(&"Invalid action".to_string());
                    Ok::<_, warp::Rejection>(error_response)
                }
            }
        });

    let server_addr = (IpAddr::V4(Ipv4Addr::LOCALHOST), 7076);
    let (addr, server) = warp::serve(rpc_route).bind_with_graceful_shutdown(server_addr, async {
        signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl+c signal");
    });

    println!("Server running on http://{}", addr);
    server.await;

    Ok(())
}

//#[tarpc::service]
pub trait RpcService {
    async fn version(&self) -> String;
}

#[derive(Clone)]
struct Service(Arc<Node>);

impl RpcService for Service {
    async fn version(&self) -> String {
        let mut txn = self.0.store.env.tx_begin_read();
        let version = self.0.store.version.get(&mut txn);
        format!("store_version: {}", version.unwrap()).to_string()
    }
}

#[derive(Deserialize)]
struct RpcRequest {
    action: String,
}

#[derive(Serialize)]
struct RpcResponse {
    message: String,
}
