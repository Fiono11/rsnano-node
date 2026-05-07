use std::{
    cmp::max,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rsnano_ledger::{AnySet, ConfirmedSet, Ledger};
use rsnano_network::token_bucket::TokenBucket;
use rsnano_types::{Account, AccountInfo, ConfirmationHeightInfo};
use rsnano_utils::stats::{StatsCollection, StatsSource};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BacklogScanConfig {
    /// Control if ongoing backlog population is enabled. If not, backlog population can still be triggered by RPC
    pub enabled: bool,

    /// Number of accounts per second to process.
    pub batch_size: usize,

    /// Number of accounts to scan per second
    pub rate_limit: usize,
}

impl Default for BacklogScanConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            batch_size: 1000,
            rate_limit: 10_000,
        }
    }
}

impl BacklogScanConfig {
    fn wait_time(&self) -> Duration {
        let wait_time =
            Duration::from_millis(1000 / max(self.rate_limit / self.batch_size, 1) as u64 / 2);
        max(wait_time, Duration::from_millis(10))
    }
}

/// Continuously scan the ledger for unconfirmed blocks and activate them
pub struct BacklogScan {
    scan_loop: Option<BacklogScanLoop>,
    stats: Arc<BacklogScanStats>,
    flags: Arc<Mutex<BacklogScanFlags>>,
    condition: Arc<Condvar>,
    publish: Arc<RwLock<Option<Box<dyn Fn(Vec<UnconfirmedInfo>) + Send + Sync>>>>,
    /** Thread that runs the backlog implementation logic. The thread always runs, even if
     *  backlog population is disabled, so that it can service a manual trigger (e.g. via RPC). */
    thread: Option<JoinHandle<()>>,
}

impl BacklogScan {
    pub fn new(
        config: BacklogScanConfig,
        ledger: Arc<Ledger>,
        publish: impl Fn(Vec<UnconfirmedInfo>) + Send + Sync + 'static,
    ) -> Self {
        let stats = Arc::new(BacklogScanStats::default());

        let flags = Arc::new(Mutex::new(BacklogScanFlags {
            stopped: false,
            triggered: false,
        }));

        let condition = Arc::new(Condvar::new());
        let publish: Box<dyn Fn(Vec<UnconfirmedInfo>) + Send + Sync> = Box::new(publish);
        let publish = Arc::new(RwLock::new(Some(publish)));
        Self {
            scan_loop: Some(BacklogScanLoop {
                ledger,
                stats: stats.clone(),
                publish: publish.clone(),
                limiter: Mutex::new(TokenBucket::new(config.rate_limit)),
                config,
                flags: flags.clone(),
                condition: condition.clone(),
            }),
            stats,
            flags,
            condition,
            publish,
            thread: None,
        }
    }

    pub fn start(&mut self) {
        let scan_loop = self
            .scan_loop
            .take()
            .expect("Tried to start backlog scan twice");

        self.thread = Some(
            thread::Builder::new()
                .name("Backlog scan".to_owned())
                .spawn(move || {
                    scan_loop.run();
                })
                .unwrap(),
        );
    }

    pub fn stop(&mut self) {
        {
            let mut lock = self.flags.lock().unwrap();
            lock.stopped = true;
        }
        self.condition.notify_all();
        let handle = self.thread.take();
        if let Some(handle) = handle {
            handle.join().unwrap()
        }
        *self.publish.write().unwrap() = None;
    }

    /** Manually trigger backlog population */
    pub fn trigger(&self) {
        {
            let mut lock = self.flags.lock().unwrap();
            lock.triggered = true;
        }
        self.condition.notify_all();
    }

    pub fn stats(&self) -> Arc<BacklogScanStats> {
        self.stats.clone()
    }
}

impl Drop for BacklogScan {
    fn drop(&mut self) {
        self.stop();
    }
}

struct BacklogScanFlags {
    stopped: bool,
    /** This is a manual trigger, the ongoing backlog population does not use this.
     *  It can be triggered even when backlog population (frontiers confirmation) is disabled. */
    triggered: bool,
}

struct BacklogScanLoop {
    ledger: Arc<Ledger>,
    stats: Arc<BacklogScanStats>,
    publish: Arc<RwLock<Option<Box<dyn Fn(Vec<UnconfirmedInfo>) + Send + Sync>>>>,
    config: BacklogScanConfig,
    flags: Arc<Mutex<BacklogScanFlags>>,
    condition: Arc<Condvar>,
    limiter: Mutex<TokenBucket>,
}

impl BacklogScanLoop {
    fn run(&self) {
        let mut flags = self.flags.lock().unwrap();
        while !flags.stopped {
            if self.predicate(&flags) {
                flags = self.iterate_all_accounts(flags);
            } else {
                flags = self
                    .condition
                    .wait_while(flags, |l| !l.stopped && !self.predicate(l))
                    .unwrap();
            }
        }
    }

    fn predicate(&self, flags: &BacklogScanFlags) -> bool {
        flags.triggered || self.config.enabled
    }

    fn iterate_all_accounts<'a>(
        &'a self,
        mut flags: MutexGuard<'a, BacklogScanFlags>,
    ) -> MutexGuard<'a, BacklogScanFlags> {
        self.stats.looped.fetch_add(1, Ordering::Relaxed);
        flags.triggered = false;
        let mut next = Account::ZERO;
        let mut done = false;

        while !flags.stopped && !done {
            self.wait_for_rate_limiter(flags);
            let result = Self::scan_batch(&self.ledger, next, self.config.batch_size);
            done = result.done;
            next = result.next;
            self.add_stats(&result);
            self.notify_observers(result);
            flags = self.flags.lock().unwrap();
        }
        flags
    }

    fn wait_for_rate_limiter<'a>(&'a self, mut lock: MutexGuard<'a, BacklogScanFlags>) {
        // Wait for the rate limiter
        while !self
            .limiter
            .lock()
            .unwrap()
            .try_consume(self.config.batch_size)
        {
            lock = self
                .condition
                .wait_timeout_while(lock, self.config.wait_time(), |i| !i.stopped)
                .unwrap()
                .0;

            if lock.stopped {
                break;
            }
        }
    }

    fn scan_batch(ledger: &Ledger, next: Account, batch_size: usize) -> BacklogScanResult {
        let mut result = BacklogScanResult {
            next,
            done: true,
            ..Default::default()
        };
        let any = ledger.any();
        let mut it = any.accounts_range(result.next..);
        while let Some((account, account_info)) = it.next() {
            let conf_info = any.confirmed().get_conf_info(&account).unwrap_or_default();
            let is_confirmed = conf_info.height >= account_info.block_count;

            if is_confirmed {
                result.fully_confirmed += 1;
            } else {
                result.unconfirmed.push(UnconfirmedInfo {
                    account,
                    account_info,
                    conf_info,
                });
            }

            result.next = account.inc_or_max();
            if result.len() >= batch_size {
                result.done = it.next().is_none();
                break;
            }
        }

        result
    }

    fn add_stats(&self, result: &BacklogScanResult) {
        self.stats
            .scanned
            .fetch_add(result.len() as u64, Ordering::Relaxed);

        self.stats
            .activated
            .fetch_add(result.unconfirmed.len() as u64, Ordering::Relaxed);
    }

    fn notify_observers(&self, result: BacklogScanResult) {
        if !result.unconfirmed.is_empty() {
            let guard = self.publish.read().unwrap();
            if let Some(callback) = &*guard {
                callback(result.unconfirmed);
            }
        }
    }
}

impl StatsSource for BacklogScan {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result);
    }
}

#[derive(Default)]
pub struct BacklogScanResult {
    fully_confirmed: usize,
    unconfirmed: Vec<UnconfirmedInfo>,
    next: Account,
    done: bool,
}

impl BacklogScanResult {
    pub fn len(&self) -> usize {
        self.fully_confirmed + self.unconfirmed.len()
    }
}

#[derive(Clone)]
pub struct UnconfirmedInfo {
    pub account: Account,
    pub account_info: AccountInfo,
    pub conf_info: ConfirmationHeightInfo,
}

#[derive(Default)]
pub(crate) struct BacklogScanStats {
    looped: AtomicU64,
    scanned: AtomicU64,
    activated: AtomicU64,
}

impl StatsSource for BacklogScanStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert("backlog_scan", "loop", self.looped.load(Ordering::Relaxed));
        result.insert(
            "backlog_scan",
            "scanned",
            self.scanned.load(Ordering::Relaxed),
        );
        result.insert(
            "backlog_scan",
            "activated",
            self.activated.load(Ordering::Relaxed),
        );
    }
}

#[derive(Default)]
pub(crate) struct UnconfirmedQueueStats {
    pub enqueued: AtomicU64,
    pub enqueued_total: AtomicU64,
    pub overfill: AtomicU64,
    pub process_duration: AtomicU64,
}

impl StatsSource for UnconfirmedQueueStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "unconfirmed_queue",
            "enqueued",
            self.enqueued.load(Ordering::Relaxed),
        );
        result.insert(
            "unconfirmed_queue",
            "enqueued_total",
            self.enqueued_total.load(Ordering::Relaxed),
        );
        result.insert(
            "unconfirmed_queue",
            "overfill",
            self.overfill.load(Ordering::Relaxed),
        );
        result.insert(
            "unconfirmed_queue",
            "dur_process",
            self.process_duration.load(Ordering::Relaxed),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_accounts() {
        let ledger = Arc::new(
            Ledger::new_null_builder()
                .account_info(&Account::from(1), &AccountInfo::new_test_instance())
                .account_info(&Account::from(2), &AccountInfo::new_test_instance())
                .finish(),
        );
        let found = Arc::new(Mutex::new(Vec::new()));
        let found2 = found.clone();
        let done = Arc::new(Condvar::new());
        let done2 = done.clone();
        let callback = move |unconfirmed: Vec<UnconfirmedInfo>| {
            {
                let mut guard = found2.lock().unwrap();
                if !guard.is_empty() {
                    return;
                }

                guard.extend(unconfirmed.into_iter().map(|i| i.account));
            }
            done2.notify_all();
        };

        let mut backlog_scan = BacklogScan::new(BacklogScanConfig::default(), ledger, callback);

        backlog_scan.start();

        let mut found_guard = found.lock().unwrap();
        found_guard = done
            .wait_timeout_while(found_guard, Duration::from_secs(5), |i| i.is_empty())
            .unwrap()
            .0;

        assert_eq!(*found_guard, [Account::from(1), Account::from(2)]);
    }

    #[test]
    fn iterate_ledger_multiple_times() {
        let ledger = Arc::new(
            Ledger::new_null_builder()
                .account_info(&Account::from(1), &AccountInfo::new_test_instance())
                .account_info(&Account::from(2), &AccountInfo::new_test_instance())
                .finish(),
        );
        let found = Arc::new(Mutex::new(Vec::new()));
        let found2 = found.clone();
        let done = Arc::new(Condvar::new());
        let done2 = done.clone();
        let callback = move |unconfirmed: Vec<UnconfirmedInfo>| {
            {
                let mut guard = found2.lock().unwrap();
                if guard.len() >= 4 {
                    return;
                }

                guard.extend(unconfirmed.into_iter().map(|i| i.account));
            }
            done2.notify_all();
        };

        let mut backlog_scan = BacklogScan::new(BacklogScanConfig::default(), ledger, callback);

        backlog_scan.start();

        let mut found_guard = found.lock().unwrap();
        found_guard = done
            .wait_timeout_while(found_guard, Duration::from_secs(5), |i| i.len() != 4)
            .unwrap()
            .0;

        assert_eq!(
            *found_guard,
            [
                Account::from(1),
                Account::from(2),
                Account::from(1),
                Account::from(2)
            ]
        );
    }
}
