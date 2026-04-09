use rsnano_types::{Account, Block, BlockHash};
use rustc_hash::FxHashMap;
use std::collections::VecDeque;

pub(crate) struct BlockQueue {
    max_accounts: usize,
    accounts: FxHashMap<Account, AccountBlocks>,
    waiting_to_be_inserted_into_block_processor: FxHashMap<BlockHash, Account>,
    in_block_processor: FxHashMap<BlockHash, Account>,
    blocks: usize,
}

impl BlockQueue {
    pub const DEFAULT_MAX_ACCOUNTS: usize = 1024 * 64;

    pub fn with_max_accounts(max_accounts: usize) -> Self {
        Self {
            max_accounts,
            accounts: Default::default(),
            waiting_to_be_inserted_into_block_processor: Default::default(),
            in_block_processor: Default::default(),
            blocks: 0,
        }
    }

    #[allow(dead_code)]
    pub fn blocks(&self) -> usize {
        self.blocks
    }

    #[allow(dead_code)]
    pub fn accounts(&self) -> usize {
        self.accounts.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.accounts() == 0
    }

    pub const fn max_accounts(&self) -> usize {
        self.max_accounts
    }

    pub fn insert(&mut self, data: AccountBlocks) -> bool {
        if self.accounts.len() >= self.max_accounts() {
            return false;
        }

        let Some(first_hash) = data.blocks.front().map(|b| b.hash()) else {
            return false;
        };

        self.blocks += data.blocks.len();
        let account = data.account;

        if let Some(old) = self.accounts.insert(account, data) {
            self.blocks -= old.blocks.len();
            if let Some(front) = old.blocks.front() {
                self.waiting_to_be_inserted_into_block_processor
                    .remove(&front.hash());
                self.in_block_processor.remove(&front.hash());
            }
        }

        self.waiting_to_be_inserted_into_block_processor
            .insert(first_hash, account);

        true
    }

    pub fn contains_account(&self, account: &Account) -> bool {
        self.accounts.contains_key(account)
    }

    pub fn next_to_process(&self) -> Option<(&Block, u64)> {
        let (hash, account) = self
            .waiting_to_be_inserted_into_block_processor
            .iter()
            .next()?;

        let data = self.accounts.get(account).expect("account should be found");

        let block = data
            .blocks
            .front()
            .expect("There should be at least one block");

        debug_assert_eq!(*hash, block.hash());
        Some((block, data.query_id))
    }

    pub fn enqueued_for_processing(&mut self, block_hash: &BlockHash) {
        if let Some(account) = self
            .waiting_to_be_inserted_into_block_processor
            .remove(block_hash)
        {
            let old = self.in_block_processor.insert(*block_hash, account);
            debug_assert!(old.is_none());
        }
    }

    pub fn processed(&mut self, block_hash: &BlockHash) -> BlockInfo {
        let Some(account) = self.in_block_processor.remove(block_hash) else {
            return BlockInfo {
                account: None,
                was_last: false,
            };
        };

        let blocks = &mut self
            .accounts
            .get_mut(&account)
            .expect("account should be found")
            .blocks;

        let front = blocks
            .pop_front()
            .expect("At least one block should be in queue");

        debug_assert_eq!(front.hash(), *block_hash);
        self.blocks -= 1;

        let was_last = if let Some(next) = blocks.front() {
            self.waiting_to_be_inserted_into_block_processor
                .insert(next.hash(), account);

            false
        } else {
            self.accounts.remove(&account);
            true
        };

        BlockInfo {
            account: Some(account),
            was_last,
        }
    }

    pub fn processing_failed(&mut self, block_hash: &BlockHash) -> bool {
        let Some(account) = self.in_block_processor.remove(block_hash) else {
            return false;
        };

        let data = self
            .accounts
            .remove(&account)
            .expect("account should be found");

        self.blocks -= data.blocks.len();
        true
    }
}

impl Default for BlockQueue {
    fn default() -> Self {
        Self::with_max_accounts(Self::DEFAULT_MAX_ACCOUNTS)
    }
}

pub(crate) struct AccountBlocks {
    pub(crate) account: Account,
    pub(crate) query_id: u64,
    pub(crate) blocks: VecDeque<Block>,
}

pub(crate) struct BlockInfo {
    pub account: Option<Account>,
    pub was_last: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_empty_after_creation() {
        let queue = BlockQueue::default();
        assert_eq!(queue.blocks(), 0, "blocks");
        assert_eq!(queue.accounts(), 0, "accounts");
        assert!(queue.is_empty(), "Should be empty");
        assert_eq!(
            queue.contains_account(&Account::from(1)),
            false,
            "Should not contain account"
        );
        assert!(
            queue.next_to_process().is_none(),
            "Should not have anything to process"
        );
    }

    #[test]
    fn has_default_max_accounts() {
        let queue = BlockQueue::default();
        assert_eq!(BlockQueue::DEFAULT_MAX_ACCOUNTS, 1024 * 64);
        assert_eq!(queue.max_accounts(), BlockQueue::DEFAULT_MAX_ACCOUNTS);
    }

    #[test]
    fn enqueues_a_block() {
        let mut queue = BlockQueue::default();
        let account = Account::from(1);
        let block = Block::new_test_instance();

        queue.insert(AccountBlocks {
            account,
            query_id: 42,
            blocks: [block.clone()].into(),
        });

        assert_eq!(queue.accounts(), 1, "accounts");
        assert_eq!(queue.blocks(), 1, "blocks");
        assert!(queue.contains_account(&account));
        assert_eq!(queue.next_to_process().unwrap().0.hash(), block.hash());
    }

    #[test]
    fn enqueues_blocks_for_different_accounts() {
        let mut queue = BlockQueue::default();
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        queue.insert(AccountBlocks {
            account: account1,
            blocks: [Block::new_test_instance()].into(),
            query_id: 1,
        });
        queue.insert(AccountBlocks {
            account: account2,
            blocks: [Block::new_test_instance()].into(),
            query_id: 2,
        });
        assert_eq!(queue.accounts(), 2, "accounts");
        assert_eq!(queue.blocks(), 2, "blocks");
        assert!(queue.contains_account(&account1), "Should contain account1");
        assert!(queue.contains_account(&account2), "Should contain account2");
    }

    #[test]
    fn replaces_previously_enqueued_blocks_for_same_account() {
        let mut queue = BlockQueue::default();
        let account = Account::from(1);
        let block1 = Block::new_test_instance_with_key(1);
        let block2 = Block::new_test_instance_with_key(2);
        queue.insert(AccountBlocks {
            account,
            blocks: [block1].into(),
            query_id: 1,
        });
        queue.insert(AccountBlocks {
            account,
            blocks: [block2.clone()].into(),
            query_id: 2,
        });
        assert_eq!(queue.accounts(), 1, "accounts");
        assert_eq!(queue.blocks(), 1, "blocks");
        assert_eq!(queue.next_to_process().unwrap().0.hash(), block2.hash());
    }

    #[test]
    fn can_enqueue_multiple_blocks_for_one_account_at_once() {
        let mut queue = BlockQueue::default();
        let account = Account::from(1);
        let block1 = Block::new_test_instance_with_key(1);
        let block2 = Block::new_test_instance_with_key(2);

        queue.insert(AccountBlocks {
            account,
            blocks: [block1.clone(), block2].into(),
            query_id: 1,
        });

        assert_eq!(queue.accounts(), 1, "accounts");
        assert_eq!(queue.blocks(), 2, "blocks");
        assert_eq!(queue.next_to_process().unwrap().0.hash(), block1.hash());
    }

    #[test]
    fn max_accounts_is_configurable() {
        let queue = BlockQueue::with_max_accounts(3);
        assert_eq!(queue.max_accounts(), 3);
    }

    #[test]
    fn dont_enqueue_when_max_accounts_reached() {
        let mut queue = BlockQueue::with_max_accounts(3);
        let enqueued = queue.insert(AccountBlocks {
            account: Account::from(1),
            blocks: [
                Block::new_test_instance_with_key(10),
                Block::new_test_instance_with_key(20),
            ]
            .into(),
            query_id: 0,
        });
        assert!(enqueued, "Should enqueue account1");

        let enqueued = queue.insert(AccountBlocks {
            account: Account::from(2),
            blocks: [
                Block::new_test_instance_with_key(30),
                Block::new_test_instance_with_key(40),
            ]
            .into(),
            query_id: 0,
        });
        assert!(enqueued, "Should enqueue account2");

        let enqueued = queue.insert(AccountBlocks {
            account: Account::from(3),
            blocks: [
                Block::new_test_instance_with_key(50),
                Block::new_test_instance_with_key(60),
            ]
            .into(),
            query_id: 0,
        });
        assert!(enqueued, "Should enqueue account3");

        let enqueued = queue.insert(AccountBlocks {
            account: Account::from(4),
            blocks: [
                Block::new_test_instance_with_key(70),
                Block::new_test_instance_with_key(80),
            ]
            .into(),
            query_id: 0,
        });
        assert_eq!(enqueued, false, "Should NOT enqueue account4");

        assert_eq!(queue.accounts(), 3, "accounts");
        assert_eq!(queue.blocks(), 6, "blocks");
        assert!(
            queue.contains_account(&Account::from(1)),
            "Should contain account1"
        );
        assert!(
            queue.contains_account(&Account::from(2)),
            "Should contain account2"
        );
        assert!(
            queue.contains_account(&Account::from(3)),
            "Should contain account3"
        );
        assert_eq!(
            queue.contains_account(&Account::from(4)),
            false,
            "Should NOT contain account4"
        );
    }

    #[test]
    fn process_multiple_blocks() {
        let mut queue = BlockQueue::default();
        let block1 = Block::new_test_instance_with_key(10);
        let block2 = Block::new_test_instance_with_key(20);

        queue.insert(AccountBlocks {
            account: Account::from(1),
            blocks: [block1.clone(), block2.clone()].into(),
            query_id: 0,
        });
        assert_eq!(
            queue.next_to_process().unwrap().0.hash(),
            block1.hash(),
            "Should require processing for block1"
        );
        assert_eq!(queue.blocks(), 2);

        queue.enqueued_for_processing(&block1.hash());
        assert!(
            queue.next_to_process().is_none(),
            "Should require no processing after block1 enqueued"
        );
        assert_eq!(queue.blocks(), 2);

        queue.processed(&block1.hash());
        assert_eq!(
            queue.next_to_process().unwrap().0.hash(),
            block2.hash(),
            "Should require processing for block2"
        );
        assert_eq!(queue.blocks(), 1);

        queue.enqueued_for_processing(&block2.hash());
        assert!(
            queue.next_to_process().is_none(),
            "Should require no processing after block2 enqueued"
        );
        assert_eq!(queue.blocks(), 1);

        queue.processed(&block2.hash());
        assert!(
            queue.next_to_process().is_none(),
            "Should require no processing after block2 processed"
        );
        assert_eq!(queue.blocks(), 0, "blocks");
        assert_eq!(queue.accounts(), 0, "accounts");
    }

    #[test]
    fn when_procesing_failed_should_remove_all_remaining_blocks_for_the_failed_account() {
        let mut queue = BlockQueue::default();
        let block1 = Block::new_test_instance_with_key(10);
        let block2 = Block::new_test_instance_with_key(20);

        queue.insert(AccountBlocks {
            account: Account::from(1),
            blocks: [block1.clone(), block2.clone()].into(),
            query_id: 0,
        });
        queue.enqueued_for_processing(&block1.hash());
        queue.processing_failed(&block1.hash());

        assert_eq!(queue.accounts(), 0);
        assert_eq!(queue.blocks(), 0);
    }
}
