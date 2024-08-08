use axum::{extract::State, response::IntoResponse, routing::post, Json};
use axum::{
    http::{Request, StatusCode},
    middleware::map_request,
    Router,
};
use rsnano_node::node::Node;
use serde::Deserialize;
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Service {
    pub(crate) node: Arc<Node>,
}

#[derive(Deserialize)]
pub(crate) struct RpcRequest {
    pub(crate) action: String,
    pub(crate) account: Option<String>,
    pub(crate) only_confirmed: Option<bool>,
}

async fn set_header<B>(mut request: Request<B>) -> Request<B> {
    request
        .headers_mut()
        .insert("Content-Type", "application/json".parse().unwrap());
    request
}

pub async fn run_server(node: Arc<Node>) -> anyhow::Result<()> {
    let service = Service { node };

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
            (StatusCode::OK, response).into_response()
        }
        "account_block_count" => {
            let response = service
                .account_block_count(rpc_request.account.unwrap())
                .await;
            (StatusCode::OK, response).into_response()
        }
        "account_balance" => {
            let only_confirmed = rpc_request.only_confirmed.unwrap_or(true);
            let response = service
                .account_balance(rpc_request.account.unwrap(), only_confirmed)
                .await;
            (StatusCode::OK, response).into_response()
        }
        _ => {
            let error_response = "Invalid action".to_string();
            (StatusCode::BAD_REQUEST, error_response).into_response()
        }
    }
}
