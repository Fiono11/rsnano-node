use crate::representatives::{QuorumSnapshot, tracker::registry::RegisteredRep};
use primitive_types::U256;
use rsnano_ledger::RepWeights;
use rsnano_types::Amount;
use std::cmp::max;

pub const ONLINE_WEIGHT_QUORUM: u8 = 67;

pub(crate) fn calculate_quorum(
    reps: &[RegisteredRep],
    trended_weight: Amount,
    online_weight_minimum: Amount,
    weights: &RepWeights,
) -> QuorumSnapshot {
    let trended_or_min_weight = max(trended_weight, online_weight_minimum);
    let mut online_weight = Amount::ZERO;
    let mut peered_weight = Amount::ZERO;

    for rep in reps {
        let weight = weights.weight(&rep.public_key);
        online_weight += weight;
        if rep.channel_id.is_some() {
            peered_weight += weight;
        }
    }

    let weight = max(online_weight, trended_or_min_weight);
    let minimum_principal_weight = trended_or_min_weight / 1000; // 0.1% of trended online weight

    // Using a larger container to ensure maximum precision
    let delta = U256::from(weight.number()) * U256::from(ONLINE_WEIGHT_QUORUM) / U256::from(100);
    let quorum_delta = Amount::raw(delta.as_u128());

    QuorumSnapshot {
        trended_or_min_weight,
        quorum_delta,
        peered_weight,
        online_weight,
        online_weight_minimum: online_weight_minimum,
        quorum_percent: ONLINE_WEIGHT_QUORUM,
        minimum_principal_weight,
    }
}
