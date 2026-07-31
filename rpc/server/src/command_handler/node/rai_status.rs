use crate::command_handler::RpcCommandHandler;
use rsnano_rpc_messages::RaiStatusResponse;

impl RpcCommandHandler {
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_status(&self) -> anyhow::Result<RaiStatusResponse> {
        let (state, close_hashes, cut_hashes, drains) = self.node.aec.rai_epoch_status();
        Ok(RaiStatusResponse {
            open_epoch: state.open_epoch.number().into(),
            closing_epoch: state.closing.map(|value| value.epoch.number().into()),
            closing_phase: state.closing.map(|value| format!("{:?}", value.phase)),
            closed_through: state.closed_through.map(|value| value.number().into()),
            cut_hashes: cut_hashes
                .into_iter()
                .map(|(epoch, hash)| (epoch.number().to_string(), hash.to_string()))
                .collect(),
            close_hashes: close_hashes
                .into_iter()
                .map(|(epoch, hash)| (epoch.number().to_string(), hash.to_string()))
                .collect(),
            drain_obligations: drains
                .iter()
                .map(|(epoch, (obligations, _))| {
                    (epoch.number().to_string(), (*obligations as u64).into())
                })
                .collect(),
            drain_finalized: drains
                .into_iter()
                .map(|(epoch, (_, finalized))| {
                    (epoch.number().to_string(), (finalized as u64).into())
                })
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
