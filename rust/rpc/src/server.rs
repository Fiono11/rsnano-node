use crate::format_error_message;
use crate::request::{NodeRpcRequest, RpcRequest, WalletRpcRequest};
use crate::response::{
    account_balance, account_block_count, account_create, account_get, account_key, account_list,
    account_remove, account_representative, account_weight, accounts_create, available_supply,
    block_account, block_confirm, block_count, version,
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

#[derive(Clone)]
struct Service {
    node: Arc<Node>,
    enable_control: bool,
}

pub async fn run_rpc_server(
    node: Arc<Node>,
    server_addr: SocketAddr,
    enable_control: bool,
) -> Result<()> {
    let service = Service {
        node,
        enable_control,
    };

    let app = Router::new()
        .route("/", post(handle_rpc))
        .layer(map_request(set_header))
        .with_state(service);

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
) -> Response {
    let response = match rpc_request {
        RpcRequest::Node(node_request) => match node_request {
            NodeRpcRequest::Version => version(service.node).await,
            NodeRpcRequest::AccountBlockCount { account } => {
                account_block_count(service.node, account).await
            }
            NodeRpcRequest::AccountBalance {
                account,
                only_confirmed,
            } => account_balance(service.node, account, only_confirmed).await,
            NodeRpcRequest::AccountGet { key } => account_get(key).await,
            NodeRpcRequest::AccountKey { account } => account_key(account).await,
            NodeRpcRequest::AccountRepresentative { account } => {
                account_representative(service.node, account).await
            }
            NodeRpcRequest::AccountWeight { account } => {
                account_weight(service.node, account).await
            }
            NodeRpcRequest::AvailableSupply => available_supply(service.node).await,
            NodeRpcRequest::BlockCount { include_cemented } => {
                block_count(service.node, include_cemented).await
            }
            NodeRpcRequest::BlockAccount { hash } => block_account(service.node, hash).await,
            NodeRpcRequest::BlockConfirm { hash } => block_confirm(service.node, hash).await,
        },
        RpcRequest::Wallet(wallet_request) => match wallet_request {
            WalletRpcRequest::AccountCreate { wallet, index } => {
                if service.enable_control {
                    account_create(service.node, wallet, index).await
                } else {
                    format_error_message("Enable control is disabled")
                }
            }
            WalletRpcRequest::AccountsCreate { wallet, count } => {
                if service.enable_control {
                    accounts_create(service.node, wallet, count).await
                } else {
                    format_error_message("Enable control is disabled")
                }
            }
            WalletRpcRequest::AccountList { wallet } => account_list(service.node, wallet).await,
            WalletRpcRequest::AccountRemove { wallet, account } => {
                if service.enable_control {
                    account_remove(service.node, wallet, account).await
                } else {
                    format_error_message("Enable control is disabled")
                }
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
