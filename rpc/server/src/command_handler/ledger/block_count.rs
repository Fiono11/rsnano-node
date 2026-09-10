use crate::command_handler::RpcCommandHandler;
use rsnano_rpc_messages::BlockCountResponse;

impl RpcCommandHandler {
    pub(crate) fn block_count(&self) -> BlockCountResponse {
        let count = self.node.ledger.block_count();
        let unchecked = self.node.unchecked.lock().unwrap().len() as u64;
        let cemented = self.node.ledger.confirmed_count();
        BlockCountResponse {
            #[cfg(feature = "rai_protocol")]
            draining_epoch: {
                let e = self
                    .node
                    .ledger
                    .draining_epoch
                    .load(std::sync::atomic::Ordering::Acquire);
                (e != u64::MAX).then(|| e.into())
            },
            #[cfg(not(feature = "rai_protocol"))]
            draining_epoch: None,
            #[cfg(feature = "rai_protocol")]
            closed_epochs: Some(
                self.node
                    .ledger
                    .closed_epochs()
                    .into_iter()
                    .map(|(e, h)| (e.to_string(), h))
                    .collect(),
            ),
            #[cfg(not(feature = "rai_protocol"))]
            closed_epochs: None,
            // Close digests are sufficient for epoch agreement; do not scan ledger contents.
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
