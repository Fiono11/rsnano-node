use super::OnlineReps;
use rsnano_ledger::RepWeightCache;
use rsnano_types::{Amount, Networks};
use std::{sync::Arc, time::Duration};

pub struct OnlineRepsBuilder {
    rep_weights: Option<Arc<RepWeightCache>>,
    online_weight_minimum: Amount,
    representative_weight_minimum: Amount,
    trended: Option<Amount>,
}

impl OnlineRepsBuilder {
    pub(super) fn new() -> Self {
        Self {
            rep_weights: None,
            online_weight_minimum: OnlineReps::DEFAULT_ONLINE_WEIGHT_MINIMUM,
            representative_weight_minimum: Amount::ZERO,
            trended: None,
        }
    }
    pub fn rep_weights(mut self, weights: Arc<RepWeightCache>) -> Self {
        self.rep_weights = Some(weights);
        self
    }

    pub fn online_weight_minimum(mut self, minimum: Amount) -> Self {
        self.online_weight_minimum = minimum;
        self
    }

    pub fn representative_weight_minimum(mut self, minimum: Amount) -> Self {
        self.representative_weight_minimum = minimum;
        self
    }

    pub fn trended(mut self, trended: Amount) -> Self {
        self.trended = Some(trended);
        self
    }

    pub fn finish(self) -> OnlineReps {
        let rep_weights = self
            .rep_weights
            .unwrap_or_else(|| Arc::new(RepWeightCache::new()));

        let mut online_reps = OnlineReps::new(
            rep_weights,
            self.online_weight_minimum,
            self.representative_weight_minimum,
        );
        if let Some(trended) = self.trended {
            online_reps.set_trended(trended);
        }
        online_reps
    }
}
