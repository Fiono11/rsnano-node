use rsnano_ledger::{BootstrapWeights, RepWeights};
use rsnano_types::{Amount, PublicKey};
use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input_file = args.get(1).unwrap();
    let input = std::fs::read(input_file).unwrap();

    let weights = deserialize_bootstrap_weights(&input);
    println!("{}", weights.max_blocks);

    for (key, amount) in weights.weights.iter() {
        println!("{}:{}", key.as_account().encode_account(), amount.number());
    }
}

fn deserialize_bootstrap_weights(mut buffer: &[u8]) -> BootstrapWeights {
    let mut weights = RepWeights::default();
    let mut count_bytes = [0u8; 16];
    buffer.read_exact(&mut count_bytes).unwrap();
    let max_blocks = u128::from_be_bytes(count_bytes) as u64;

    loop {
        let Ok(account) = PublicKey::deserialize(&mut buffer) else {
            break;
        };
        let Ok(weight) = Amount::deserialize(&mut buffer) else {
            break;
        };
        weights.put(account, weight);
    }

    BootstrapWeights {
        max_blocks,
        weights,
    }
}
