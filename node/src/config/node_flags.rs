#[derive(Clone)]
pub struct NodeFlags {
    pub disable_backup: bool,
    pub disable_rep_crawler: bool,
    /// Disables the AEC ticker
    pub disable_request_loop: bool, // For testing only
    pub disable_providing_telemetry_metrics: bool,
    /// Disables the local block rebroadcaster
    pub disable_block_processor_republishing: bool,
    pub disable_search_pending: bool, // For testing only
    pub enable_voting: bool,
    pub disable_connection_cleanup: bool,
    pub skip_consistency_check: bool, // For testing only
}

impl NodeFlags {
    pub fn new() -> Self {
        Self {
            disable_backup: false,
            disable_rep_crawler: false,
            disable_request_loop: false,
            disable_providing_telemetry_metrics: false,
            disable_block_processor_republishing: false,
            disable_search_pending: false,
            enable_voting: false,
            disable_connection_cleanup: false,
            skip_consistency_check: false,
        }
    }
}

impl Default for NodeFlags {
    fn default() -> Self {
        Self::new()
    }
}
