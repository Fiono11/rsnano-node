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
        let draining = aec.draining_epoch();
        // Only what is due this tick is copied out of the elections: a
        // request goes once per interval, a winner broadcast once per its
        // own, and cloning every election at every tick - vote maps and all
        // - was the ticker's largest cost with thousands of them open
        let mut broadcaster = self.winner_block_broadcaster.lock().unwrap();
        let confirm_req_sender = &self.confirm_req_sender;
        let (winners, requests): (Vec<_>, Vec<_>) = aec.round_robin(|elections_iter| {
            let mut winners = Vec::new();
            let mut requests = Vec::new();
            for election in
                elections_iter.filter(|e| Self::should_solicit(e, now, e.epoch() < current_epoch))
            {
                if broadcaster.broadcast_due(&election.winner().hash()) {
                    winners.push((election.winner().clone(), election.votes().clone()));
                }
                let urgency = Self::urgency(election, draining, current_epoch);
                if confirm_req_sender.should_send_confirm_req(election, urgency) {
                    requests.push((election.clone(), urgency));
                }
            }
            (winners, requests)
        });
        for (winner, votes) in &winners {
            broadcaster.try_broadcast_winner(winner, votes);
        }
        drop(broadcaster);
        for (election, urgency) in &requests {
            self.confirm_req_sender
                .send_confirm_req(&mut solicitor, election, *urgency);
        }
        // RAI: the rounds of the close elections collect their evidence too
        #[cfg(feature = "rai_protocol")]
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
    /// RAI: the ending epoch is left once its instances have terminated,
    /// and its close waits for them to settle: the instances still waiting
    /// for a certificate are asked at every tick, those of an epoch left
    /// which are not settled every base latency
    fn urgency(
        election: &Election,
        draining: Option<ConsensusEpoch>,
        current_epoch: ConsensusEpoch,
    ) -> Urgency {
        if draining == Some(election.epoch()) && !election.state().is_terminated() {
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
            ElectionState::Terminated | ElectionState::Settled => {
                #[cfg(feature = "rai_protocol")]
                {
                    election.should_solicit_evidence(now, epoch_left)
                }
                #[cfg(not(feature = "rai_protocol"))]
                {
                    false
                }
            }
            _ => false,
        }
    }
}
