use std::{collections::HashMap, time::Duration};

use rsnano_nullable_clock::Timestamp;

/// RAI: which chunks of a fetch to ask for now - the missing ones not asked
/// for within the retry interval, at most `size` outstanding at once.
/// Asking for one chunk at a time makes a large object a long series of
/// round trips; asking for all of them at once swamps the inbound queues of
/// the node that serves them.
pub(super) struct ChunkWindow {
    size: usize,
    asked: HashMap<u32, Timestamp>,
}

impl ChunkWindow {
    pub fn new(size: usize) -> Self {
        Self {
            size,
            asked: HashMap::new(),
        }
    }

    /// The starts of `missing` to ask for now; they count as asked from now on
    pub fn due(&mut self, missing: Vec<u32>, now: Timestamp, retry: Duration) -> Vec<u32> {
        self.asked.retain(|start, _| missing.contains(start));
        let pending = |asked: &Timestamp| asked.elapsed(now) < retry;
        let outstanding = self.asked.values().filter(|asked| pending(asked)).count();
        let mut due = Vec::new();
        for start in missing {
            if due.len() + outstanding >= self.size {
                break;
            }
            if self.asked.get(&start).is_some_and(pending) {
                continue;
            }
            self.asked.insert(start, now);
            due.push(start);
        }
        due
    }

    /// A chunk arrived: its slot in the window is free again
    pub fn received(&mut self, start: u32) {
        self.asked.remove(&start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_for_at_most_a_window_and_again_after_the_retry_interval() {
        let now = Timestamp::new_test_instance();
        let retry = Duration::from_millis(300);
        let mut window = ChunkWindow::new(2);

        assert_eq!(window.due(vec![0, 10, 20], now, retry), vec![0, 10]);
        assert!(window.due(vec![0, 10, 20], now, retry).is_empty());

        window.received(0);
        assert_eq!(window.due(vec![10, 20], now, retry), vec![20]);

        let later = now + retry;
        assert_eq!(window.due(vec![10, 20], later, retry), vec![10, 20]);
    }
}
