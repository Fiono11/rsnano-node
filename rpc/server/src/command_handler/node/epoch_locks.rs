use rsnano_rpc_messages::{EpochLock, EpochLocksResponse};

use crate::command_handler::RpcCommandHandler;

impl RpcCommandHandler {
    /// RAI: the locks of the latest checkpoint decided on this node, which
    /// an owner resolves with a fresh child
    pub(crate) fn epoch_locks(&self) -> EpochLocksResponse {
        let Some((epoch, state)) = self.node.aec.checkpoint_snapshot() else {
            return EpochLocksResponse {
                epoch: None,
                state_hash: None,
                finalized_certificate: None,
                finalized_derived: None,
                locks: Vec::new(),
            };
        };
        EpochLocksResponse {
            epoch: Some(epoch.as_u64().into()),
            state_hash: Some(state.state_hash()),
            finalized_certificate: Some(
                ((state.finalized_count() - state.derived_count()) as u64).into(),
            ),
            finalized_derived: Some((state.derived_count() as u64).into()),
            locks: state
                .locks()
                .map(|(slot, hash)| EpochLock {
                    account: slot.account,
                    height: slot.height.into(),
                    hash,
                })
                .collect(),
        }
    }
}
