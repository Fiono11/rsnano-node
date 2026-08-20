mod builder;
mod quorum;
mod registry;
mod snapshot;

pub use builder::RepresentativeTrackerBuilder;
pub use quorum::ONLINE_WEIGHT_QUORUM;
pub use registry::RegisteredRep;
pub use snapshot::{RegisteredRepSnapshot, RepRegistrySnapshot, RepRegistrySnapshotStub};

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tracing::{debug, info, warn};

use rsnano_ledger::{RepWeightCache, RepWeights};
use rsnano_network::{ChannelEvent, ChannelId};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{
    Account, Amount, NetworkType, PublicKey, VoteError,
    currency_constants::DEFAULT_ONLINE_WEIGHT_MINIMUM,
};
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use crate::consensus::{AecFact, aggregate_vote_results};
use crate::representatives::tracker::{quorum::calculate_quorum, registry::RegisterResult};
use registry::RepresentativeRegistry;

/// Keeps track of all representatives that are online
/// and all representatives to which we have a direct connection
pub struct RepresentativeTracker {
    clock: SteadyClock,
    rep_weights: Arc<RepWeightCache>,
    state: Mutex<RepresentativeTrackerState>,
    trim_counter: AtomicU64,
    representative_weight_minimum: Amount,
}

impl RepresentativeTracker {
    pub const fn default_interval_for(network: NetworkType) -> Duration {
        match network {
            NetworkType::NanoDevNetwork => Duration::from_secs(1),
            _ => Duration::from_secs(10),
        }
    }

    pub fn new(
        rep_weights: Arc<RepWeightCache>,
        online_weight_minimum: Amount,
        representative_weight_minimum: Amount,
    ) -> Self {
        Self::new_impl(
            SteadyClock::default(),
            rep_weights,
            online_weight_minimum,
            representative_weight_minimum,
        )
    }

    pub fn new_null() -> Self {
        let rep = PublicKey::from(1);

        let rep_weights = Arc::new(RepWeightCache::default());
        rep_weights.put(rep, Amount::nano(80_000_000));

        let clock = SteadyClock::new_null();
        let min_online = DEFAULT_ONLINE_WEIGHT_MINIMUM;
        let min_rep_weight = Amount::nano(1000);
        let tracker = Self::new_impl(clock, rep_weights, min_online, min_rep_weight);
        let channel = ChannelId::from(42);
        tracker.set_channel(rep, channel);
        tracker
    }

    pub fn new_null_with_peered_weight(peered_weight: Amount) -> Self {
        let rep = PublicKey::from(1);

        let rep_weights = Arc::new(RepWeightCache::default());
        rep_weights.put(rep, peered_weight);

        let clock = SteadyClock::new_null();
        let min_online = DEFAULT_ONLINE_WEIGHT_MINIMUM;
        let min_rep_weight = Amount::nano(1000);
        let tracker = Self::new_impl(clock, rep_weights, min_online, min_rep_weight);
        let channel = ChannelId::from(42);
        tracker.set_channel(rep, channel);
        tracker
    }

    fn new_impl(
        clock: SteadyClock,
        rep_weights: Arc<RepWeightCache>,
        online_weight_minimum: Amount,
        representative_weight_minimum: Amount,
    ) -> Self {
        Self {
            clock,
            rep_weights,
            state: Mutex::new(RepresentativeTrackerState::new(online_weight_minimum)),
            trim_counter: AtomicU64::new(0),
            representative_weight_minimum,
        }
    }

    pub fn builder() -> RepresentativeTrackerBuilder {
        RepresentativeTrackerBuilder::new()
    }

    pub fn set_trended(&self, trended: Amount) {
        let weights = self.rep_weights.read();
        let mut state = self.state.lock().unwrap();
        state.trended_weight = trended;
        recalculate(&weights, &mut state);
    }

    /// Total number of peered representatives
    pub fn peered_reps_count(&self) -> usize {
        self.state.lock().unwrap().registry.peered_count()
    }

    /// Total number of online representatives
    pub fn online_reps_count(&self) -> usize {
        self.state.lock().unwrap().registry.len()
    }

    pub fn quorum_snapshot(&self) -> QuorumSnapshot {
        self.state.lock().unwrap().quorum_snapshot.clone()
    }

    pub fn with_snapshot<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&RepRegistrySnapshot) -> T,
    {
        let weights = self.rep_weights.read();
        let state = self.state.lock().unwrap();
        let snapshot = RepRegistrySnapshot::new(&state.registry, &weights, &state.quorum_snapshot);
        f(&snapshot)
    }

    /// Request a list of the top \p count known representatives in descending order of weight, with at least \p weight_a voting weight, and optionally with a minimum version \p minimum_protocol_version
    pub fn peered_reps(&self) -> Vec<PeeredRepInfo> {
        self.peered_representatives_filter(Amount::ZERO)
    }

    pub fn is_rep(&self, channel_id: ChannelId) -> bool {
        self.state
            .lock()
            .unwrap()
            .registry
            .contains_channel(channel_id)
    }

    /// Request a list of the top known principal representatives in descending order of weight
    pub fn peered_principal_reps(&self) -> Vec<PeeredRepInfo> {
        let min_weight = self.quorum_snapshot().minimum_principal_weight;
        self.peered_representatives_filter(min_weight)
    }

    /// Request a list of known representatives in descending order
    /// of weight, with at least **weight** voting weight
    fn peered_representatives_filter(&self, min_weight: Amount) -> Vec<PeeredRepInfo> {
        let mut result: Vec<PeeredRepInfo> = {
            let rep_weights = self.rep_weights.read();
            self.state
                .lock()
                .unwrap()
                .registry
                .iter()
                .filter_map(|rep| {
                    rep.channel_id.and_then(|id| {
                        let weight = rep_weights
                            .get(&rep.public_key)
                            .cloned()
                            .unwrap_or_default();

                        if weight > min_weight {
                            Some(PeeredRepInfo {
                                rep_key: rep.public_key,
                                channel_id: id,
                                weight,
                            })
                        } else {
                            None
                        }
                    })
                })
                .collect()
        };

        result.sort_by(|a, b| b.weight.cmp(&a.weight));
        result
    }

    /// Mark a representative as online, without associating it with a channel.
    pub fn vote_observed(&self, rep: PublicKey) {
        self.observe(rep, None);
    }

    /// Mark a representative as online and associate it with the channel it was directly observed on.
    pub fn set_channel(&self, rep: PublicKey, channel_id: ChannelId) {
        self.observe(rep, Some(channel_id));
    }

    fn observe(&self, rep: PublicKey, channel_id: Option<ChannelId>) {
        let result;
        {
            let now = self.clock.now();
            let weights = self.rep_weights.read();
            let weight = weights.weight(&rep);
            let mut state = self.state.lock().unwrap();
            if weight < self.representative_weight_minimum {
                return;
            }

            result = state.registry.register(rep, channel_id, now);

            recalculate(&weights, &mut state);
        }

        match result {
            RegisterResult::Inserted => {
                info!(
                    "Found representative: {}",
                    rep.as_account().encode_account(),
                );
            }
            RegisterResult::ChannelChanged(channel_id) => {
                warn!(
                    %channel_id,
                    "Representative channel changed: {}",
                    rep.as_account().encode_account(),
                )
            }
            RegisterResult::Updated => {}
        }
    }

    pub fn trim(&self) {
        self.trim_counter.fetch_add(1, Ordering::Relaxed);

        let now = self.clock.now();
        let trimmed;
        {
            let weights = self.rep_weights.read();
            let mut state = self.state.lock().unwrap();
            trimmed = state
                .registry
                .trim(now.checked_sub(Duration::from_mins(10)).unwrap_or_default());

            recalculate(&weights, &mut state);
        }

        for (rep_key, time) in &trimmed {
            debug!(
                "Removing representative: {}, last observed {}s ago",
                rep_key.as_account().encode_account(),
                time.elapsed(now).as_secs()
            );
        }
    }

    pub fn remove_peer(&self, channel_id: ChannelId) -> Vec<PublicKey> {
        let weights = self.rep_weights.read();
        let mut state = self.state.lock().unwrap();
        let removed = state.registry.disconnected(channel_id);
        recalculate(&weights, &mut state);
        removed
    }

    #[cfg(feature = "ledger_snapshots")]
    pub(crate) fn get_consensus_params(&self) -> ConsensusParams {
        let rep_weights = self.rep_weights.read().clone();
        let quorum_weight = self.quorum_snapshot().quorum_delta;
        ConsensusParams {
            quorum_weight,
            rep_weights,
        }
    }
}

fn recalculate(weights: &RepWeights, state: &mut RepresentativeTrackerState) {
    let trended = state.trended_weight;
    let quorum = calculate_quorum(
        &state.registry,
        trended,
        state.online_weight_minimum,
        &weights,
    );
    state.quorum_snapshot = quorum;
}

impl Default for RepresentativeTracker {
    fn default() -> Self {
        Self::builder().finish()
    }
}

impl ContainerInfoProvider for RepresentativeTracker {
    fn container_info(&self) -> ContainerInfo {
        [("reps", self.state.lock().unwrap().registry.len(), 0)].into()
    }
}

impl StatsSource for RepresentativeTracker {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "online_reps",
            "rep_trim",
            self.trim_counter.load(Ordering::Relaxed),
        );
    }
}

impl EventHandler<ChannelEvent> for RepresentativeTracker {
    fn handle(&self, event: &ChannelEvent) {
        if let ChannelEvent::Removed(id) = event {
            let removed_reps = self.remove_peer(*id);
            for rep in removed_reps {
                info!(
                    "Evicting representative {} with dead channel",
                    Account::from(rep).encode_account(),
                );
            }
        }
    }
}

impl EventHandler<AecFact> for RepresentativeTracker {
    fn handle(&self, event: &AecFact) {
        if let AecFact::VoteProcessed(vote, _weight, results) = event {
            // Representative is defined as online if replying to live votes
            let result = aggregate_vote_results(results);
            let should_observe = matches!(
                result,
                Ok(()) | Err(VoteError::Replay) | Err(VoteError::Ignored)
            );
            if should_observe {
                self.vote_observed(vote.voter);
            }
        }
    }
}

struct RepresentativeTrackerState {
    trended_weight: Amount,
    online_weight_minimum: Amount,
    quorum_snapshot: QuorumSnapshot,
    registry: RepresentativeRegistry,
}

impl RepresentativeTrackerState {
    pub fn new(online_weight_minimum: Amount) -> Self {
        Self {
            trended_weight: Amount::ZERO,
            registry: RepresentativeRegistry::new(),
            online_weight_minimum,
            quorum_snapshot: QuorumSnapshot {
                trended_or_min_weight: online_weight_minimum,
                quorum_delta: online_weight_minimum,
                peered_weight: Amount::ZERO,
                online_weight: Amount::ZERO,
                online_weight_minimum,
                quorum_percent: ONLINE_WEIGHT_QUORUM,
                minimum_principal_weight: online_weight_minimum / 1000,
                #[cfg(feature = "rai_protocol")]
                total_weight: online_weight_minimum,
                #[cfg(feature = "rai_protocol")]
                faulty_weight: quorum::rai_fault_slack_budget(online_weight_minimum),
                #[cfg(feature = "rai_protocol")]
                slack_weight: quorum::rai_fault_slack_budget(online_weight_minimum),
            },
        }
    }
}

#[derive(Clone)]
pub struct PeeredRepInfo {
    pub rep_key: PublicKey,
    pub channel_id: ChannelId,
    pub weight: Amount,
}

#[derive(Clone, Default)]
pub struct QuorumSnapshot {
    pub trended_or_min_weight: Amount,
    /// The quorum required for confirmation
    pub quorum_delta: Amount,
    pub peered_weight: Amount,
    pub online_weight: Amount,
    pub online_weight_minimum: Amount,
    pub quorum_percent: u8,
    pub minimum_principal_weight: Amount,
    #[cfg(feature = "rai_protocol")]
    pub total_weight: Amount,
    #[cfg(feature = "rai_protocol")]
    pub faulty_weight: Amount,
    #[cfg(feature = "rai_protocol")]
    pub slack_weight: Amount,
}

impl QuorumSnapshot {
    pub fn new_test_instance() -> Self {
        Self {
            trended_or_min_weight: Amount::nano(100_000_000),
            quorum_delta: Amount::nano(67_000_000),
            peered_weight: Amount::nano(90_000_000),
            online_weight: Amount::nano(99_000_000),
            online_weight_minimum: Amount::nano(60_000_000),
            quorum_percent: ONLINE_WEIGHT_QUORUM,
            minimum_principal_weight: Amount::nano(100_000),
            #[cfg(feature = "rai_protocol")]
            total_weight: Amount::nano(100_000_000),
            #[cfg(feature = "rai_protocol")]
            faulty_weight: quorum::rai_fault_slack_budget(Amount::nano(100_000_000)),
            #[cfg(feature = "rai_protocol")]
            slack_weight: quorum::rai_fault_slack_budget(Amount::nano(100_000_000)),
        }
    }

    /// Calculates minimum time delay between subsequent votes when processing non-final votes
    pub fn cooldown_time(&self, rep_weight: Amount) -> Duration {
        if rep_weight > self.trended_or_min_weight / 20 {
            // Reps with more than 5% weight
            Duration::from_secs(1)
        } else if rep_weight > self.trended_or_min_weight / 100 {
            // Reps with more than 1% weight
            Duration::from_secs(5)
        } else {
            // The rest of smaller reps
            Duration::from_secs(15)
        }
    }
}

#[cfg(feature = "ledger_snapshots")]
pub(crate) struct ConsensusParams {
    pub(crate) rep_weights: rsnano_ledger::RepWeights,
    pub(crate) quorum_weight: Amount,
}

#[cfg(feature = "ledger_snapshots")]
impl Default for ConsensusParams {
    fn default() -> Self {
        Self {
            rep_weights: Default::default(),
            quorum_weight: Amount::MAX,
        }
    }
}

#[cfg(feature = "ledger_snapshots")]
impl ConsensusParams {
    #[cfg(test)]
    pub(crate) fn set_rep_weights(
        &mut self,
        rep_weights: rsnano_ledger::RepWeights,
        quorum_weight: Amount,
    ) {
        self.rep_weights = rep_weights;
        self.quorum_weight = quorum_weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty() {
        let tracker = make_tracker();

        let specs = tracker.quorum_snapshot();

        assert_eq!(specs.online_weight_minimum, TEST_MIN_ONLINE_WEIGHT);
        assert_eq!(
            specs.trended_or_min_weight, TEST_MIN_ONLINE_WEIGHT,
            "trended"
        );
        assert_eq!(specs.online_weight, Amount::ZERO, "online");
        assert_eq!(specs.peered_weight, Amount::ZERO, "peered");
        assert_eq!(tracker.peered_reps_count(), 0, "peered count");
        assert_eq!(specs.quorum_percent, 67, "quorum percent");
        assert_eq!(specs.quorum_delta, TEST_MIN_ONLINE_WEIGHT, "quorum delta");
        assert_eq!(
            specs.minimum_principal_weight,
            TEST_MIN_ONLINE_WEIGHT / 1000
        );
    }

    #[test]
    fn observe_vote() {
        let rep = PublicKey::from(1);
        let weight = Amount::nano(100_000);
        let tracker = make_tracker_with_weights([(rep, weight)]);

        tracker.vote_observed(rep);

        let specs = tracker.quorum_snapshot();
        assert_eq!(specs.online_weight, weight, "online");
        assert_eq!(specs.peered_weight, Amount::ZERO, "peered");
    }

    #[test]
    fn observe_direct_vote() {
        let rep = PublicKey::from(1);
        let weight = Amount::nano(100_000);
        let tracker = make_tracker_with_weights([(rep, weight)]);

        let channel = ChannelId::from(42);
        tracker.set_channel(rep, channel);
        let specs = tracker.quorum_snapshot();

        assert_eq!(specs.online_weight, weight, "online");
        assert_eq!(specs.peered_weight, weight, "peered");
    }

    #[test]
    fn trended_weight() {
        let tracker = make_tracker();
        tracker.set_trended(Amount::nano(10_000));
        let specs = tracker.quorum_snapshot();
        assert_eq!(specs.trended_or_min_weight, Amount::nano(60_000_000));

        tracker.set_trended(Amount::nano(100_000_000));
        let specs = tracker.quorum_snapshot();
        assert_eq!(specs.trended_or_min_weight, Amount::nano(100_000_000));
    }

    #[test]
    fn minimum_principal_weight() {
        let tracker = make_tracker();
        assert_eq!(
            tracker.quorum_snapshot().minimum_principal_weight,
            Amount::nano(60_000)
        );

        tracker.set_trended(Amount::nano(110_000_000));
        // 0.1% of trended weight
        assert_eq!(
            tracker.quorum_snapshot().minimum_principal_weight,
            Amount::nano(110_000)
        );
    }

    #[test]
    fn quorum_delta() {
        let rep = PublicKey::from(42);
        let weight = Amount::nano(100_000_000);
        let tracker = make_tracker_with_weights([(rep, weight)]);

        tracker.vote_observed(rep);

        assert_eq!(
            tracker.quorum_snapshot().quorum_delta,
            Amount::nano(67_000_000)
        );
    }

    #[test]
    fn discard_old_votes() {
        let rep_a = PublicKey::from(1);
        let rep_b = PublicKey::from(2);
        let rep_c = PublicKey::from(3);
        let tracker = make_tracker_with_weights([
            (rep_a, Amount::nano(100_000)),
            (rep_b, Amount::nano(200_000)),
            (rep_c, Amount::nano(400_000)),
        ]);

        tracker.vote_observed(rep_a);
        tracker.clock.advance(Duration::from_secs(10));
        tracker.vote_observed(rep_b);
        tracker.clock.advance(Duration::from_secs(59 * 10 + 1));
        tracker.vote_observed(rep_c);

        tracker.trim();

        assert_eq!(
            tracker.quorum_snapshot().online_weight,
            Amount::nano(600_000)
        );
    }

    #[cfg(feature = "ledger_snapshots")]
    #[test]
    fn default_quorum_weight_is_max() {
        let params = ConsensusParams::default();
        assert_eq!(params.quorum_weight, Amount::MAX);
    }

    /*
     * Nullability
     */

    #[test]
    fn can_be_nulled() {
        let tracker = RepresentativeTracker::new_null();
        let snap = tracker.quorum_snapshot();
        assert_ne!(snap.quorum_delta, Amount::ZERO, "quorum delta");
        assert_ne!(snap.quorum_percent, 0, "quorum percent");
        assert_ne!(snap.online_weight_minimum, Amount::ZERO, "online minimum");
        assert_ne!(snap.online_weight, Amount::ZERO, "online weight");
        assert_ne!(
            snap.trended_or_min_weight,
            Amount::ZERO,
            "trended or minimum"
        );
        assert_eq!(snap.peered_weight, Amount::nano(80_000_000), "peered");
    }

    #[test]
    fn can_be_nulled_with_configurable_peered_weight() {
        let weight = Amount::nano(99_000_000);
        let tracker = RepresentativeTracker::new_null_with_peered_weight(weight);
        assert_eq!(tracker.quorum_snapshot().peered_weight, weight);
        assert_eq!(tracker.quorum_snapshot().online_weight, weight);
    }

    /*
     * Test helpers
     */

    const TEST_MIN_ONLINE_WEIGHT: Amount = Amount::nano(60_000_000);
    const TEST_MIN_REP_WEIGHT: Amount = Amount::nano(10);

    fn make_tracker() -> RepresentativeTracker {
        make_tracker_with_weights([])
    }

    fn make_tracker_with_weights(
        weights: impl IntoIterator<Item = (PublicKey, Amount)>,
    ) -> RepresentativeTracker {
        let clock = SteadyClock::new_null();
        let weight_cache = Arc::new(RepWeightCache::default());
        for (rep, weight) in weights {
            weight_cache.put(rep, weight);
        }
        RepresentativeTracker::new_impl(
            clock,
            weight_cache,
            TEST_MIN_ONLINE_WEIGHT,
            TEST_MIN_REP_WEIGHT,
        )
    }
}
