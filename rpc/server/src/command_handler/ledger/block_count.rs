use crate::command_handler::RpcCommandHandler;
use rsnano_rpc_messages::BlockCountResponse;

impl RpcCommandHandler {
    pub(crate) fn block_count(&self) -> BlockCountResponse {
        let count = self.node.ledger.block_count();
        let unchecked = self.node.unchecked.lock().unwrap().len() as u64;
        let cemented = self.node.ledger.confirmed_count();
        BlockCountResponse {
            #[cfg(feature = "rai_protocol")]
            confirmation_epochs: Some(
                self.node
                    .ledger
                    .confirmation_epoch_sets()
                    .into_iter()
                    .map(|(epoch, (count, digest))| {
                        (
                            epoch.to_string(),
                            rsnano_rpc_messages::ConfirmationEpochSet {
                                count: count.into(),
                                digest,
                            },
                        )
                    })
                    .collect(),
            ),
            #[cfg(not(feature = "rai_protocol"))]
            confirmation_epochs: None,
            current_epoch: cfg!(feature = "rai_protocol")
                .then(|| self.node.ledger.current_epoch().into()),
            count: count.into(),
            unchecked: unchecked.into(),
            cemented: cemented.into(),
            full: None,
            pruned: None,
        }
    }
}
