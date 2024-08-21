use super::{
    Channel, ChannelDirection, ChannelId, ChannelMode, DeadChannelCleanupStep,
    DeadChannelCleanupTarget, DropPolicy, NetworkFilter, NetworkInfo, OutboundBandwidthLimiter,
    TrafficType,
};
use crate::{stats::Stats, utils::into_ipv6_socket_address, NetworkParams, DEV_NETWORK_PARAMS};
use rsnano_core::utils::NULL_ENDPOINT;
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_tcp::TcpStream;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use tracing::{debug, warn};

pub struct NetworkOptions {
    pub publish_filter: Arc<NetworkFilter>,
    pub network_params: NetworkParams,
    pub stats: Arc<Stats>,
    pub limiter: Arc<OutboundBandwidthLimiter>,
    pub clock: Arc<SteadyClock>,
    pub network_info: Arc<RwLock<NetworkInfo>>,
}

impl NetworkOptions {
    pub fn new_test_instance() -> Self {
        NetworkOptions {
            publish_filter: Arc::new(NetworkFilter::default()),
            network_params: DEV_NETWORK_PARAMS.clone(),
            stats: Arc::new(Default::default()),
            limiter: Arc::new(OutboundBandwidthLimiter::default()),
            clock: Arc::new(SteadyClock::new_null()),
            network_info: Arc::new(RwLock::new(NetworkInfo::new_test_instance())),
        }
    }
}

pub struct Network {
    channels: Mutex<HashMap<ChannelId, Arc<Channel>>>,
    pub info: Arc<RwLock<NetworkInfo>>,
    stats: Arc<Stats>,
    network_params: Arc<NetworkParams>,
    limiter: Arc<OutboundBandwidthLimiter>,
    pub publish_filter: Arc<NetworkFilter>,
    clock: Arc<SteadyClock>,
}

impl Network {
    pub fn new(options: NetworkOptions) -> Self {
        let network = Arc::new(options.network_params);

        Self {
            channels: Mutex::new(HashMap::new()),
            stats: options.stats,
            network_params: network,
            limiter: options.limiter,
            publish_filter: options.publish_filter,
            clock: options.clock,
            info: options.network_info,
        }
    }

    pub(crate) async fn wait_for_available_inbound_slot(&self) {
        let last_log = Instant::now();
        let log_interval = if self.network_params.network.is_dev_network() {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(15)
        };
        while {
            let info = self.info.read().unwrap();
            !info.is_inbound_slot_available() && !info.is_stopped()
        } {
            if last_log.elapsed() >= log_interval {
                warn!("Waiting for available slots to accept new connections");
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn add(
        &self,
        stream: TcpStream,
        direction: ChannelDirection,
        planned_mode: ChannelMode,
    ) -> anyhow::Result<Arc<Channel>> {
        let peer_addr = stream
            .peer_addr()
            .map(into_ipv6_socket_address)
            .unwrap_or(NULL_ENDPOINT);

        let local_addr = stream
            .local_addr()
            .map(into_ipv6_socket_address)
            .unwrap_or(NULL_ENDPOINT);

        let channel_info = self.info.write().unwrap().add(
            local_addr,
            peer_addr,
            direction,
            planned_mode,
            self.clock.now(),
        )?;

        let channel = Channel::create(
            channel_info,
            stream,
            self.stats.clone(),
            self.limiter.clone(),
            self.info.clone(),
            self.clock.clone(),
        )
        .await;
        self.channels
            .lock()
            .unwrap()
            .insert(channel.channel_id(), channel.clone());

        debug!(?peer_addr, ?direction, "Accepted connection");

        Ok(channel)
    }

    pub(crate) fn new_null() -> Self {
        Self::new(NetworkOptions::new_test_instance())
    }

    pub(crate) fn try_send_buffer(
        &self,
        channel_id: ChannelId,
        buffer: &[u8],
        drop_policy: DropPolicy,
        traffic_type: TrafficType,
    ) -> bool {
        if let Some(channel) = self.channels.lock().unwrap().get(&channel_id).cloned() {
            channel.try_send_buffer(buffer, drop_policy, traffic_type)
        } else {
            false
        }
    }

    pub async fn send_buffer(
        &self,
        channel_id: ChannelId,
        buffer: &[u8],
        traffic_type: TrafficType,
    ) -> anyhow::Result<()> {
        let channel = self.channels.lock().unwrap().get(&channel_id).cloned();

        if let Some(channel) = channel {
            channel.send_buffer(buffer, traffic_type).await
        } else {
            Err(anyhow!("Channel not found"))
        }
    }

    pub fn port(&self) -> u16 {
        self.info.read().unwrap().listening_port()
    }
}

impl DeadChannelCleanupTarget for Arc<Network> {
    fn dead_channel_cleanup_step(&self) -> Box<dyn super::DeadChannelCleanupStep> {
        Box::new(NetworkCleanup(Arc::clone(self)))
    }
}

struct NetworkCleanup(Arc<Network>);

impl DeadChannelCleanupStep for NetworkCleanup {
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        let mut channels = self.0.channels.lock().unwrap();
        for channel_id in dead_channel_ids {
            channels.remove(channel_id);
        }
    }
}

#[derive(PartialEq, Eq)]
pub enum AcceptResult {
    Invalid,
    Accepted,
    Rejected,
    Error,
}

#[derive(Default)]
pub(crate) struct ChannelsInfo {
    pub total: usize,
    pub realtime: usize,
    pub bootstrap: usize,
    pub inbound: usize,
    pub outbound: usize,
}
