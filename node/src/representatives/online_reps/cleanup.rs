use super::OnlineReps;
use rsnano_network::ChannelEvent;
use rsnano_types::Account;
use rsnano_utils::EventHandler;
use std::sync::{Arc, Mutex};
use tracing::info;

/// Removes reps with dead channels
pub struct OnlineRepsCleanup(Arc<Mutex<OnlineReps>>);

impl OnlineRepsCleanup {
    pub fn new(reps: Arc<Mutex<OnlineReps>>) -> Self {
        Self(reps)
    }
}

impl EventHandler<ChannelEvent> for OnlineRepsCleanup {
    fn handle(&self, event: &ChannelEvent) {
        if let ChannelEvent::Removed(id) = event {
            let removed_reps = self.0.lock().unwrap().remove_peer(*id);
            for rep in removed_reps {
                info!(
                    "Evicting representative {} with dead channel",
                    Account::from(rep).encode_account(),
                );
            }
        }
    }
}
