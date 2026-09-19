use std::collections::BTreeMap;

use rsnano_ledger::{AnySet, ConfirmedSet};
use rsnano_node::consensus::election::{
    Certificates, ElectionState, EpochState, FinalStateHash, SlotOutcome, slot_outcome,
};
use rsnano_rpc_messages::{
    ConflictingRoot, EpochCloseState, EpochFinalState, FinalStateArgs, FinalStateEntry,
    FinalStateResponse,
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
        let mut epochs: BTreeMap<ConsensusEpoch, EpochState> = self
            .node
            .aec
            .finalized_by_epoch()
            .into_iter()
            .map(|(epoch, hash)| (epoch, EpochState::with_finalized(&hash)))
            .collect();
        // Elections of the epoch that are not settled although their block
        // is cemented; they are still collecting the certificates of the epoch
        let mut cemented_undecided: BTreeMap<ConsensusEpoch, u64> = BTreeMap::new();
        let closes: BTreeMap<_, _> = self
            .node
            .aec
            .epoch_closes()
            .into_iter()
            .map(|close| (close.epoch, close))
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
            let outcome = slot_outcome(election.state, &election.certificates);
            epochs.entry(election.epoch).or_default().add_election(
                &election.account,
                election.height,
                election.state,
                &election.certificates,
            );
            match outcome {
                SlotOutcome::Pending => {
                    if cemented {
                        *cemented_undecided.entry(election.epoch).or_default() += 1;
                    }
                    pending += 1;
                    all_settled = false;
                    all_terminated &= election.state.is_terminated();
                }
                SlotOutcome::Single(block) => {
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
                SlotOutcome::Conflicting => conflicting.push(ConflictingRoot {
                    root: election.root.clone(),
                    epoch: election.epoch.as_u64().into(),
                    blocks: election.candidates.clone(),
                }),
                SlotOutcome::Empty => empty += 1,
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
            current_epoch: self.node.aec.current_epoch().as_u64().into(),
            epochs: epochs
                .into_iter()
                .map(|(epoch, state)| {
                    let cemented_undecided = cemented_undecided.get(&epoch).copied().unwrap_or(0);
                    EpochFinalState {
                        epoch: epoch.as_u64().into(),
                        hash: state.hash.value(),
                        finalized: state.finalized.into(),
                        single_notarized: state.single_notarized.into(),
                        pending: (state.pending - cemented_undecided).into(),
                        cemented_undecided: cemented_undecided.into(),
                        empty: state.empty.into(),
                        conflicting: state.conflicting.into(),
                        close: closes.get(&epoch).map(|close| EpochCloseState {
                            ready: close.ready.into(),
                            value: close.value,
                            started: close.started.into(),
                            round: (close.round as u64).into(),
                            closed_value: close.closed.map(|(_, value)| value),
                            closed_round: close.closed.map(|(round, _)| (round as u64).into()),
                        }),
                    }
                })
                .collect(),
        }
    }
}
