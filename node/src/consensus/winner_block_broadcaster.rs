use std::{
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
}

impl WinnerBlockBroadcaster {
    pub(crate) fn new(
        clock: Arc<SteadyClock>,
        networks: NetworkType,
        message_flooder: MessageFlooder,
        _rep_tracker: Arc<RepresentativeTracker>,
        _network: Arc<RwLock<Network>>,
    ) -> Self {
        Self {
            clock,
            broadcast_tracker: BroadcastTracker::new(networks),
            message_flooder,
            // TODO: Make rate limit configurable
            rebroadcast_limiter: TokenBucket::with_burst_ratio(100, 2.0),
            broadcast_listener: OutputListenerMt::default(),
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
        )
    }

    #[allow(dead_code)]
    pub fn track(&self) -> Arc<OutputTrackerMt<BlockHash>> {
        self.broadcast_listener.track()
    }

    pub fn try_broadcast_winner(
        &mut self,
        winner_block: &Block,
        _votes: &HashMap<PublicKey, VoteSummary>,
    ) {
        let now = self.clock.now();
        let winner_hash = winner_block.hash();
        self.broadcast_listener.emit(winner_hash);

        if !self.broadcast_tracker.should_broadcast(now, &winner_hash) {
            return;
        }

        if !self.rebroadcast_limiter.try_consume(1) {
            return;
        }

        let winner_msg = Message::Publish(Publish::new_forward(winner_block.clone()));

        self.message_flooder.flood_prs_and_some_non_prs(
            &winner_msg,
            TrafficType::BlockBroadcast,
            0.5,
        );

        self.broadcast_tracker.insert(now, winner_hash);
    }
}

impl StatsSource for WinnerBlockBroadcaster {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.broadcast_tracker.collect_stats(result);
    }
}

struct BroadcastTracker {
    last_broadcasts: BoundedHashMap<BlockHash, Timestamp>,
    broadcast_interval: Duration,
    broadcast_initial: u64,
    broadcast_repeat: u64,
}

impl BroadcastTracker {
    pub fn new(network: NetworkType) -> Self {
        Self {
            last_broadcasts: BoundedHashMap::new(1024 * 32),
            broadcast_interval: match network {
                NetworkType::NanoDevNetwork => Duration::from_millis(500),
                _ => Duration::from_secs(150),
            },
            broadcast_initial: 0,
            broadcast_repeat: 0,
        }
    }

    pub fn insert(&mut self, now: Timestamp, hash: BlockHash) -> bool {
        let is_initial = self.last_broadcasts.insert(hash, now).is_none();

        if is_initial {
            self.broadcast_initial += 1;
        } else {
            self.broadcast_repeat += 1;
        }

        is_initial
    }

    pub fn should_broadcast(&self, now: Timestamp, block_hash: &BlockHash) -> bool {
        // Broadcast the block if enough time has passed since the last broadcast (or it's the first broadcast)
        if let Some(last_broadcast) = self.last_broadcasts.get(block_hash) {
            last_broadcast.elapsed(now) >= self.broadcast_interval
        } else {
            true
        }
    }
}

impl Default for BroadcastTracker {
    fn default() -> Self {
        Self::new(NetworkType::NanoLiveNetwork)
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
