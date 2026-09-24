use rsnano_rpc_messages::{EpochLock, EpochLocksResponse};

use crate::command_handler::RpcCommandHandler;

impl RpcCommandHandler {
    /// RAI: the locks of the latest checkpoint decided on this node, which
    /// an owner resolves with a fresh child
    pub(crate) fn epoch_locks(&self) -> EpochLocksResponse {
        let Some((epoch, locks)) = self.node.aec.checkpoint_locks() else {
            return EpochLocksResponse {
                epoch: None,
                locks: Vec::new(),
            };
        };
        EpochLocksResponse {
            epoch: Some(epoch.as_u64().into()),
            locks: locks
                .into_iter()
                .map(|(account, height, hash)| EpochLock {
                    account,
                    height: height.into(),
                    hash,
                })
                .collect(),
        }
    }
}
