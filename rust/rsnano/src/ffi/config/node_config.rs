use std::{convert::TryFrom, ffi::c_void};

use crate::{
    config::NodeConfig,
    ffi::{secure::NetworkParamsDto, toml::FfiToml},
    numbers::Amount,
    secure::NetworkParams,
};

#[repr(C)]
pub struct NodeConfigDto {
    pub peering_port: u16,
    pub bootstrap_fraction_numerator: u32,
    pub receive_minimum: [u8; 16],
    pub online_weight_minimum: [u8; 16],
    pub election_hint_weight_percent: u32,
    pub password_fanout: u32,
    pub io_threads: u32,
    pub network_threads: u32,
    pub work_threads: u32,
    pub signature_checker_threads: u32,
    pub enable_voting: bool,
    pub bootstrap_connections: u32,
    pub bootstrap_connections_max: u32,
    pub bootstrap_initiator_threads: u32,
    pub bootstrap_frontier_request_count: u32,
    pub block_processor_batch_max_time_ms: i64,
    pub allow_local_peers: bool,
    pub vote_minimum: [u8; 16],
    pub vote_generator_delay_ms: i64,
    pub vote_generator_threshold: u32,
    pub unchecked_cutoff_time_s: i64,
    pub tcp_io_timeout_s: i64,
    pub pow_sleep_interval_ns: i64,
    pub external_address: [u8; 128],
    pub external_address_len: usize,
    pub external_port: u16,
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_config_create(
    dto: *mut NodeConfigDto,
    peering_port: u16,
    network_params: &NetworkParamsDto,
) -> i32 {
    let network_params = match NetworkParams::try_from(network_params) {
        Ok(n) => n,
        Err(_) => return -1,
    };
    let cfg = NodeConfig::new(peering_port, &network_params);
    let dto = &mut (*dto);
    dto.peering_port = cfg.peering_port;
    dto.bootstrap_fraction_numerator = cfg.bootstrap_fraction_numerator;
    dto.receive_minimum = cfg.receive_minimum.to_be_bytes();
    dto.online_weight_minimum = cfg.online_weight_minimum.to_be_bytes();
    dto.election_hint_weight_percent = cfg.election_hint_weight_percent;
    dto.password_fanout = cfg.password_fanout;
    dto.io_threads = cfg.io_threads;
    dto.network_threads = cfg.network_threads;
    dto.work_threads = cfg.work_threads;
    dto.signature_checker_threads = cfg.signature_checker_threads;
    dto.enable_voting = cfg.enable_voting;
    dto.bootstrap_connections = cfg.bootstrap_connections;
    dto.bootstrap_connections_max = cfg.bootstrap_connections_max;
    dto.bootstrap_initiator_threads = cfg.bootstrap_initiator_threads;
    dto.bootstrap_frontier_request_count = cfg.bootstrap_frontier_request_count;
    dto.block_processor_batch_max_time_ms = cfg.block_processor_batch_max_time_ms;
    dto.allow_local_peers = cfg.allow_local_peers;
    dto.vote_minimum = cfg.vote_minimum.to_be_bytes();
    dto.vote_generator_delay_ms = cfg.vote_generator_delay_ms;
    dto.vote_generator_threshold = cfg.vote_generator_threshold;
    dto.unchecked_cutoff_time_s = cfg.unchecked_cutoff_time_s;
    dto.tcp_io_timeout_s = cfg.tcp_io_timeout_s;
    dto.pow_sleep_interval_ns = cfg.pow_sleep_interval_ns;
    let bytes: &[u8] = cfg.external_address.as_bytes();
    dto.external_address[..bytes.len()].copy_from_slice(bytes);
    dto.external_address_len = bytes.len();
    dto.external_port = cfg.external_port;
    0
}

#[no_mangle]
pub extern "C" fn rsn_node_config_serialize_toml(dto: &NodeConfigDto, toml: *mut c_void) -> i32 {
    let cfg = match NodeConfig::try_from(dto) {
        Ok(c) => c,
        Err(_) => return -1,
    };
    let mut toml = FfiToml::new(toml);
    match cfg.serialize_toml(&mut toml) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

impl TryFrom<&NodeConfigDto> for NodeConfig {
    type Error = anyhow::Error;

    fn try_from(value: &NodeConfigDto) -> Result<Self, Self::Error> {
        let cfg = NodeConfig {
            peering_port: value.peering_port,
            bootstrap_fraction_numerator: value.bootstrap_fraction_numerator,
            receive_minimum: Amount::from_be_bytes(value.receive_minimum),
            online_weight_minimum: Amount::from_be_bytes(value.online_weight_minimum),
            election_hint_weight_percent: value.election_hint_weight_percent,
            password_fanout: value.password_fanout,
            io_threads: value.io_threads,
            network_threads: value.network_threads,
            work_threads: value.work_threads,
            signature_checker_threads: value.signature_checker_threads,
            enable_voting: value.enable_voting,
            bootstrap_connections: value.bootstrap_connections,
            bootstrap_connections_max: value.bootstrap_connections_max,
            bootstrap_initiator_threads: value.bootstrap_initiator_threads,
            bootstrap_frontier_request_count: value.bootstrap_frontier_request_count,
            block_processor_batch_max_time_ms: value.block_processor_batch_max_time_ms,
            allow_local_peers: value.allow_local_peers,
            vote_minimum: Amount::from_be_bytes(value.vote_minimum),
            vote_generator_delay_ms: value.vote_generator_delay_ms,
            vote_generator_threshold: value.vote_generator_threshold,
            unchecked_cutoff_time_s: value.unchecked_cutoff_time_s,
            tcp_io_timeout_s: value.tcp_io_timeout_s,
            pow_sleep_interval_ns: value.pow_sleep_interval_ns,
            external_address: String::from_utf8_lossy(
                &value.external_address[..value.external_address_len],
            )
            .to_string(),
            external_port: value.external_port,
        };

        Ok(cfg)
    }
}
