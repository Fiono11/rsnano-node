use crate::representatives::{QuorumSnapshot, tracker::registry::RegisteredRep};
use primitive_types::U256;
use rsnano_ledger::RepWeights;
use rsnano_types::Amount;
use std::cmp::max;

pub const ONLINE_WEIGHT_QUORUM: u8 = 67;

pub(crate) fn calculate_quorum<'a>(
    reps: impl IntoIterator<Item = &'a RegisteredRep>,
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
    let minimum_principal_weight = weight / 1000; // 0.1% of online weight

    // Using a larger container to ensure maximum precision
    let delta = U256::from(weight.number()) * U256::from(ONLINE_WEIGHT_QUORUM) / U256::from(100);
    let quorum_delta = Amount::raw(delta.as_u128());

    QuorumSnapshot {
        trended_or_min_weight,
        quorum_delta,
        peered_weight,
        online_weight,
        online_weight_minimum,
        quorum_percent: ONLINE_WEIGHT_QUORUM,
        minimum_principal_weight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_network::ChannelId;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::PublicKey;

    #[test]
    fn empty_registry_falls_back_to_zero() {
        let quorum = calculate_quorum(&[], Amount::ZERO, Amount::ZERO, &RepWeights::default());

        assert_eq!(quorum.trended_or_min_weight, Amount::ZERO);
        assert_eq!(quorum.online_weight, Amount::ZERO);
        assert_eq!(quorum.peered_weight, Amount::ZERO);
        assert_eq!(quorum.quorum_delta, Amount::ZERO);
        assert_eq!(quorum.minimum_principal_weight, Amount::ZERO);
        assert_eq!(quorum.quorum_percent, ONLINE_WEIGHT_QUORUM);
    }

    #[test]
    fn empty_registry_uses_online_weight_minimum_as_floor() {
        let min = Amount::raw(1000);

        let quorum = calculate_quorum(&[], Amount::ZERO, min, &RepWeights::default());

        assert_eq!(quorum.trended_or_min_weight, min);
        assert_eq!(quorum.online_weight_minimum, min);
        assert_eq!(quorum.quorum_delta, Amount::raw(670)); // 67% of 1000
        assert_eq!(quorum.minimum_principal_weight, Amount::raw(1)); // 1000 / 1000
    }

    #[test]
    fn trended_weight_wins_when_higher_than_minimum() {
        let quorum = calculate_quorum(
            &[],
            Amount::raw(5000),
            Amount::raw(1000),
            &RepWeights::default(),
        );

        assert_eq!(quorum.trended_or_min_weight, Amount::raw(5000));
    }

    #[test]
    fn online_weight_minimum_wins_when_higher_than_trended() {
        let quorum = calculate_quorum(
            &[],
            Amount::raw(500),
            Amount::raw(2000),
            &RepWeights::default(),
        );

        assert_eq!(quorum.trended_or_min_weight, Amount::raw(2000));
    }

    #[test]
    fn sums_weight_of_a_single_peered_rep() {
        let mut weights = RepWeights::default();
        weights.put(PublicKey::from(1), Amount::raw(2000));
        let reps = [rep(1, Some(1))];

        let quorum = calculate_quorum(&reps, Amount::ZERO, Amount::ZERO, &weights);

        assert_eq!(quorum.online_weight, Amount::raw(2000));
        assert_eq!(quorum.peered_weight, Amount::raw(2000));
        assert_eq!(quorum.quorum_delta, Amount::raw(1340)); // 67% of 2000
    }

    #[test]
    fn unpeered_reps_count_towards_online_weight_but_not_peered_weight() {
        let mut weights = RepWeights::default();
        weights.put(PublicKey::from(1), Amount::raw(2000));
        let reps = [rep(1, None)];

        let quorum = calculate_quorum(&reps, Amount::ZERO, Amount::ZERO, &weights);

        assert_eq!(quorum.online_weight, Amount::raw(2000));
        assert_eq!(quorum.peered_weight, Amount::ZERO);
    }

    #[test]
    fn sums_weight_across_multiple_reps() {
        let mut weights = RepWeights::default();
        weights.put(PublicKey::from(1), Amount::raw(1000));
        weights.put(PublicKey::from(2), Amount::raw(4000));
        let reps = [rep(1, Some(1)), rep(2, None)];

        let quorum = calculate_quorum(&reps, Amount::ZERO, Amount::ZERO, &weights);

        assert_eq!(quorum.online_weight, Amount::raw(5000));
        assert_eq!(quorum.peered_weight, Amount::raw(1000)); // only rep 1 is peered
    }

    #[test]
    fn a_registered_rep_with_unknown_weight_contributes_zero() {
        let weights = RepWeights::default(); // no weight registered for any key
        let reps = [rep(1, Some(1))];

        let quorum = calculate_quorum(&reps, Amount::ZERO, Amount::ZERO, &weights);

        assert_eq!(quorum.online_weight, Amount::ZERO);
        assert_eq!(quorum.peered_weight, Amount::ZERO);
    }

    #[test]
    fn online_weight_above_the_trended_floor_drives_the_quorum_delta() {
        let mut weights = RepWeights::default();
        weights.put(PublicKey::from(1), Amount::raw(10_000));
        let reps = [rep(1, Some(1))];

        // trended_or_min_weight is 0, so online_weight (10_000) wins the max().
        let quorum = calculate_quorum(&reps, Amount::ZERO, Amount::ZERO, &weights);

        assert_eq!(quorum.online_weight, Amount::raw(10_000));
        assert_eq!(quorum.quorum_delta, Amount::raw(6_700)); // 67% of online_weight
    }

    #[test]
    fn trended_floor_drives_the_quorum_delta_when_online_weight_is_lower() {
        let mut weights = RepWeights::default();
        weights.put(PublicKey::from(1), Amount::raw(500));
        let reps = [rep(1, Some(1))];

        let quorum = calculate_quorum(&reps, Amount::raw(100_000), Amount::ZERO, &weights);

        assert_eq!(quorum.online_weight, Amount::raw(500));
        // online_weight (500) is below trended_or_min_weight (100_000), so the
        // latter is used for the delta calculation instead.
        assert_eq!(quorum.quorum_delta, Amount::raw(67_000)); // 67% of 100_000
    }

    #[test]
    fn minimum_principal_weight_is_a_tenth_of_a_percent_of_the_trended_floor() {
        let quorum = calculate_quorum(
            &[],
            Amount::raw(123_000),
            Amount::ZERO,
            &RepWeights::default(),
        );

        assert_eq!(quorum.minimum_principal_weight, Amount::raw(123));
    }

    #[test]
    fn minimum_principal_weight_scales_with_online_weight_above_the_trended_floor() {
        // minimum_principal_weight is derived from the same `weight` used for
        // quorum_delta (max(online_weight, trended_or_min_weight)), so it rises
        // when online_weight exceeds the trended/minimum floor.
        let mut weights = RepWeights::default();
        weights.put(PublicKey::from(1), Amount::raw(1_000_000));
        let reps = [rep(1, Some(1))];

        let quorum = calculate_quorum(&reps, Amount::ZERO, Amount::ZERO, &weights);

        assert_eq!(quorum.online_weight, Amount::raw(1_000_000));
        assert_eq!(quorum.minimum_principal_weight, Amount::raw(1_000)); // 0.1% of 1_000_000
    }

    #[test]
    fn quorum_delta_rounds_down() {
        let quorum = calculate_quorum(&[], Amount::raw(99), Amount::ZERO, &RepWeights::default());

        // 99 * 67 / 100 = 66.33, which truncates to 66.
        assert_eq!(quorum.quorum_delta, Amount::raw(66));
    }

    #[test]
    fn quorum_delta_does_not_overflow_for_maximum_weight() {
        let quorum = calculate_quorum(&[], Amount::MAX, Amount::ZERO, &RepWeights::default());

        // Must not panic, and must stay within (0, weight).
        assert!(quorum.quorum_delta.number() > 0);
        assert!(quorum.quorum_delta.number() < Amount::MAX.number());
    }

    /* Test helpers */

    fn rep(key: u64, channel: Option<usize>) -> RegisteredRep {
        RegisteredRep {
            public_key: PublicKey::from(key),
            last_seen: Timestamp::new_test_instance(),
            channel_id: channel.map(ChannelId::from),
        }
    }
}
