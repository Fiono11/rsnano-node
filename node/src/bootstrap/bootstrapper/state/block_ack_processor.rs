use std::collections::VecDeque;

use tracing::trace;

use rsnano_messages::BlocksAckPayload;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use crate::bootstrap::bootstrapper::state::{
    BootstrapQueue, PriorityDownResult, RunningQuery, VerifyResult,
};

#[derive(Default)]
pub struct BlockAckProcessor {
    stats: BlockAckStats,
}

impl BlockAckProcessor {
    pub(crate) fn process(
        &mut self,
        queue: &mut BootstrapQueue,
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
                self.process_valid_blocks(queue, query, response);
                true
            }
            VerifyResult::NothingNew => {
                self.process_empty_response(queue, query);
                true
            }
            VerifyResult::Invalid => {
                queue.download_finished(&query.account, VecDeque::new());
                self.stats.invalid += 1;
                false
            }
        }
    }

    fn process_valid_blocks(
        &mut self,
        queue: &mut BootstrapQueue,
        query: &RunningQuery,
        response: BlocksAckPayload,
    ) {
        self.stats.verified += 1;
        self.stats.blocks += response.blocks().len() as u64;

        let mut blocks = response.take_blocks();

        // Avoid re-processing the block we already have
        if blocks
            .front()
            .expect("valid blocks should at least have one entry")
            .hash()
            == query.start.into()
        {
            blocks.pop_front();
        }

        queue.download_finished(&query.account, blocks);
    }

    fn process_empty_response(&mut self, queue: &mut BootstrapQueue, query: &RunningQuery) {
        self.stats.nothing_new += 1;
        queue.download_finished(&query.account, VecDeque::new());
        match queue.priority_down(&query.account) {
            PriorityDownResult::Deprioritized => {
                self.stats.deprioritize += 1;
            }
            PriorityDownResult::Removed => {
                self.stats.deprioritize += 1;
                self.stats.priority_erase_theshold += 1;
            }
            PriorityDownResult::AccountNotFound => {
                self.stats.deprioritize_failed += 1;
            }
        }
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
        result.insert("bootstrap_verify_blocks", "nothing_new", self.nothing_new);
        result.insert("bootstrap", "blocks", self.blocks);
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
        let mut queue = BootstrapQueue::default();

        let query = RunningQuery::new_test_instance();
        let response = BlocksAckPayload::new_test_instance();
        let ok = processor.process(&mut queue, &query, response);
        assert!(!ok);
        assert_eq!(processor.stats.invalid, 1);
    }

    #[test]
    fn handle_empty_response() {
        let mut processor = BlockAckProcessor::default();
        let mut queue = BootstrapQueue::default();
        let account = Account::from(42);

        let query = RunningQuery {
            query_type: QueryType::BlocksByAccount,
            account,
            ..RunningQuery::new_test_instance()
        };

        let response = BlocksAckPayload::empty();
        let ok = processor.process(&mut queue, &query, response);

        assert!(ok);
        assert_eq!(processor.stats.nothing_new, 1);
    }
}
