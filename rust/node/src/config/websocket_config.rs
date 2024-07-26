use super::NetworkConstants;
use anyhow::Result;
use rsnano_core::utils::TomlWriter;
use serde::{Deserialize, Serialize};
use std::net::Ipv6Addr;

#[derive(Clone, Deserialize, Serialize)]
pub struct WebsocketConfig {
    pub enabled: bool,
    pub port: u16,
    pub address: String,
}

impl WebsocketConfig {
    pub fn new(network: &NetworkConstants) -> Self {
        Self {
            enabled: false,
            port: network.default_websocket_port,
            address: Ipv6Addr::LOCALHOST.to_string(),
        }
    }

    pub fn serialize_toml(&self, toml: &mut dyn TomlWriter) -> Result<()> {
        toml.put_bool(
            "enable",
            self.enabled,
            "Enable or disable WebSocket server.\ntype:bool",
        )?;
        toml.put_str(
            "address",
            &self.address,
            "WebSocket server bind address.\ntype:string,ip",
        )?;
        toml.put_u16(
            "port",
            self.port,
            "WebSocket server listening port.\ntype:uint16",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_core::work::WorkThresholds;

    #[test]
    fn websocket_config() {
        let cfg = WebsocketConfig::new(&NetworkConstants::new(
            WorkThresholds::publish_full().clone(),
            crate::config::Networks::NanoLiveNetwork,
        ));
        assert_eq!(cfg.enabled, false);
        assert_eq!(cfg.port, 7078);
        assert_eq!(cfg.address, "::1");
    }
}
