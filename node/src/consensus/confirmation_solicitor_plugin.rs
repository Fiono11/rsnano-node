use std::{
    any::Any,
    sync::{Arc, Mutex},
};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::ConsensusEpoch;

use super::{
    AecService, AecTickerPlugin, ConfirmationSolicitor,
    confirm_req_sender::{ConfirmReqSender, Urgency},
    election::{Election, ElectionState},
    winner_block_broadcaster::WinnerBlockBroadcaster,
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
        let now = aec.now();
        let current_epoch = aec.current_epoch();
        let elections: Vec<_> = aec.round_robin(|elections_iter| {
            elections_iter
                .filter(|e| Self::should_solicit(e, now, e.epoch() < current_epoch))
                .cloned()
                .collect()
        });

        let draining = aec.draining_epoch();
        for election in &elections {
            self.winner_block_broadcaster
                .lock()
                .unwrap()
                .try_broadcast_winner(&election.winner().clone(), election.votes());
            let urgency = Self::request_urgency(election, current_epoch, draining);
            self.confirm_req_sender
                .send_confirm_req(&mut solicitor, election, urgency);
        }
        // RAI: the rounds of the close elections collect their evidence too
        for (id, value) in aec.close_solicitations(now) {
            solicitor.add_request(id.epoch, value, id.root.root);
        }

        solicitor.flush();
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ConfirmationSolicitorPlugin {
    fn request_urgency(
        election: &Election,
        current_epoch: ConsensusEpoch,
        draining: Option<ConsensusEpoch>,
    ) -> Urgency {
        // RAI defers the boundary while the previous checkpoint closes;
        // account voting continues normally. Retrying every tick here only
        // adds traffic to the same workers needed to finish that checkpoint.
        if !cfg!(feature = "rai_protocol")
            && draining == Some(election.epoch())
            && !election.state().is_terminated()
        {
            Urgency::Now
        } else if election.epoch() < current_epoch {
            if election.state() == ElectionState::Settled {
                Urgency::Slow
            } else {
                Urgency::Soon
            }
        } else {
            Urgency::Normal
        }
    }

    fn should_solicit(election: &Election, now: Timestamp, epoch_left: bool) -> bool {
        match election.state() {
            ElectionState::Active => true,
            // Kudzu: a terminated election still collects votes and certificates
            // until it is settled, and a settled one until it can no longer be finalized
            ElectionState::Terminated | ElectionState::TimedOut | ElectionState::Settled => {
                cfg!(feature = "rai_protocol") && election.should_solicit_evidence(now, epoch_left)
            }
            _ => false,
        }
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use rsnano_types::SavedBlock;

    use super::*;

    #[test]
    fn deferred_boundary_keeps_normal_account_retry_rate() {
        let election = Election::new_test_instance_with(SavedBlock::new_test_instance());

        assert_eq!(
            ConfirmationSolicitorPlugin::request_urgency(
                &election,
                election.epoch(),
                Some(election.epoch()),
            ),
            Urgency::Normal
        );
        // Frozen elections still retry evidence collection at the old rate.
        assert_eq!(
            ConfirmationSolicitorPlugin::request_urgency(&election, election.epoch().next(), None,),
            Urgency::Soon
        );
    }
}
