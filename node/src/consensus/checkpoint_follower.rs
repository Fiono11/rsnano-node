use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_ledger::{AnySet, ConfirmedSet, Ledger, LedgerSet};
use rsnano_messages::{BlocksReq, Message, Publish};
use rsnano_network::{Channel, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{Account, BlockHash};
use rsnano_utils::{
    CancellationToken,
    stats::{DetailType, Direction, StatType, Stats},
    ticker::Tickable,
};

use crate::{cementation::ConfirmingSet, transport::MessageFlooder};

/// RAI: the blocks a decided checkpoint finalized that this node has not
/// cemented yet, and the retained blocks its ledger does not hold. A
/// checkpoint can finalize a block a node never received, or the rival of
/// the block its ledger holds; the ledger must still end up with the
/// finalized one, cemented, like every other node. A retained branch is
/// held the same way, uncemented: "Recovery through a fresh child" extends
/// it. This is the pure part: what to cement and what to ask for, given
/// what the ledger holds.
#[derive(Default)]
pub(crate) struct PendingCheckpointBlocks {
    pending: HashMap<BlockHash, Pending>,
}

/// What the ledger must do with a pending block
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Goal {
    /// Finalized by the checkpoint: held and cemented
    Cement,
    /// Retained by the checkpoint at a position: held, not cemented. Given
    /// up once the position is cemented, whichever block it holds.
    Hold { account: Account, height: u64 },
}

struct Pending {
    goal: Goal,
    last: Option<Timestamp>,
    /// Requests sent without the block arriving: each doubles the wait
    requests: u32,
}

/// What one pass over the pending blocks asks of the infrastructure
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CheckpointWork {
    /// Held unconfirmed: hand to the confirming set
    pub cement: Vec<BlockHash>,
    /// Not held: ask the other nodes for them
    pub request: Vec<BlockHash>,
    /// Cemented since, or held for a retained block: done
    pub done: usize,
}

impl PendingCheckpointBlocks {
    /// More than a whole epoch at the benchmark rate; older ones are dropped
    const MAX_PENDING: usize = 100_000;
    /// How often one block is cemented, or asked for the first time again
    pub const RETRY: Duration = Duration::from_secs(1);
    /// A block no peer sends is asked for less and less often: the wait
    /// doubles with every request, up to this many doublings
    const MAX_DOUBLINGS: u32 = 3;

    pub fn add(&mut self, hashes: impl IntoIterator<Item = BlockHash>) {
        self.add_with(hashes.into_iter().map(|hash| (hash, Goal::Cement)));
    }

    /// Retained blocks: held, not cemented
    pub fn hold(&mut self, blocks: impl IntoIterator<Item = (Account, u64, BlockHash)>) {
        self.add_with(
            blocks
                .into_iter()
                .map(|(account, height, hash)| (hash, Goal::Hold { account, height })),
        );
    }

    fn add_with(&mut self, blocks: impl IntoIterator<Item = (BlockHash, Goal)>) {
        for (hash, goal) in blocks {
            if self.pending.len() >= Self::MAX_PENDING {
                break;
            }
            let pending = self.pending.entry(hash).or_insert(Pending {
                goal,
                last: None,
                requests: 0,
            });
            // Finalized after it was retained: cemented
            if goal == Goal::Cement {
                pending.goal = Goal::Cement;
            }
        }
    }

    fn wait(requests: u32) -> Duration {
        Self::RETRY * (1 << requests.min(Self::MAX_DOUBLINGS))
    }

    pub fn wants(&self, hash: &BlockHash) -> bool {
        self.pending.contains_key(hash)
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// One pass: drop what is done, and for the rest due again, cement
    /// what the ledger holds and ask for what it lacks. `position_cemented`
    /// tells whether the ledger cemented an account up to a height.
    pub fn tick(
        &mut self,
        now: Timestamp,
        is_cemented: impl Fn(&BlockHash) -> bool,
        is_held: impl Fn(&BlockHash) -> bool,
        position_cemented: impl Fn(&Account, u64) -> bool,
    ) -> CheckpointWork {
        let mut work = CheckpointWork::default();
        self.pending.retain(|hash, pending| {
            let held = || is_held(hash);
            let done = match pending.goal {
                Goal::Cement => is_cemented(hash),
                Goal::Hold { account, height } => held() || position_cemented(&account, height),
            };
            if done {
                work.done += 1;
                return false;
            }
            let wait = match pending.goal {
                // Cementing is retried at the plain interval: the ancestors
                // it waits for arrive by themselves
                Goal::Cement if held() => Self::RETRY,
                _ => Self::wait(pending.requests.saturating_sub(1)),
            };
            if pending.last.is_some_and(|last| last.elapsed(now) < wait) {
                return true;
            }
            pending.last = Some(now);
            if held() {
                work.cement.push(*hash);
            } else {
                pending.requests += 1;
                work.request.push(*hash);
            }
            true
        });
        work
    }
}

/// RAI: makes the ledger follow the decided checkpoints. The blocks a
/// checkpoint finalized and this node has not cemented are cemented once
/// held; the missing ones are asked for with `BlocksReq`, and on arrival
/// they are forced in, which rolls back an unconfirmed rival.
pub struct CheckpointFollower {
    pending: Mutex<PendingCheckpointBlocks>,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    flooder: Mutex<MessageFlooder>,
    clock: Arc<SteadyClock>,
    stats: Arc<Stats>,
}

impl CheckpointFollower {
    pub fn new(
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        flooder: MessageFlooder,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            pending: Mutex::new(PendingCheckpointBlocks::default()),
            ledger,
            confirming_set,
            flooder: Mutex::new(flooder),
            clock,
            stats,
        }
    }

    pub fn new_null() -> Self {
        Self::new(
            Arc::new(Ledger::new_null()),
            Arc::new(ConfirmingSet::new_null()),
            MessageFlooder::new_null(),
            Arc::new(SteadyClock::new_null()),
            Arc::new(Stats::default()),
        )
    }

    /// The blocks a checkpoint finalized that the ledger has not cemented
    pub fn add(&self, hashes: impl IntoIterator<Item = BlockHash>) {
        self.pending.lock().unwrap().add(hashes);
    }

    /// The retained blocks of a checkpoint the ledger does not hold: on
    /// arrival they are forced in, which rolls back an unconfirmed rival
    pub fn hold(&self, blocks: impl IntoIterator<Item = (Account, u64, BlockHash)>) {
        self.pending.lock().unwrap().hold(blocks);
    }

    /// A block arriving that a checkpoint finalized: it is forced in
    pub fn wants(&self, hash: &BlockHash) -> bool {
        self.pending.lock().unwrap().wants(hash)
    }

    pub fn pending_len(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// Another node asks for blocks: the ones held here go back as evidence
    /// publishes, which pass the duplicate filter
    pub fn handle_request(&self, request: BlocksReq, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::BlocksReq, Direction::In);
        let blocks: Vec<_> = {
            let any = self.ledger.any();
            request
                .hashes
                .iter()
                .take(BlocksReq::MAX_HASHES)
                .filter_map(|hash| any.get_block(hash))
                .collect()
        };
        let mut flooder = self.flooder.lock().unwrap();
        for block in blocks {
            flooder.try_send(
                channel,
                &Message::Publish(Publish::new_evidence(block.into())),
                TrafficType::VoteReply,
            );
        }
    }

    fn tick(&self) {
        let now = self.clock.now();
        let work = {
            let any = self.ledger.any();
            let confirmed = self.ledger.confirmed();
            self.pending.lock().unwrap().tick(
                now,
                |hash| confirmed.block_exists(hash),
                |hash| any.block_exists(hash),
                |account, height| {
                    confirmed
                        .get_conf_info(account)
                        .is_some_and(|info| info.height >= height)
                },
            )
        };
        for hash in &work.cement {
            self.confirming_set.add_block(*hash);
        }
        self.stats.add(
            StatType::Ledger,
            DetailType::CheckpointBlockCemented,
            work.done as u64,
        );
        for chunk in work.request.chunks(BlocksReq::MAX_HASHES) {
            self.stats.add(
                StatType::Ledger,
                DetailType::CheckpointBlockRequested,
                chunk.len() as u64,
            );
            self.stats
                .inc_dir(StatType::Message, DetailType::BlocksReq, Direction::Out);
            self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                &Message::BlocksReq(BlocksReq {
                    hashes: chunk.to_vec(),
                }),
                TrafficType::Generic,
                1.0,
            );
        }
        if !work.cement.is_empty() || !work.request.is_empty() || work.done > 0 {
            crate::utils::diagnostic!(
                "EPOCH_FOLLOW cemented={} cementing={} requested={} pending={}",
                work.done,
                work.cement.len(),
                work.request.len(),
                self.pending_len()
            );
        }
    }
}

/// Drives the follower on the ticker pool
pub(crate) struct CheckpointFollowerTicker(pub Arc<CheckpointFollower>);

impl Tickable for CheckpointFollowerTicker {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        self.0.tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn cements_what_is_held_and_asks_for_what_is_missing() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.add([hash(1), hash(2), hash(3)]);
        let cemented: HashSet<_> = [hash(1)].into();
        let held: HashSet<_> = [hash(1), hash(2)].into();
        let mut work = pending.tick(
            now(0),
            |h| cemented.contains(h),
            |h| held.contains(h),
            no_position,
        );
        work.request.sort();
        assert_eq!(work.done, 1);
        assert_eq!(work.cement, vec![hash(2)]);
        assert_eq!(work.request, vec![hash(3)]);
        assert!(!pending.wants(&hash(1)));
        assert!(pending.wants(&hash(3)));
    }

    #[test]
    fn a_block_is_asked_for_again_only_after_the_retry_interval() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.add([hash(1)]);
        let nothing = |_: &BlockHash| false;
        assert_eq!(
            pending
                .tick(now(0), nothing, nothing, no_position)
                .request
                .len(),
            1
        );
        assert!(
            pending
                .tick(now(0), nothing, nothing, no_position)
                .request
                .is_empty()
        );
        let later = now(0) + PendingCheckpointBlocks::RETRY;
        assert_eq!(
            pending
                .tick(later, nothing, nothing, no_position)
                .request
                .len(),
            1
        );
    }

    #[test]
    fn an_arrived_block_is_cemented_then_dropped() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.add([hash(1)]);
        let nothing = |_: &BlockHash| false;
        pending.tick(now(0), nothing, nothing, no_position);
        let later = now(0) + PendingCheckpointBlocks::RETRY;
        let work = pending.tick(later, nothing, |_| true, no_position);
        assert_eq!(work.cement, vec![hash(1)]);
        let work = pending.tick(later, |_| true, |_| true, no_position);
        assert_eq!(work.done, 1);
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn a_block_no_peer_sends_is_asked_for_less_and_less_often() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.add([hash(1)]);
        let nothing = |_: &BlockHash| false;
        let mut asked = Vec::new();
        for second in 0..16 {
            if !pending
                .tick(now(second), nothing, nothing, no_position)
                .request
                .is_empty()
            {
                asked.push(second);
            }
        }
        assert_eq!(asked, vec![0, 1, 3, 7, 15]);
    }

    #[test]
    fn a_retained_block_is_held_not_cemented() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.hold([(Account::from(1), 2, hash(1))]);
        let nothing = |_: &BlockHash| false;
        assert_eq!(
            pending.tick(now(0), nothing, nothing, no_position).request,
            vec![hash(1)]
        );
        // Arrived: done without cementing
        let work = pending.tick(now(1), nothing, |_| true, no_position);
        assert!(work.cement.is_empty());
        assert_eq!(work.done, 1);
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn a_retained_block_is_given_up_once_its_position_is_cemented() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.hold([(Account::from(1), 2, hash(1))]);
        let nothing = |_: &BlockHash| false;
        let cemented_to_2 =
            |account: &Account, height: u64| *account == Account::from(1) && height <= 2;
        let work = pending.tick(now(0), nothing, nothing, cemented_to_2);
        assert!(work.request.is_empty());
        assert_eq!(work.done, 1);
    }

    #[test]
    fn a_retained_block_finalized_later_is_cemented() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.hold([(Account::from(1), 2, hash(1))]);
        pending.add([hash(1)]);
        let work = pending.tick(now(0), |_| false, |_| true, no_position);
        assert_eq!(work.cement, vec![hash(1)]);
    }

    /*
     * Test helpers
     */

    fn no_position(_: &Account, _: u64) -> bool {
        false
    }

    fn hash(i: u64) -> BlockHash {
        BlockHash::from(i)
    }

    fn now(secs: u64) -> Timestamp {
        Timestamp::new_test_instance() + Duration::from_secs(secs)
    }
}
