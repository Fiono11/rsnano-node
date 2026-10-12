use std::{cell::RefCell, sync::OnceLock};

use rand::{RngExt, SeedableRng, rngs::StdRng};

static SEED: OnceLock<u64> = OnceLock::new();

thread_local! {
    static RNG: RefCell<Option<StdRng>> = const { RefCell::new(None) };
}

/// Seeds every workload generator created from now on (`--seed`)
pub(crate) fn set_seed(seed: u64) {
    let _ = SEED.set(seed);
}

/// Runs `f` with this thread's workload generator: the choices of which
/// account sends, to whom, how much, and whether a block is forked. Seeded
/// with `--seed`, two runs make the same choices as long as the same
/// accounts are ready to send, which depends on confirmation timing.
pub(crate) fn with_rng<T>(f: impl FnOnce(&mut StdRng) -> T) -> T {
    RNG.with(|cell| {
        let mut slot = cell.borrow_mut();
        let rng = slot.get_or_insert_with(|| match SEED.get() {
            Some(seed) => StdRng::seed_from_u64(*seed),
            None => StdRng::seed_from_u64(rand::rng().random()),
        });
        f(rng)
    })
}
