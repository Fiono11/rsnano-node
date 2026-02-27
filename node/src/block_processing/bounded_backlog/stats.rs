use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use rsnano_utils::stats::{StatsCollection, StatsSource};

#[derive(Default)]
pub(crate) struct BoundedBacklogStats {
    pub(crate) loop_scan: AtomicUsize,
    pub(crate) scanned: AtomicUsize,
    pub(crate) loop_rollback: AtomicUsize,
    pub(crate) gathered_targets: AtomicUsize,
    pub(crate) no_targets: AtomicUsize,
}

impl StatsSource for BoundedBacklogStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert("bounded_backlog", "loop_scan", self.loop_scan.load(Relaxed));
        result.insert("bounded_backlog", "scanned", self.scanned.load(Relaxed));
        result.insert("bounded_backlog", "loop", self.loop_rollback.load(Relaxed));
        result.insert(
            "bounded_backlog",
            "gathered_targets",
            self.gathered_targets.load(Relaxed),
        );
        result.insert(
            "bounded_backlog",
            "no_targets",
            self.no_targets.load(Relaxed),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_stats() {
        let stats = BoundedBacklogStats {
            loop_scan: AtomicUsize::new(1),
            scanned: AtomicUsize::new(2),
            loop_rollback: AtomicUsize::new(3),
            gathered_targets: AtomicUsize::new(4),
            no_targets: AtomicUsize::new(5),
        };

        let mut result = StatsCollection::new();
        stats.collect_stats(&mut result);

        assert_eq!(result.get("bounded_backlog", "loop_scan"), 1);
        assert_eq!(result.get("bounded_backlog", "scanned"), 2);
        assert_eq!(result.get("bounded_backlog", "loop"), 3);
        assert_eq!(result.get("bounded_backlog", "gathered_targets"), 4);
        assert_eq!(result.get("bounded_backlog", "no_targets"), 5);
    }
}
