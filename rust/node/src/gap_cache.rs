use std::{sync::{Mutex, Arc}, collections::BTreeSet};
use indexmap::IndexMap;
use rsnano_core::{BlockHash, Account};
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

    pub fn add(&mut self, hash_a: &BlockHash, time_point_a: i64) {
        let mut lock = self.blocks.lock().unwrap();
        match lock.map.get_mut(hash_a) {
            Some(block) => {
                block.arrival = time_point_a;
            }
            None => {
                let gap_information = GapInformation::new(time_point_a, *hash_a);
                lock.map.insert(*hash_a, gap_information);
                lock.set.insert(*hash_a);
                if lock.map.len() > MAX as usize {
                    let entry = lock.set.first().unwrap().clone();
                    lock.map.remove_entry(&entry);
                }
            }
        }
    }

    pub fn erase(&mut self, hash_a: &BlockHash) {
        let mut lock = self.blocks.lock().unwrap();
        lock.set.remove(hash_a);
        lock.map.remove(hash_a);
    }

    pub fn vote(&mut self, vote_a: &Vote) {
        let mut lock = self.blocks.lock().unwrap();
        if !lock.map.is_empty() {
            let last1 = lock.map.get(lock.set.last().unwrap());
            let last = last1.unwrap().clone();
            for hash in &vote_a.hashes {
                if let Some(gap_info) = lock.map.get_mut(hash) {
                    if last != *gap_info && gap_info.bootstrap_started {
                        let inserted = gap_info.voters.insert(vote_a.voting_account);
                        if inserted {
                            if self.bootstrap_check(&gap_info.voters, hash) {
                                gap_info.bootstrap_started = true;
                            }
                        }
                    }      
                }
            }
        }
    }

    pub fn bootstrap_check(&self, voters_a: &BTreeSet<Account>, hash_a: &BlockHash) -> bool {
        let mut tally = 0u128;
	    for voter in voters_a {
		    tally += self.ledger.weight(voter).number();
	    }
	    let mut start_bootstrap = false;
	    if !self.node_flags.disable_lazy_bootstrap {
		    if tally >= self.online_reps.lock().unwrap().delta().number() {
			    start_bootstrap = true;
		    }
	    }
	    else if !self.node_flags.disable_legacy_bootstrap && tally > self.bootstrap_threshold() as u128 {
		    start_bootstrap = true;
	    }
	    if start_bootstrap && !self.ledger.block_or_pruned_exists(hash_a) {
		    self.bootstrap_start(*hash_a);
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
}
