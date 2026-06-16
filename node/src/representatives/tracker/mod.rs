mod builder;
mod cleanup;
mod online_container;
mod peered_container;
mod peered_rep;

pub use builder::RepresentativeTrackerBuilder;
pub use cleanup::OnlineRepsCleanup;
pub use peered_container::InsertResult;
pub use peered_rep::PeeredRep;

use std::{
    cmp::max,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use primitive_types::U256;
use tracing::debug;

use rsnano_ledger::{RepWeightCache, RepWeights};
use rsnano_network::{Channel, ChannelId};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Amount, NetworkType, PublicKey};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use {online_container::OnlineContainer, peered_container::PeeredContainer};

pub const ONLINE_WEIGHT_QUORUM: u8 = 67;

/// Keeps track of all representatives that are online
/// and all representatives to which we have a direct connection
pub struct RepresentativeTracker {
    rep_weights: Arc<RepWeightCache>,
    state: Mutex<RepresentativeTrackerState>,
    trim_counter: AtomicU64,
}

impl RepresentativeTracker {
    pub const DEFAULT_ONLINE_WEIGHT_MINIMUM: Amount = Amount::nano(60_000_000);

    pub const fn default_interval_for(network: NetworkType) -> Duration {
        match network {
            NetworkType::NanoDevNetwork => Duration::from_secs(1),
            _ => Duration::from_secs(10),
        }
    }

    pub(crate) fn new(
        rep_weights: Arc<RepWeightCache>,
        online_weight_minimum: Amount,
        representative_weight_minimum: Amount,
    ) -> Self {
        Self {
            rep_weights,
            state: Mutex::new(RepresentativeTrackerState::new(
                online_weight_minimum,
                representative_weight_minimum,
            )),
            trim_counter: AtomicU64::new(0),
        }
    }

    pub fn new_test_instance() -> Self {
        let rep = PublicKey::from(1);

        let rep_weights = Arc::new(RepWeightCache::default());
        rep_weights.put(rep, Amount::nano(80_000_000));

        let tracker = Self::new(rep_weights, Amount::nano(60_000_000), Amount::nano(1000));
        let channel = Arc::new(Channel::new_test_instance());
        tracker.vote_observed_directly(rep, channel, Timestamp::new_test_instance());
        tracker
    }

    pub fn builder() -> RepresentativeTrackerBuilder {
        RepresentativeTrackerBuilder::new()
    }

    pub fn set_trended(&self, trended: Amount) {
        let weights = self.rep_weights.read();
        let mut state = self.state.lock().unwrap();
        state.trended_weight = trended;
        state.calculate(&weights);
    }

    /// Query if a peer manages a principle representative
    pub fn is_principal_rep(&self, channel_id: ChannelId) -> bool {
        let rep_weights = self.rep_weights.read();
        let min_weight = self.quorum_specs().minimum_principal_weight;
        self.state
            .lock()
            .unwrap()
            .peered_reps
            .accounts_by_channel(channel_id)
            .any(|account| rep_weights.get(account).cloned().unwrap_or_default() >= min_weight)
    }

    /// Total number of peered representatives
    pub fn peered_reps_count(&self) -> usize {
        self.state.lock().unwrap().peered_reps.len()
    }

    pub fn quorum_specs(&self) -> QuorumSpecs {
        self.state.lock().unwrap().quorum_specs.clone()
    }

    pub fn on_rep_request(&self, channel_id: ChannelId, now: Timestamp) {
        // Find and update the timestamp on all reps available on the endpoint (a single host may have multiple reps)
        self.state
            .lock()
            .unwrap()
            .peered_reps
            .modify_by_channel(channel_id, |rep| {
                rep.last_request = now;
            });
    }

    pub fn last_request_elapsed(&self, channel_id: ChannelId, now: Timestamp) -> Option<Duration> {
        self.state
            .lock()
            .unwrap()
            .peered_reps
            .iter_by_channel(channel_id)
            .next()
            .map(|rep| rep.last_request.elapsed(now))
    }

    /// List of online representatives, both the currently sampling ones and the ones observed in the previous sampling period
    pub fn online_reps(&self) -> Vec<OnlineRepInfo> {
        let weight_reader = self.rep_weights.read();
        let state = self.state.lock().unwrap();

        state
            .online_reps
            .iter()
            .map(|rep_key| OnlineRepInfo {
                rep_key: *rep_key,
                weight: weight_reader.weight(rep_key),
                is_peered: state.peered_reps.contains(rep_key),
            })
            .collect()
    }

    /// Request a list of the top \p count known representatives in descending order of weight, with at least \p weight_a voting weight, and optionally with a minimum version \p minimum_protocol_version
    pub fn peered_reps(&self) -> Vec<PeeredRepInfo> {
        self.peered_representatives_filter(Amount::ZERO)
    }

    /// Request a list of the top known principal representatives in descending order of weight
    pub fn peered_principal_reps(&self) -> Vec<PeeredRepInfo> {
        let min_weight = self.quorum_specs().minimum_principal_weight;
        self.peered_representatives_filter(min_weight)
    }

    /// Request a list of known representatives in descending order
    /// of weight, with at least **weight** voting weight
    fn peered_representatives_filter(&self, min_weight: Amount) -> Vec<PeeredRepInfo> {
        let mut reps_with_weight = Vec::new();

        {
            let rep_weights = self.rep_weights.read();
            let state = self.state.lock().unwrap();
            for rep in state.peered_reps.iter() {
                let weight = rep_weights.get(&rep.account).cloned().unwrap_or_default();
                if weight > min_weight {
                    reps_with_weight.push((rep.clone(), weight));
                }
            }
        }

        reps_with_weight.sort_by(|a, b| b.1.cmp(&a.1));

        reps_with_weight
            .drain(..)
            .map(|(rep, weight)| PeeredRepInfo {
                rep_key: rep.account,
                channel: rep.channel,
                weight,
            })
            .collect()
    }

    /// Add voting account rep_account to the set of online representatives.
    /// This can happen for directly connected or indirectly connected reps.
    /// Returns whether it is a rep which has more than min weight
    pub fn vote_observed(&self, rep_account: PublicKey, now: Timestamp) -> bool {
        let weights = self.rep_weights.read();
        let weight = weights.weight(&rep_account);
        let mut state = self.state.lock().unwrap();
        if weight < state.representative_weight_minimum {
            return false;
        }

        state.online_reps.insert(rep_account, now);
        state.calculate(&weights);

        true
    }

    pub fn trim(&self, now: Timestamp) {
        self.trim_counter.fetch_add(1, Ordering::Relaxed);
        let trimmed;
        {
            let weights = self.rep_weights.read();
            let mut state = self.state.lock().unwrap();
            trimmed = state
                .online_reps
                .trim(now.checked_sub(Duration::from_mins(10)).unwrap_or_default());
            state.calculate(&weights);
        }

        for (rep_key, time) in &trimmed {
            debug!(
                "Removing representative: {}, last observed {}s ago",
                rep_key.as_account().encode_account(),
                time.elapsed(now).as_secs()
            );
        }
    }

    pub fn recalculate(&self) {
        let weights = self.rep_weights.read();
        self.state.lock().unwrap().calculate(&weights);
    }

    /// Add rep_account to the set of peered representatives
    pub fn vote_observed_directly(
        &self,
        rep_account: PublicKey,
        channel: Arc<Channel>,
        now: Timestamp,
    ) -> InsertResult {
        let is_rep = self.vote_observed(rep_account, now);
        if is_rep {
            let weights = self.rep_weights.read();
            let mut state = self.state.lock().unwrap();
            let result = state
                .peered_reps
                .update_or_insert(rep_account, channel, now);
            // TODO: don't calculate twice here
            state.calculate(&weights);
            result
        } else {
            InsertResult::Updated
        }
    }

    pub fn remove_peer(&self, channel_id: ChannelId) -> Vec<PublicKey> {
        self.state.lock().unwrap().peered_reps.remove(channel_id)
    }

    pub fn get_rep_weights(&self) -> RepWeights {
        self.rep_weights.read().clone()
    }

    #[cfg(feature = "ledger_snapshots")]
    pub(crate) fn get_consensus_params(&self) -> ConsensusParams {
        let rep_weights = self.get_rep_weights();
        let quorum_weight = self.quorum_specs().quorum_delta;
        ConsensusParams {
            quorum_weight,
            rep_weights,
        }
    }
}

impl Default for RepresentativeTracker {
    fn default() -> Self {
        Self::builder().finish()
    }
}

impl ContainerInfoProvider for RepresentativeTracker {
    fn container_info(&self) -> ContainerInfo {
        let state = self.state.lock().unwrap();
        [
            (
                "online",
                state.online_reps.len(),
                OnlineContainer::ELEMENT_SIZE,
            ),
            (
                "peered",
                state.peered_reps.len(),
                PeeredContainer::ELEMENT_SIZE,
            ),
        ]
        .into()
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

struct RepresentativeTrackerState {
    online_reps: OnlineContainer,
    peered_reps: PeeredContainer,
    trended_weight: Amount,
    online_weight: Amount,
    online_weight_minimum: Amount,
    representative_weight_minimum: Amount,
    quorum_specs: QuorumSpecs,
}

impl RepresentativeTrackerState {
    pub fn new(online_weight_minimum: Amount, representative_weight_minimum: Amount) -> Self {
        Self {
            online_reps: OnlineContainer::new(),
            peered_reps: PeeredContainer::new(),
            trended_weight: Amount::ZERO,
            online_weight: Amount::ZERO,
            online_weight_minimum,
            representative_weight_minimum,
            quorum_specs: QuorumSpecs {
                trended_or_min_weight: online_weight_minimum,
                quorum_delta: online_weight_minimum,
                peered_weight: Amount::ZERO,
                online_weight: Amount::ZERO,
                online_weight_minimum,
                quorum_percent: ONLINE_WEIGHT_QUORUM,
                minimum_principal_weight: online_weight_minimum / 1000,
            },
        }
    }

    fn calculate(&mut self, weights: &RepWeights) {
        self.online_weight = Amount::ZERO;
        for account in self.online_reps.iter() {
            self.online_weight += weights.get(account).cloned().unwrap_or_default();
        }

        let trended_or_min_weight = max(self.trended_weight, self.online_weight_minimum);
        let mut peered_weight = Amount::ZERO;
        for account in self.peered_reps.accounts() {
            peered_weight += weights.get(account).cloned().unwrap_or_default();
        }

        let weight = max(self.online_weight, trended_or_min_weight);
        let minimum_principal_weight = trended_or_min_weight / 1000; // 0.1% of trended online weight

        // Using a larger container to ensure maximum precision
        let delta =
            U256::from(weight.number()) * U256::from(ONLINE_WEIGHT_QUORUM) / U256::from(100);
        let quorum_delta = Amount::raw(delta.as_u128());

        let quorum_specs = QuorumSpecs {
            trended_or_min_weight,
            quorum_delta,
            peered_weight,
            online_weight: self.online_weight,
            online_weight_minimum: self.online_weight_minimum,
            quorum_percent: ONLINE_WEIGHT_QUORUM,
            minimum_principal_weight,
        };

        self.quorum_specs = quorum_specs;
    }
}

#[derive(Clone)]
pub struct PeeredRepInfo {
    pub rep_key: PublicKey,
    pub channel: Arc<Channel>,
    pub weight: Amount,
}

#[derive(Clone)]
pub struct OnlineRepInfo {
    pub rep_key: PublicKey,
    pub weight: Amount,
    // Does this node have a direct connection to that rep?
    pub is_peered: bool,
}

impl PeeredRepInfo {
    pub fn channel_id(&self) -> ChannelId {
        self.channel.channel_id()
    }
}

#[derive(Clone)]
pub struct QuorumSpecs {
    pub trended_or_min_weight: Amount,
    /// The quorum required for confirmation
    pub quorum_delta: Amount,
    pub peered_weight: Amount,
    pub online_weight: Amount,
    pub online_weight_minimum: Amount,
    pub quorum_percent: u8,
    pub minimum_principal_weight: Amount,
}

impl QuorumSpecs {
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

    pub fn new_test_instance() -> Self {
        QuorumSpecs {
            trended_or_min_weight: Amount::nano(100_000_000),
            quorum_delta: Amount::nano(67_000_000),
            online_weight: Amount::nano(100_000_000),
            peered_weight: Amount::nano(90_000_000),
            online_weight_minimum: Amount::nano(60_000_000),
            quorum_percent: 67,
            minimum_principal_weight: Amount::nano(100_000),
        }
    }
}

#[cfg(feature = "ledger_snapshots")]
pub(crate) struct ConsensusParams {
    pub(crate) rep_weights: RepWeights,
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
    pub(crate) fn set_rep_weights(&mut self, rep_weights: RepWeights, quorum_weight: Amount) {
        self.rep_weights = rep_weights;
        self.quorum_weight = quorum_weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_clock::SteadyClock;
    use std::time::Duration;

    #[test]
    fn empty() {
        let tracker = RepresentativeTracker::default();
        let specs = tracker.quorum_specs();
        assert_eq!(specs.online_weight_minimum, Amount::nano(60_000_000));
        assert_eq!(
            specs.trended_or_min_weight,
            Amount::nano(60_000_000),
            "trended"
        );
        assert_eq!(specs.online_weight, Amount::ZERO, "online");
        assert_eq!(specs.peered_weight, Amount::ZERO, "peered");
        assert_eq!(tracker.peered_reps_count(), 0, "peered count");
        assert_eq!(specs.quorum_percent, 67, "quorum percent");
        assert_eq!(specs.quorum_delta, Amount::nano(60_000_000), "quorum delta");
        assert_eq!(specs.minimum_principal_weight, Amount::nano(60_000));
    }

    #[test]
    fn observe_vote() {
        let clock = SteadyClock::new_null();
        let account = PublicKey::from(1);
        let weight = Amount::nano(100_000);
        let weights = Arc::new(RepWeightCache::default());
        weights.put(account, weight);
        let tracker = RepresentativeTracker::builder()
            .rep_weights(weights)
            .finish();

        tracker.vote_observed(account, clock.now());

        let specs = tracker.quorum_specs();
        assert_eq!(specs.online_weight, weight, "online");
        assert_eq!(specs.peered_weight, Amount::ZERO, "peered");
    }

    #[test]
    fn observe_direct_vote() {
        let clock = SteadyClock::new_null();
        let account = PublicKey::from(1);
        let weight = Amount::nano(100_000);
        let weights = Arc::new(RepWeightCache::default());
        weights.put(account, weight);
        let tracker = RepresentativeTracker::builder()
            .rep_weights(weights)
            .finish();

        let channel = Arc::new(Channel::new_test_instance());
        tracker.vote_observed_directly(account, channel, clock.now());
        let specs = tracker.quorum_specs();

        assert_eq!(specs.online_weight, weight, "online");
        assert_eq!(specs.peered_weight, weight, "peered");
    }

    #[test]
    fn trended_weight() {
        let tracker = RepresentativeTracker::default();
        tracker.set_trended(Amount::nano(10_000));
        let specs = tracker.quorum_specs();
        assert_eq!(specs.trended_or_min_weight, Amount::nano(60_000_000));

        tracker.set_trended(Amount::nano(100_000_000));
        let specs = tracker.quorum_specs();
        assert_eq!(specs.trended_or_min_weight, Amount::nano(100_000_000));
    }

    #[test]
    fn minimum_principal_weight() {
        let tracker = RepresentativeTracker::default();
        assert_eq!(
            tracker.quorum_specs().minimum_principal_weight,
            Amount::nano(60_000)
        );

        tracker.set_trended(Amount::nano(110_000_000));
        // 0.1% of trended weight
        assert_eq!(
            tracker.quorum_specs().minimum_principal_weight,
            Amount::nano(110_000)
        );
    }

    #[test]
    fn is_pr() {
        let clock = SteadyClock::new_null();
        let weights = Arc::new(RepWeightCache::default());
        let tracker = RepresentativeTracker::builder()
            .rep_weights(weights.clone())
            .finish();
        let rep_account = PublicKey::from(42);
        let channel = Arc::new(Channel::new_test_instance());
        let channel_id = channel.channel_id();
        weights.put(rep_account, Amount::nano(50_000));

        // unknown channel
        assert_eq!(tracker.is_principal_rep(channel_id), false);

        // below PR limit
        tracker.vote_observed_directly(rep_account, channel, clock.now());
        assert_eq!(tracker.is_principal_rep(channel_id), false);

        // above PR limit
        weights.put(rep_account, Amount::nano(100_000));
        assert_eq!(tracker.is_principal_rep(channel_id), true);
    }

    #[test]
    fn quorum_delta() {
        let weights = Arc::new(RepWeightCache::default());
        let tracker = RepresentativeTracker::builder()
            .rep_weights(weights.clone())
            .finish();

        let rep_account = PublicKey::from(42);
        weights.put(rep_account, Amount::nano(100_000_000));
        tracker.vote_observed(rep_account, Timestamp::new_test_instance());

        assert_eq!(
            tracker.quorum_specs().quorum_delta,
            Amount::nano(67_000_000)
        );
    }

    #[test]
    fn discard_old_votes() {
        let rep_a = PublicKey::from(1);
        let rep_b = PublicKey::from(2);
        let rep_c = PublicKey::from(3);
        let weights = Arc::new(RepWeightCache::default());
        weights.put(rep_a, Amount::nano(100_000));
        weights.put(rep_b, Amount::nano(200_000));
        weights.put(rep_c, Amount::nano(400_000));
        let tracker = RepresentativeTracker::builder()
            .rep_weights(weights)
            .finish();

        let start = SteadyClock::new_null().now();
        tracker.vote_observed(rep_a, start);
        tracker.vote_observed(rep_b, start + Duration::from_secs(10));
        tracker.vote_observed(rep_c, start + Duration::from_secs(60 * 10 + 1));

        tracker.trim(start + Duration::from_secs(60 * 10 + 1));

        assert_eq!(tracker.quorum_specs().online_weight, Amount::nano(600_000));
    }

    #[test]
    fn test_instance() {
        let tracker = RepresentativeTracker::new_test_instance();
        let specs = tracker.quorum_specs();
        assert_ne!(specs.quorum_delta, Amount::ZERO, "quorum delta");
        assert_ne!(specs.quorum_percent, 0, "quorum percent");
        assert_ne!(specs.online_weight_minimum, Amount::ZERO, "online minimum");
        assert_ne!(specs.online_weight, Amount::ZERO, "online weight");
        assert_ne!(
            specs.trended_or_min_weight,
            Amount::ZERO,
            "trended or minimum"
        );
        assert_ne!(specs.peered_weight, Amount::ZERO, "peered");
    }

    #[cfg(feature = "ledger_snapshots")]
    #[test]
    fn default_quorum_weight_is_max() {
        let params = ConsensusParams::default();
        assert_eq!(params.quorum_weight, Amount::MAX);
    }
}
