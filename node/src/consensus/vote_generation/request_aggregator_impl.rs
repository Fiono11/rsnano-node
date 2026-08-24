use rsnano_ledger::{AnySet, LedgerSet};
#[cfg(not(feature = "rai_protocol"))]
use rsnano_types::Account;
use rsnano_types::{Block, BlockHash, Root, SavedBlock};
use rsnano_utils::stats::{DetailType, StatType, Stats};

pub(super) struct RequestAggregatorImpl<'a> {
    stats: &'a Stats,
    any: &'a dyn AnySet,

    pub to_generate: Vec<SavedBlock>,
    pub to_generate_final: Vec<SavedBlock>,
}

impl<'a> RequestAggregatorImpl<'a> {
    pub fn new(stats: &'a Stats, any: &'a dyn AnySet) -> Self {
        Self {
            stats,
            any,
            to_generate: Vec::new(),
            to_generate_final: Vec::new(),
        }
    }

    fn search_for_block(&self, hash: &BlockHash, root: &Root) -> Option<SavedBlock> {
        // Ledger by hash
        let block = self.any.get_block(hash);
        if block.is_some() {
            return block;
        }

        #[cfg(feature = "rai_protocol")]
        {
            // A RAI vote request is for an exact slot value. If that value is no
            // longer in the ledger, silence is the response; never substitute a
            // successor or a different fork selected through the root.
            let _ = root;
            return None;
        }

        #[cfg(not(feature = "rai_protocol"))]
        if !root.is_zero() {
            // Search for successor of root
            if let Some(successor) = self.any.block_successor(&(*root).into()) {
                return self.any.get_block(&successor);
            }

            // If that fails treat root as account
            if let Some(info) = self.any.get_account(&Account::from(*root)) {
                return self.any.get_block(&info.open_block);
            }
        }

        #[cfg(not(feature = "rai_protocol"))]
        return None;
    }

    pub fn add_votes(&mut self, requests: &[(BlockHash, Root)]) {
        for (hash, root) in requests {
            let block = self.search_for_block(hash, root);

            let should_generate_final_vote = |block: &Block| {
                // Check if final vote is set for this block
                if let Some(final_hash) = self.any.get_final_vote(&block.qualified_root()) {
                    final_hash == block.hash()
                } else {
                    // If the final vote is not set, generate vote if the block is confirmed
                    self.any.confirmed().block_exists(&block.hash())
                }
            };

            if let Some(block) = block {
                if should_generate_final_vote(&block) {
                    self.to_generate_final.push(block);
                    self.stats
                        .inc(StatType::Requests, DetailType::RequestsFinal);
                } else {
                    self.stats
                        .inc(StatType::Requests, DetailType::RequestsNonFinal);
                }
            } else {
                self.stats
                    .inc(StatType::Requests, DetailType::RequestsUnknown);
            }
        }
    }

    pub fn get_result(self) -> AggregateResult {
        AggregateResult {
            remaining_normal: self.to_generate,
            remaining_final: self.to_generate_final,
        }
    }
}

pub(super) struct AggregateResult {
    pub remaining_normal: Vec<SavedBlock>,
    pub remaining_final: Vec<SavedBlock>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_ledger::{Ledger, test_helpers::UnsavedBlockLatticeBuilder};

    #[test]
    fn generates_final_vote_for_confirmed_block() {
        let ledger = Ledger::new_null();

        let block = UnsavedBlockLatticeBuilder::new().genesis().send(100, 1);
        let root = block.root();
        ledger.process_one(&block).unwrap();
        ledger.confirm(block.hash());

        let result = run_aggregator(&ledger, &[(block.hash(), root)]);

        assert_eq!(result.remaining_final.len(), 1);
        assert_eq!(result.remaining_final[0].hash(), block.hash());
    }

    #[test]
    fn generates_final_vote_for_previously_final_voted_block() {
        let ledger = Ledger::new_null();

        let block = UnsavedBlockLatticeBuilder::new().genesis().send(100, 1);
        let root = block.root();
        ledger.process_one(&block).unwrap();
        ledger.confirm(block.hash());
        ledger.verify_votes([(root, block.hash())].into(), true);

        let result = run_aggregator(&ledger, &[(block.hash(), root)]);

        assert_eq!(result.remaining_final.len(), 1);
        assert_eq!(result.remaining_final[0].hash(), block.hash());
    }

    #[test]
    #[cfg(not(feature = "rai_protocol"))]
    fn generates_final_vote_for_previously_final_voted_fork() {
        let ledger = Ledger::new_null();

        let fork_a = UnsavedBlockLatticeBuilder::new().genesis().send(100, 1);
        let fork_b = UnsavedBlockLatticeBuilder::new().genesis().send(200, 1);
        let root = fork_a.root();
        ledger.process_one(&fork_a).unwrap();
        ledger.confirm(fork_a.hash());
        ledger.verify_votes([(root, fork_a.hash())].into(), true);

        let result = run_aggregator(&ledger, &[(fork_b.hash(), root)]);

        assert_eq!(result.remaining_final.len(), 1);
        assert_eq!(result.remaining_final[0].hash(), fork_a.hash());
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn does_not_substitute_a_confirmed_fork_for_an_absent_requested_hash() {
        let ledger = Ledger::new_null();

        let present = UnsavedBlockLatticeBuilder::new().genesis().send(100, 1);
        let absent = UnsavedBlockLatticeBuilder::new().genesis().send(200, 1);
        ledger.process_one(&present).unwrap();
        ledger.confirm(present.hash());

        let result = run_aggregator(&ledger, &[(absent.hash(), absent.root())]);

        assert!(result.remaining_normal.is_empty());
        assert!(result.remaining_final.is_empty());
    }

    /*
     * Test helpers
     */

    fn run_aggregator(ledger: &Ledger, requests: &[(BlockHash, Root)]) -> AggregateResult {
        let stats = Stats::default();
        let any = ledger.any();
        let mut aggregator = RequestAggregatorImpl::new(&stats, &any);
        aggregator.add_votes(requests);
        aggregator.get_result()
    }
}
