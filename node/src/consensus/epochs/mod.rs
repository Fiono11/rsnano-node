use rsnano_types::{BlockHash, QualifiedRoot, SlotRoot};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

pub struct VoteGate {
    state: Mutex<GateInner>,
    drained: Condvar,
    cut_generation: AtomicU64,
    fast_open: AtomicBool,
    fast_in_flight: AtomicUsize,
}

impl Default for VoteGate {
    fn default() -> Self {
        Self {
            state: Default::default(),
            drained: Default::default(),
            cut_generation: Default::default(),
            fast_open: AtomicBool::new(true),
            fast_in_flight: Default::default(),
        }
    }
}

mod coordinator;
pub use coordinator::EpochCoordinator;

#[derive(Default)]
struct GateInner {
    policy: VoteGateState,
    in_flight: usize,
    finalized: HashMap<u64, HashMap<SlotRoot, BlockHash>>,
}

#[derive(Default)]
enum VoteGateState {
    #[default]
    Open,
    Paused,
    Collecting {
        closing_epoch: u64,
        open_epoch: Option<u64>,
    },
    Draining {
        closing_epoch: u64,
        open_epoch: Option<u64>,
        cut_elections: HashSet<SlotRoot>,
    },
}

impl VoteGate {
    fn allows(policy: &VoteGateState, root: &QualifiedRoot) -> bool {
        match policy {
            VoteGateState::Open => true,
            VoteGateState::Paused => false,
            VoteGateState::Collecting { open_epoch, .. } => {
                open_epoch.is_some_and(|epoch| root.epoch == epoch)
            }
            VoteGateState::Draining {
                closing_epoch,
                open_epoch,
                cut_elections,
            } => {
                open_epoch.is_some_and(|epoch| root.epoch == epoch)
                    || (root.epoch == *closing_epoch && cut_elections.contains(&root.slot()))
            }
        }
    }

    pub fn enter(self: &Arc<Self>, root: &QualifiedRoot) -> Option<VotePermit> {
        if self.fast_open.load(Ordering::Acquire) {
            self.fast_in_flight.fetch_add(1, Ordering::AcqRel);
            if self.fast_open.load(Ordering::Acquire) {
                return Some(VotePermit {
                    gate: self.clone(),
                    fast: true,
                });
            }
            self.leave_fast();
        }

        let mut state = self.state.lock().unwrap();
        if !Self::allows(&state.policy, root) {
            return None;
        }
        state.in_flight += 1;
        Some(VotePermit {
            gate: self.clone(),
            fast: false,
        })
    }

    /// Authorizes only a solicited final-vote recovery response for a winner this node already
    /// finalized in the closing epoch. It does not authorize first/non-final vote generation.
    pub fn allows_final_recovery(&self, root: &QualifiedRoot, hash: &BlockHash) -> bool {
        let state = self.state.lock().unwrap();
        matches!(
            state.policy,
            VoteGateState::Collecting { closing_epoch, .. }
                | VoteGateState::Draining { closing_epoch, .. }
                if root.epoch == closing_epoch
        ) && state
            .finalized
            .get(&root.epoch)
            .and_then(|finalized| finalized.get(&root.slot()))
            == Some(hash)
    }

    pub fn set_finalized(&self, epoch: u64, finalized: HashMap<SlotRoot, BlockHash>) {
        self.state
            .lock()
            .unwrap()
            .finalized
            .insert(epoch, finalized);
    }

    pub fn clear_finalized(&self, epoch: u64) {
        self.state.lock().unwrap().finalized.remove(&epoch);
    }

    pub fn open(&self) {
        self.state.lock().unwrap().policy = VoteGateState::Open;
        self.fast_open.store(true, Ordering::Release);
    }

    pub fn pause(&self) {
        self.fast_open.store(false, Ordering::Release);
        let mut state = self.state.lock().unwrap();
        state.policy = VoteGateState::Paused;
        while state.in_flight > 0 || self.fast_in_flight.load(Ordering::Acquire) > 0 {
            state = self.drained.wait(state).unwrap();
        }
    }

    /// Opens the next scheduled epoch while the closing epoch's pending-election reports are
    /// collected. New closing-epoch phase votes remain blocked; cached replies bypass this gate,
    /// and `allows_final_recovery` admits final recovery for locally finalized winners.
    pub fn start_collecting(&self, closing_epoch: u64, open_epoch: Option<u64>) {
        self.fast_open.store(false, Ordering::Release);
        self.state.lock().unwrap().policy = VoteGateState::Collecting {
            closing_epoch,
            open_epoch,
        };
    }

    pub fn install_cut(
        &self,
        epoch: u64,
        open_epoch: Option<u64>,
        cut_elections: HashSet<SlotRoot>,
    ) {
        self.fast_open.store(false, Ordering::Release);
        self.state.lock().unwrap().policy = VoteGateState::Draining {
            closing_epoch: epoch,
            open_epoch,
            cut_elections,
        };
        self.cut_generation.fetch_add(1, Ordering::Release);
    }

    pub fn cut_generation(&self) -> u64 {
        self.cut_generation.load(Ordering::Acquire)
    }

    fn leave_fast(&self) {
        if self.fast_in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _state = self.state.lock().unwrap();
            self.drained.notify_all();
        }
    }
}

pub struct VotePermit {
    gate: Arc<VoteGate>,
    fast: bool,
}

impl Drop for VotePermit {
    fn drop(&mut self) {
        if self.fast {
            self.gate.leave_fast();
            return;
        }
        let mut state = self.gate.state.lock().unwrap();
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.gate.drained.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{BlockHash, Root};

    #[test]
    fn draining_allows_next_epoch_and_only_cut_elections_from_closing_epoch() {
        let gate = VoteGate::default();
        let slot = SlotRoot {
            root: Root::from(1),
            previous: BlockHash::from(2),
        };
        gate.install_cut(1, Some(2), [slot].into());
        let gate = Arc::new(gate);
        assert!(gate.enter(&slot.with_epoch(1)).is_some());
        assert_eq!(gate.cut_generation(), 1);
        assert!(gate.enter(&slot.with_epoch(2)).is_some());
        assert!(
            gate.enter(
                &SlotRoot {
                    root: Root::from(3),
                    previous: BlockHash::from(4)
                }
                .with_epoch(1)
            )
            .is_none()
        );
        assert!(
            gate.enter(
                &SlotRoot {
                    root: Root::from(3),
                    previous: BlockHash::from(4)
                }
                .with_epoch(2)
            )
            .is_some()
        );
        assert!(gate.enter(&slot.with_epoch(3)).is_none());
    }

    #[test]
    fn final_recovery_requires_exact_finalized_epoch_slot_and_hash() {
        let slot = SlotRoot {
            root: Root::from(1),
            previous: BlockHash::from(2),
        };
        let winner = BlockHash::from(3);
        let gate = VoteGate::default();
        gate.install_cut(1, Some(2), Default::default());
        gate.set_finalized(1, [(slot, winner)].into());

        assert!(gate.allows_final_recovery(&slot.with_epoch(1), &winner));
        assert!(!gate.allows_final_recovery(&slot.with_epoch(2), &winner));
        assert!(!gate.allows_final_recovery(&slot.with_epoch(1), &BlockHash::from(4)));
    }

    #[test]
    fn collecting_opens_next_epoch_and_blocks_new_closing_epoch_votes() {
        let slot = SlotRoot {
            root: Root::from(1),
            previous: BlockHash::from(2),
        };
        let gate = Arc::new(VoteGate::default());
        gate.start_collecting(1, Some(2));

        assert!(gate.enter(&slot.with_epoch(1)).is_none());
        assert!(gate.enter(&slot.with_epoch(2)).is_some());
        assert!(gate.enter(&slot.with_epoch(3)).is_none());
    }
}
