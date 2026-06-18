use std::{
    mem::size_of,
    ops::Deref,
    sync::{
        Arc, RwLock, RwLockReadGuard,
        atomic::{AtomicBool, Ordering},
    },
};

use rustc_hash::FxHashMap;

use rsnano_store_lmdb::LedgerCache;
use rsnano_types::{Account, Amount, PublicKey};
use rsnano_utils::container_info::ContainerInfo;

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct RepWeights {
    entries: FxHashMap<PublicKey, Amount>,

    /// Representatives with a weight below this min_weight are discarded
    min_weight: Amount,
}

impl RepWeights {
    pub fn new(min_weight: Amount) -> Self {
        Self {
            entries: FxHashMap::default(),
            min_weight,
        }
    }

    pub fn weight(&self, rep: &PublicKey) -> Amount {
        self.get(rep).cloned().unwrap_or_default()
    }

    pub fn put(&mut self, rep: PublicKey, new_weight: Amount) {
        if new_weight < self.min_weight || new_weight.is_zero() {
            self.entries.remove(&rep);
        } else {
            self.entries.insert(rep, new_weight);
        }
    }
}

impl Deref for RepWeights {
    type Target = FxHashMap<PublicKey, Amount>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

#[derive(Default)]
pub struct BootstrapWeights {
    pub weights: RepWeights,
    pub max_blocks: u64,
}

/// Returns the cached vote weight for the given representative.
/// If the weight is below the cache limit it returns 0.
/// During bootstrap it returns the preconfigured bootstrap weights.
pub struct RepWeightCache {
    weights: Arc<RwLock<RepWeights>>,
    bootstrap_weights: RwLock<RepWeights>,
    max_blocks: u64,
    pub ledger_cache: Arc<LedgerCache>,
    check_bootstrap_weights: AtomicBool,
}

impl RepWeightCache {
    pub fn new(min_weight: Amount) -> Self {
        Self {
            weights: Arc::new(RwLock::new(RepWeights::new(min_weight))),
            bootstrap_weights: RwLock::new(RepWeights::default()),
            max_blocks: 0,
            ledger_cache: Arc::new(LedgerCache::new()),
            check_bootstrap_weights: AtomicBool::new(false),
        }
    }

    pub fn with_bootstrap_weights(
        bootstrap_weights: BootstrapWeights,
        ledger_cache: Arc<LedgerCache>,
        min_weight: Amount,
    ) -> Self {
        Self {
            weights: Arc::new(RwLock::new(RepWeights::new(min_weight))),
            bootstrap_weights: RwLock::new(bootstrap_weights.weights),
            max_blocks: bootstrap_weights.max_blocks,
            ledger_cache,
            check_bootstrap_weights: AtomicBool::new(true),
        }
    }

    pub fn read(&self) -> RwLockReadGuard<'_, RepWeights> {
        if self.use_bootstrap_weights() {
            self.bootstrap_weights.read().unwrap()
        } else {
            self.weights.read().unwrap()
        }
    }

    pub fn use_bootstrap_weights(&self) -> bool {
        if self.check_bootstrap_weights.load(Ordering::SeqCst) {
            if self.ledger_cache.block_count.load(Ordering::SeqCst) < self.max_blocks {
                return true;
            } else {
                self.check_bootstrap_weights.store(false, Ordering::SeqCst);
            }
        }
        false
    }

    pub fn weight(&self, rep: &PublicKey) -> Amount {
        let weights = if self.use_bootstrap_weights() {
            &self.bootstrap_weights
        } else {
            &self.weights
        };

        weights
            .read()
            .unwrap()
            .get(rep)
            .cloned()
            .unwrap_or_default()
    }

    pub fn bootstrap_weights_max_blocks(&self) -> u64 {
        self.max_blocks
    }

    pub fn bootstrap_weights(&self) -> FxHashMap<PublicKey, Amount> {
        self.bootstrap_weights
            .read()
            .unwrap()
            .deref()
            .deref()
            .clone()
    }

    pub fn block_count(&self) -> u64 {
        self.ledger_cache.block_count.load(Ordering::SeqCst)
    }

    pub fn len(&self) -> usize {
        self.weights.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn put(&self, account: PublicKey, weight: Amount) {
        self.weights.write().unwrap().put(account, weight);
    }

    pub(super) fn inner(&self) -> Arc<RwLock<RepWeights>> {
        self.weights.clone()
    }

    pub fn container_info(&self) -> ContainerInfo {
        [("rep_weights", self.len(), size_of::<(Account, Amount)>())].into()
    }
}

impl From<RepWeights> for RepWeightCache {
    fn from(value: RepWeights) -> Self {
        Self {
            weights: Arc::new(RwLock::new(value)),
            bootstrap_weights: RwLock::new(RepWeights::default()),
            max_blocks: 0,
            ledger_cache: Arc::new(LedgerCache::new()),
            check_bootstrap_weights: AtomicBool::new(false),
        }
    }
}

impl Default for RepWeightCache {
    fn default() -> Self {
        Self::new(Amount::ZERO)
    }
}
