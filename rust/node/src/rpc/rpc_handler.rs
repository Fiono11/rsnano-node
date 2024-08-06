use std::sync::Arc;

#[derive(Clone)]
pub struct RPCHandler {
    rpc_config: Arc<RpcConfig>,
    body: String,
    request_id: String,
    response: Arc<dyn Fn(String) -> (StatusCode, String) + Send + Sync>,
    rpc_handler_interface: Arc<dyn RPCHandlerInterface + Send + Sync>,
}

#[derive(Clone)]
pub struct RPCHandlerRequestParams {
    pub rpc_version: usize,
    pub credentials: Option<String>,
    pub correlation_id: Option<String>,
    pub path: String,
}

impl RPCHandler {
    pub fn new(
        rpc_config: Arc<RpcConfig>,
        body: String,
        request_id: String,
        response: Arc<dyn Fn(String) -> (StatusCode, String) + Send + Sync>,
        rpc_handler_interface: Arc<dyn RPCHandlerInterface + Send + Sync>,
    ) -> Self {
        Self {
            rpc_config,
            body,
            request_id,
            response,
            rpc_handler_interface,
        }
    }

    pub async fn process_request(&self, request_params: RPCHandlerRequestParams) {
        // Process the request here, using self.rpc_handler_interface
        // Example:
        let action = "example_action".to_string(); // Extract action from the request body
        let body = self.body.clone();
        (self.rpc_handler_interface)
            .process_request(action, body, self.response.clone())
            .await;
    }
}

use async_trait::async_trait;
use http::StatusCode;

use super::RpcConfig;

#[async_trait]
pub trait RPCHandlerInterface: Send + Sync {
    async fn process_request(
        &self,
        action: String,
        body: String,
        response: Arc<dyn Fn(String) -> (StatusCode, String) + Send + Sync>,
    );
    async fn process_request_v2(
        &self,
        request_params: RPCHandlerRequestParams,
        body: String,
        response: Box<dyn Fn(String) + Send>,
    );
}
