use std::{sync::Arc, time::Duration};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::QualifiedRoot;
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{
    ConfirmationSolicitor,
    bounded_hash_map::BoundedHashMap,
    election::{Election, ElectionBehavior},
};

pub(crate) struct ConfirmReqSender {
    stats: Arc<Stats>,
    last_requests: BoundedHashMap<QualifiedRoot, Timestamp>,
    clock: Arc<SteadyClock>,
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use crate::consensus::{
        election::Election,
        rai::{RaiCloseElectionId, RaiCloseKind},
    };
    use rsnano_ledger::RepWeights;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{BlockHash, QualifiedRoot, RaiEpoch, SavedBlock};
    use std::sync::Arc;

    #[test]
    fn close_elections_retry_at_base_latency() {
        let base_latency = Duration::from_secs(1);
        let election = Election::new_close(
            RaiCloseElectionId {
                kind: RaiCloseKind::Cut,
                epoch: RaiEpoch::ZERO,
                round: 0,
            },
            QualifiedRoot::new_test_instance(),
            BlockHash::from(1),
            Arc::new(RepWeights::default()),
            base_latency,
            Timestamp::new_test_instance(),
        );

        assert_eq!(
            ConfirmReqSender::confirm_req_interval(&election),
            base_latency
        );
    }

    #[test]
    fn pending_slot_elections_retry_at_base_latency() {
        let base_latency = Duration::from_secs(1);
        let election = Election::new_slot(
            SavedBlock::new_test_instance(),
            ElectionBehavior::Priority,
            base_latency,
            Timestamp::new_test_instance(),
            RaiEpoch::ZERO,
        );

        assert_eq!(
            ConfirmReqSender::confirm_req_interval(&election),
            base_latency
        );
    }
}

impl ConfirmReqSender {
    pub(crate) fn new(stats: Arc<Stats>, clock: Arc<SteadyClock>) -> Self {
        Self {
            stats,
            clock,
            last_requests: BoundedHashMap::new(1024 * 32),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn new_null() -> Self {
        let stats = Arc::new(Stats::default());
        let clock = Arc::new(SteadyClock::new_null());
        Self::new(stats, clock)
    }

    pub fn send_confirm_req(&mut self, solicitor: &mut ConfirmationSolicitor, election: &Election) {
        if self.should_send_confirm_req(election) && solicitor.add(election) {
            self.last_requests
                .insert(election.qualified_root().clone(), self.clock.now());
            self.stats
                .inc(StatType::Election, DetailType::ConfirmationRequest);
        }
    }

    fn should_send_confirm_req(&self, election: &Election) -> bool {
        if let Some(last_req) = self.last_requests.get(election.qualified_root()) {
            last_req.elapsed(self.clock.now()) >= Self::confirm_req_interval(election)
        } else {
            true
        }
    }

    /// Calculates time delay between broadcasting confirmation requests
    fn confirm_req_interval(election: &Election) -> Duration {
        #[cfg(feature = "rai_protocol")]
        if election.is_rai_close() || election.rai_requires_retention() {
            return election.base_latency();
        }
        match election.behavior() {
            ElectionBehavior::Priority | ElectionBehavior::Manual | ElectionBehavior::Hinted => {
                election.base_latency() * 5
            }
            ElectionBehavior::Optimistic => election.base_latency() * 2,
        }
    }
}
