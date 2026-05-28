mod backpressure_event_processor;
mod processing_queue;
mod rate_calculator;

use std::{net::Ipv6Addr, sync::LazyLock};

use blake2::{
    Blake2bVar,
    digest::{Update, VariableOutput},
};
use rand::RngExt;

pub(crate) use backpressure_event_processor::{
    BackpressureEventProcessor, spawn_backpressure_processor,
};
pub use processing_queue::*;
pub use rate_calculator::RateCalculator;

pub fn ip_address_hash_raw(address: &Ipv6Addr, port: u16) -> u64 {
    let address_bytes = address.octets();
    let mut hasher = Blake2bVar::new(8).unwrap();
    hasher.update(&RANDOM_128.to_be_bytes());
    if port != 0 {
        hasher.update(&port.to_ne_bytes());
    }
    hasher.update(&address_bytes);
    let mut buffer = [0; 8];
    hasher.finalize_variable(&mut buffer).unwrap();
    u64::from_ne_bytes(buffer)
}

static RANDOM_128: LazyLock<u128> = LazyLock::new(|| {
    let mut rng = rand::rng();
    u128::from_ne_bytes(rng.random::<[u8; 16]>())
});
