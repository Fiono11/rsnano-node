use crate::consensus::election::{Election, EpochLedger};
use rsnano_types::ConsensusEpoch;
use std::{collections::BTreeMap, sync::Arc};
pub(super) type DecidedStates = BTreeMap<ConsensusEpoch, Arc<EpochLedger>>;
pub(super) fn is_late(decided: &DecidedStates, election: &Election) -> bool {
    decided.get(&election.epoch()).is_some_and(|state| {
        let slot =
            crate::consensus::election::AccountSlot::new(election.account(), election.height());
        election.certificates().notar.iter().any(|block| {
            !state.is_finalized(&slot, block) && !state.notarized(&slot).contains(block)
        })
    })
}
