use super::{ChannelDirection, ChannelMode, Network, ResponseServerFactory};
use crate::{
    stats::{DetailType, Direction, StatType, Stats},
    transport::TcpStream,
    utils::AsyncRuntime,
};
use async_trait::async_trait;
use std::{
    net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6},
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

/// Server side portion of tcp sessions. Listens for new socket connections and spawns tcp_server objects when connected.
pub struct TcpListener {
    port: AtomicU16,
    network: Arc<Network>,
    stats: Arc<Stats>,
    runtime: Arc<AsyncRuntime>,
    data: Mutex<TcpListenerData>,
    condition: Condvar,
    cancel_token: CancellationToken,
    response_server_factory: Arc<ResponseServerFactory>,
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        debug_assert!(self.data.lock().unwrap().stopped);
    }
}

struct TcpListenerData {
    stopped: bool,
    local_addr: SocketAddrV6,
}

impl TcpListener {
    pub(crate) fn new(
        port: u16,
        network: Arc<Network>,
        runtime: Arc<AsyncRuntime>,
        stats: Arc<Stats>,
        response_server_factory: Arc<ResponseServerFactory>,
    ) -> Self {
        Self {
            port: AtomicU16::new(port),
            network,
            data: Mutex::new(TcpListenerData {
                stopped: false,
                local_addr: SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0),
            }),
            runtime: Arc::clone(&runtime),
            stats,
            condition: Condvar::new(),
            cancel_token: CancellationToken::new(),
            response_server_factory,
        }
    }

    pub fn stop(&self) {
        self.data.lock().unwrap().stopped = true;
        self.cancel_token.cancel();
        self.condition.notify_all();
    }

    pub fn realtime_count(&self) -> usize {
        self.network.count_by_mode(ChannelMode::Realtime)
    }

    pub fn local_address(&self) -> SocketAddrV6 {
        let guard = self.data.lock().unwrap();
        if !guard.stopped {
            guard.local_addr
        } else {
            SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0)
        }
    }
}

#[async_trait]
pub trait TcpListenerExt {
    fn start(&self);
    async fn run(&self, listener: tokio::net::TcpListener);
}

#[async_trait]
impl TcpListenerExt for Arc<TcpListener> {
    fn start(&self) {
        let self_l = Arc::clone(self);
        self.runtime.tokio.spawn(async move {
            let port = self_l.port.load(Ordering::SeqCst);
            let Ok(listener) = tokio::net::TcpListener::bind(SocketAddr::new(
                IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                port,
            ))
            .await
            else {
                error!("Error while binding for incoming connections on: {}", port);
                return;
            };

            let addr = listener
                .local_addr()
                .map(|a| match a {
                    SocketAddr::V6(v6) => v6,
                    _ => unreachable!(), // We only use V6 addresses
                })
                .unwrap_or(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0));
            debug!("Listening for incoming connections on: {}", addr);

            self_l.network.set_port(addr.port());
            self_l.data.lock().unwrap().local_addr =
                SocketAddrV6::new(Ipv6Addr::LOCALHOST, addr.port(), 0, 0);

            self_l.run(listener).await
        });
    }

    async fn run(&self, listener: tokio::net::TcpListener) {
        let run_loop = async {
            loop {
                self.network.wait_for_available_inbound_slot().await;

                let Ok((stream, _)) = listener.accept().await else {
                    self.stats.inc_dir(
                        StatType::TcpListener,
                        DetailType::AcceptFailure,
                        Direction::In,
                    );
                    continue;
                };

                let tcp_stream = TcpStream::new(stream);
                match self
                    .network
                    .add(
                        tcp_stream,
                        ChannelDirection::Inbound,
                        ChannelMode::Undefined,
                    )
                    .await
                {
                    Ok(channel) => {
                        self.response_server_factory.start_response_server(channel);
                    }
                    Err(e) => {
                        warn!("Could not accept incoming connection: {:?}", e);
                    }
                };

                // Sleep for a while to prevent busy loop
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };

        tokio::select! {
            _ = self.cancel_token.cancelled() => { },
            _ = run_loop => {}
        }
    }
}
