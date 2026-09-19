use rsnano_rpc_messages::EpochStartResponse;

use crate::command_handler::RpcCommandHandler;

impl RpcCommandHandler {
    /// RAI: the setup of a run is over, the consensus epochs start now
    pub(crate) fn epoch_start(&self) -> EpochStartResponse {
        self.node.aec.start_epochs();
        EpochStartResponse {
            epoch: self.node.aec.current_epoch().as_u64().into(),
        }
    }
}
