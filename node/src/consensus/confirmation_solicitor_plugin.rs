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
}

impl ConfirmationSolicitorPlugin {
    #[allow(dead_code)]
    pub fn new_null() -> Self {
        Self {
            message_flooder: MessageFlooder::new_null(),
            rep_tracker: RepresentativeTracker::new_null().into(),
            winner_block_broadcaster: Mutex::new(WinnerBlockBroadcaster::new_null()).into(),
            confirm_req_sender: ConfirmReqSender::new_null(),
        }
    }
}

#[cfg(feature = "rai_protocol")]
const MAX_RAI_SOLICITATIONS_PER_PASS: usize = rsnano_messages::ConfirmReq::HASHES_MAX;

#[cfg(feature = "rai_protocol")]
fn rai_due_elections<'a>(
    sender: &ConfirmReqSender,
    elections: &mut dyn Iterator<Item = &'a super::election::Election>,
) -> Vec<super::election::Election> {
    let mut close = Vec::new();
    let mut slots = Vec::new();
    for election in elections {
        if election.state() != ElectionState::Active
            || !sender.should_send_confirm_req(election)
        {
            continue;
        }
        if election.is_rai_close() {
            // Close elections gate every later epoch and must not sit behind
            // an always-full slot prefix. There are only a bounded number of
            // live close rounds, so retain all of them before filling the
            // ordinary legacy request window.
            close.push(election.clone());
        } else if close.len() + slots.len() < MAX_RAI_SOLICITATIONS_PER_PASS {
            // Legacy active elections are bounded by the AEC container size.
            // RAI drain elections may temporarily exceed that bound, so keep
            // the slot portion of one pass within one ConfirmReq packet.
            slots.push(election.clone());
        }
    }
    close.extend(
        slots
            .into_iter()
            .take(MAX_RAI_SOLICITATIONS_PER_PASS.saturating_sub(close.len())),
    );
    close
}

impl AecTickerPlugin for ConfirmationSolicitorPlugin {
    fn run(&mut self, aec: &AecService) {
        let peered_prs = self.rep_tracker.peered_principal_reps();

        // TODO don't clone flooder!'
        let flooder = self.message_flooder.clone();
        let mut solicitor = ConfirmationSolicitor::new(flooder);
        solicitor.prepare(&peered_prs);

        /*
         * Loop through active elections requesting confirmation
         *
         * Only up to a certain amount of elections are queued for confirmation request and block rebroadcasting.
         * The remaining elections can still be confirmed if votes arrive
         * Elections extending the soft config.size limit are flushed after a certain time-to-live cutoff
         * Flushed elections are later re-activated via frontier confirmation
         */
        #[cfg(feature = "rai_protocol")]
        let elections = aec.round_robin(|elections_iter| {
            rai_due_elections(&self.confirm_req_sender, elections_iter)
        });
        #[cfg(not(feature = "rai_protocol"))]
        let elections: Vec<_> = aec.round_robin(|elections_iter| {
            elections_iter
                .filter(|e| e.state() == ElectionState::Active)
                .cloned()
                .collect()
        });

        for election in &elections {
            self.winner_block_broadcaster
                .lock()
                .unwrap()
                .try_broadcast_winner(&election.winner().clone(), election.votes());
            self.confirm_req_sender
                .send_confirm_req(&mut solicitor, election);
        }

        solicitor.flush();
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use crate::consensus::election::{Election, ElectionBehavior};
    use rsnano_nullable_clock::{SteadyClock, Timestamp};
    use rsnano_types::{RaiEpoch, SavedBlock};
    use rsnano_utils::stats::Stats;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn solicitation_clones_only_when_slot_cadence_is_due() {
        let base_latency = Duration::from_millis(25);
        let clock = Arc::new(SteadyClock::new_null());
        let mut sender = ConfirmReqSender::new(Arc::new(Stats::default()), clock.clone());
        let mut election = Election::new_slot(
            SavedBlock::new_test_instance(),
            ElectionBehavior::Priority,
            base_latency,
            Timestamp::new_test_instance(),
            RaiEpoch::ZERO,
        );
        election.transition_active();
        sender.record_request_for_test(&election);

        let mut elections = std::iter::once(&election);
        assert!(rai_due_elections(&sender, &mut elections).is_empty());

        clock.advance(base_latency * 5 - Duration::from_millis(1));
        let mut elections = std::iter::once(&election);
        assert!(rai_due_elections(&sender, &mut elections).is_empty());

        clock.advance(Duration::from_millis(1));
        let mut elections = std::iter::once(&election);
        assert_eq!(rai_due_elections(&sender, &mut elections).len(), 1);
    }

    #[test]
    fn solicitation_pass_is_bounded_for_rai_drain_bursts() {
        let base_latency = Duration::from_millis(25);
        let clock = Arc::new(SteadyClock::new_null());
        let sender = ConfirmReqSender::new(Arc::new(Stats::default()), clock);
        let mut elections = (0..MAX_RAI_SOLICITATIONS_PER_PASS + 10)
            .map(|_| {
                let block = SavedBlock::new_test_instance();
                let mut election = Election::new_slot(
                    block,
                    ElectionBehavior::Priority,
                    base_latency,
                    Timestamp::new_test_instance(),
                    RaiEpoch::ZERO,
                );
                election.transition_active();
                election
            })
            .collect::<Vec<_>>();

        let mut iter = elections.iter_mut().map(|election| &*election);
        assert_eq!(
            rai_due_elections(&sender, &mut iter).len(),
            MAX_RAI_SOLICITATIONS_PER_PASS
        );
    }

    #[test]
    fn close_election_is_not_starved_behind_a_full_slot_window() {
        use crate::consensus::rai::{RaiCloseElectionId, RaiCloseKind};
        use rsnano_ledger::RepWeights;
        use rsnano_types::{BlockHash, QualifiedRoot};

        let base_latency = Duration::from_millis(25);
        let clock = Arc::new(SteadyClock::new_null());
        let sender = ConfirmReqSender::new(Arc::new(Stats::default()), clock);
        let mut elections = (0..MAX_RAI_SOLICITATIONS_PER_PASS + 10)
            .map(|_| {
                let mut election = Election::new_slot(
                    SavedBlock::new_test_instance(),
                    ElectionBehavior::Priority,
                    base_latency,
                    Timestamp::new_test_instance(),
                    RaiEpoch::ZERO,
                );
                election.transition_active();
                election
            })
            .collect::<Vec<_>>();
        let mut close = Election::new_close(
            RaiCloseElectionId {
                kind: RaiCloseKind::Cut,
                epoch: RaiEpoch::ZERO,
                round: 0,
            },
            QualifiedRoot::new_test_instance(),
            BlockHash::from(7),
            Arc::new(RepWeights::default()),
            base_latency,
            Timestamp::new_test_instance(),
        );
        close.transition_active();
        elections.push(close);

        let selected = rai_due_elections(&sender, &mut elections.iter());
        assert_eq!(selected.len(), MAX_RAI_SOLICITATIONS_PER_PASS);
        assert!(selected.iter().any(Election::is_rai_close));
    }
}
