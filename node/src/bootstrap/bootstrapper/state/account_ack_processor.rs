use rsnano_messages::AccountInfoAckPayload;
use rsnano_types::{Account, BlockHash};
use rsnano_utils::stats::{StatsCollection, StatsSource};

use crate::bootstrap::bootstrapper::state::{BootstrapQueue, PriorityUpResult, RunningQuery};

#[derive(Default)]
pub(super) struct AccountAckProcessor {
    stats: AccountAckStats,
}

impl AccountAckProcessor {
    pub fn process(
        &mut self,
        queue: &mut BootstrapQueue,
        query: &RunningQuery,
        response: &AccountInfoAckPayload,
    ) -> bool {
        if response.account.is_zero() {
            self.stats.empty += 1;
            // OK, but nothing to do
            return true;
        }

        // Prioritize account containing the dependency
        self.update_dependency(queue, &query.hash, response.account);
        self.prioritize(queue, &response.account);

        // OK, no way to verify the response
        true
    }

    fn update_dependency(
        &mut self,
        queue: &mut BootstrapQueue,
        dependency: &BlockHash,
        dependency_account: Account,
    ) {
        let updated = queue.dependency_update(dependency, dependency_account);

        if updated > 0 {
            self.stats.dependency_update += updated as u64;
        } else {
            self.stats.dependency_update_failed += 1;
        }
    }

    fn prioritize(&mut self, queue: &mut BootstrapQueue, account: &Account) {
        if matches!(
            queue.priority_up(account),
            PriorityUpResult::Inserted | PriorityUpResult::Updated
        ) {
            self.stats.priority_insert += 1;
        } else {
            self.stats.prioritize_failed += 1;
        };
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
    pub dependency_update: u64,
    pub dependency_update_failed: u64,
    pub priority_insert: u64,
    pub prioritize_failed: u64,
}

impl StatsSource for AccountAckStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const PROCESSOR: &str = "bootstr_acc_ack_proc";

        result.insert(PROCESSOR, "account_info_empty", self.empty);
        result.insert(PROCESSOR, "dependency_update", self.dependency_update);
        result.insert(
            PROCESSOR,
            "dependency_update_failed",
            self.dependency_update_failed,
        );
        result.insert(PROCESSOR, "priority_insert", self.priority_insert);
        result.insert(PROCESSOR, "prioritize_failed", self.prioritize_failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::bootstrapper::state::Priority;
    use rsnano_nullable_clock::Timestamp;

    #[test]
    fn empty_response() {
        let mut processor = AccountAckProcessor::default();
        let mut queue = BootstrapQueue::default();
        let query = RunningQuery::new_test_instance();

        let response = AccountInfoAckPayload {
            account: Account::ZERO,
            ..AccountInfoAckPayload::new_test_instance()
        };

        assert!(processor.process(&mut queue, &query, &response));

        assert_eq!(processor.stats.empty, 1);
        assert_eq!(queue.prioritized_len(), 0);
    }

    #[test]
    fn when_not_blocked_should_only_prioritize() {
        let mut processor = AccountAckProcessor::default();
        let mut queue = BootstrapQueue::default();
        let query = RunningQuery::new_test_instance();
        let response = AccountInfoAckPayload::new_test_instance();

        assert!(processor.process(&mut queue, &query, &response));

        assert!(queue.prioritized(&response.account));
        assert_eq!(processor.stats.dependency_update_failed, 1);
        assert_eq!(processor.stats.priority_insert, 1);
    }

    #[test]
    fn update_dependency() {
        let mut processor = AccountAckProcessor::default();
        let mut queue = BootstrapQueue::default();
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

        queue.priority_set(&blocked_account, Priority::INITIAL);

        queue.block(
            blocked_account,
            unknown_source,
            Timestamp::new_test_instance(),
        );

        assert!(processor.process(&mut queue, &query, &response));

        assert!(queue.blocked(&blocked_account));
        assert!(queue.prioritized(&source_account));
        let target = queue.next_target(Timestamp::new_test_instance(), |_| true);
        assert_eq!(target.account, source_account);
        assert_eq!(processor.stats.dependency_update, 1);
        assert_eq!(processor.stats.priority_insert, 1);
    }

    #[test]
    fn dependency_update_fails() {
        let mut processor = AccountAckProcessor::default();
        let mut queue = BootstrapQueue::default();

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
        queue.block(
            blocked_account,
            unknown_source,
            Timestamp::new_test_instance(),
        );
        queue.dependency_update(&unknown_source, source_account);
        queue.priority_up(&source_account);

        assert!(processor.process(&mut queue, &query, &response));

        assert_eq!(processor.stats.dependency_update_failed, 1);
    }
}
