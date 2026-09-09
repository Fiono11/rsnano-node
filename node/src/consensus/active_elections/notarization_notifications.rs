use rsnano_types::{BlockHash, ElectionId};
use std::collections::{HashSet, VecDeque};

/// Best-effort acceleration only: periodic recovery remains responsible for retries.
#[derive(Default)]
pub(super) struct NotarizationNotifications {
    queue: VecDeque<(ElectionId, BlockHash)>,
    pending: HashSet<(ElectionId, BlockHash)>,
}
impl NotarizationNotifications {
    const CAPACITY: usize = 1024;
    pub fn push(&mut self, item: (ElectionId, BlockHash)) {
        if self.queue.len() < Self::CAPACITY && self.pending.insert(item.clone()) {
            self.queue.push_back(item);
        }
    }
    pub fn take(&mut self) -> Vec<(ElectionId, BlockHash)> {
        let mut result = Vec::with_capacity(64);
        for _ in 0..64 {
            let Some(item) = self.queue.pop_front() else {
                break;
            };
            self.pending.remove(&item);
            result.push(item);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_coalesced_and_budgeted() {
        let mut q = NotarizationNotifications::default();
        let id = ElectionId::new(rsnano_types::QualifiedRoot::new_test_instance(), 0);
        for n in 0..2048 {
            let item = (id.clone(), BlockHash::from(n as u64));
            q.push(item.clone());
            q.push(item);
        }
        assert_eq!(q.queue.len(), 1024);
        assert_eq!(q.pending.len(), 1024);
        assert_eq!(q.take().len(), 64);
        assert_eq!(q.pending.len(), 960);
        q.push((id, BlockHash::from(0)));
        assert_eq!(q.pending.len(), 961);
    }
}
