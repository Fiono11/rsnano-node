#[cfg(feature = "rai_protocol")]
use crate::consensus::AecService;
use rsnano_ledger::{AnySet, LedgerSet};
#[cfg(feature = "rai_protocol")]
use rsnano_types::VoteType;
use rsnano_types::{Account, Block, BlockHash, MaybeSavedBlock, Root};
use rsnano_utils::stats::{DetailType, StatType, Stats};

pub(super) struct RequestAggregatorImpl<'a> {
    stats: &'a Stats,
    any: &'a dyn AnySet,
    #[cfg(feature = "rai_protocol")]
    aec: Option<&'a AecService>,
    #[cfg(feature = "rai_protocol")]
    epoch: u64,
    #[cfg(feature = "rai_protocol")]
    vote_type: Option<VoteType>,

    pub to_generate: Vec<MaybeSavedBlock>,
    pub to_generate_final: Vec<MaybeSavedBlock>,
    #[cfg(feature = "rai_protocol")]
    pub to_generate_first: Vec<MaybeSavedBlock>,
}

impl<'a> RequestAggregatorImpl<'a> {
    pub fn new(
        stats: &'a Stats,
        any: &'a dyn AnySet,
        #[cfg(feature = "rai_protocol")] aec: Option<&'a AecService>,
        #[cfg(feature = "rai_protocol")] epoch: u64,
        #[cfg(feature = "rai_protocol")] vote_type: Option<VoteType>,
    ) -> Self {
        Self {
            stats,
            any,
            #[cfg(feature = "rai_protocol")]
            aec,
            #[cfg(feature = "rai_protocol")]
            epoch,
            #[cfg(feature = "rai_protocol")]
            vote_type,
            to_generate: Vec::new(),
            to_generate_final: Vec::new(),
            #[cfg(feature = "rai_protocol")]
            to_generate_first: Vec::new(),
        }
    }

    fn search_for_block(&self, hash: &BlockHash, root: &Root) -> Option<MaybeSavedBlock> {
        #[cfg(feature = "rai_protocol")]
        if hash.is_zero() {
            if let Some(block) = self.aec.and_then(|aec| aec.election_candidate(self.epoch, root)) {
                return Some(block);
            }
        }
        #[cfg(feature = "rai_protocol")]
        if let Some(block) = self
            .aec
            .and_then(|aec| aec.candidate_block(self.epoch, hash))
        {
            return Some(block);
        }
        // Ledger by hash
        let block = self.any.get_block(hash).map(MaybeSavedBlock::Saved);
        if block.is_some() {
            return block;
        }

        if !root.is_zero() {
            // Search for successor of root
            if let Some(successor) = self.any.block_successor(&(*root).into()) {
                return self.any.get_block(&successor).map(MaybeSavedBlock::Saved);
            }

            // If that fails treat root as account
            if let Some(info) = self.any.get_account(&Account::from(*root)) {
                return self
                    .any
                    .get_block(&info.open_block)
                    .map(MaybeSavedBlock::Saved);
            }
        }

        None
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
                #[cfg(feature = "rai_protocol")]
                let recovery_vote_type = self.vote_type.or_else(|| {
                    self.aec
                        .and_then(|aec| aec.recovery_vote_type(self.epoch, hash))
                });
                #[cfg(not(feature = "rai_protocol"))]
                let recovery_vote_type: Option<()> = None;

                #[cfg(feature = "rai_protocol")]
                let should_generate_non_final = recovery_vote_type == Some(VoteType::NonFinal);
                #[cfg(feature = "rai_protocol")]
                let should_generate_first = recovery_vote_type == Some(VoteType::First);
                #[cfg(not(feature = "rai_protocol"))]
                let should_generate_first = false;
                #[cfg(not(feature = "rai_protocol"))]
                let should_generate_non_final = false;
                #[cfg(feature = "rai_protocol")]
                let should_generate_final = recovery_vote_type == Some(VoteType::Final)
                    || (recovery_vote_type.is_none() && should_generate_final_vote(&block));
                #[cfg(not(feature = "rai_protocol"))]
                let should_generate_final = should_generate_final_vote(&block);

                #[cfg(feature = "rai_protocol")]
                if should_generate_first {
                    // Recovery must fill phase holes, not merely the single phase inferred from
                    // this replica's state. The First generator enforces one initial choice per
                    // representative/root, so this safely creates or replays only the missing
                    // initial vote while the normal generator fills the second-look phase.
                    self.to_generate_first.push(block.clone());
                }

                if should_generate_non_final {
                    self.to_generate.push(block);
                    self.stats
                        .inc(StatType::Requests, DetailType::RequestsNonFinal);
                } else if should_generate_final {
                    self.to_generate_final.push(block);
                    self.stats
                        .inc(StatType::Requests, DetailType::RequestsFinal);
                } else if should_generate_first {
                    #[cfg(feature = "rai_protocol")]
                    let _ = block;
                    self.stats
                        .inc(StatType::Requests, DetailType::RequestsNonFinal);
                } else {
                    #[cfg(not(feature = "rai_protocol"))]
                    {
                        self.to_generate.push(block);
                        self.stats
                            .inc(StatType::Requests, DetailType::RequestsNonFinal);
                    }
                    #[cfg(feature = "rai_protocol")]
                    {
                        // No new phase is admissible for this candidate in the responder's local
                        // state, but the requester may still be missing an earlier signed phase.
                        // Route it through a generator so all cached phases are replayed before
                        // the First generator's history guard declines any conflicting new vote.
                        self.to_generate_first.push(block);
                    }
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
            #[cfg(feature = "rai_protocol")]
            remaining_first: self.to_generate_first,
        }
    }
}

pub(super) struct AggregateResult {
    pub remaining_normal: Vec<MaybeSavedBlock>,
    pub remaining_final: Vec<MaybeSavedBlock>,
    #[cfg(feature = "rai_protocol")]
    pub remaining_first: Vec<MaybeSavedBlock>,
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
    fn generates_initial_vote_for_unconfirmed_block() {
        let ledger = Ledger::new_null();
        let block = UnsavedBlockLatticeBuilder::new().genesis().send(100, 1);
        let root = block.root();
        ledger.process_one(&block).unwrap();

        let result = run_aggregator(&ledger, &[(block.hash(), root)]);

        #[cfg(not(feature = "rai_protocol"))]
        assert_eq!(result.remaining_normal.len(), 1);
        #[cfg(feature = "rai_protocol")]
        {
            assert_eq!(result.remaining_first.len(), 1);
            assert!(result.remaining_normal.is_empty());
        }
        assert!(result.remaining_final.is_empty());
    }

    /*
     * Test helpers
     */

    fn run_aggregator(ledger: &Ledger, requests: &[(BlockHash, Root)]) -> AggregateResult {
        let stats = Stats::default();
        let any = ledger.any();
        let mut aggregator = RequestAggregatorImpl::new(
            &stats,
            &any,
            #[cfg(feature = "rai_protocol")]
            None,
            #[cfg(feature = "rai_protocol")]
            0,
            #[cfg(feature = "rai_protocol")]
            None,
        );
        aggregator.add_votes(requests);
        aggregator.get_result()
    }
}
