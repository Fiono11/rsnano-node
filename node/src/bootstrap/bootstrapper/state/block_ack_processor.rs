use tracing::trace;

use rsnano_messages::BlocksAckPayload;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use crate::bootstrap::bootstrapper::state::{
    CandidateAccounts, PriorityDownResult, RunningQuery, VerifyResult,
    block_queue::{AccountBlocks, BlockQueue},
};

#[derive(Default)]
pub struct BlockAckProcessor {
    pub(crate) stats: BlockAckStats,
    pub(crate) block_queue: BlockQueue,
}

impl BlockAckProcessor {
    pub(crate) fn process(
        &mut self,
        candidates: &mut CandidateAccounts,
        query: &RunningQuery,
        response: BlocksAckPayload,
    ) -> bool {
        trace!(
            query_id = query.id,
            blocks = response.blocks().len(),
            "Process response"
        );

        let result = query.verify_blocks(&response);
        match result {
            VerifyResult::Ok => {
                self.process_valid_blocks(query, response);
                true
            }
            VerifyResult::NothingNew => {
                self.process_empty_response(candidates, query);
                true
            }
            VerifyResult::Invalid => {
                self.stats.invalid += 1;
                false
            }
        }
    }

    fn process_valid_blocks(&mut self, query: &RunningQuery, response: BlocksAckPayload) {
        self.stats.verified += 1;
        self.stats.blocks += response.blocks().len() as u64;

        let mut blocks = response.take_blocks();

        // Avoid re-processing the block we already have
        assert!(!blocks.is_empty());
        if blocks.front().unwrap().hash() == query.start.into() {
            blocks.pop_front();
        }

        self.block_queue.insert(AccountBlocks {
            account: query.account,
            query_id: query.id,
            blocks: blocks.clone(),
        });
    }

    fn process_empty_response(&mut self, candidates: &mut CandidateAccounts, query: &RunningQuery) {
        self.stats.nothing_new += 1;
        match candidates.priority_down(&query.account) {
            PriorityDownResult::Deprioritized => {
                self.stats.deprioritize += 1;
            }
            PriorityDownResult::Erased => {
                self.stats.deprioritize += 1;
                self.stats.priority_erase_theshold += 1;
            }
            PriorityDownResult::AccountNotFound => {
                self.stats.deprioritize_failed += 1;
            }
            PriorityDownResult::InvalidAccount => {}
        }

        candidates.reset_last_request(&query.account);
    }
}

impl StatsSource for BlockAckProcessor {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result);
    }
}

#[derive(Default)]
pub(crate) struct BlockAckStats {
    invalid: u64,
    verified: u64,
    blocks: u64,
    nothing_new: u64,
    deprioritize: u64,
    priority_erase_theshold: u64,
    deprioritize_failed: u64,
}

impl StatsSource for BlockAckStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert("bootstrap_verify_blocks", "invalid", self.invalid);
        result.insert("bootstrap_verify_blocks", "ok", self.verified);
        result.insert("bootstrap", "blocks", self.blocks);
        result.insert("bootstrap_verify_blocks", "nothing_new", self.nothing_new);
        result.insert("bootstrap_account_sets", "deprioritize", self.deprioritize);
        result.insert(
            "bootstrap_account_sets",
            "priority_erase_threshold",
            self.priority_erase_theshold,
        );
        result.insert(
            "bootstrap_account_sets",
            "deprioritize_failed",
            self.deprioritize_failed,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::bootstrapper::state::QueryType;
    use rsnano_types::Account;

    #[test]
    fn response_doesnt_match_query() {
        let mut processor = BlockAckProcessor::default();
        let mut candidates = CandidateAccounts::default();

        let query = RunningQuery::new_test_instance();
        let response = BlocksAckPayload::new_test_instance();
        let ok = processor.process(&mut candidates, &query, response);
        assert!(!ok);
        assert_eq!(processor.stats.invalid, 1);
    }

    #[test]
    fn handle_empty_response() {
        let mut processor = BlockAckProcessor::default();
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(42);

        let query = RunningQuery {
            query_type: QueryType::BlocksByAccount,
            account,
            ..RunningQuery::new_test_instance()
        };

        let response = BlocksAckPayload::empty();
        let ok = processor.process(&mut candidates, &query, response);

        assert!(ok);
        assert_eq!(processor.stats.nothing_new, 1);
    }
}
