use std::sync::Arc;

use tracing::info;

use rsnano_network::ChannelEvent;
use rsnano_types::Account;
use rsnano_utils::EventHandler;

use super::RepresentativeTracker;

/// Removes reps with dead channels
pub struct OnlineRepsCleanup(Arc<RepresentativeTracker>);

impl OnlineRepsCleanup {
    pub fn new(rep_tracker: Arc<RepresentativeTracker>) -> Self {
        Self(rep_tracker)
    }
}

impl EventHandler<ChannelEvent> for OnlineRepsCleanup {
    fn handle(&self, event: &ChannelEvent) {
        if let ChannelEvent::Removed(id) = event {
            let removed_reps = self.0.remove_peer(*id);
            for rep in removed_reps {
                info!(
                    "Evicting representative {} with dead channel",
                    Account::from(rep).encode_account(),
                );
            }
        }
    }
}
