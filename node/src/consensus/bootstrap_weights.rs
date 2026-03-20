use rsnano_ledger::{BootstrapWeights, RepWeightCache, RepWeights};
use rsnano_types::{Account, Amount, NetworkType};
use tracing::info;

pub(crate) fn get_bootstrap_weights(network: NetworkType) -> BootstrapWeights {
    let buffer = get_bootstrap_weights_text(network);
    deserialize_bootstrap_weights(buffer)
}

fn get_bootstrap_weights_text(network: NetworkType) -> &'static str {
    if network == NetworkType::NanoLiveNetwork {
        #[cfg(not(feature = "banano"))]
        {
            include_str!("../../rep_weights/Nano/live.txt")
        }
        #[cfg(feature = "banano")]
        {
            include_str!("../../rep_weights/Banano/live.txt")
        }
    } else {
        #[cfg(not(feature = "banano"))]
        {
            include_str!("../../rep_weights/Nano/beta.txt")
        }
        #[cfg(feature = "banano")]
        {
            include_str!("../../rep_weights/Banano/beta.txt")
        }
    }
}

fn deserialize_bootstrap_weights(buffer: &str) -> BootstrapWeights {
    let mut weights = RepWeights::default();
    let mut first_line = true;
    let mut max_blocks = 0;
    for line in buffer.lines() {
        if first_line {
            max_blocks = line.parse().unwrap();
            first_line = false;
            continue;
        }

        let mut it = line.split(':');
        let account = Account::parse(it.next().unwrap()).unwrap();
        let weight = Amount::decode_dec(it.next().unwrap()).unwrap();
        weights.put(account.into(), weight);
    }

    BootstrapWeights {
        max_blocks,
        weights,
    }
}

pub(crate) fn log_bootstrap_weights(weight_cache: &RepWeightCache) {
    let mut bootstrap_weights = weight_cache.bootstrap_weights();
    if !bootstrap_weights.is_empty() {
        info!(
            "Initial bootstrap height: {}",
            weight_cache.bootstrap_weights_max_blocks()
        );
        info!("Current ledger height:    {}", weight_cache.block_count());

        // Use bootstrap weights if initial bootstrap is not completed
        if weight_cache.use_bootstrap_weights() {
            info!(
                "Using predefined representative weights, since block count is less than bootstrap threshold"
            );
            info!(
                "************************************ Bootstrap weights ************************************"
            );
            // Sort the weights
            let mut sorted_weights = bootstrap_weights.drain().collect::<Vec<_>>();
            sorted_weights.sort_by(|(_, weight_a), (_, weight_b)| weight_b.cmp(weight_a));

            for (rep, weight) in sorted_weights {
                info!(
                    "Using bootstrap rep weight: {} -> {}",
                    Account::from(&rep).encode_account(),
                    weight.format_balance(0)
                );
            }
            info!(
                "************************************ ================= ************************************"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_weights_text() {
        assert_eq!(
            get_bootstrap_weights_text(NetworkType::NanoLiveNetwork).len(),
            14126,
            "expected live weights don't match'"
        );
        assert_eq!(
            get_bootstrap_weights_text(NetworkType::NanoBetaNetwork).len(),
            1161,
            "expected beta weights don't match'"
        );
    }

    #[test]
    fn bootstrap_weights() {
        let result = get_bootstrap_weights(NetworkType::NanoLiveNetwork);
        assert_eq!(result.weights.len(), 137);
        assert_eq!(result.max_blocks, 207_494_994);
    }
}
