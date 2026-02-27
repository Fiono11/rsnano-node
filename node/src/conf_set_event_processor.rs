use crate::{
    cementation::ConfirmingSetEvent,
    consensus::{ActiveElectionsContainer, AecCooldownReason},
    utils::BackpressureEventProcessor,
};
use std::sync::{Arc, RwLock};

pub(crate) struct ConfirmingSetEventProcessor {
    pub(crate) active_elections: Arc<RwLock<ActiveElectionsContainer>>,
}

impl BackpressureEventProcessor<ConfirmingSetEvent> for ConfirmingSetEventProcessor {
    fn cool_down(&mut self) {}

    fn recovered(&mut self) {}

    fn process(&mut self, event: ConfirmingSetEvent) {
        match event {
            ConfirmingSetEvent::ConfirmationFailed(hash) => {
                // The block never got confirmed! Clean up the election, so
                // that a new election for this block can be started
                self.active_elections
                    .write()
                    .unwrap()
                    .remove_recently_confirmed(&hash);
            }
            ConfirmingSetEvent::NearFull => {
                self.active_elections
                    .write()
                    .unwrap()
                    .set_cooldown(true, AecCooldownReason::ConfirmingSetFull);
            }
            ConfirmingSetEvent::Recovered => {
                self.active_elections
                    .write()
                    .unwrap()
                    .set_cooldown(false, AecCooldownReason::ConfirmingSetFull);
            }
        }
    }
}
