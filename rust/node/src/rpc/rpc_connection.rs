use axum::{
    extract::{Extension, Json},
    response::IntoResponse,
    routing::post,
    Router,
};
use http::StatusCode;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info};
use uuid::Uuid;

use super::{
    rpc_handler::{RPCHandler, RPCHandlerInterface, RPCHandlerRequestParams},
    Rpc, RpcConfig,
};

pub struct RPCConnection {
    responded: Arc<AtomicBool>,
    rpc_config: Arc<RpcConfig>,
    rpc_handler_interface: Arc<dyn RPCHandlerInterface + Send + Sync>,
}

impl RPCConnection {
    pub fn new(
        rpc_config: Arc<RpcConfig>,
        rpc_handler_interface: Arc<dyn RPCHandlerInterface + Send + Sync>,
    ) -> Self {
        Self {
            responded: Arc::new(AtomicBool::new(false)),
            rpc_config,
            rpc_handler_interface,
        }
    }

    pub async fn parse_request(
        Extension(rpc_config): Extension<Arc<RpcConfig>>,
        Extension(rpc_handler_interface): Extension<Arc<dyn RPCHandlerInterface + Send + Sync>>,
        Json(payload): Json<serde_json::Value>,
    ) -> impl IntoResponse {
        let request_id = Uuid::new_v4().to_string();
        let responded = Arc::new(AtomicBool::new(false));
        let response_handler = {
            let responded = responded.clone();
            Arc::new(move |response_body: String| {
                if !responded.swap(true, Ordering::SeqCst) {
                    (StatusCode::OK, response_body)
                } else {
                    debug!("RPC already responded and should only respond once");
                    (StatusCode::INTERNAL_SERVER_ERROR, String::new())
                }
            })
        };

        let path = "/";
        let rpc_version = if path.starts_with("/api/v2") { 2 } else { 1 };

        let handler = RPCHandler::new(
            rpc_config.clone(),
            payload.to_string(),
            request_id,
            response_handler.clone(),
            rpc_handler_interface.clone(),
        );

        tokio::spawn(async move {
            let request_params = RPCHandlerRequestParams {
                rpc_version,
                credentials: None,
                correlation_id: None,
                path: path.trim_start_matches("/api/v2/").to_string(),
            };

            handler.process_request(request_params).await;
        });

        (
            StatusCode::OK,
            json!({ "status": "Request processing started" }).to_string(),
        )
    }

    pub fn router(
        rpc_config: Arc<RpcConfig>,
        rpc_handler_interface: Arc<dyn RPCHandlerInterface + Send + Sync>,
    ) -> Router {
        Router::new()
            .route("/", post(Self::parse_request))
            .layer(Extension(rpc_config))
            .layer(Extension(rpc_handler_interface))
    }
}
