use std::{
    any::Any,
    sync::{Arc, Mutex},
};

use super::{
    AecService, AecTickerPlugin, ConfirmationSolicitor, confirm_req_sender::ConfirmReqSender,
    election::ElectionState, winner_block_broadcaster::WinnerBlockBroadcaster,
};
use crate::{representatives::RepresentativeTracker, transport::MessageFlooder};

pub(crate) struct ConfirmationSolicitorPlugin {
    pub(crate) message_flooder: MessageFlooder,
    pub(crate) rep_tracker: Arc<RepresentativeTracker>,
    pub(crate) winner_block_broadcaster: Arc<Mutex<WinnerBlockBroadcaster>>,
    pub(crate) confirm_req_sender: ConfirmReqSender,
    pub(crate) broadcast_cursor: usize,
    pub(crate) recovery_cursor: usize,
    #[cfg(feature = "rai_protocol")]
    pub(crate) vote_generators: Option<Arc<super::VoteGenerators>>,
}

impl ConfirmationSolicitorPlugin {
    #[allow(dead_code)]
    pub fn new_null() -> Self {
        Self {
            message_flooder: MessageFlooder::new_null(),
            rep_tracker: RepresentativeTracker::new_null().into(),
            winner_block_broadcaster: Mutex::new(WinnerBlockBroadcaster::new_null()).into(),
            confirm_req_sender: ConfirmReqSender::new_null(),
            broadcast_cursor: 0,
            recovery_cursor: 0,
            #[cfg(feature = "rai_protocol")]
            vote_generators: None,
        }
    }
}

impl AecTickerPlugin for ConfirmationSolicitorPlugin {
    fn run(&mut self, aec: &AecService) {
        let peered_prs = self.rep_tracker.peered_principal_reps();

        // TODO don't clone flooder!'
        let flooder = self.message_flooder.clone();
        let mut solicitor = ConfirmationSolicitor::new(flooder);
        solicitor.prepare(&peered_prs);

        #[cfg(feature = "rai_protocol")]
        let eligible = self
            .vote_generators
            .as_ref()
            .map(|g| g.solicitation_filter());
        let elections: Vec<_> = aec.round_robin(|elections_iter| {
            #[cfg(feature = "rai_protocol")]
            {
                let (live, recovery): (Vec<_>, Vec<_>) = elections_iter
                    .filter(|e| solicitation_active(e))
                    .partition(|e| {
                        !e.has_quorum()
                            && !e.is_timed_out()
                            && eligible
                                .as_ref()
                                .is_none_or(|f| f(e.qualified_root(), e.epoch))
                    });
                let mut elections: Vec<_> = live.into_iter().cloned().collect();
                // Notarized and frozen elections still need vote recovery. Share
                // a bounded rotating batch so retained forks cannot monopolize
                // requests as they accumulate. Local final voting stays immediate.
                let mut remaining = rsnano_messages::ConfirmReq::HASHES_MAX;
                visit_with_budget(recovery.len(), &mut self.recovery_cursor, |index| {
                    if remaining == 0 {
                        return false;
                    }
                    remaining -= 1;
                    elections.push(recovery[index].clone());
                    true
                });
                elections
            }
            #[cfg(not(feature = "rai_protocol"))]
            elections_iter
                .filter(|e| solicitation_active(e))
                .cloned()
                .collect()
        });

        for election in &elections {
            self.confirm_req_sender
                .send_confirm_req(&mut solicitor, election);
        }

        solicitor.flush();
        let mut broadcaster = self.winner_block_broadcaster.lock().unwrap();
        // Resume at the first election blocked by the shared token bucket. Starting
        // at the first bucket every tick starves retained elections later in the list.
        visit_with_budget(elections.len(), &mut self.broadcast_cursor, |index| {
            let election = &elections[index];
            broadcaster.try_broadcast_winner(election.winner(), election.votes())
        });
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A timeout certificate ends voting, not evidence collection: late second-look
/// notarizations must still be learned so that every replica's epoch membership
/// converges. Timed-out elections therefore share the bounded recovery rotation.
fn solicitation_active(e: &super::election::Election) -> bool {
    e.state() == ElectionState::Active
}

fn visit_with_budget(len: usize, cursor: &mut usize, mut visit: impl FnMut(usize) -> bool) {
    if len == 0 {
        *cursor = 0;
        return;
    }
    *cursor %= len;
    for _ in 0..len {
        if !visit(*cursor) {
            return;
        }
        *cursor = (*cursor + 1) % len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn timeout_certificate_keeps_recovery_solicitation() {
        use rsnano_nullable_clock::Timestamp;
        use rsnano_types::{Amount, PrivateKey, SavedBlock, Vote, VoteKind};
        let mut election = super::super::election::Election::new_test_instance_with(
            SavedBlock::new_test_instance(),
        );
        election
            .transition_time(Timestamp::new_test_instance() + std::time::Duration::from_secs(10));
        assert!(solicitation_active(&election));
        let hash = election.winner().hash();
        let mut weights = rustc_hash::FxHashMap::default();
        for i in 1..=6 {
            let key = PrivateKey::from(i);
            weights.insert(key.public_key(), Amount::raw(100));
            if i <= 5 {
                election
                    .add_kudzu_vote(
                        Arc::new(Vote::new_with_kind(
                            &key,
                            vec![hash],
                            0,
                            VoteKind::FirstTimeout,
                        )),
                        hash,
                        Timestamp::new_test_instance(),
                    )
                    .unwrap();
            }
        }
        election.update_kudzu_tallies(&weights, Amount::raw(600));
        assert_eq!(election.state(), ElectionState::Active);
        assert!(election.is_timed_out());
        assert!(
            solicitation_active(&election),
            "late notarizations are still recovered after a timeout certificate"
        );
    }

    #[test]
    fn broadcast_budget_does_not_starve_later_elections() {
        let mut cursor = 0;
        let mut sent = Vec::new();
        // The same persistent elections remain eligible on every tick.
        for _ in 0..3 {
            let mut budget = 2;
            visit_with_budget(6, &mut cursor, |index| {
                if budget == 0 {
                    return false;
                }
                budget -= 1;
                sent.push(index);
                true
            });
        }
        assert_eq!(sent, vec![0, 1, 2, 3, 4, 5]);
        visit_with_budget(0, &mut cursor, |_| panic!("empty list"));
        assert_eq!(cursor, 0);
        cursor = 8;
        visit_with_budget(2, &mut cursor, |index| {
            assert_eq!(index, 0);
            false
        });
        assert_eq!(cursor, 0);
    }
}
