use std::{
    collections::HashSet,
    sync::{Arc, Mutex, RwLock},
};

use rsnano_ledger::{BlockSource, Ledger, LedgerSet};
use rsnano_network::ChannelId;
#[cfg(not(feature = "rai_protocol"))]
use rsnano_types::Block;
use rsnano_types::{Account, BlockHash, ConsensusEpoch};

use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    cementation::ConfirmingSet,
    consensus::{AecService, ForkCache},
};

/// RAI: brings the ledger in line with a decided checkpoint. It checks every
/// block the checkpoint finalized against the ledger, which takes a few
/// hundred milliseconds for an epoch of twenty thousand blocks; the caller
/// runs it off the AEC fact thread, whose cementations would otherwise wait.
pub(crate) struct CheckpointInstaller {
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) confirming_set: Arc<ConfirmingSet>,
    pub(crate) fork_cache: Arc<RwLock<ForkCache>>,
    pub(crate) active_elections: Arc<AecService>,
    pub(crate) block_processor_queue: Arc<BlockProcessorQueue>,
    /// Blocks a decided checkpoint finalized that this ledger lacked;
    /// force-inserted and cemented once present
    awaiting_cement: Mutex<HashSet<BlockHash>>,
}

impl CheckpointInstaller {
    pub(crate) fn new(
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        fork_cache: Arc<RwLock<ForkCache>>,
        active_elections: Arc<AecService>,
        block_processor_queue: Arc<BlockProcessorQueue>,
    ) -> Self {
        Self {
            ledger,
            confirming_set,
            fork_cache,
            active_elections,
            block_processor_queue,
            awaiting_cement: Mutex::new(HashSet::new()),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn new_null() -> Self {
        Self::new(
            Arc::new(Ledger::new_null()),
            Arc::new(ConfirmingSet::new_null()),
            Arc::new(RwLock::new(ForkCache::new())),
            Arc::new(AecService::new_null()),
            Arc::new(BlockProcessorQueue::new_null()),
        )
    }

    /// RAI: the blocks a decided checkpoint finalized are cemented. One the
    /// ledger already cemented is left alone; one it holds unconfirmed is
    /// handed to the confirming set, which cements it with its ancestors;
    /// one it does not hold at all is reported, and the finality stands in
    /// the decided state until the block arrives.
    pub(crate) fn install(&self, epoch: ConsensusEpoch, hashes: Vec<BlockHash>) {
        let mut cemented = 0;
        let mut queued = 0;
        let mut missing = 0;
        let mut fetched = 0;
        let mut absent = Vec::new();
        {
            let any = self.ledger.any();
            let confirmed = self.ledger.confirmed();
            for hash in &hashes {
                if confirmed.block_exists(hash) {
                    cemented += 1;
                } else if any.block_exists(hash) {
                    self.confirming_set.add_block(*hash);
                    queued += 1;
                } else {
                    missing += 1;
                    absent.push(*hash);
                }
            }
        }
        // A finalized block this ledger lacks - its rival held here instead,
        // or never received - is inserted in place of the rival, parents
        // first (the hashes come in position order), and cemented once present
        for hash in absent {
            if self.force_insert(&hash) {
                fetched += 1;
            }
            self.awaiting_cement.lock().unwrap().insert(hash);
            self.active_elections.await_checkpoint_blocks([hash]);
        }
        if missing > 0 {
            crate::utils::diagnostic!(
                "EPOCH_INSTALL_FETCH epoch={} missing={} fetched={}",
                epoch,
                missing,
                fetched
            );
        }
        crate::utils::diagnostic!(
            "EPOCH_INSTALLED epoch={} finalized={} cemented={} queued={} missing={}",
            epoch,
            hashes.len(),
            cemented,
            queued,
            missing
        );
    }

    /// RAI: "Recovery through a fresh child": the owner extends a retained
    /// tip, so the ledger must hold the retained branch. A node whose ledger
    /// holds an omitted rival at a retained position rolls it back and
    /// installs the retained block, from the fork cache or the retained
    /// report data; a retained block this node does not hold yet is
    /// installed when it arrives as evidence (see the message processor).
    /// Cemented blocks are never rolled back.
    pub(crate) fn follow_retained_branches(
        &self,
        epoch: ConsensusEpoch,
        retained: Vec<(Account, u64, BlockHash)>,
    ) {
        let mut held = 0;
        let mut forced = 0;
        let mut missing = 0;
        let absent: Vec<BlockHash> = {
            let any = self.ledger.any();
            retained
                .iter()
                .map(|(_, _, hash)| *hash)
                .filter(|hash| {
                    let exists = any.block_exists(hash);
                    if exists {
                        held += 1;
                    }
                    !exists
                })
                .collect()
        };
        for hash in &absent {
            if self.force_insert(hash) {
                forced += 1;
            } else {
                missing += 1;
            }
        }
        crate::utils::diagnostic!(
            "EPOCH_RETAINED epoch={} retained={} held={} forced={} missing={}",
            epoch,
            retained.len(),
            held,
            forced,
            missing
        );
    }

    /// Whether blocks are awaited
    pub(crate) fn is_awaiting(&self) -> bool {
        !self.awaiting_cement.lock().unwrap().is_empty()
    }

    /// RAI: cement the checkpoint-finalized blocks that arrived since they
    /// were found missing
    pub(crate) fn cement_arrived(&self) {
        let arrived: Vec<BlockHash> = {
            let any = self.ledger.any();
            let mut awaiting = self.awaiting_cement.lock().unwrap();
            let arrived: Vec<BlockHash> = awaiting
                .iter()
                .filter(|hash| any.block_exists(hash))
                .copied()
                .collect();
            for hash in &arrived {
                awaiting.remove(hash);
            }
            arrived
        };
        for hash in arrived {
            self.active_elections.checkpoint_block_arrived(&hash);
            self.confirming_set.add_block(hash);
        }
    }

    /// Queues a block this node holds only in the fork cache or the report
    /// data for forced insertion; false if it holds it nowhere
    fn force_insert(&self, hash: &BlockHash) -> bool {
        #[cfg(feature = "rai_protocol")]
        let block = self
            .fork_cache
            .read()
            .unwrap()
            .block(hash)
            .or_else(|| self.active_elections.report_block(hash));
        #[cfg(not(feature = "rai_protocol"))]
        let block: Option<Block> = {
            let _ = (hash, &self.fork_cache);
            None
        };
        match block {
            Some(block) => {
                self.block_processor_queue.push(BlockContext::new(
                    block,
                    BlockSource::Forced,
                    ChannelId::LOOPBACK,
                ));
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finalized_block_the_ledger_lacks_is_awaited() {
        let installer = CheckpointInstaller::new_null();

        installer.install(ConsensusEpoch::new(0), vec![BlockHash::from(1)]);

        assert!(installer.is_awaiting());
    }

    #[test]
    fn an_awaited_block_stays_awaited_until_it_arrives() {
        let installer = CheckpointInstaller::new_null();
        installer.install(ConsensusEpoch::new(0), vec![BlockHash::from(1)]);

        installer.cement_arrived();

        assert!(installer.is_awaiting());
    }

    #[test]
    fn nothing_is_awaited_for_retained_branches() {
        let installer = CheckpointInstaller::new_null();

        installer.follow_retained_branches(
            ConsensusEpoch::new(0),
            vec![(Account::from(1), 1, BlockHash::from(1))],
        );

        assert!(!installer.is_awaiting());
    }
}
