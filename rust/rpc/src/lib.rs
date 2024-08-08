use axum::{extract::State, response::IntoResponse, routing::post, Json};
use axum::{
    http::{Request, StatusCode},
    middleware::map_request,
    Router,
};
use rsnano_core::Account;
use rsnano_node::node::Node;
use rsnano_node::working_path;
use rsnano_store_lmdb::{LmdbAccountStore, LmdbEnv};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

#[derive(Clone)]
struct Service(Arc<Node>);

async fn set_header<B>(mut request: Request<B>) -> Request<B> {
    request
        .headers_mut()
        .insert("Content-Type", "application/json".parse().unwrap());
    request
}

pub async fn run_server(node: Arc<Node>) -> anyhow::Result<()> {
    let service = Service(node);

    let app = Router::new()
        .route("/", post(handle_rpc))
        .layer(map_request(set_header))
        .with_state(service);

    let server_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7076);

    println!("Server running on http://{}", server_addr);

    let listener = tokio::net::TcpListener::bind(server_addr).await.unwrap();
    println!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();

    Ok(())
}

async fn handle_rpc(
    State(service): State<Service>,
    Json(rpc_request): Json<RpcRequest>,
) -> impl IntoResponse {
    match rpc_request.action.as_str() {
        "version" => {
            let response = service.version().await;
            let json_response = Json(RpcResponse(response));
            (StatusCode::OK, json_response).into_response()
        }
        "account_block_count" => {
            let response = service
                .account_block_count(rpc_request.account.unwrap())
                .await;
            let json_response = Json(RpcResponse(response));
            (StatusCode::OK, json_response).into_response()
        }
        _ => {
            let error_response = Json(RpcResponse("Invalid action".to_string()));
            (StatusCode::BAD_REQUEST, error_response).into_response()
        }
    }
}

#[derive(Deserialize)]
struct RpcRequest {
    action: String,
    account: Option<String>,
}

#[derive(Serialize)]
struct RpcResponse(String);

#[async_trait::async_trait]
pub trait RpcService {
    async fn version(&self) -> String;

    async fn account_block_count(&self, account: String) -> String;
}

#[async_trait::async_trait]
impl RpcService for Service {
    async fn version(&self) -> String {
        let mut txn = self.0.store.env.tx_begin_read();
        let version = self.0.store.version.get(&mut txn);
        format!("store_version: {}", version.unwrap()).to_string()
    }

    async fn account_block_count(&self, account_str: String) -> String {
        let tx = self.0.ledger.read_txn();
        match Account::decode_account(&account_str) {
            Ok(account) => match self.0.ledger.store.account.get(&tx, &account) {
                Some(account_info) => {
                    format!("block_count: {}", account_info.block_count).to_string()
                }
                None => "Account not found".to_string(),
            },
            Err(e) => e.to_string(),
        }
    }
}
