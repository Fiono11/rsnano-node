use rsnano_rpc_messages::EpochAdvanceResponse;

use crate::command_handler::RpcCommandHandler;

impl RpcCommandHandler {
    /// RAI: leave the current consensus epoch now, whatever its count. The
    /// epoch's close election starts once its instances have settled.
    pub(crate) fn epoch_advance(&self) -> EpochAdvanceResponse {
        self.node.aec.leave_epoch();
        EpochAdvanceResponse {
            epoch: self.node.aec.current_epoch().as_u64().into(),
        }
    }
}
