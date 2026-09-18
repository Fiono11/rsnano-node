use rsnano_ledger::{AnySet, ConfirmedSet};
use rsnano_node::consensus::election::{FinalStateHash, SlotOutcome, slot_outcome};
use rsnano_rpc_messages::{ConflictingRoot, FinalStateResponse};

use crate::command_handler::RpcCommandHandler;

impl RpcCommandHandler {
    pub(crate) fn final_state(&self) -> FinalStateResponse {
        let any = self.node.ledger.any();
        let mut hash = FinalStateHash::default();
        let mut accounts = 0;
        for (account, _) in any.iter_accounts() {
            if let Some(conf) = any.confirmed().get_conf_info(&account)
                && conf.height > 0
            {
                hash.add(&account, conf.height, &conf.frontier);
                accounts += 1;
            }
        }

        let mut all_terminated = true;
        let mut all_settled = true;
        let mut single_notarized = 0;
        let mut pending = 0;
        let mut empty = 0;
        let mut conflicting = Vec::new();
        self.node.aec.round_robin(|elections| {
            for election in elections {
                match slot_outcome(election.state(), election.certificates()) {
                    SlotOutcome::Pending => {
                        pending += 1;
                        all_settled = false;
                        all_terminated &= election.state().is_terminated();
                    }
                    SlotOutcome::Single(block) => {
                        let account = election.account();
                        let conf = any.confirmed().get_conf_info(&account).unwrap_or_default();
                        // The election exists because the previous height is cemented;
                        // the notarized block replaces the cemented frontier as the entry
                        if conf.height + 1 != election.height() {
                            continue;
                        }
                        if conf.height > 0 {
                            hash.remove(&account, conf.height, &conf.frontier);
                        }
                        hash.add(&account, election.height(), &block);
                        single_notarized += 1;
                    }
                    SlotOutcome::Conflicting => conflicting.push(ConflictingRoot {
                        root: election.qualified_root().clone(),
                        blocks: election.candidate_blocks().keys().copied().collect(),
                    }),
                    SlotOutcome::Empty => empty += 1,
                }
            }
        });

        FinalStateResponse {
            hash: hash.value(),
            all_terminated: all_terminated.into(),
            all_settled: all_settled.into(),
            accounts: accounts.into(),
            single_notarized: single_notarized.into(),
            pending: pending.into(),
            empty: empty.into(),
            conflicting,
        }
    }
}
