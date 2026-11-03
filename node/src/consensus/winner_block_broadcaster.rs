use std::{
    cmp::max,
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

use rsnano_messages::{Message, Publish};
use rsnano_network::{Network, TrafficType, token_bucket::TokenBucket};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{Block, BlockHash, NetworkType, PublicKey};
use rsnano_utils::stats::{StatsCollection, StatsSource};

use super::{bounded_hash_map::BoundedHashMap, election::VoteSummary};
use crate::{representatives::RepresentativeTracker, transport::MessageFlooder};

/// Broadcasts the winner block of an election
pub(crate) struct WinnerBlockBroadcaster {
    clock: Arc<SteadyClock>,
    broadcast_tracker: BroadcastTracker,
    message_flooder: MessageFlooder,
    rebroadcast_limiter: TokenBucket,
    broadcast_listener: OutputListenerMt<BlockHash>,
    rep_tracker: Arc<RepresentativeTracker>,
    network: Arc<RwLock<Network>>,
}

impl WinnerBlockBroadcaster {
    pub(crate) fn new(
        clock: Arc<SteadyClock>,
        networks: NetworkType,
        message_flooder: MessageFlooder,
        rep_tracker: Arc<RepresentativeTracker>,
        network: Arc<RwLock<Network>>,
        min_interval: Duration,
    ) -> Self {
        Self {
            clock,
            broadcast_tracker: BroadcastTracker::new(networks, min_interval),
            message_flooder,
            // TODO: Make rate limit configurable
            rebroadcast_limiter: TokenBucket::with_burst_ratio(100, 2.0),
            broadcast_listener: OutputListenerMt::default(),
            rep_tracker,
            network,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn new_null() -> Self {
        let clock = Arc::new(SteadyClock::new_null());
        let networks = NetworkType::NanoLiveNetwork;
        let rep_tracker = RepresentativeTracker::default();
        let network = RwLock::new(Network::new_null());
        Self::new(
            clock,
            networks,
            MessageFlooder::new_null(),
            rep_tracker.into(),
            network.into(),
            Duration::ZERO,
        )
    }

    #[allow(dead_code)]
    pub fn track(&self) -> Arc<OutputTrackerMt<BlockHash>> {
        self.broadcast_listener.track()
    }

    /// Whether the winner is due for a broadcast at `now`
    pub(crate) fn broadcast_due(&self, winner_hash: &BlockHash, now: Timestamp) -> bool {
        self.broadcast_tracker.should_broadcast(now, winner_hash)
    }

    pub fn try_broadcast_winner(
        &mut self,
        winner_block: &Block,
        votes: &HashMap<PublicKey, VoteSummary>,
    ) {
        let now = self.clock.now();
        let winner_hash = winner_block.hash();
        self.broadcast_listener.emit(winner_hash);

        if !self.broadcast_tracker.should_broadcast(now, &winner_hash) {
            return;
        }

        // Maximum amount of directed broadcasts to be sent per election.
        // RAI: every representative without a vote for the winner gets it: a
        // replica that missed the block cannot take part in the instance
        let max_election_broadcasts = {
            #[cfg(feature = "rai_protocol")]
            {
                usize::MAX
            }
            #[cfg(not(feature = "rai_protocol"))]
            {
                max(self.network.read().unwrap().fanout(1.0) / 2, 1)
            }
        };

        if !self.rebroadcast_limiter.try_consume(1) {
            return;
        }

        let winner_msg = Message::Publish(Publish::new_forward(winner_block.clone()));

        let peered_prs = self.rep_tracker.peered_principal_reps();

        let mut count = 0;
        // Directed broadcasting to principal representatives
        for i in &peered_prs {
            if count >= max_election_broadcasts {
                break;
            }
            let should_broadcast = if let Some(existing) = votes.get(&i.rep_key) {
                // Don't rebroadcast to a PR if this PR has voted for the block!
                existing.hash != winner_hash
            } else {
                true
            };
            if should_broadcast {
                count += 1;
                self.message_flooder.try_send_channel_id(
                    i.channel_id,
                    &winner_msg,
                    TrafficType::BlockBroadcast,
                );
            }
        }

        // Random flood for block propagation
        // TODO: Avoid broadcasting to the same peers that were already broadcasted to
        self.message_flooder
            .flood(&winner_msg, TrafficType::BlockBroadcast, 0.5);

        self.broadcast_tracker.insert(now, winner_hash);
    }
}

impl StatsSource for WinnerBlockBroadcaster {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.broadcast_tracker.collect_stats(result);
    }
}

struct BroadcastTracker {
    /// When each winner was last broadcast, and how often it was repeated
    last_broadcasts: BoundedHashMap<BlockHash, (Timestamp, u32)>,
    broadcast_interval: Duration,
    broadcast_initial: u64,
    broadcast_repeat: u64,
}

impl BroadcastTracker {
    /// The longest wait a repeated broadcast of one winner backs off to
    const LONGEST_REPEAT: Duration = Duration::from_secs(8);
    const MAX_DOUBLINGS: u32 = 4;

    pub fn new(network: NetworkType, min_interval: Duration) -> Self {
        Self {
            last_broadcasts: BoundedHashMap::new(1024 * 32),
            // RAI: a representative without the block is asked about the
            // instance every second and gets the block at the same pace;
            // across wide-area links not before a round trip
            broadcast_interval: match network {
                NetworkType::NanoDevNetwork => Duration::from_millis(500),
                #[cfg(feature = "rai_protocol")]
                _ => Duration::from_secs(1),
                #[cfg(not(feature = "rai_protocol"))]
                _ => Duration::from_secs(150),
            }
            .max(min_interval),
            broadcast_initial: 0,
            broadcast_repeat: 0,
        }
    }

    pub fn insert(&mut self, now: Timestamp, hash: BlockHash) -> bool {
        let repeats = self
            .last_broadcasts
            .get(&hash)
            .map_or(0, |(_, repeats)| repeats + 1);
        let is_initial = self.last_broadcasts.insert(hash, (now, repeats)).is_none();

        if is_initial {
            self.broadcast_initial += 1;
        } else {
            self.broadcast_repeat += 1;
        }

        is_initial
    }

    pub fn should_broadcast(&self, now: Timestamp, block_hash: &BlockHash) -> bool {
        // Broadcast the block if enough time has passed since the last broadcast (or it's the first broadcast)
        // A repeat waits twice as long as the one before: an instance open
        // for minutes - contested forks under a Byzantine member - otherwise
        // re-sent its winner to every representative without a vote for it
        // at every interval, most of all to the replica furthest behind,
        // and the repeats used up the rate limit the first broadcasts need
        if let Some((last_broadcast, repeats)) = self.last_broadcasts.get(block_hash) {
            let wait = self
                .broadcast_interval
                .saturating_mul(1 << (*repeats).min(Self::MAX_DOUBLINGS))
                .min(Self::LONGEST_REPEAT.max(self.broadcast_interval));
            last_broadcast.elapsed(now) >= wait
        } else {
            true
        }
    }
}

impl Default for BroadcastTracker {
    fn default() -> Self {
        Self::new(NetworkType::NanoLiveNetwork, Duration::ZERO)
    }
}

impl StatsSource for BroadcastTracker {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "election",
            "broadcast_block_initial",
            self.broadcast_initial,
        );
        result.insert("election", "broadcast_block_repeat", self.broadcast_repeat);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_winner_is_broadcast_at_once_the_first_time() {
        let tracker = BroadcastTracker::new(NetworkType::NanoDevNetwork, Duration::ZERO);
        assert!(tracker.should_broadcast(Timestamp::new_test_instance(), &BlockHash::from(1)));
    }

    /// Each repeat of the same winner waits twice as long as the one before
    #[test]
    fn repeated_broadcasts_of_a_winner_back_off() {
        let mut tracker = BroadcastTracker::new(NetworkType::NanoDevNetwork, Duration::ZERO);
        let hash = BlockHash::from(1);
        let start = Timestamp::new_test_instance();
        tracker.insert(start, hash);
        assert!(!tracker.should_broadcast(start + Duration::from_millis(499), &hash));
        let first_repeat = start + Duration::from_millis(500);
        assert!(tracker.should_broadcast(first_repeat, &hash));
        tracker.insert(first_repeat, hash);
        assert!(!tracker.should_broadcast(first_repeat + Duration::from_millis(999), &hash));
        assert!(tracker.should_broadcast(first_repeat + Duration::from_secs(1), &hash));
    }

    #[test]
    fn the_interval_is_at_least_the_minimum() {
        let mut tracker =
            BroadcastTracker::new(NetworkType::NanoDevNetwork, Duration::from_millis(700));
        let hash = BlockHash::from(1);
        let start = Timestamp::new_test_instance();
        tracker.insert(start, hash);
        assert!(!tracker.should_broadcast(start + Duration::from_millis(699), &hash));
        assert!(tracker.should_broadcast(start + Duration::from_millis(700), &hash));
    }
}
