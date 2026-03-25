use std::time::Duration;

use rsnano_types::{
    NetworkType, ProtocolInfo,
    currency_constants::{DEFAULT_PORT_NODE, DEFAULT_PORT_RPC, DEFAULT_PORT_WEBSOCKET},
};
use rsnano_work::WorkThresholds;

use crate::bootstrap::BootstrapConfig;

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkConstants {
    pub work: WorkThresholds,
    pub default_node_port: u16,
    pub default_rpc_port: u16,
    pub default_websocket_port: u16,
    pub aec_loop_interval: Duration,
    /** How often to send keepalive messages */
    pub keepalive_period: Duration,
    pub sync_cookie_cutoff: Duration,
    /** Maximum number of peers per IP. It is also the max number of connections per IP*/
    pub max_peers_per_ip: usize,
    /** Maximum number of peers per subnetwork */
    pub max_peers_per_subnetwork: usize,

    pub current_network: NetworkType,
    /** Current protocol version */
    pub protocol_version: u8,
    /** Minimum accepted protocol version */
    pub protocol_version_min: u8,
    /** Minimum accepted protocol version used when bootstrapping */
    pub bootstrap_protocol_version_min: u8,

    /** How often to request telemetry from peers */
    pub telemetry_request_interval_ms: i64,
    /** How often to broadcast telemetry to peers */
    pub telemetry_broadcast_interval_ms: i64,
    /** Telemetry data older than this value is considered stale */
    pub telemetry_cache_cutoff_ms: i64, // 2 * `telemetry_broadcast_interval` + some margin
    /// How much to delay activation of optimistic elections to avoid interfering with election scheduler
    pub optimistic_activation_delay: Duration,
    pub rep_crawler_normal_interval: Duration,
    pub rep_crawler_warmup_interval: Duration,
}

impl NetworkConstants {
    pub fn empty() -> Self {
        Self::new(WorkThresholds::publish_dev().clone(), NetworkType::Invalid)
    }

    pub fn new(work: WorkThresholds, network: NetworkType) -> Self {
        match network {
            NetworkType::NanoDevNetwork => Self::dev(work),
            NetworkType::NanoBetaNetwork => Self::beta(work),
            NetworkType::NanoLiveNetwork | NetworkType::Invalid => Self::live(work),
            NetworkType::NanoTestNetwork => Self::test(work),
        }
    }

    pub fn for_network(network: NetworkType) -> Self {
        Self::new(WorkThresholds::default_for(network), network)
    }

    pub fn protocol_info(&self) -> ProtocolInfo {
        ProtocolInfo {
            version_using: self.protocol_version,
            version_max: self.protocol_version,
            version_min: self.protocol_version_min,
            network: self.current_network,
        }
    }

    fn live(work: WorkThresholds) -> Self {
        let protocol_info = ProtocolInfo::default();
        Self {
            work,
            current_network: NetworkType::NanoLiveNetwork,
            protocol_version: protocol_info.version_using,
            protocol_version_min: protocol_info.version_min,
            bootstrap_protocol_version_min: BootstrapConfig::default().min_protocol_version,
            default_node_port: DEFAULT_PORT_NODE,
            default_rpc_port: DEFAULT_PORT_RPC,
            default_websocket_port: DEFAULT_PORT_WEBSOCKET,
            aec_loop_interval: Duration::from_millis(300),
            keepalive_period: Duration::from_secs(15),
            sync_cookie_cutoff: Duration::from_secs(5),
            max_peers_per_ip: 4,
            max_peers_per_subnetwork: 16,
            telemetry_request_interval_ms: 1000 * 60,
            telemetry_broadcast_interval_ms: 1000 * 60,
            telemetry_cache_cutoff_ms: 1000 * 130, //  2 * `telemetry_broadcast_interval` + some margin
            optimistic_activation_delay: Duration::from_secs(30),
            rep_crawler_normal_interval: Duration::from_secs(7),
            rep_crawler_warmup_interval: Duration::from_secs(3),
        }
    }

    pub fn for_beta() -> Self {
        Self {
            current_network: NetworkType::NanoBetaNetwork,
            default_node_port: 54000,
            default_rpc_port: 55000,
            default_websocket_port: 57000,
            max_peers_per_ip: 256,
            max_peers_per_subnetwork: 256,
            ..Self::live(WorkThresholds::publish_beta().clone())
        }
    }

    fn beta(work: WorkThresholds) -> Self {
        Self {
            current_network: NetworkType::NanoBetaNetwork,
            default_node_port: 54000,
            default_rpc_port: 55000,
            default_websocket_port: 57000,
            max_peers_per_ip: 256,
            max_peers_per_subnetwork: 256,
            ..Self::live(work)
        }
    }

    fn test(work: WorkThresholds) -> Self {
        Self {
            current_network: NetworkType::NanoTestNetwork,
            default_node_port: test_node_port(),
            default_rpc_port: test_rpc_port(),
            default_websocket_port: test_websocket_port(),
            ..Self::live(work)
        }
    }

    fn dev(work: WorkThresholds) -> Self {
        Self {
            current_network: NetworkType::NanoDevNetwork,
            default_node_port: 44000,
            default_rpc_port: 45000,
            default_websocket_port: 47000,
            aec_loop_interval: Duration::from_millis(20),
            keepalive_period: Duration::from_secs(1),
            max_peers_per_ip: 256, // During tests, all peers are on localhost
            max_peers_per_subnetwork: 256,
            telemetry_cache_cutoff_ms: 2000,
            telemetry_request_interval_ms: 500,
            telemetry_broadcast_interval_ms: 500,
            optimistic_activation_delay: Duration::from_secs(2),
            rep_crawler_normal_interval: Duration::from_millis(500),
            rep_crawler_warmup_interval: Duration::from_millis(500),
            ..Self::live(work)
        }
    }

    pub fn is_live_network(&self) -> bool {
        self.current_network == NetworkType::NanoLiveNetwork
    }

    pub fn is_beta_network(&self) -> bool {
        self.current_network == NetworkType::NanoBetaNetwork
    }

    pub fn is_dev_network(&self) -> bool {
        self.current_network == NetworkType::NanoDevNetwork
    }

    pub fn is_test_network(&self) -> bool {
        self.current_network == NetworkType::NanoTestNetwork
    }

    pub fn get_current_network_as_string(&self) -> &str {
        match self.current_network {
            NetworkType::NanoDevNetwork => "dev",
            NetworkType::NanoBetaNetwork => "beta",
            NetworkType::NanoLiveNetwork => "live",
            NetworkType::NanoTestNetwork => "test",
            NetworkType::Invalid => panic!("invalid network"),
        }
    }

    pub fn default_for(network: NetworkType) -> Self {
        match network {
            NetworkType::Invalid => Self::empty(),
            NetworkType::NanoBetaNetwork => Self::new(
                WorkThresholds::publish_beta().clone(),
                NetworkType::NanoBetaNetwork,
            ),
            NetworkType::NanoDevNetwork => Self::new(
                WorkThresholds::publish_dev().clone(),
                NetworkType::NanoDevNetwork,
            ),
            NetworkType::NanoLiveNetwork => Self::new(
                WorkThresholds::publish_full().clone(),
                NetworkType::NanoLiveNetwork,
            ),
            NetworkType::NanoTestNetwork => Self::new(
                WorkThresholds::publish_test().clone(),
                NetworkType::NanoTestNetwork,
            ),
        }
    }
}

pub fn test_node_port() -> u16 {
    get_env_or_default("NANO_TEST_NODE_PORT", 17075)
}

fn test_rpc_port() -> u16 {
    get_env_or_default("NANO_TEST_RPC_PORT", 17076)
}

fn test_websocket_port() -> u16 {
    get_env_or_default("NANO_TEST_WEBSOCKET_PORT", 17078)
}

fn get_env_or_default<T>(variable_name: &str, default: T) -> T
where
    T: core::str::FromStr + Copy,
{
    std::env::var(variable_name)
        .map(|v| v.parse::<T>().unwrap_or(default))
        .unwrap_or(default)
}
