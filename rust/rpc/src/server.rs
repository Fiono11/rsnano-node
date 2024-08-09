use crate::calls::{
    handle_account_balance, handle_account_block_count, handle_account_get, handle_account_key,
    handle_account_representative,
};
use anyhow::{anyhow, Context, Error, Result};
use axum::response::Response;
use axum::{extract::State, response::IntoResponse, routing::post, Json};
use axum::{
    http::{Request, StatusCode},
    middleware::map_request,
    Router,
};
use rsnano_node::node::Node;
use serde::Deserialize;
use serde_json::{json, to_string_pretty};
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::net::TcpListener;

#[derive(Clone)]
pub(crate) struct Service {
    pub(crate) node: Arc<Node>,
}

#[derive(Deserialize)]
pub(crate) struct RpcRequest {
    pub(crate) action: String,
    pub(crate) account: Option<String>,
    pub(crate) only_confirmed: Option<bool>,
    pub(crate) key: Option<String>,
}

type RpcResponse = Result<Response, Response>;

pub async fn run_server(node: Arc<Node>) -> Result<()> {
    let service = Service { node };

    let app = Router::new()
        .route("/", post(handle_rpc))
        .layer(map_request(set_header))
        .with_state(service);

    let server_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7076);

    let listener = TcpListener::bind(server_addr)
        .await
        .context("Failed to bind to address")?;

    axum::serve(listener, app)
        .await
        .context("Failed to run the server")?;
    Ok(())
}

async fn handle_rpc(
    State(service): State<Service>,
    Json(rpc_request): Json<RpcRequest>,
) -> RpcResponse {
    let response = match rpc_request.action.as_str() {
        "version" => Ok(service.version().await),
        "account_block_count" => handle_account_block_count(&service, rpc_request).await,
        "account_balance" => handle_account_balance(&service, rpc_request).await,
        "account_get" => handle_account_get(&service, rpc_request).await,
        "account_key" => handle_account_key(&service, rpc_request).await,
        "account_representative" => handle_account_representative(&service, rpc_request).await,
        _ => Err(json_error("Unknown command")),
    };

    response
        .map(|res| (StatusCode::OK, res).into_response())
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()).into_response())
}

async fn set_header<B>(mut request: Request<B>) -> Request<B> {
    request
        .headers_mut()
        .insert("Content-Type", "application/json".parse().unwrap());
    request
}

pub(crate) fn json_error(message: &str) -> Error {
    anyhow!(to_string_pretty(&json!({ "error": message })).unwrap())
}
