use crate::command_handler::RpcCommandHandler;
use rsnano_rpc_messages::RaiStatusResponse;

impl RpcCommandHandler {
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_status(&self) -> anyhow::Result<RaiStatusResponse> {
        let (state, close_hashes) = self.node.aec.rai_epoch_status();
        Ok(RaiStatusResponse {
            open_epoch: state.open_epoch.number().into(),
            closing_epoch: state.closing.map(|value| value.epoch.number().into()),
            closed_through: state.closed_through.map(|value| value.number().into()),
            close_hashes: close_hashes
                .into_iter()
                .map(|(epoch, hash)| (epoch.number().to_string(), hash.to_string()))
                .collect(),
            finalized_by_epoch: self
                .node
                .ledger
                .rai_finalized_counts()
                .into_iter()
                .map(|(epoch, count)| (epoch.number().to_string(), count.into()))
                .collect(),
        })
    }

    #[cfg(not(feature = "rai_protocol"))]
    pub(crate) fn rai_status(&self) -> anyhow::Result<RaiStatusResponse> {
        Err(anyhow::anyhow!("RAI protocol is disabled"))
    }
}
