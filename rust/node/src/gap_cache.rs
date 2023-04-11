use std::{sync::{Mutex, Arc}, collections::BTreeSet, mem::size_of};
use indexmap::IndexMap;
use rsnano_core::{BlockHash, Account, utils::{ContainerInfoComponent, ContainerInfo}};
use rsnano_ledger::Ledger;
use crate::{voting::Vote, config::{NodeConfig, NodeFlags}, OnlineReps};

const MAX: u32 = 256;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct GapInformation {
    arrival: i64,
    hash: BlockHash,
    voters: BTreeSet<Account>,
	bootstrap_started: bool,
}

impl GapInformation {
    fn new(arrival: i64, hash: BlockHash) -> Self {
        Self {
            arrival, 
            hash,
            voters: BTreeSet::new(),
            bootstrap_started: false,
        }
    }

    fn size() -> usize {
        size_of::<i64>() + size_of::<BlockHash>() + size_of::<Account>() + size_of::<bool>()
    }
}

struct OrderedGaps {
    map: IndexMap<BlockHash, GapInformation>,
    set: BTreeSet<BlockHash>,
}

pub struct GapCache {
    node_config: Arc<NodeConfig>,
    online_reps: Arc<Mutex<OnlineReps>>,
    ledger: Arc<Ledger>,
    node_flags: Arc<NodeFlags>,
    blocks: Mutex<OrderedGaps>,
    start_bootstrap_callback: Box<dyn Fn(BlockHash)>,
}

impl GapCache {
    pub fn new(node_config: Arc<NodeConfig>, online_reps: Arc<Mutex<OnlineReps>>, ledger: Arc<Ledger>, node_flags: Arc<NodeFlags>, start_bootstrap_callback: Box<dyn Fn(BlockHash)>) -> Self {
        Self {
            node_config, 
            online_reps,
            ledger,
            node_flags, 
            blocks: Mutex::new(OrderedGaps { map: IndexMap::new(), set: BTreeSet::new() }),
            start_bootstrap_callback,
        }
    }

    pub fn add(&mut self, hash: &BlockHash, time_point: i64) {
        let mut lock = self.blocks.lock().unwrap();
        if let Some(block) = lock.map.get_mut(hash) {
            block.arrival = time_point;
        } else {
            let gap_information = GapInformation::new(time_point, *hash);
            lock.map.insert(*hash, gap_information);
            lock.set.insert(*hash);
            if lock.map.len() > MAX as usize {
                let entry = lock.set.first().unwrap().clone();
                lock.map.remove_entry(&entry);
            }
        }
    }

    pub fn erase(&mut self, hash_a: &BlockHash) {
        let mut lock = self.blocks.lock().unwrap();
        lock.set.remove(hash_a);
        lock.map.remove(hash_a);
    }

    pub fn vote(&mut self, vote: &Vote) {
        let mut lock = self.blocks.lock().unwrap();
        for hash in &vote.hashes {
            if let Some(gap_info) = lock.map.get_mut(hash) {
                if gap_info.bootstrap_started && gap_info.voters.insert(vote.voting_account) {
                    if self.bootstrap_check(&gap_info.voters, hash) {
                        gap_info.bootstrap_started = true;
                    }
                }
            }
        }
    }

    pub fn bootstrap_check(&self, voters: &BTreeSet<Account>, hash: &BlockHash) -> bool {
        let tally: u128 = voters.iter().map(|voter| self.ledger.weight(voter).number()).sum();

        let start_bootstrap = if !self.node_flags.disable_lazy_bootstrap {
            tally >= self.online_reps.lock().unwrap().delta().number()
        } else if !self.node_flags.disable_legacy_bootstrap {
            tally > self.bootstrap_threshold() as u128
        } else {
            false
        };

        if start_bootstrap && !self.ledger.block_or_pruned_exists(hash) {
            self.bootstrap_start(*hash);
        }

        start_bootstrap
    }

    pub fn bootstrap_start(&self, hash_a: BlockHash) {
	    (self.start_bootstrap_callback)(hash_a);
    }

    pub fn bootstrap_threshold(&self) -> usize {
        ((self.online_reps.lock().unwrap().trended().number() / 256) as usize) * self.node_config.bootstrap_fraction_numerator as usize
    }

    pub fn size(&self) -> usize {
        let lock = self.blocks.lock().unwrap();
        lock.set.len()
    }

    pub fn block_exists(&self, hash: &BlockHash) -> bool {
        let lock = self.blocks.lock().unwrap();
        match lock.map.get(hash) {
            Some(_) => true,
            None => false,
        }
    }

    pub fn earliest(&self) -> i64 {
        let lock = self.blocks.lock().unwrap();
        lock.map.first().unwrap().1.arrival
    }

    pub fn block_arrival(&self, hash: &BlockHash) -> i64 {
        let lock = self.blocks.lock().unwrap();
        lock.map.get(hash).unwrap().arrival
    }

    pub fn size_of_element() -> usize {
        size_of::<BlockHash>() * 2 + GapInformation::size()
    }

    pub fn collect_container_info(&self, name: String) -> ContainerInfoComponent {
        let children = vec![ContainerInfoComponent::Leaf(ContainerInfo {
            name: "gap_cache".to_owned(),
            count: self.size(),
            sizeof_element: Self::size_of_element(),
        })];

        ContainerInfoComponent::Composite(name, children)
    }
}
