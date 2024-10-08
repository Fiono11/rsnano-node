use super::account_block_count;
use super::account_get;
use super::account_history;
use super::account_info;
use super::account_key;
use super::account_representative;
use super::account_weight;
use super::accounts_balances;
use super::accounts_frontiers;
use super::accounts_representatives;
use super::available_supply;
use super::block_account;
use super::block_confirm;
use super::block_count;
use super::block_hash;
use super::block_info;
use super::blocks;
use super::blocks_info;
use super::bootstrap;
use super::bootstrap_any;
use super::bootstrap_lazy;
use super::chain;
use super::confirmation_active;
use super::confirmation_quorum;
use super::delegators;
use super::delegators_count;
use super::deterministic_key;
use super::frontier_count;
use super::frontiers;
use super::keepalive;
use super::key_expand;
use super::nano_to_raw;
use super::node_id;
use super::password_change;
use super::password_enter;
use super::password_valid;
use super::peers;
use super::populate_backlog;
use super::process;
use super::raw_to_nano;
use super::receive_minimum;
use super::representatives;
use super::search_receivable;
use super::search_receivable_all;
use super::send;
use super::sign;
use super::stats_clear;
use super::stop;
use super::unchecked_clear;
use super::unopened;
use super::validate_account_number;
use super::wallet_add;
use super::wallet_add_watch;
use super::wallet_change_seed;
use super::wallet_contains;
use super::wallet_destroy;
use super::wallet_export;
use super::wallet_frontiers;
use super::wallet_info;
use super::wallet_lock;
use super::wallet_locked;
use super::wallet_receivable;
use super::wallet_representative;
use super::wallet_representative_set;
use super::wallet_republish;
use super::wallet_work_get;
use super::work_cancel;
use super::work_get;
use super::work_set;
use super::work_validate;
use super::{
    account_create, account_list, account_move, account_remove, accounts_create, key_create,
    wallet_create,
};
use crate::account_balance;
use crate::uptime;
use anyhow::{Context, Result};
use axum::body::Body;
use axum::response::Response;
use axum::{extract::State, response::IntoResponse, routing::post, Json};
use axum::{
    http::{Request, StatusCode},
    middleware::map_request,
    Router,
};
use rsnano_node::node::Node;
use rsnano_rpc_messages::AccountBalanceDto;
use rsnano_rpc_messages::ErrorDto;
use rsnano_rpc_messages::{AccountMoveArgs, RpcCommand, WalletAddArgs};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

// Define a common result type for RPC responses
pub type RpcResult<T> = std::result::Result<PrettyJson<T>, (StatusCode, PrettyJson<ErrorDto>)>;

#[derive(Clone)]
struct RpcService {
    node: Arc<Node>,
    enable_control: bool,
}

pub async fn run_rpc_server(
    node: Arc<Node>,
    server_addr: SocketAddr,
    enable_control: bool,
) -> Result<()> {
    let rpc_service = RpcService {
        node,
        enable_control,
    };

    let app = Router::new()
        .route("/", post(handle_rpc))
        .layer(map_request(set_header))
        .with_state(rpc_service);

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
    State(rpc_service): State<RpcService>,
    Json(rpc_command): Json<RpcCommand>,
) -> RpcResult<impl Serialize> {
    match rpc_command {
        RpcCommand::AccountBalance(args) => {
            account_balance(rpc_service.node, args.account, args.include_only_confirmed).await
        }
        // ... other commands ...
        _ => Err((StatusCode::NOT_IMPLEMENTED, PrettyJson(ErrorDto {
            error: "Command not implemented".to_string(),
        }))),
    }
}

async fn set_header<B>(mut request: Request<B>) -> Request<B> {
    request
        .headers_mut()
        .insert("Content-Type", "application/json".parse().unwrap());
    request
}

#[derive(Debug)]
pub struct PrettyJson<T>(pub T);

impl<T> IntoResponse for PrettyJson<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response<Body> {
        match serde_json::to_string_pretty(&self.0) {
            Ok(body) => Response::builder()
                .header("Content-Type", "application/json")
                .status(StatusCode::OK)
                .body(body.into())
                .unwrap(),
            Err(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to serialize response body: {}", err),
            )
                .into_response(),
        }
    }
}