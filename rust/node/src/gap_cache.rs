use crate::{
    config::{NodeConfig, NodeFlags},
    voting::Vote,
    OnlineReps,
};
use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoComponent},
    Account, Amount, BlockHash,
};
use rsnano_ledger::Ledger;
use std::{
    collections::{BTreeMap, HashMap},
    mem::size_of,
    sync::{Arc, Mutex},
};

const MAX: usize = 256;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
struct GapInformation {
    arrival: i64,
    hash: BlockHash,
    voters: Vec<Account>, // todo: Should this be a HashSet?
    bootstrap_started: bool,
}

impl GapInformation {
    fn new(arrival: i64, hash: BlockHash) -> Self {
        Self {
            arrival,
            hash,
            voters: Vec::new(),
            bootstrap_started: false,
        }
    }

    fn size() -> usize {
        size_of::<i64>() + size_of::<BlockHash>() + size_of::<Account>() + size_of::<bool>()
    }

    #[cfg(test)]
    fn create_test_instance() -> Self {
        Self {
            arrival: 123,
            hash: BlockHash::from(42),
            voters: Vec::new(),
            bootstrap_started: false,
        }
    }
}

struct OrderedGaps {
    gap_infos: HashMap<BlockHash, GapInformation>,
    by_arrival: BTreeMap<i64, BlockHash>,
}

impl OrderedGaps {
    fn new() -> Self {
        Self {
            gap_infos: HashMap::new(),
            by_arrival: BTreeMap::new(),
        }
    }

    fn len(&self) -> usize {
        self.gap_infos.len()
    }

    fn add(&mut self, gap_info: GapInformation) {
        self.by_arrival.insert(gap_info.arrival, gap_info.hash);
        self.gap_infos.insert(gap_info.hash, gap_info);
    }

    fn get(&self, hash: &BlockHash) -> Option<&GapInformation> {
        self.gap_infos.get(hash)
    }

    fn get_mut(&mut self, hash: &BlockHash) -> Option<&mut GapInformation> {
        self.gap_infos.get_mut(hash)
    }

    fn remove(&mut self, hash: &BlockHash) -> Option<BlockHash> {
        if let Some(gap_info) = self.gap_infos.remove(hash) {
            self.by_arrival.remove(&gap_info.arrival)
        }
        else {
            None
        }
    }

    fn trim(&mut self, max: usize) {
        while self.by_arrival.len() > max {
            let (_, hash) = self.by_arrival.pop_first().unwrap();
            self.gap_infos.remove(&hash);
        }
    }

    fn earliest(&self) -> Option<i64>{
        self.by_arrival.first_key_value().map(|(&arrival,_)| arrival)        
    }

    pub fn size_of_element() -> usize {
        size_of::<BlockHash>() * 2 + GapInformation::size() + size_of::<i64>()
    }
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
    pub fn new(
        node_config: Arc<NodeConfig>,
        online_reps: Arc<Mutex<OnlineReps>>,
        ledger: Arc<Ledger>,
        node_flags: Arc<NodeFlags>,
        start_bootstrap_callback: Box<dyn Fn(BlockHash)>,
    ) -> Self {
        Self {
            node_config,
            online_reps,
            ledger,
            node_flags,
            blocks: Mutex::new(OrderedGaps::new()),
            start_bootstrap_callback,
        }
    }

    pub fn add(&mut self, hash: &BlockHash, time_point: i64) {
        let mut lock = self.blocks.lock().unwrap();
        if let Some(block) = lock.gap_infos.get_mut(hash) {
            block.arrival = time_point;
        } else {
            let gap_information = GapInformation::new(time_point, *hash);
            lock.gap_infos.insert(*hash, gap_information);
            lock.by_arrival.insert(time_point, *hash);
            lock.trim(MAX);
        }
    }

    pub fn erase(&mut self, hash: &BlockHash) {
        let mut lock = self.blocks.lock().unwrap();
        lock.remove(hash);
    }

    pub fn vote(&mut self, vote: &Vote) {
        let mut lock = self.blocks.lock().unwrap();
        for hash in &vote.hashes {
            if let Some(gap_info) = lock.gap_infos.get_mut(hash) {
                if !gap_info.bootstrap_started {
                    let is_new = !gap_info.voters.iter().any(|v| *v == vote.voting_account);
                    if is_new {
                        gap_info.voters.push(vote.voting_account);

                        if self.bootstrap_check(&gap_info.voters, hash) {
                            gap_info.bootstrap_started = true;
                        }
                    }
                }
            }
        }
    }

    pub fn bootstrap_check(&self, voters: &Vec<Account>, hash: &BlockHash) -> bool {
        let tally = Amount::raw(
            voters
                .iter()
                .map(|voter| self.ledger.weight(voter).number())
                .sum(),
        );

        let start_bootstrap = if !self.node_flags.disable_lazy_bootstrap {
            tally >= self.online_reps.lock().unwrap().delta()
        } else if !self.node_flags.disable_legacy_bootstrap {
            tally > self.bootstrap_threshold()
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

    pub fn bootstrap_threshold(&self) -> Amount {
        Amount::raw(
            (self.online_reps.lock().unwrap().trended().number() / 256)
                * self.node_config.bootstrap_fraction_numerator as u128,
        )
    }

    pub fn size(&self) -> usize {
        let lock = self.blocks.lock().unwrap();
        lock.gap_infos.len()
    }

    pub fn block_exists(&self, hash: &BlockHash) -> bool {
        let lock = self.blocks.lock().unwrap();
        match lock.gap_infos.get(hash) {
            Some(_) => true,
            None => false,
        }
    }

    pub fn earliest(&self) -> i64 {
        let lock = self.blocks.lock().unwrap();
        let (_, hash) = lock.by_arrival.first_key_value().unwrap();
        lock.gap_infos.get(hash).unwrap().arrival
    }

    pub fn block_arrival(&self, hash: &BlockHash) -> i64 {
        let lock = self.blocks.lock().unwrap();
        lock.gap_infos.get(hash).unwrap().arrival
    }

    pub fn collect_container_info(&self, name: String) -> ContainerInfoComponent {
        let children = vec![ContainerInfoComponent::Leaf(ContainerInfo {
            name: "blocks".to_owned(),
            count: self.size(),
            sizeof_element: OrderedGaps::size_of_element(),
        })];

        ContainerInfoComponent::Composite(name, children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ordered_gaps_is_empty() {
        let gaps = OrderedGaps::new();
        assert_eq!(gaps.len(), 0);
        assert_eq!(gaps.earliest(), None);
    }

    #[test]
    fn add_gap_information_to_ordered_gaps() {
        let mut gaps = OrderedGaps::new();
        gaps.add(GapInformation::create_test_instance());
        assert_eq!(gaps.len(), 1);
    }

    #[test]
    fn remove_existing_gap_information_of_ordered_gaps() {
        let mut gaps = OrderedGaps::new();
        let gap_info = GapInformation::create_test_instance();
        let hash = gap_info.hash;
        gaps.add(gap_info);
        assert_eq!(gaps.len(), 1);
        assert!(gaps.remove(&hash).is_some());
        assert_eq!(gaps.len(), 0);
    }

    #[test]
    fn try_to_remove_non_existing_gap_information_of_ordered_gaps() {
        let mut gaps = OrderedGaps::new();
        let gap_info = GapInformation::create_test_instance();
        let hash = BlockHash::from(10);
        assert!(hash != gap_info.hash);
        gaps.add(gap_info);
        assert_eq!(gaps.len(), 1);
        assert!(gaps.remove(&hash).is_none());
        assert_eq!(gaps.len(), 1);
    }

    #[test]
    fn get_gap_info_by_block_hash() {
        let mut gaps = OrderedGaps::new();
        let gap_info = GapInformation::create_test_instance();

        gaps.add(gap_info.clone());

        let result = gaps.get(&gap_info.hash).unwrap();
        assert_eq!(result, &gap_info);
    }

    #[test]
    fn add_same_gap_information_to_ordered_gaps_twice_replaces_the_first_insert() {
        let mut gaps = OrderedGaps::new();
        let gap_info = GapInformation::create_test_instance();
        gaps.add(gap_info.clone());
        gaps.add(gap_info);
        assert_eq!(gaps.len(), 1);
    }

    #[test]
    fn trim_removes_oldest_entries() {
        let mut gaps = OrderedGaps::new();
        
        // will be removed by trim
        gaps.add(GapInformation{
            hash: BlockHash::from(1),
            arrival: 100,
            ..GapInformation::create_test_instance()});

        // will be kept
        gaps.add(GapInformation{
            hash: BlockHash::from(3),
            arrival: 101,
            ..GapInformation::create_test_instance()});

        // will be kept
        gaps.add(GapInformation{
            hash: BlockHash::from(4),
            arrival: 102,
            ..GapInformation::create_test_instance()});

        // will be removed by trim
        gaps.add(GapInformation{
            hash: BlockHash::from(2),
            arrival: 99,
            ..GapInformation::create_test_instance()});
    
        gaps.trim(2);
        
        assert_eq!(gaps.len(), 2);
        assert!(gaps.get(&BlockHash::from(3)).is_some());
        assert!(gaps.get(&BlockHash::from(4)).is_some());
        assert_eq!(gaps.earliest(), Some(101));
    }

    #[test]
    fn can_modify_gap_information() {
        let mut gaps = OrderedGaps::new();
        let hash = BlockHash::from(4);
        gaps.add(GapInformation{
            hash,
            bootstrap_started: false,
            ..GapInformation::create_test_instance()});

        gaps.get_mut(&hash).unwrap().bootstrap_started = true;

        assert_eq!(gaps.get(&hash).unwrap().bootstrap_started, true);
    }

}
