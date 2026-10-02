use rsnano_node::consensus::election::AccountFrontier;
use rsnano_rpc_messages::EpochStartResponse;

use crate::command_handler::RpcCommandHandler;

impl RpcCommandHandler {
    /// RAI: the setup of a run is over, the consensus epochs start now. The
    /// ledger as it stands is the genesis committee: every account at its
    /// frontier, delegating to its representative. It is the same on every
    /// PR once every setup block reached all of them.
    pub(crate) fn epoch_start(&self) -> EpochStartResponse {
        let frontiers: Vec<AccountFrontier> = self
            .node
            .ledger
            .any()
            .iter_accounts()
            .map(|(account, info)| AccountFrontier {
                account,
                height: info.block_count,
                hash: info.head,
                representative: info.representative,
                balance: info.balance,
            })
            .collect();
        self.node.aec.set_genesis_committee(frontiers);
        self.node.aec.start_epochs();
        EpochStartResponse {
            epoch: self.node.aec.current_epoch().as_u64().into(),
        }
    }
}
