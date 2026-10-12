use std::{any::Any, sync::Arc};

use rsnano_ledger::{AnySet, Ledger};
use rsnano_nullable_clock::SteadyClock;

use super::{AecService, AecTickerPlugin};

/// RAI, "checkpoints decide": every block the latest checkpoint keeps gets
/// its instance in the open epoch. A kept fork candidate that this node's
/// ledger took in place of its rival only after the checkpoint (the
/// checkpoint follower forces it in) has no saved body in its election; this
/// plugin reads it from the ledger and continues it.
pub(crate) struct KeptBlockLoader {
    ledger: Arc<Ledger>,
    clock: Arc<SteadyClock>,
}

impl KeptBlockLoader {
    pub(crate) fn new(ledger: Arc<Ledger>, clock: Arc<SteadyClock>) -> Self {
        Self { ledger, clock }
    }
}

impl AecTickerPlugin for KeptBlockLoader {
    fn run(&mut self, aec: &AecService) {
        let awaiting = aec.kept_blocks_awaiting_ledger();
        if awaiting.is_empty() {
            return;
        }
        let blocks: Vec<_> = {
            let any = self.ledger.any();
            awaiting
                .iter()
                .filter_map(|hash| any.get_block(hash))
                .collect()
        };
        let now = self.clock.now();
        for block in blocks {
            aec.continue_kept_block(block, now);
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
