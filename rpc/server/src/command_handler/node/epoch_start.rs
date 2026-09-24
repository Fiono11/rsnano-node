use rsnano_ledger::{AnySet, ConfirmedSet};
use rsnano_node::consensus::election::{AccountFrontier, AccountSlot, EpochLedger, PlacedBlock};
use rsnano_rpc_messages::EpochStartResponse;

use crate::command_handler::RpcCommandHandler;

impl RpcCommandHandler {
    /// RAI: the setup of a run is over, the consensus epochs start now. The
    /// ledger as it stands is the genesis committee: every account at its
    /// frontier, delegating to its representative. It is the same on every
    /// PR once every setup block reached all of them.
    pub(crate) fn epoch_start(&self) -> anyhow::Result<EpochStartResponse> {
        // Genesis is an explicit trust anchor. Require a fully confirmed setup
        // and preserve parent links so strict closure can validate every prefix.
        // Do this once, before the benchmark's measured account workload.
        if !self.node.aec.epochs_started() {
            let any = self.node.ledger.any();
            let confirmed = any.confirmed();
            let mut frontiers = Vec::new();
            let mut history = EpochLedger::new();
            for (account, info) in any.iter_accounts() {
                let conf = confirmed
                    .get_conf_info(&account)
                    .ok_or_else(|| anyhow::anyhow!("genesis account is not confirmed"))?;
                anyhow::ensure!(
                    conf.frontier == info.head && conf.height == info.block_count,
                    "genesis setup must be fully confirmed"
                );
                frontiers.push(AccountFrontier {
                    account,
                    height: info.block_count,
                    hash: info.head,
                    representative: info.representative,
                    balance: info.balance,
                });
                let mut hash = info.head;
                while !hash.is_zero() {
                    let block = confirmed
                        .get_block(&hash)
                        .ok_or_else(|| anyhow::anyhow!("genesis ancestry is unavailable"))?;
                    history.finalize_genesis_block(
                        AccountSlot::new(account, block.height()),
                        PlacedBlock::new(hash, block.previous()),
                    );
                    hash = block.previous();
                }
            }
            self.node.aec.set_genesis_committee(frontiers, history);
            self.node.aec.start_epochs();
        }
        Ok(EpochStartResponse {
            epoch: self.node.aec.current_epoch().as_u64().into(),
        })
    }
}
