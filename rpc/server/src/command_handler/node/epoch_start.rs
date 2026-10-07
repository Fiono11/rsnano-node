use rsnano_ledger::{AnySet, ConfirmedSet};
use rsnano_node::consensus::election::{AccountFrontier, AccountSlot};
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
        // Every account's confirmed chain: a block the setup finalized is
        // final in the genesis state, whatever committee counted it then
        let history = {
            let any = self.node.ledger.any();
            let confirmed = any.confirmed();
            let mut history = Vec::new();
            for (account, _) in any.iter_accounts() {
                let Some(conf) = confirmed.get_conf_info(&account) else {
                    continue;
                };
                let mut hash = conf.frontier;
                while !hash.is_zero() {
                    let Some(block) = confirmed.get_block(&hash) else {
                        break;
                    };
                    history.push((
                        AccountSlot::new(account, block.height()),
                        hash,
                        block.previous(),
                    ));
                    hash = block.previous();
                }
            }
            history
        };
        if !self.node.aec.epochs_started() {
            self.node.aec.set_genesis_committee(frontiers);
            self.node.aec.set_genesis_history(history);
        }
        self.node.aec.start_epochs();
        EpochStartResponse {
            epoch: self.node.aec.current_epoch().as_u64().into(),
        }
    }
}
