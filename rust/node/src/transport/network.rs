use super::{
    attempt_container::AttemptContainer, channel_container::ChannelContainer, BufferDropPolicy,
    ChannelDirection, ChannelEnum, ChannelFake, ChannelMode, ChannelTcp, InboundMessageQueue,
    NetworkFilter, NullSocketObserver, OutboundBandwidthLimiter, PeerExclusion, ResponseServerImpl,
    Socket, SocketExtensions, SocketObserver, TcpConfig, TrafficType, TransportType,
};
use crate::{
    config::{NetworkConstants, NodeFlags},
    stats::{DetailType, Direction, StatType, Stats},
    transport::{Channel, ResponseServerExt},
    utils::{
        ipv4_address_or_ipv6_subnet, is_ipv4_or_v4_mapped_address, map_address_to_subnetwork,
        reserved_address, AsyncRuntime,
    },
    NetworkParams, DEV_NETWORK_PARAMS,
};
use rand::{seq::SliceRandom, thread_rng, Rng};
use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoComponent},
    Account, PublicKey,
};
use rsnano_messages::*;
use std::{
    net::{Ipv6Addr, SocketAddrV6},
    sync::{
        atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};
use tracing::{debug, warn};

pub struct NetworkOptions {
    pub allow_local_peers: bool,
    pub tcp_config: TcpConfig,
    pub publish_filter: Arc<NetworkFilter>,
    pub async_rt: Arc<AsyncRuntime>,
    pub network_params: NetworkParams,
    pub stats: Arc<Stats>,
    pub inbound_queue: Arc<InboundMessageQueue>,
    pub port: u16,
    pub flags: NodeFlags,
    pub limiter: Arc<OutboundBandwidthLimiter>,
    pub observer: Arc<dyn SocketObserver>,
}

impl NetworkOptions {
    pub fn new_test_instance() -> Self {
        NetworkOptions {
            allow_local_peers: true,
            tcp_config: TcpConfig::for_dev_network(),
            publish_filter: Arc::new(NetworkFilter::default()),
            async_rt: Arc::new(AsyncRuntime::default()),
            network_params: DEV_NETWORK_PARAMS.clone(),
            stats: Arc::new(Default::default()),
            inbound_queue: Arc::new(InboundMessageQueue::default()),
            port: 8088,
            flags: NodeFlags::default(),
            limiter: Arc::new(OutboundBandwidthLimiter::default()),
            observer: Arc::new(NullSocketObserver::new()),
        }
    }
}

pub struct Network {
    state: Mutex<State>,
    port: AtomicU16,
    stopped: AtomicBool,
    allow_local_peers: bool,
    // TODO remove inbound_queue as soon as it isn't used by C++ anymore
    pub inbound_queue: Arc<InboundMessageQueue>,
    flags: NodeFlags,
    stats: Arc<Stats>,
    next_channel_id: AtomicUsize,
    network_params: Arc<NetworkParams>,
    limiter: Arc<OutboundBandwidthLimiter>,
    async_rt: Arc<AsyncRuntime>,
    tcp_config: TcpConfig,
    pub publish_filter: Arc<NetworkFilter>,
    observer: Arc<dyn SocketObserver>,
}

impl Drop for Network {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Network {
    pub fn new(options: NetworkOptions) -> Self {
        let network = Arc::new(options.network_params);

        Self {
            port: AtomicU16::new(options.port),
            stopped: AtomicBool::new(false),
            allow_local_peers: options.allow_local_peers,
            inbound_queue: options.inbound_queue,
            state: Mutex::new(State {
                attempts: Default::default(),
                channels: Default::default(),
                network_constants: network.network.clone(),
                new_channel_observers: Vec::new(),
                excluded_peers: PeerExclusion::new(),
                stats: options.stats.clone(),
                node_flags: options.flags.clone(),
                config: options.tcp_config.clone(),
            }),
            tcp_config: options.tcp_config,
            flags: options.flags,
            stats: options.stats,
            next_channel_id: AtomicUsize::new(1),
            network_params: network,
            limiter: options.limiter,
            publish_filter: options.publish_filter,
            observer: options.observer,
            async_rt: options.async_rt,
        }
    }

    pub fn dump_channels(&self) {
        let state = self.state.lock().unwrap();
        println!(
            "Dumping {} channels. Local port is {}",
            state.channels.len(),
            self.port()
        );
        for i in state.channels.iter() {
            println!(
                "    remote: {}, direction: {:?}, mode: {:?}",
                i.channel.remote_endpoint(),
                i.channel.direction(),
                i.channel.mode()
            )
        }
    }

    pub async fn wait_for_available_inbound_slot(&self) {
        let last_log = Instant::now();
        let log_interval = if self.network_params.network.is_dev_network() {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(15)
        };
        while self.count_by_direction(ChannelDirection::Inbound)
            >= self.tcp_config.max_inbound_connections
            && !self.stopped.load(Ordering::SeqCst)
        {
            if last_log.elapsed() >= log_interval {
                warn!(
                    "Waiting for available slots to accept new connections (current: {} / max: {})",
                    self.count_by_direction(ChannelDirection::Inbound),
                    self.tcp_config.max_inbound_connections
                );
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn add(
        &self,
        socket: &Arc<Socket>,
        response_server: &Arc<ResponseServerImpl>,
        direction: ChannelDirection,
    ) -> anyhow::Result<()> {
        let Some(remote_endpoint) = socket.get_remote() else {
            return Err(anyhow!("no remote endpoint"));
        };

        let result = self.check_limits(remote_endpoint.ip(), direction);

        if result != AcceptResult::Accepted {
            self.stats.inc_dir(
                StatType::TcpListener,
                DetailType::AcceptRejected,
                direction.into(),
            );
            if direction == ChannelDirection::Outbound {
                self.stats.inc_dir(
                    StatType::TcpListener,
                    DetailType::ConnectFailure,
                    Direction::Out,
                );
            }
            debug!(
                "Rejected connection from: {} ({:?})",
                remote_endpoint, direction
            );
            // Rejection reason should be logged earlier

            if let Err(e) = socket.shutdown().await {
                self.stats.inc_dir(
                    StatType::TcpListener,
                    DetailType::CloseError,
                    direction.into(),
                );
                debug!(
                    "Error while closing socket after refusing connection: {:?} ({:?})",
                    e, direction
                )
            }
            if direction == ChannelDirection::Inbound {
                self.stats.inc_dir(
                    StatType::TcpListener,
                    DetailType::AcceptFailure,
                    Direction::In,
                );
                // Refusal reason should be logged earlier
            }
            return Err(anyhow!("check_limits failed"));
        }

        self.stats.inc_dir(
            StatType::TcpListener,
            DetailType::AcceptSuccess,
            direction.into(),
        );

        debug!("Accepted connection: {} ({:?})", remote_endpoint, direction);

        socket.set_timeout(self.network_params.network.idle_timeout);

        let tcp_channel = ChannelTcp::new(
            socket.clone(),
            SystemTime::now(),
            Arc::clone(&self.stats),
            Arc::clone(&self.limiter),
            &self.async_rt,
            self.get_next_channel_id(),
            self.network_params.network.protocol_info(),
        );
        tcp_channel.update_remote_endpoint();
        let channel = Arc::new(ChannelEnum::Tcp(Arc::new(tcp_channel)));
        response_server.set_channel(channel.clone());

        self.state
            .lock()
            .unwrap()
            .channels
            .insert(channel, Some(response_server.clone()));

        socket.start();
        let response_server_l = response_server.clone();
        self.async_rt
            .tokio
            .spawn(async move { response_server_l.run().await });

        self.observer.socket_connected(Arc::clone(&socket));

        if direction == ChannelDirection::Outbound {
            self.stats.inc_dir(
                StatType::TcpListener,
                DetailType::ConnectSuccess,
                Direction::Out,
            );
            debug!("Successfully connected to: {}", remote_endpoint);
            response_server.initiate_handshake().await;
        }

        Ok(())
    }

    pub fn new_null() -> Self {
        Self::new(NetworkOptions::new_test_instance())
    }

    pub fn stop(&self) {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            self.close();
        }
    }

    fn close(&self) {
        self.state.lock().unwrap().close_channels();
    }

    pub fn get_next_channel_id(&self) -> usize {
        self.next_channel_id.fetch_add(1, Ordering::SeqCst)
    }

    pub fn not_a_peer(&self, endpoint: &SocketAddrV6, allow_local_peers: bool) -> bool {
        endpoint.ip().is_unspecified()
            || reserved_address(endpoint, allow_local_peers)
            || endpoint
                == &SocketAddrV6::new(Ipv6Addr::LOCALHOST, self.port.load(Ordering::SeqCst), 0, 0)
    }

    pub fn on_new_channel(&self, callback: Arc<dyn Fn(Arc<ChannelEnum>) + Send + Sync>) {
        self.state
            .lock()
            .unwrap()
            .new_channel_observers
            .push(callback);
    }

    pub fn insert_fake(&self, endpoint: SocketAddrV6) {
        let fake = Arc::new(ChannelEnum::Fake(ChannelFake::new(
            SystemTime::now(),
            self.get_next_channel_id(),
            &self.async_rt,
            Arc::clone(&self.limiter),
            Arc::clone(&self.stats),
            endpoint,
            self.network_params.network.protocol_info(),
        )));
        fake.set_node_id(PublicKey::from(fake.channel_id() as u64));
        let mut channels = self.state.lock().unwrap();
        channels.channels.insert(fake, None);
    }

    pub(crate) fn check_limits(&self, ip: &Ipv6Addr, direction: ChannelDirection) -> AcceptResult {
        self.state.lock().unwrap().check_limits(ip, direction)
    }

    pub(crate) fn remove_attempt(&self, remote: &SocketAddrV6) {
        self.state.lock().unwrap().attempts.remove(&remote);
    }

    fn check(&self, endpoint: &SocketAddrV6, node_id: &Account, channels: &State) -> bool {
        if self.stopped.load(Ordering::SeqCst) {
            return false; // Reject
        }

        if self.not_a_peer(endpoint, self.allow_local_peers) {
            self.stats
                .inc(StatType::TcpChannelsRejected, DetailType::NotAPeer);
            debug!("Rejected invalid endpoint channel from: {}", endpoint);

            return false; // Reject
        }

        let has_duplicate = channels.channels.iter().any(|entry| {
            if entry.endpoint().ip() == endpoint.ip() {
                // Only counsider channels with the same node id as duplicates if they come from the same IP
                if entry.node_id() == Some(*node_id) {
                    return true;
                }
            }

            false
        });

        if has_duplicate {
            self.stats
                .inc(StatType::TcpChannelsRejected, DetailType::ChannelDuplicate);
            debug!(
                "Duplicate channel rejected from: {} ({})",
                endpoint,
                node_id.to_node_id()
            );

            return false; // Reject
        }

        true // OK
    }

    pub fn find_channel(&self, endpoint: &SocketAddrV6) -> Option<Arc<ChannelEnum>> {
        self.state.lock().unwrap().find_channel(endpoint)
    }

    pub fn random_channels(&self, count: usize, min_version: u8) -> Vec<Arc<ChannelEnum>> {
        self.state
            .lock()
            .unwrap()
            .random_realtime_channels(count, min_version)
    }

    pub fn get_peers(&self) -> Vec<SocketAddrV6> {
        self.state.lock().unwrap().get_realtime_peers()
    }

    pub fn find_node_id(&self, node_id: &PublicKey) -> Option<Arc<ChannelEnum>> {
        self.state.lock().unwrap().find_node_id(node_id)
    }

    pub fn collect_container_info(&self, name: impl Into<String>) -> ContainerInfoComponent {
        self.state.lock().unwrap().collect_container_info(name)
    }

    pub fn random_fill(&self, endpoints: &mut [SocketAddrV6]) {
        self.state.lock().unwrap().random_fill(endpoints);
    }

    pub fn random_fanout(&self, scale: f32) -> Vec<Arc<ChannelEnum>> {
        self.state.lock().unwrap().random_fanout(scale)
    }

    pub fn random_list(&self, count: usize, min_version: u8) -> Vec<Arc<ChannelEnum>> {
        self.state
            .lock()
            .unwrap()
            .random_realtime_channels(count, min_version)
    }

    pub fn flood_message2(&self, message: &Message, drop_policy: BufferDropPolicy, scale: f32) {
        let channels = self.random_fanout(scale);
        for channel in channels {
            channel.send(message, None, drop_policy, TrafficType::Generic)
        }
    }

    pub fn flood_message(&self, message: &Message, scale: f32) {
        let channels = self.random_fanout(scale);
        for channel in channels {
            channel.send(
                message,
                None,
                BufferDropPolicy::Limiter,
                TrafficType::Generic,
            )
        }
    }

    pub fn max_ip_or_subnetwork_connections(&self, endpoint: &SocketAddrV6) -> bool {
        self.max_ip_connections(endpoint) || self.max_subnetwork_connections(endpoint)
    }

    fn max_ip_connections(&self, endpoint: &SocketAddrV6) -> bool {
        if self.flags.disable_max_peers_per_ip {
            return false;
        }
        let mut result;
        let address = ipv4_address_or_ipv6_subnet(endpoint.ip());
        let lock = self.state.lock().unwrap();
        result = lock.channels.count_by_ip(&address) >= lock.network_constants.max_peers_per_ip;
        if !result {
            result =
                lock.attempts.count_by_address(&address) >= lock.network_constants.max_peers_per_ip;
        }
        if result {
            self.stats
                .inc_dir(StatType::Tcp, DetailType::MaxPerIp, Direction::Out);
        }
        result
    }

    fn max_subnetwork_connections(&self, endoint: &SocketAddrV6) -> bool {
        if self.flags.disable_max_peers_per_subnetwork {
            return false;
        }

        let subnet = map_address_to_subnetwork(endoint.ip());
        let is_max = {
            let guard = self.state.lock().unwrap();
            guard.channels.count_by_subnet(&subnet)
                >= self.network_params.network.max_peers_per_subnetwork
                || guard.attempts.count_by_subnetwork(&subnet)
                    >= self.network_params.network.max_peers_per_subnetwork
        };

        if is_max {
            self.stats
                .inc_dir(StatType::Tcp, DetailType::MaxPerSubnetwork, Direction::Out);
        }

        is_max
    }

    pub fn track_connection_attempt(&self, endpoint: &SocketAddrV6) -> bool {
        if self.flags.disable_tcp_realtime {
            return false;
        }

        // Don't contact invalid IPs
        if self.not_a_peer(endpoint, self.allow_local_peers) {
            return false;
        }

        // Don't overload single IP
        if self.max_ip_or_subnetwork_connections(endpoint) {
            return false;
        }

        let mut state = self.state.lock().unwrap();
        if state.excluded_peers.is_excluded(endpoint) {
            return false;
        }

        // Don't connect to nodes that already sent us something
        if state.find_channel(endpoint).is_some() {
            return false;
        }

        if state.attempts.contains(endpoint) {
            return false;
        }

        let count = state.attempts.count_by_address(endpoint.ip());
        if count >= self.tcp_config.max_attempts_per_ip {
            self.stats.inc_dir(
                StatType::TcpListenerRejected,
                DetailType::MaxAttemptsPerIp,
                Direction::Out,
            );
            debug!(
                        "Connection attempt already in progress ({}), unable to initiate new connection: {}",
                        count, endpoint.ip()
                    );
            return false; // Rejected
        }

        if state.check_limits(endpoint.ip(), ChannelDirection::Outbound) != AcceptResult::Accepted {
            self.stats.inc_dir(
                StatType::TcpListener,
                DetailType::ConnectRejected,
                Direction::Out,
            );
            // Refusal reason should be logged earlier

            return false; // Rejected
        }

        self.stats.inc_dir(
            StatType::TcpListener,
            DetailType::ConnectInitiate,
            Direction::Out,
        );
        debug!("Initiate outgoing connection to: {}", endpoint);

        state.attempts.insert(*endpoint, ChannelDirection::Outbound);
        true
    }

    pub fn len_sqrt(&self) -> f32 {
        self.state.lock().unwrap().len_sqrt()
    }
    /// Desired fanout for a given scale
    /// Simulating with sqrt_broadcast_simulate shows we only need to broadcast to sqrt(total_peers) random peers in order to successfully publish to everyone with high probability
    pub fn fanout(&self, scale: f32) -> usize {
        self.state.lock().unwrap().fanout(scale)
    }

    /// Returns channel IDs of removed channels
    pub fn purge(&self, cutoff: SystemTime) -> Vec<usize> {
        let mut guard = self.state.lock().unwrap();
        guard.purge(cutoff)
    }

    pub fn erase_channel_by_endpoint(&self, endpoint: &SocketAddrV6) {
        self.state
            .lock()
            .unwrap()
            .channels
            .remove_by_endpoint(endpoint);
    }

    pub fn count_by_mode(&self, mode: ChannelMode) -> usize {
        self.state.lock().unwrap().channels.count_by_mode(mode)
    }

    pub fn count_by_direction(&self, direction: ChannelDirection) -> usize {
        self.state
            .lock()
            .unwrap()
            .channels
            .count_by_direction(direction)
    }

    pub fn bootstrap_peer(&self) -> SocketAddrV6 {
        self.state.lock().unwrap().bootstrap_peer()
    }

    pub fn list_channels(&self, min_version: u8) -> Vec<Arc<ChannelEnum>> {
        let mut result = self.state.lock().unwrap().list_realtime(min_version);
        result.sort_by_key(|i| i.remote_endpoint());
        result
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::SeqCst);
    }

    pub fn create_keepalive_message(&self) -> Message {
        let mut peers = [SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0); 8];
        self.random_fill(&mut peers);
        Message::Keepalive(Keepalive { peers })
    }

    pub fn sample_keepalive(&self) -> Option<Keepalive> {
        let channels = self.state.lock().unwrap();
        let mut rng = thread_rng();
        for _ in 0..channels.channels.len() {
            let index = rng.gen_range(0..channels.channels.len());
            if let Some(channel) = channels.channels.get_by_index(index) {
                if let Some(server) = &channel.response_server {
                    if let Some(keepalive) = server.pop_last_keepalive() {
                        return Some(keepalive);
                    }
                }
            }
        }

        None
    }

    pub fn is_excluded(&self, addr: &SocketAddrV6) -> bool {
        self.state.lock().unwrap().is_excluded(addr)
    }

    pub fn is_excluded_ip(&self, ip: &Ipv6Addr) -> bool {
        self.state.lock().unwrap().is_excluded_ip(ip)
    }

    pub fn peer_misbehaved(&self, channel: &Arc<ChannelEnum>) {
        // Add to peer exclusion list
        self.state
            .lock()
            .unwrap()
            .peer_misbehaved(&channel.remote_endpoint());

        // Disconnect
        if channel.get_type() == TransportType::Tcp {
            self.erase_channel_by_endpoint(&channel.remote_endpoint())
        }
    }
}

pub trait NetworkExt {
    fn upgrade_to_realtime_connection(&self, remote_endpoint: &SocketAddrV6, node_id: Account);
    fn keepalive(&self);
}

impl NetworkExt for Arc<Network> {
    fn upgrade_to_realtime_connection(&self, remote_endpoint: &SocketAddrV6, node_id: Account) {
        let (observers, channel) = {
            let mut state = self.state.lock().unwrap();

            if self.stopped.load(Ordering::SeqCst) {
                return;
            }

            let Some(entry) = state.channels.get(remote_endpoint) else {
                return;
            };

            if let Some(other) = state.channels.get_by_node_id(&node_id) {
                if other.ip_address() == entry.ip_address() {
                    // We already have a connection to that node. We allow duplicate node ids, but
                    // only if they come from different IP addresses
                    let endpoint = entry.endpoint();
                    state.channels.remove_by_endpoint(&endpoint);
                    drop(state);
                    debug!(
                        node_id = node_id.to_node_id(),
                        remote = %endpoint,
                        "Dropping channel, because another channel for the same node ID was found"
                    );
                    return;
                }
            }

            entry.channel.set_node_id(node_id);
            entry.channel.set_mode(ChannelMode::Realtime);

            let observers = state.new_channel_observers.clone();
            let channel = entry.channel.clone();
            (observers, channel)
        };

        self.stats
            .inc(StatType::TcpChannels, DetailType::ChannelAccepted);
        debug!(
            "Accepted new channel from: {} ({})",
            remote_endpoint,
            node_id.to_node_id()
        );

        for observer in observers {
            observer(channel.clone());
        }
    }

    fn keepalive(&self) {
        let message = self.create_keepalive_message();

        // Wake up channels
        let to_wake_up = {
            let guard = self.state.lock().unwrap();
            guard.keepalive_list()
        };

        for channel in to_wake_up {
            let ChannelEnum::Tcp(tcp) = channel.as_ref() else {
                continue;
            };
            tcp.send(
                &message,
                None,
                BufferDropPolicy::Limiter,
                TrafficType::Generic,
            );
        }
    }
}

struct State {
    attempts: AttemptContainer,
    channels: ChannelContainer,
    network_constants: NetworkConstants,
    new_channel_observers: Vec<Arc<dyn Fn(Arc<ChannelEnum>) + Send + Sync>>,
    excluded_peers: PeerExclusion,
    stats: Arc<Stats>,
    node_flags: NodeFlags,
    config: TcpConfig,
}

impl State {
    pub fn bootstrap_peer(&mut self) -> SocketAddrV6 {
        let mut channel_endpoint = None;
        let mut peering_endpoint = None;
        for channel in self.channels.iter_by_last_bootstrap_attempt() {
            if channel.channel.mode() == ChannelMode::Realtime
                && channel.network_version() >= self.network_constants.protocol_version_min
            {
                if let ChannelEnum::Tcp(tcp) = channel.channel.as_ref() {
                    channel_endpoint = Some(channel.endpoint());
                    peering_endpoint = Some(tcp.peering_endpoint());
                    break;
                }
            }
        }

        match (channel_endpoint, peering_endpoint) {
            (Some(ep), Some(peering)) => {
                self.channels
                    .set_last_bootstrap_attempt(&ep, SystemTime::now());
                peering
            }
            _ => SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0),
        }
    }

    pub fn close_channels(&mut self) {
        for channel in self.channels.iter() {
            channel.close();
            // Remove response server
            if let Some(server) = &channel.response_server {
                server.stop();
            }
        }
        self.channels.clear();
    }

    pub fn purge(&mut self, cutoff: SystemTime) -> Vec<usize> {
        self.channels.close_idle_channels(cutoff);

        // Check if any tcp channels belonging to old protocol versions which may still be alive due to async operations
        self.channels
            .close_old_protocol_versions(self.network_constants.protocol_version_min);

        // Remove channels with dead underlying sockets
        let purged_channel_ids = self.channels.remove_dead();

        // Remove keepalive attempt tracking for attempts older than cutoff
        self.attempts.purge(cutoff);
        purged_channel_ids
    }

    pub fn random_realtime_channels(&self, count: usize, min_version: u8) -> Vec<Arc<ChannelEnum>> {
        let mut channels = self.list_realtime(min_version);
        let mut rng = thread_rng();
        channels.shuffle(&mut rng);
        if count > 0 {
            channels.truncate(count)
        }
        channels
    }

    pub fn list_realtime(&self, min_version: u8) -> Vec<Arc<ChannelEnum>> {
        self.channels
            .iter()
            .filter(|c| {
                c.network_version() >= min_version
                    && c.channel.is_alive()
                    && c.channel.mode() == ChannelMode::Realtime
            })
            .map(|c| c.channel.clone())
            .collect()
    }

    pub fn keepalive_list(&self) -> Vec<Arc<ChannelEnum>> {
        let cutoff = SystemTime::now() - self.network_constants.keepalive_period;
        let mut result = Vec::new();
        for channel in self.channels.iter() {
            if channel.channel.mode() == ChannelMode::Realtime
                && channel.last_packet_sent() < cutoff
            {
                result.push(channel.channel.clone());
            }
        }

        result
    }

    pub fn find_channel(&self, endpoint: &SocketAddrV6) -> Option<Arc<ChannelEnum>> {
        self.channels.get(endpoint).map(|c| c.channel.clone())
    }

    pub fn get_realtime_peers(&self) -> Vec<SocketAddrV6> {
        // We can't hold the mutex while starting a write transaction, so
        // we collect endpoints to be saved and then release the lock.
        self.channels
            .iter()
            .filter(|c| c.channel.mode() == ChannelMode::Realtime)
            .map(|c| c.endpoint())
            .collect()
    }

    pub fn find_node_id(&self, node_id: &PublicKey) -> Option<Arc<ChannelEnum>> {
        self.channels
            .get_by_node_id(node_id)
            .map(|c| c.channel.clone())
    }

    pub fn random_fanout(&self, scale: f32) -> Vec<Arc<ChannelEnum>> {
        self.random_realtime_channels(self.fanout(scale), 0)
    }

    pub fn random_fill(&self, endpoints: &mut [SocketAddrV6]) {
        // Don't include channels with ephemeral remote ports
        let peers = self.random_realtime_channels(endpoints.len(), 0);
        let null_endpoint = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0);
        for (i, target) in endpoints.iter_mut().enumerate() {
            let endpoint = if i < peers.len() {
                let ChannelEnum::Tcp(tcp) = peers[i].as_ref() else {
                    panic!("not a tcp channel")
                };
                tcp.peering_endpoint()
            } else {
                null_endpoint
            };
            *target = endpoint;
        }
    }

    pub fn len_sqrt(&self) -> f32 {
        f32::sqrt(self.channels.count_by_mode(ChannelMode::Realtime) as f32)
    }

    pub fn fanout(&self, scale: f32) -> usize {
        (self.len_sqrt() * scale).ceil() as usize
    }

    pub fn is_excluded(&mut self, endpoint: &SocketAddrV6) -> bool {
        self.excluded_peers.is_excluded(endpoint)
    }

    pub fn is_excluded_ip(&mut self, ip: &Ipv6Addr) -> bool {
        self.excluded_peers.is_excluded_ip(ip)
    }

    pub fn peer_misbehaved(&mut self, addr: &SocketAddrV6) {
        self.excluded_peers.peer_misbehaved(addr);
    }

    pub fn check_limits(&mut self, ip: &Ipv6Addr, direction: ChannelDirection) -> AcceptResult {
        if self.is_excluded_ip(ip) {
            self.stats.inc_dir(
                StatType::TcpListenerRejected,
                DetailType::Excluded,
                direction.into(),
            );

            debug!("Rejected connection from excluded peer: {}", ip);
            return AcceptResult::Rejected;
        }

        if !self.node_flags.disable_max_peers_per_ip {
            let count = self.channels.count_by_ip(ip);
            if count >= self.network_constants.max_peers_per_ip {
                self.stats.inc_dir(
                    StatType::TcpListenerRejected,
                    DetailType::MaxPerIp,
                    direction.into(),
                );
                debug!(
                    "Max connections per IP reached ({}), unable to open new connection",
                    ip
                );
                return AcceptResult::Rejected;
            }
        }

        // If the address is IPv4 we don't check for a network limit, since its address space isn't big as IPv6/64.
        if !self.node_flags.disable_max_peers_per_subnetwork
            && !is_ipv4_or_v4_mapped_address(&(*ip).into())
        {
            let subnet = map_address_to_subnetwork(ip);
            let count = self.channels.count_by_subnet(&subnet);
            if count >= self.network_constants.max_peers_per_subnetwork {
                self.stats.inc_dir(
                    StatType::TcpListenerRejected,
                    DetailType::MaxPerSubnetwork,
                    direction.into(),
                );
                debug!(
                    "Max connections per subnetwork reached ({}), unable to open new connection",
                    ip
                );
                return AcceptResult::Rejected;
            }
        }

        match direction {
            ChannelDirection::Inbound => {
                let count = self.channels.count_by_direction(ChannelDirection::Inbound);

                if count >= self.config.max_inbound_connections {
                    self.stats.inc_dir(
                        StatType::TcpListenerRejected,
                        DetailType::MaxAttempts,
                        direction.into(),
                    );
                    debug!(
                        "Max inbound connections reached ({}), unable to accept new connection: {}",
                        count, ip
                    );
                    return AcceptResult::Rejected;
                }
            }
            ChannelDirection::Outbound => {
                let count = self.channels.count_by_direction(ChannelDirection::Outbound);

                if count >= self.config.max_outbound_connections {
                    self.stats.inc_dir(
                        StatType::TcpListenerRejected,
                        DetailType::MaxAttempts,
                        direction.into(),
                    );
                    debug!(
                        "Max outbound connections reached ({}), unable to initiate new connection: {}",
                        count, ip
                    );
                    return AcceptResult::Rejected;
                }
            }
        }

        AcceptResult::Accepted
    }

    pub fn collect_container_info(&self, name: impl Into<String>) -> ContainerInfoComponent {
        ContainerInfoComponent::Composite(
            name.into(),
            vec![
                ContainerInfoComponent::Leaf(ContainerInfo {
                    name: "channels".to_string(),
                    count: self.channels.len(),
                    sizeof_element: ChannelContainer::ELEMENT_SIZE,
                }),
                ContainerInfoComponent::Leaf(ContainerInfo {
                    name: "attempts".to_string(),
                    count: self.attempts.len(),
                    sizeof_element: AttemptContainer::ELEMENT_SIZE,
                }),
                ContainerInfoComponent::Leaf(ContainerInfo {
                    name: "peers".to_string(),
                    count: self.excluded_peers.size(),
                    sizeof_element: PeerExclusion::element_size(),
                }),
            ],
        )
    }
}

#[derive(PartialEq, Eq)]
pub enum AcceptResult {
    Invalid,
    Accepted,
    Rejected,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "todo"]
    async fn initiate_handshake_when_outbound_connection_added() {
        let network = Network::new_null();
        let socket = Arc::new(Socket::new_null());
        let response_server = Arc::new(ResponseServerImpl::new_null());

        network
            .add(&socket, &response_server, ChannelDirection::Outbound)
            .await
            .unwrap();

        // TODO assert that initiate handshake was called
    }
}