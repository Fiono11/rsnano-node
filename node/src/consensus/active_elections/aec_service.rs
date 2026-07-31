use std::{collections::HashMap, sync::RwLock, time::Duration};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Account, Amount, Block, BlockHash, PublicKey, QualifiedRoot, SavedBlock, VoteError,
};
#[cfg(feature = "rai_protocol")]
use rsnano_utils::{CancellationToken, ticker::Tickable};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};

#[cfg(feature = "rai_protocol")]
use super::RaiCloseElectionSpec;
use super::{
    ActiveElectionsConfig, ActiveElectionsContainer, ActiveElectionsInfo, AecCooldownReason,
    AecFact, AecInsertError, AecInsertRequest, ApplyVoteArgs,
};
use crate::consensus::{
    ElectionCandidateSource,
    election::{ConfirmedElection, Election, ElectionBehavior, ElectionState},
};

pub struct AecService {
    aec: RwLock<ActiveElectionsContainer>,
    clock: SteadyClock,
}

#[cfg(feature = "rai_protocol")]
pub struct RaiEpochTicker {
    aec: std::sync::Arc<AecService>,
    clock: std::sync::Arc<SteadyClock>,
    wallet_reps: std::sync::Arc<std::sync::Mutex<crate::wallets::WalletRepresentatives>>,
    ledger: std::sync::Arc<rsnano_ledger::Ledger>,
    epoch_duration: Duration,
    flooder: crate::transport::MessageFlooder,
    reports: Vec<crate::consensus::rai::RaiReport>,
}

#[cfg(feature = "rai_protocol")]
impl RaiEpochTicker {
    pub fn new(
        aec: std::sync::Arc<AecService>,
        clock: std::sync::Arc<SteadyClock>,
        wallet_reps: std::sync::Arc<std::sync::Mutex<crate::wallets::WalletRepresentatives>>,
        ledger: std::sync::Arc<rsnano_ledger::Ledger>,
        epoch_duration: Duration,
        flooder: crate::transport::MessageFlooder,
    ) -> Self {
        Self {
            aec,
            clock,
            wallet_reps,
            ledger,
            epoch_duration,
            flooder,
            reports: Vec::new(),
        }
    }
}

#[cfg(feature = "rai_protocol")]
impl Tickable for RaiEpochTicker {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        let Some(local_key) = keys.first() else {
            // Reports are committee votes and must be signed by a voting
            // representative. A node-id signature has no committee weight.
            return;
        };
        let now = self.clock.now();
        self.reports
            .extend(self.aec.rai_tick(now, local_key, self.epoch_duration));
        if let Some(closing) = self.aec.rai_epoch_status().0.closing {
            self.aec
                .rai_progress_close(self.ledger.rai_confirmation_frontiers(closing.epoch), now);
        }
        let closing = self
            .aec
            .rai_epoch_status()
            .0
            .closing
            .map(|state| state.epoch);
        self.reports.retain(|report| Some(report.epoch) == closing);

        // Reports are quorum material, not best-effort announcements. Repeat
        // them throughout the close and target all known PR peers so a missed
        // first flood cannot leave part of the network in CollectingReports.
        for report in &self.reports {
            self.flooder.flood_prs_and_some_non_prs(
                &rsnano_messages::Message::RaiReport(report.clone().into()),
                rsnano_network::TrafficType::Generic,
                2.0,
            );
        }
    }
}

impl AecService {
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::new(config, base_latency)),
            clock: SteadyClock::default(),
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_with_rai_committee(
        config: ActiveElectionsConfig,
        base_latency: Duration,
        genesis_committee: std::sync::Arc<rsnano_ledger::RepWeights>,
        genesis_governing_hash: BlockHash,
    ) -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::new_with_rai_committee(
                config,
                base_latency,
                genesis_committee,
                genesis_governing_hash,
            )),
            clock: SteadyClock::default(),
        }
    }

    pub fn new_null() -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::default()),
            clock: SteadyClock::new_null(),
        }
    }

    // --- Read forwarding ---

    pub fn check_vacancy<T>(&self, source: &T) -> bool
    where
        T: ElectionCandidateSource,
    {
        self.aec.read().unwrap().check_vacancy(source)
    }

    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<Election> {
        self.aec.read().unwrap().election_for_root(root).cloned()
    }

    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<Election> {
        self.aec
            .read()
            .unwrap()
            .election_for_block(block_hash)
            .cloned()
    }

    pub fn max_len(&self) -> usize {
        self.aec.read().unwrap().max_len()
    }

    pub fn len(&self) -> usize {
        self.aec.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.aec.read().unwrap().is_empty()
    }

    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.aec.read().unwrap().is_active_root(root)
    }

    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.aec.read().unwrap().is_active_hash(block_hash)
    }

    pub fn was_recently_confirmed(&self, block_hash: &BlockHash) -> bool {
        self.aec.read().unwrap().was_recently_confirmed(block_hash)
    }

    pub fn count_by_behavior(&self, behavior: ElectionBehavior) -> usize {
        self.aec.read().unwrap().count_by_behavior(behavior)
    }

    pub fn vacancy(&self) -> i64 {
        self.aec.read().unwrap().vacancy()
    }

    pub fn info(&self) -> ActiveElectionsInfo {
        let now = self.clock.now();
        self.aec.read().unwrap().info(now)
    }

    pub fn round_robin<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&mut dyn Iterator<Item = &Election>) -> T,
    {
        let guard = self.aec.read().unwrap();
        f(&mut guard.iter_round_robin())
    }

    // --- Write forwarding ---

    pub fn set_observer(&self, observer: Sender<AecFact>) {
        self.aec.write().unwrap().set_observer(observer)
    }

    pub fn insert(&self, request: AecInsertRequest, now: Timestamp) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert(request, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_cut(
        &self,
        spec: RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert_close_cut(spec, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_record(
        &self,
        spec: RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert_close_record(spec, now)
    }

    pub fn try_add_fork(&self, fork: &Block, fork_tally: Amount) -> bool {
        self.aec.write().unwrap().try_add_fork(fork, fork_tally)
    }

    pub fn apply_vote<'a>(
        &self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        self.aec.write().unwrap().apply_vote(args)
    }

    pub fn transition_time(&self, now: Timestamp) {
        self.aec.write().unwrap().transition_time(now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_tick(
        &self,
        now: Timestamp,
        local_key: &rsnano_types::PrivateKey,
        epoch_duration: Duration,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.aec
            .write()
            .unwrap()
            .rai_tick(now, local_key, epoch_duration)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_report_received(&self, report: crate::consensus::rai::RaiReport) {
        let now = self.clock.now();
        self.aec.write().unwrap().rai_report_received(
            report,
            &rsnano_types::PrivateKey::from(0),
            Duration::from_secs(1),
            now,
        );
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_progress_close(
        &self,
        frontiers: crate::consensus::rai::RaiFrontierMap,
        now: Timestamp,
    ) {
        self.aec.write().unwrap().rai_progress_close(frontiers, now);
    }

    pub fn transition_active(&self, block_hash: &BlockHash) -> bool {
        self.aec.write().unwrap().transition_active(block_hash)
    }

    pub fn refill<T>(&self, source: &mut T, now: Timestamp)
    where
        T: ElectionCandidateSource,
    {
        self.aec.write().unwrap().refill(source, now);
    }

    pub fn remove_votes<'a>(
        &self,
        root: &QualifiedRoot,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        self.aec.write().unwrap().remove_votes(root, voters)
    }

    pub fn erase(&self, root: &QualifiedRoot) -> bool {
        self.aec.write().unwrap().erase(root)
    }

    pub fn confirm_dependent_elections(
        &self,
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>)>,
        now: Timestamp,
    ) {
        self.aec
            .write()
            .unwrap()
            .confirm_dependent_elections(confirmed, now)
    }

    pub fn remove_recently_confirmed(&self, block_hash: &BlockHash) {
        self.aec
            .write()
            .unwrap()
            .remove_recently_confirmed(block_hash)
    }

    pub fn set_cooldown(&self, cool_down: bool, reason: AecCooldownReason) {
        self.aec.write().unwrap().set_cooldown(cool_down, reason)
    }

    pub fn cancel(&self, root: &QualifiedRoot) {
        self.aec.write().unwrap().cancel(root)
    }

    pub fn cancel_all(&self) {
        self.aec.write().unwrap().cancel_all()
    }

    pub fn clear_recently_confirmed(&self) {
        self.aec.write().unwrap().clear_recently_confirmed()
    }

    pub fn stop(&self) {
        self.aec.write().unwrap().stop()
    }

    pub fn force_confirm(&self, block_hash: &BlockHash, now: Timestamp) {
        self.aec.write().unwrap().force_confirm(block_hash, now)
    }

    pub fn simulate_event(&self, event: AecFact) {
        self.aec.read().unwrap().simulate_event(event)
    }

    pub fn snapshot(&self) -> AecSnapshot {
        let now = self.clock.now();
        self.aec.read().unwrap().snapshot(now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_epoch_status(
        &self,
    ) -> (
        crate::consensus::rai::RaiEpochState,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, BlockHash>,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, BlockHash>,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, (usize, usize)>,
    ) {
        let aec = self.aec.read().unwrap();
        let state = *aec.rai_epoch_state();
        let mut hashes = std::collections::BTreeMap::new();
        if let Some(last) = state.closed_through {
            for number in 0..=last.number() {
                let epoch = rsnano_types::RaiEpoch::new(number);
                if let Some(hash) = aec.rai_installed_close_hash(epoch) {
                    hashes.insert(epoch, hash);
                }
            }
        }
        let cut_hashes = aec.rai_decided_cut_hashes().clone();
        let drains = aec
            .rai_happy_path_drains()
            .iter()
            .map(|(epoch, drain)| (*epoch, (drain.obligations.len(), drain.finalized.len())))
            .collect();
        (state, hashes, cut_hashes, drains)
    }
}

impl StatsSource for AecService {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.aec.read().unwrap().collect_stats(result)
    }
}

impl ContainerInfoProvider for AecService {
    fn container_info(&self) -> ContainerInfo {
        self.aec.read().unwrap().container_info()
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use std::{sync::Arc, time::Duration};

    use rsnano_ledger::RepWeights;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{Amount, BlockHash, PrivateKey, RaiEpoch};

    use super::*;

    #[test]
    fn live_tick_opens_epoch_one_at_the_deadline() {
        let key = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(key.public_key(), Amount::raw(1));
        let service = AecService::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_millis(25),
            Arc::new(weights),
            BlockHash::ZERO,
        );
        let duration = Duration::from_secs(30);
        let start = Timestamp::default();

        service.rai_tick(start, &key, duration);
        assert_eq!(service.rai_epoch_status().0.open_epoch, RaiEpoch::ZERO);

        service.rai_tick(start + duration, &key, duration);
        let state = service.rai_epoch_status().0;
        assert_eq!(state.open_epoch, RaiEpoch::new(1));
        assert_eq!(state.closing.unwrap().epoch, RaiEpoch::ZERO);
    }

    #[test]
    fn received_report_quorum_starts_the_live_cut_election() {
        use crate::consensus::rai::{RaiClosingPhase, RaiReport, rai_close_cut_root};

        let keys = (1..=4).map(PrivateKey::from).collect::<Vec<_>>();
        let mut weights = RepWeights::default();
        for key in &keys {
            weights.put(key.public_key(), Amount::raw(1));
        }
        let service = AecService::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_millis(25),
            Arc::new(weights),
            BlockHash::ZERO,
        );
        let duration = Duration::from_secs(30);
        let deadline = Timestamp::default() + duration;

        let reports = service.rai_tick(deadline, &keys[0], duration);
        assert_eq!(reports.len(), 1);
        assert_eq!(
            service.rai_epoch_status().0.closing.unwrap().phase,
            RaiClosingPhase::CollectingReports
        );
        assert!(!service.is_active_root(&rai_close_cut_root(RaiEpoch::ZERO, 0)));

        service.rai_report_received(RaiReport::new(&keys[1], RaiEpoch::ZERO, []));
        service.rai_report_received(RaiReport::new(&keys[2], RaiEpoch::ZERO, []));
        assert_eq!(
            service.rai_epoch_status().0.closing.unwrap().phase,
            RaiClosingPhase::CollectingReports
        );
        service.rai_report_received(RaiReport::new(&keys[3], RaiEpoch::ZERO, []));

        assert_eq!(
            service.rai_epoch_status().0.closing.unwrap().phase,
            RaiClosingPhase::ElectingCut
        );
        assert!(service.is_active_root(&rai_close_cut_root(RaiEpoch::ZERO, 0)));
    }
}

#[derive(Default)]
pub struct AecSnapshot {
    pub buckets: Vec<BucketSnapshot>,
}

pub struct BucketSnapshot {
    pub bucket_index: usize,
    pub election_count: usize,
    pub elections: Vec<ElectionSnapshot>,
}

pub struct ElectionSnapshot {
    pub winner_hash: BlockHash,
    pub non_final_tally: Amount,
    pub final_tally: Amount,
    pub root: QualifiedRoot,
    pub account: Account,
    pub state: ElectionState,
    pub candidate_blocks: Vec<BlockHash>,
    pub is_final: bool,
    pub elapsed: Duration,
}
