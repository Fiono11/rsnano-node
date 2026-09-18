use std::collections::BTreeMap;

use rsnano_ledger::{AnySet, ConfirmedSet};
use rsnano_node::consensus::election::{
    Certificates, ElectionState, FinalStateHash, SlotOutcome, slot_outcome,
};
use rsnano_rpc_messages::{
    ConflictingRoot, EpochFinalState, FinalStateArgs, FinalStateEntry, FinalStateResponse,
};
use rsnano_types::{Account, BlockHash, ConsensusEpoch, QualifiedRoot};

use crate::command_handler::RpcCommandHandler;

/// What the final state needs of an election, copied out of the AEC
struct ElectionView {
    account: Account,
    height: u64,
    epoch: ConsensusEpoch,
    root: QualifiedRoot,
    state: ElectionState,
    certificates: Certificates,
    candidates: Vec<BlockHash>,
}

/// RAI: the outcome of one epoch's elections on this node, see `EpochFinalState`
#[derive(Default)]
struct EpochOutcome {
    hash: FinalStateHash,
    finalized: u64,
    single_notarized: u64,
    pending: u64,
    cemented_undecided: u64,
    empty: u64,
    conflicting: u64,
}

impl RpcCommandHandler {
    pub(crate) fn final_state(&self, args: FinalStateArgs) -> FinalStateResponse {
        let listed = args.epoch.map(|e| ConsensusEpoch::new(e.inner()));
        let mut entries: Option<Vec<FinalStateEntry>> = listed.map(|epoch| {
            self.node
                .aec
                .finalized_in(epoch)
                .into_iter()
                .map(|(account, height, hash)| FinalStateEntry {
                    account,
                    height: height.into(),
                    hash,
                    kind: "finalized".to_string(),
                })
                .collect()
        });
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
        let mut epochs: BTreeMap<ConsensusEpoch, EpochOutcome> = self
            .node
            .aec
            .finalized_by_epoch()
            .into_iter()
            .map(|(epoch, hash)| {
                let finalized = hash.entries();
                (
                    epoch,
                    EpochOutcome {
                        hash,
                        finalized,
                        ..Default::default()
                    },
                )
            })
            .collect();
        // The AEC lock is held while the elections are copied out and released
        // before anything else of the node is asked: a second read of the lock
        // from inside would deadlock against a waiting writer
        let elections: Vec<ElectionView> = self.node.aec.round_robin(|elections| {
            elections
                .map(|election| ElectionView {
                    account: election.account(),
                    height: election.height(),
                    epoch: election.epoch(),
                    root: election.qualified_root().clone(),
                    state: election.state(),
                    certificates: election.certificates().clone(),
                    candidates: election.candidate_blocks().keys().copied().collect(),
                })
                .collect()
        });
        for election in &elections {
            let conf = any
                .confirmed()
                .get_conf_info(&election.account)
                .unwrap_or_default();
            let cemented = conf.height >= election.height;
            let outcome = epochs.entry(election.epoch).or_default();
            match slot_outcome(election.state, &election.certificates) {
                SlotOutcome::Pending => {
                    if cemented {
                        outcome.cemented_undecided += 1;
                    } else {
                        outcome.pending += 1;
                    }
                    pending += 1;
                    all_settled = false;
                    all_terminated &= election.state.is_terminated();
                }
                SlotOutcome::Single(block) => {
                    outcome.hash.add(&election.account, election.height, &block);
                    outcome.single_notarized += 1;
                    if listed == Some(election.epoch)
                        && let Some(entries) = entries.as_mut()
                    {
                        entries.push(FinalStateEntry {
                            account: election.account,
                            height: election.height.into(),
                            hash: block,
                            kind: "single".to_string(),
                        });
                    }
                    // The election exists because the previous height is cemented;
                    // the notarized block replaces the cemented frontier as the entry
                    if conf.height + 1 != election.height {
                        continue;
                    }
                    if conf.height > 0 {
                        hash.remove(&election.account, conf.height, &conf.frontier);
                    }
                    hash.add(&election.account, election.height, &block);
                    single_notarized += 1;
                }
                SlotOutcome::Conflicting => {
                    outcome.conflicting += 1;
                    conflicting.push(ConflictingRoot {
                        root: election.root.clone(),
                        epoch: election.epoch.as_u64().into(),
                        blocks: election.candidates.clone(),
                    })
                }
                SlotOutcome::Empty => {
                    outcome.empty += 1;
                    empty += 1;
                }
            }
        }

        FinalStateResponse {
            hash: hash.value(),
            all_terminated: all_terminated.into(),
            all_settled: all_settled.into(),
            accounts: accounts.into(),
            single_notarized: single_notarized.into(),
            pending: pending.into(),
            empty: empty.into(),
            conflicting,
            entries,
            epochs: epochs
                .into_iter()
                .map(|(epoch, o)| EpochFinalState {
                    epoch: epoch.as_u64().into(),
                    hash: o.hash.value(),
                    finalized: o.finalized.into(),
                    single_notarized: o.single_notarized.into(),
                    pending: o.pending.into(),
                    cemented_undecided: o.cemented_undecided.into(),
                    empty: o.empty.into(),
                    conflicting: o.conflicting.into(),
                })
                .collect(),
        }
    }
}
