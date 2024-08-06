use super::rpc_config::RpcConfig;
use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{info, warn};

#[async_trait::async_trait]
pub trait RpcHandlerInterface: Send + Sync {
    async fn handle_request(&self, connection: TcpStream);
}

pub struct Rpc {
    config: RpcConfig,
    listener: TcpListener,
    handler: Arc<dyn RpcHandlerInterface>,
    stopped: Arc<Mutex<bool>>,
}

impl Rpc {
    pub async fn new(config: RpcConfig, handler: Arc<dyn RpcHandlerInterface>) -> Result<Self> {
        let addr: SocketAddr = format!("{}:{}", config.address, config.port)
            .parse()
            .unwrap();
        let listener = TcpListener::bind(&addr).await?;

        Ok(Rpc {
            config,
            listener,
            handler,
            stopped: Arc::new(Mutex::new(false)),
        })
    }

    pub async fn start(self: Arc<Self>) {
        let addr = self.listener.local_addr().unwrap();
        let is_loopback = addr.ip().is_loopback();

        if !is_loopback && self.config.enable_control {
            warn!("Control-level RPCs are enabled on non-local address {}, potentially allowing wallet access outside local computer", addr);
        }

        info!("RPC listening address: {}", addr);

        loop {
            let (socket, _) = self.listener.accept().await.unwrap();
            let handler = self.handler.clone();
            tokio::spawn(async move {
                handler.handle_request(socket).await;
            });
        }
    }

    pub async fn stop(&self) {
        let mut stopped = self.stopped.lock().await;
        *stopped = true;
    }
}
