use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_ledger::{AnySet, ConfirmedSet, Ledger, LedgerSet};
use rsnano_messages::{BlocksReq, Message, Publish};
use rsnano_network::{Channel, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{Account, Block, BlockHash};
use rsnano_utils::{
    CancellationToken,
    stats::{DetailType, Direction, StatType, Stats},
    ticker::Tickable,
};

use super::bounded_hash_map::BoundedHashMap;
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

    /// Blocks handed to the block processor from this node's own elections:
    /// asked for only if still absent a retry later
    pub fn supplied(&mut self, hashes: &[BlockHash], now: Timestamp) {
        for hash in hashes {
            if let Some(pending) = self.pending.get_mut(hash) {
                pending.last = Some(now);
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
        self.tick_known(now, |_| true, is_cemented, is_held, position_cemented)
    }

    /// The blocks pending, each with what it is followed for: what a tick
    /// looks up in the ledger
    pub fn goals(&self) -> Vec<(BlockHash, Goal)> {
        self.pending
            .iter()
            .map(|(hash, pending)| (*hash, pending.goal))
            .collect()
    }

    /// `tick` for the blocks `known` says were looked up; a block added
    /// since waits for the next tick
    pub fn tick_known(
        &mut self,
        now: Timestamp,
        known: impl Fn(&BlockHash) -> bool,
        is_cemented: impl Fn(&BlockHash) -> bool,
        is_held: impl Fn(&BlockHash) -> bool,
        position_cemented: impl Fn(&Account, u64) -> bool,
    ) -> CheckpointWork {
        let mut work = CheckpointWork::default();
        self.pending.retain(|hash, pending| {
            if !known(hash) {
                return true;
            }
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
                // A finalized block is held by the correct nodes that
                // finalized it: no answer means one was lost, not that no
                // peer has it, and the next committee waits for it. Asked
                // for, like cementing, at the plain interval.
                Goal::Cement => Self::RETRY,
                Goal::Hold { .. } => Self::wait(pending.requests.saturating_sub(1)),
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
    /// The blocks rolled back from the ledger. A later checkpoint can retain
    /// one whose election is gone - a late instance's discard, a rolled-back
    /// dependent - and when every node rolled it back no ledger serves it:
    /// the body forces it in again and answers the requests of the others.
    rolled_back: Mutex<BoundedHashMap<BlockHash, Block>>,
    /// Which representative the next request goes to
    next_rep: std::sync::atomic::AtomicUsize,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    flooder: Mutex<MessageFlooder>,
    clock: Arc<SteadyClock>,
    stats: Arc<Stats>,
}

impl CheckpointFollower {
    /// Several epochs of fork switches and late discards
    const MAX_ROLLED_BACK: usize = 64 * 1024;

    pub fn new(
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        flooder: MessageFlooder,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            pending: Mutex::new(PendingCheckpointBlocks::default()),
            rolled_back: Mutex::new(BoundedHashMap::new(Self::MAX_ROLLED_BACK)),
            next_rep: std::sync::atomic::AtomicUsize::new(0),
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

    /// Retained blocks this node's elections held and handed to the block
    /// processor: asked for only if still absent a retry later
    pub fn supplied(&self, hashes: &[BlockHash]) {
        if hashes.is_empty() {
            return;
        }
        let now = self.clock.now();
        self.pending.lock().unwrap().supplied(hashes, now);
    }

    /// Blocks rolled back from the ledger
    pub fn rolled_back(&self, blocks: impl IntoIterator<Item = Block>) {
        let mut rolled_back = self.rolled_back.lock().unwrap();
        for block in blocks {
            rolled_back.insert(block.hash(), block);
        }
    }

    /// The body of a block rolled back from the ledger
    pub fn rolled_back_body(&self, hash: &BlockHash) -> Option<Block> {
        self.rolled_back.lock().unwrap().get(hash).cloned()
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
        let blocks: Vec<Block> = {
            let any = self.ledger.any();
            let rolled_back = self.rolled_back.lock().unwrap();
            request
                .hashes
                .iter()
                .take(BlocksReq::MAX_HASHES)
                .filter_map(|hash| {
                    any.get_block(hash)
                        .map(Block::from)
                        .or_else(|| rolled_back.get(hash).cloned())
                })
                .collect()
        };
        let mut flooder = self.flooder.lock().unwrap();
        for block in blocks {
            flooder.try_send(
                channel,
                &Message::Publish(Publish::new_evidence(block)),
                TrafficType::VoteReply,
            );
        }
    }

    fn tick(&self) {
        let now = self.clock.now();
        // The ledger is read without holding the pending blocks, which every
        // arriving block is checked against: a decided checkpoint hands over
        // tens of thousands at once
        let goals = self.pending.lock().unwrap().goals();
        let (looked_up, positions) = {
            let any = self.ledger.any();
            let confirmed = self.ledger.confirmed();
            let mut looked_up: HashMap<BlockHash, (bool, bool)> = HashMap::new();
            let mut positions: HashMap<(Account, u64), bool> = HashMap::new();
            for (hash, goal) in goals {
                looked_up.insert(
                    hash,
                    (confirmed.block_exists(&hash), any.block_exists(&hash)),
                );
                if let Goal::Hold { account, height } = goal {
                    positions.insert(
                        (account, height),
                        confirmed
                            .get_conf_info(&account)
                            .is_some_and(|info| info.height >= height),
                    );
                }
            }
            (looked_up, positions)
        };
        let work = self.pending.lock().unwrap().tick_known(
            now,
            |hash| looked_up.contains_key(hash),
            |hash| looked_up.get(hash).is_some_and(|(cemented, _)| *cemented),
            |hash| looked_up.get(hash).is_some_and(|(_, held)| *held),
            |account, height| positions.get(&(*account, height)).copied().unwrap_or(false),
        );
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
            // One representative a request, the next one each round: every
            // holder answers a flooded request with an evidence publish,
            // which passes the duplicate filter, and a fork sibling is
            // verified anew in each copy - five copies of a thousand blocks
            // a second after an installation. A representative that lacks
            // a block leaves it to the next round's.
            let request = Message::BlocksReq(BlocksReq {
                hashes: chunk.to_vec(),
            });
            let pick = self
                .next_rep
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut flooder = self.flooder.lock().unwrap();
            if !flooder.send_to_principal_rep(&request, TrafficType::Generic, pick) {
                flooder.flood_prs_and_some_non_prs(&request, TrafficType::Generic, 1.0);
            }
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
    fn a_block_added_after_the_lookup_waits_for_the_next_tick() {
        let mut pending = PendingCheckpointBlocks::default();
        let (looked_up, added) = (BlockHash::from(1), BlockHash::from(2));
        pending.add([looked_up, added]);
        let work = pending.tick_known(
            now(0),
            |hash| *hash == looked_up,
            |_| false,
            |_| false,
            no_position,
        );
        assert_eq!(work.request, vec![looked_up]);
        assert_eq!(pending.len(), 2);
        let work = pending.tick(now(0), |_| false, |_| false, no_position);
        assert_eq!(work.request, vec![added]);
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
    fn a_finalized_block_is_asked_for_at_the_plain_interval() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.add([hash(1)]);
        let nothing = |_: &BlockHash| false;
        let asked: Vec<u64> = (0..6)
            .filter(|second| {
                !pending
                    .tick(now(*second), nothing, nothing, no_position)
                    .request
                    .is_empty()
            })
            .collect();
        assert_eq!(asked, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_retained_block_no_peer_sends_is_asked_for_less_and_less_often() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.hold([(Account::from(1), 2, hash(1))]);
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
    fn a_retained_block_supplied_locally_is_asked_for_only_a_retry_later() {
        let mut pending = PendingCheckpointBlocks::default();
        pending.hold([(Account::from(1), 2, hash(1))]);
        pending.supplied(&[hash(1)], now(0));
        let work = pending.tick(now(0), |_| false, |_| false, no_position);
        assert!(work.request.is_empty());
        let work = pending.tick(now(1), |_| false, |_| false, no_position);
        assert_eq!(work.request, vec![hash(1)]);
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

    #[test]
    fn a_rolled_back_block_keeps_its_body() {
        let follower = CheckpointFollower::new_null();
        let block = Block::new_test_instance_with_key(1);
        assert_eq!(follower.rolled_back_body(&block.hash()), None);
        follower.rolled_back([block.clone()]);
        assert_eq!(follower.rolled_back_body(&block.hash()), Some(block));
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
