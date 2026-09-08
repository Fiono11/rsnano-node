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

        let elections: Vec<_> = aec.round_robin(|elections_iter| {
            elections_iter
                .filter(|e| e.state() == ElectionState::Active)
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
