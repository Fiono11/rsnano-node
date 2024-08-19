use super::request::{NodeRpcRequest, RpcRequest, WalletRpcRequest};
use super::response::{
    account_balance, account_block_count, account_create, account_get, account_key, account_list,
    account_move, account_remove, account_representative, account_representative_set,
    account_weight, accounts_create, available_supply, block_account, block_confirm, block_count,
    version, wallet_add, wallet_balances, wallet_create, wallet_destroy,
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
use serde_json::{json, to_string_pretty};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

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

    info!("RPC listening address: {}", server_addr);

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
            NodeRpcRequest::BlockCount => block_count(service.node).await,
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
            WalletRpcRequest::AccountRepresentativeSet {
                wallet,
                account,
                representative,
                work,
            } => {
                if service.enable_control {
                    account_representative_set(service.node, wallet, account, representative, work)
                        .await
                } else {
                    format_error_message("Enable control is disabled")
                }
            }
            WalletRpcRequest::AccountMove {
                wallet,
                source,
                accounts,
            } => {
                if service.enable_control {
                    account_move(service.node, wallet, source, accounts).await
                } else {
                    format_error_message("Enable control is disabled")
                }
            }
            WalletRpcRequest::WalletAdd { wallet, key, work } => {
                if service.enable_control {
                    wallet_add(service.node, wallet, key, work).await
                } else {
                    format_error_message("Enable control is disabled")
                }
            }
            WalletRpcRequest::WalletBalances { wallet, threshold } => {
                wallet_balances(service.node, wallet, threshold).await
            }
            WalletRpcRequest::WalletCreate { seed } => {
                if service.enable_control {
                    wallet_create(service.node, seed).await
                } else {
                    format_error_message("Enable control is disabled")
                }
            }
            WalletRpcRequest::WalletDestroy { wallet } => {
                if service.enable_control {
                    wallet_destroy(service.node, wallet).await
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

pub(crate) fn format_error_message(error: &str) -> String {
    let json_value = json!({ "error": error });
    to_string_pretty(&json_value).unwrap()
}
