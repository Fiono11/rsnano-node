use crate::format_error_message;
use crate::request::{NodeRpcRequest, RpcRequest, WalletRpcRequest};
use crate::response::{
    account_balance, account_block_count, account_create, account_get, account_key, account_list,
    account_remove, account_representative, account_weight, available_supply, block_account,
    block_confirm, block_count, version,
};
use anyhow::{Context, Result};
use axum::response::Response;
use axum::{extract::State, response::IntoResponse, routing::post, Json};
use axum::{
    http::{Request, StatusCode},
    middleware::map_request,
    Router,
};
use rsnano_node::node::Node;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

pub async fn run_rpc_server(node: Arc<Node>, server_addr: SocketAddr) -> Result<()> {
    let app = Router::new()
        .route("/", post(handle_rpc))
        .layer(map_request(set_header))
        .with_state(node);

    let listener = TcpListener::bind(server_addr)
        .await
        .context("Failed to bind to address")?;

    axum::serve(listener, app)
        .await
        .context("Failed to run the server")?;
    Ok(())
}

async fn handle_rpc(
    State(node): State<Arc<Node>>,
    Json(rpc_request): Json<RpcRequest>,
) -> Response {
    let response = match rpc_request {
        RpcRequest::Node(node_request) => match node_request {
            NodeRpcRequest::Version => version(node).await,
            NodeRpcRequest::AccountBlockCount { account } => {
                account_block_count(node, account).await
            }
            NodeRpcRequest::AccountBalance {
                account,
                only_confirmed,
            } => account_balance(node, account, only_confirmed).await,
            NodeRpcRequest::AccountGet { key } => account_get(key).await,
            NodeRpcRequest::AccountKey { account } => account_key(account).await,
            NodeRpcRequest::AccountRepresentative { account } => {
                account_representative(node, account).await
            }
            NodeRpcRequest::AccountWeight { account } => account_weight(node, account).await,
            NodeRpcRequest::AvailableSupply => available_supply(node).await,
            NodeRpcRequest::BlockCount => block_count(node).await,
            NodeRpcRequest::BlockAccount { hash } => block_account(node, hash).await,
            NodeRpcRequest::BlockConfirm { hash } => block_confirm(node, hash).await,
            NodeRpcRequest::UnknownCommand => format_error_message("Unknown command"),
        },
        RpcRequest::Wallet(wallet_request) => match wallet_request {
            WalletRpcRequest::AccountCreate { wallet, index } => {
                account_create(node, wallet, index).await
            }
            WalletRpcRequest::AccountList { wallet } => account_list(node, wallet).await,
            WalletRpcRequest::AccountRemove { wallet, account } => {
                account_remove(node, wallet, account).await
            }
            WalletRpcRequest::UnknownCommand => format_error_message("Unknown command"),
        },
    };

    (StatusCode::OK, response).into_response()
}

async fn set_header<B>(mut request: Request<B>) -> Request<B> {
    request
        .headers_mut()
        .insert("Content-Type", "application/json".parse().unwrap());
    request
}
