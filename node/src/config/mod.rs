mod converters;
mod daemon_config;
mod network_constants;
mod network_params;
mod node_config;
mod node_flags;
mod toml;
mod websocket_config;

pub use daemon_config::*;
pub use network_constants::*;
pub use network_params::*;
pub use node_config::*;
pub use node_flags::*;
pub use rsnano_types::Networks;
use rsnano_wallet::WalletsConfig;
use serde::de::DeserializeOwned;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
pub use toml::DaemonToml;
pub use websocket_config::WebsocketConfig;

pub fn get_node_toml_config_path(data_path: impl Into<PathBuf>) -> PathBuf {
    let mut node_toml = data_path.into();
    node_toml.push("config-node.toml");
    node_toml
}

pub fn get_rpc_toml_config_path(data_path: impl Into<PathBuf>) -> PathBuf {
    let mut rpc_toml = data_path.into();
    rpc_toml.push("config-rpc.toml");
    rpc_toml
}

pub fn get_default_rpc_filepath_from(node_exe_path: &Path) -> PathBuf {
    let mut result = node_exe_path.to_path_buf();
    result.pop();
    result.push("nano_rpc");
    if let Some(ext) = node_exe_path.extension() {
        result.set_extension(ext);
    }
    result
}

pub struct GlobalConfig {
    pub node_config: NodeConfig,
    pub flags: NodeFlags,
    pub network_params: NetworkParams,
}

pub fn read_toml_file<T: DeserializeOwned>(path: impl AsRef<Path>) -> anyhow::Result<T> {
    let toml_str = std::fs::read_to_string(path)?;
    ::toml::from_str(&toml_str).map_err(|e| e.into())
}

impl GlobalConfig {
    pub fn wallets_config(&self) -> WalletsConfig {
        let node = &self.node_config;
        WalletsConfig {
            preconfigured_representatives: node.preconfigured_representatives.clone(),
            password_fanout: node.password_fanout as usize,
            receive_minimum: node.receive_minimum,
            vote_minimum: node.vote_minimum,
            voting_enabled: node.enable_voting,
            cached_work_generation_delay: if self.network_params.network.is_dev_network() {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(10)
            },
            kdf_work: if self.network_params.network.is_dev_network() {
                8
            } else {
                1024 * 64
            },
        }
    }
}
