use super::{
    Election, ElectionBehavior, ElectionData, ElectionState, ElectionStatus, RecentlyConfirmedCache,
};
use crate::{
    cementation::ConfirmingSet, config::NodeConfig, utils::ThreadPool, wallets::Wallets,
    NetworkParams, OnlineReps,
};
use rsnano_core::{Amount, BlockEnum, BlockHash, QualifiedRoot};
use rsnano_ledger::Ledger;
use std::{
    cmp::max,
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};
use tracing::trace;

pub struct ActiveTransactions {
    pub mutex: Mutex<ActiveTransactionsData>,
    pub condition: Condvar,
    network: NetworkParams,
    pub online_reps: Arc<Mutex<OnlineReps>>,
    wallets: Arc<Wallets>,
    pub election_winner_details: Mutex<HashMap<BlockHash, Arc<Election>>>,
    config: NodeConfig,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    workers: Arc<dyn ThreadPool>,
    pub recently_confirmed: Arc<RecentlyConfirmedCache>,
}

impl ActiveTransactions {
    pub fn new(
        network: NetworkParams,
        online_reps: Arc<Mutex<OnlineReps>>,
        wallets: Arc<Wallets>,
        config: NodeConfig,
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        workers: Arc<dyn ThreadPool>,
    ) -> Self {
        Self {
            mutex: Mutex::new(ActiveTransactionsData {
                roots: OrderedRoots::default(),
                stopped: false,
                normal_count: 0,
                hinted_count: 0,
                optimistic_count: 0,
                blocks: HashMap::new(),
            }),
            condition: Condvar::new(),
            network,
            online_reps,
            wallets,
            election_winner_details: Mutex::new(HashMap::new()),
            config,
            ledger,
            confirming_set,
            workers,
            recently_confirmed: Arc::new(RecentlyConfirmedCache::new(65536)),
        }
    }

    pub fn erase_block(&self, block: &BlockEnum) {
        self.erase_root(&block.qualified_root());
    }

    pub fn erase_root(&self, _root: &QualifiedRoot) {
        todo!()
    }

    pub fn request_loop<'a>(
        &self,
        stamp: Instant,
        guard: MutexGuard<'a, ActiveTransactionsData>,
    ) -> MutexGuard<'a, ActiveTransactionsData> {
        if !guard.stopped {
            let loop_interval =
                Duration::from_millis(self.network.network.aec_loop_interval_ms as u64);
            let min_sleep = loop_interval / 2;

            let wait_duration = max(
                min_sleep,
                (stamp + loop_interval).saturating_duration_since(Instant::now()),
            );

            self.condition
                .wait_timeout_while(guard, wait_duration, |data| !data.stopped)
                .unwrap()
                .0
        } else {
            guard
        }
    }

    pub fn cooldown_time(&self, weight: Amount) -> Duration {
        let online_stake = { self.online_reps.lock().unwrap().trended() };
        if weight > online_stake / 20 {
            // Reps with more than 5% weight
            Duration::from_secs(1)
        } else if weight > online_stake / 100 {
            // Reps with more than 1% weight
            Duration::from_secs(5)
        } else {
            // The rest of smaller reps
            Duration::from_secs(15)
        }
    }

    pub fn remove_election_winner_details(&self, hash: &BlockHash) -> Option<Arc<Election>> {
        let mut guard = self.election_winner_details.lock().unwrap();
        guard.remove(hash)
    }
}

pub struct ActiveTransactionsData {
    pub roots: OrderedRoots,
    pub stopped: bool,
    pub normal_count: u64,
    pub hinted_count: u64,
    pub optimistic_count: u64,
    pub blocks: HashMap<BlockHash, Arc<Election>>,
}

impl ActiveTransactionsData {
    pub fn count_by_behavior(&self, behavior: ElectionBehavior) -> u64 {
        match behavior {
            ElectionBehavior::Normal => self.normal_count,
            ElectionBehavior::Hinted => self.hinted_count,
            ElectionBehavior::Optimistic => self.optimistic_count,
        }
    }

    pub fn count_by_behavior_mut(&mut self, behavior: ElectionBehavior) -> &mut u64 {
        match behavior {
            ElectionBehavior::Normal => &mut self.normal_count,
            ElectionBehavior::Hinted => &mut self.hinted_count,
            ElectionBehavior::Optimistic => &mut self.optimistic_count,
        }
    }
}

#[derive(Default)]
pub struct OrderedRoots {
    by_root: HashMap<QualifiedRoot, Arc<Election>>,
    sequenced: Vec<QualifiedRoot>,
}

impl OrderedRoots {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn insert(&mut self, root: QualifiedRoot, election: Arc<Election>) {
        if self.by_root.insert(root.clone(), election).is_none() {
            self.sequenced.push(root);
        }
    }

    pub fn get(&self, root: &QualifiedRoot) -> Option<&Arc<Election>> {
        self.by_root.get(root)
    }

    pub fn erase(&mut self, root: &QualifiedRoot) {
        if let Some(_) = self.by_root.remove(root) {
            self.sequenced.retain(|x| x != root)
        }
    }

    pub fn clear(&mut self) {
        self.sequenced.clear();
        self.by_root.clear();
    }

    pub fn len(&self) -> usize {
        self.sequenced.len()
    }

    pub fn iter_sequenced(&self) -> impl Iterator<Item = (&QualifiedRoot, &Arc<Election>)> {
        self.sequenced
            .iter()
            .map(|r| (r, self.by_root.get(r).unwrap()))
    }
}

pub trait ActiveTransactionsExt {
    fn confirm_once(&self, election_lock: MutexGuard<ElectionData>, election: &Arc<Election>);
    fn process_confirmed(&self, status: &ElectionStatus, iteration: u64);
}

impl ActiveTransactionsExt for Arc<ActiveTransactions> {
    fn confirm_once(&self, mut election_lock: MutexGuard<ElectionData>, election: &Arc<Election>) {
        // This must be kept above the setting of election state, as dependent confirmed elections require up to date changes to election_winner_details
        let mut winners_guard = self.election_winner_details.lock().unwrap();
        let mut status = election_lock.status.clone();
        let old_state = election_lock.state;
        let just_confirmed = old_state != ElectionState::Confirmed;
        election_lock.state = ElectionState::Confirmed;
        if just_confirmed && !winners_guard.contains_key(&status.winner.as_ref().unwrap().hash()) {
            winners_guard.insert(status.winner.as_ref().unwrap().hash(), Arc::clone(election));
            drop(winners_guard);

            election_lock.update_status_to_confirmed(&election);
            status = election_lock.status.clone();
            todo!();

            //    recently_confirmed.put (election.qualified_root (), status_l.get_winner ()->hash ());

            //    node.logger->trace (nano::log::type::election, nano::log::detail::election_confirmed,
            //    nano::log::arg{ "qualified_root", election.qualified_root () });

            //    lock_a.unlock ();

            //    node.background ([node_l = node.shared (), status_l, election_l = election.shared_from_this ()] () {
            //        node_l->active.process_confirmed (status_l);

            //        rsnano::rsn_election_confirmation_action (election_l->handle, status_l.get_winner ()->get_handle ());
            //    });
        }
    }

    fn process_confirmed(&self, status: &ElectionStatus, mut iteration: u64) {
        let hash = status.winner.as_ref().unwrap().hash();
        let num_iters = (self.config.block_processor_batch_max_time_ms
            / self.network.node.process_confirmed_interval_ms) as u64
            * 4;
        //std::shared_ptr<nano::block> block_l;
        let block = {
            let tx = self.ledger.read_txn();
            self.ledger.get_block(&tx, &hash)
        };
        if let Some(block) = block {
            trace!(block = ?block,"process confirmed");
            self.confirming_set.add(block.hash());
        } else if iteration < num_iters {
            iteration += 1;
            let self_w = Arc::downgrade(self);
            todo!()
            //self.workers.add_delayed_task(
            //    Duration::from_millis(self.network.node.process_confirmed_interval_ms as u64),
            //    Box::new(move || {
            //        if let Some(self_l) = self_w.upgrade() {
            //            self_l.process_confirmed(status, iteration);
            //        }
            //    }),
            //);
        } else {
            // Do some cleanup due to this block never being processed by confirmation height processor
            self.remove_election_winner_details(&hash);
        }
    }
}
