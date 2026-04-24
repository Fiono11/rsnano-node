use rsnano_messages::AccountInfoAckPayload;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use crate::bootstrap::bootstrapper::logic::{BootstrapQueue, RunningQuery};

#[derive(Default)]
pub(crate) struct AccountAckProcessor {
    stats: AccountAckStats,
}

impl AccountAckProcessor {
    pub fn process(
        &mut self,
        queue: &BootstrapQueue,
        query: &RunningQuery,
        response: &AccountInfoAckPayload,
    ) -> bool {
        if response.account.is_zero() {
            queue.dependency_account_not_found(&query.hash);
            self.stats.empty += 1;
            // OK, but nothing to do
            return true;
        }

        // Prioritize account containing the dependency
        queue.dependency_update(&query.hash, response.account);
        // OK, no way to verify the response
        true
    }
}

impl StatsSource for AccountAckProcessor {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result);
    }
}

#[derive(Default)]
pub(super) struct AccountAckStats {
    pub empty: u64,
}

impl StatsSource for AccountAckStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const PROCESSOR: &str = "bootstr_acc_ack_proc";
        result.insert(PROCESSOR, "account_info_empty", self.empty);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::bootstrapper::logic::Priority;
    use rsnano_types::{Account, BlockHash};

    #[test]
    fn empty_response() {
        let mut processor = AccountAckProcessor::default();
        let mut queue = BootstrapQueue::new_null();
        let query = RunningQuery::new_test_instance();

        let response = AccountInfoAckPayload {
            account: Account::ZERO,
            ..AccountInfoAckPayload::new_test_instance()
        };

        assert!(processor.process(&mut queue, &query, &response));

        assert_eq!(processor.stats.empty, 1);
        assert_eq!(queue.info().download_queue, 0);
    }

    #[test]
    fn update_dependency() {
        let mut processor = AccountAckProcessor::default();
        let mut queue = BootstrapQueue::new_null();
        let blocked_account = Account::from(100);
        let unknown_source = BlockHash::from(42);
        let source_account = Account::from(200);

        let query = RunningQuery {
            hash: unknown_source,
            ..RunningQuery::new_test_instance()
        };

        let response = AccountInfoAckPayload {
            account: source_account,
            ..AccountInfoAckPayload::new_test_instance()
        };

        queue.priority_up_to(&blocked_account, Priority::INITIAL);

        queue.block(blocked_account, unknown_source);

        assert!(processor.process(&mut queue, &query, &response));

        assert!(queue.blocked(&blocked_account));
        assert!(queue.contains(&source_account));
        let (target, _) = queue.next_download_target().unwrap();
        assert_eq!(target, source_account);
    }

    #[test]
    fn dependency_update_fails() {
        let mut processor = AccountAckProcessor::default();
        let mut queue = BootstrapQueue::new_null();

        let blocked_account = Account::from(100);
        let unknown_source = BlockHash::from(42);
        let source_account = Account::from(200);

        let query = RunningQuery {
            hash: unknown_source,
            ..RunningQuery::new_test_instance()
        };

        let response = AccountInfoAckPayload {
            account: source_account,
            ..AccountInfoAckPayload::new_test_instance()
        };

        queue.priority_up(&blocked_account);
        queue.block(blocked_account, unknown_source);
        queue.dependency_update(&unknown_source, source_account);
        queue.priority_up(&source_account);

        assert!(processor.process(&mut queue, &query, &response));
    }
}
