use rand::Rng;

use rsnano_types::{Amount, Block, BlockHash, Link, PublicKey, StateBlockArgs, WorkNonce};

use crate::domain::AccountMap;

pub(crate) struct BlockFactory {
    max_blocks: usize,
    created: usize,
    account_map: AccountMap,
    strategy: SpamStrategy,
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum BlockResult {
    Block(Forks),
    Waiting,
}

pub(crate) struct Forks {
    pub block: Block,
    pub fork: Option<Block>,
}

impl Forks {
    pub(crate) fn new(block: Block) -> Self {
        Self { block, fork: None }
    }

    pub(crate) fn new_fork(block: Block, fork: Block) -> Self {
        Self {
            block,
            fork: Some(fork),
        }
    }
}

impl BlockResult {
    #[allow(dead_code)]
    pub fn unwrap(self) -> Block {
        match self {
            BlockResult::Waiting => panic!("Expected block, but was in waiting state"),
            BlockResult::Block(forks) => forks.block.clone(),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SpamStrategy {
    SendReceive,
    Change,
}

impl BlockFactory {
    pub(crate) fn new(account_map: AccountMap, max_blocks: usize, strategy: SpamStrategy) -> Self {
        Self {
            max_blocks,
            created: 0,
            account_map,
            strategy,
        }
    }

    pub fn create_next(&mut self, is_fork: bool) -> Option<BlockResult> {
        if self.max_blocks_reached() {
            return None;
        }

        let block_result = match self.strategy {
            SpamStrategy::SendReceive => {
                create_send_or_receive_block(&mut self.account_map, is_fork)
            }
            SpamStrategy::Change => {
                // TODO: use is_fork flag
                create_change_block(&mut self.account_map)
            }
        };

        if matches!(block_result, BlockResult::Block(_)) {
            self.created += 1;
        }

        Some(block_result)
    }

    pub fn max_blocks(&self) -> usize {
        self.max_blocks
    }

    pub fn max_blocks_reached(&mut self) -> bool {
        self.max_blocks > 0 && self.created >= self.max_blocks
    }

    pub fn confirm(&mut self, hash: &BlockHash) {
        self.account_map.confirm(hash);
    }

    pub fn created(&self) -> usize {
        self.created
    }
}

fn create_send_or_receive_block(account_map: &mut AccountMap, is_fork: bool) -> BlockResult {
    if let Some((receiver, send_hash, amount_sent)) = account_map.next_receivable() {
        let state = account_map.state(&receiver).unwrap();
        assert!(state.confirmed());
        let receive: Block = StateBlockArgs {
            key: &state.key,
            previous: state.confirmed_frontier,
            representative: state.key.public_key(),
            balance: state.balance + amount_sent,
            link: send_hash.into(),
            work: 0.into(),
        }
        .into();

        let receive_hash = receive.hash();
        let mut fork_hash = None;

        let result = if is_fork {
            let fork: Block = StateBlockArgs {
                key: &state.key,
                previous: state.confirmed_frontier,
                representative: PublicKey::from(1), // Different Rep!
                balance: state.balance + amount_sent,
                link: send_hash.into(),
                work: 0.into(),
            }
            .into();

            fork_hash = Some(fork.hash());

            BlockResult::Block(Forks::new_fork(receive, fork))
        } else {
            BlockResult::Block(Forks::new(receive))
        };
        account_map.process_receive(receiver, send_hash, receive_hash, fork_hash);
        result
    } else if let Some(state) = account_map.random_account_that_can_send() {
        assert!(state.confirmed());
        let destination = account_map.random_account().unwrap();
        let new_balance: Amount = rand::rng().random_range(..state.balance.number()).into();
        let amount_sent = state.balance - new_balance;

        let send: Block = StateBlockArgs {
            key: &state.key,
            previous: state.confirmed_frontier,
            representative: state.key.public_key(),
            balance: new_balance,
            link: destination.into(),
            work: 0.into(),
        }
        .into();

        let send_hash = send.hash();
        let mut fork_hash = None;
        let result = if is_fork {
            let fork: Block = StateBlockArgs {
                key: &state.key,
                previous: state.confirmed_frontier,
                representative: PublicKey::from(1), // Different Rep!
                balance: new_balance,
                link: destination.into(),
                work: 0.into(),
            }
            .into();
            fork_hash = Some(fork.hash());
            BlockResult::Block(Forks::new_fork(send, fork))
        } else {
            BlockResult::Block(Forks::new(send))
        };

        account_map.process_send(
            state.key.account(),
            destination,
            send_hash,
            amount_sent,
            fork_hash,
        );

        result
    } else {
        BlockResult::Waiting
    }
}

fn create_change_block(account_map: &mut AccountMap) -> BlockResult {
    let Some(state) = account_map.random_account_that_can_send() else {
        return BlockResult::Waiting;
    };
    let block: Block = StateBlockArgs {
        key: &state.key,
        previous: state.confirmed_frontier,
        representative: PublicKey::from_bytes(rand::rng().random()),
        balance: state.balance,
        link: Link::ZERO,
        work: WorkNonce::new(0),
    }
    .into();
    account_map.process_change(state.key.account(), block.hash());
    BlockResult::Block(Forks::new(block))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::PrivateKey;
    use std::time::Instant;

    const MAX_BLOCKS: usize = 4;

    #[test]
    fn initial_send_to_random_account() {
        let mut block_factory =
            BlockFactory::new(test_account_map(), MAX_BLOCKS, SpamStrategy::SendReceive);
        let block = block_factory.create_next(false).unwrap().unwrap();
        let account = block.account_field().unwrap();
        let destination = block.destination_or_link();

        assert_eq!(account, initial_test_key().account());
        assert!(block_factory.account_map.contains(&destination));
        assert!(
            block_factory
                .account_map
                .get_receivable(&destination)
                .is_some()
        );
    }

    #[test]
    fn initial_receive() {
        let mut block_factory =
            BlockFactory::new(test_account_map(), MAX_BLOCKS, SpamStrategy::SendReceive);
        // genesis send
        let send = block_factory.create_next(false).unwrap().unwrap();
        block_factory.confirm(&send.hash());
        let account = send.destination_or_link();

        let receive = block_factory.create_next(false).unwrap().unwrap();
        assert_eq!(receive.account_field().unwrap(), account);
        assert_eq!(receive.link_field().unwrap(), send.hash().into());
    }

    #[test]
    #[ignore = "run manually only"]
    fn benchmark() {
        let mut account_map = AccountMap::default();
        let initial_key = PrivateKey::new();
        account_map.add_unopened(initial_key.clone());
        account_map.set_account_state(initial_key.account(), Amount::nano(100_000_000), 123.into());
        for _ in 1..30_000 {
            account_map.add_unopened(PrivateKey::new());
        }

        let block_count = 10_000_000;

        let mut block_factory =
            BlockFactory::new(account_map, block_count, SpamStrategy::SendReceive);

        let mut start = Instant::now();
        let mut created_batch = 0;
        while let Some(BlockResult::Block(forks)) = block_factory.create_next(false) {
            block_factory.confirm(&forks.block.hash());
            created_batch += 1;
            if created_batch == 50_000 {
                println!(
                    "Created {} blocks. {} bps",
                    created_batch,
                    (created_batch as f64 / start.elapsed().as_secs_f64()) as i32
                );
                start = Instant::now();
                created_batch = 0;
            }
        }
        println!(
            "Created {} blocks. {} bps",
            created_batch,
            (created_batch as f64 / start.elapsed().as_secs_f64()) as i32
        );
    }

    fn test_account_map() -> AccountMap {
        let mut map = AccountMap::default();
        let initial_key = initial_test_key();
        map.add_unopened(initial_key.clone());
        map.set_account_state(
            initial_key.account(),
            Amount::nano(100_000_000),
            BlockHash::from(123),
        );
        map.add_unopened(1.into());
        map.add_unopened(2.into());
        map.add_unopened(3.into());
        map.add_unopened(4.into());
        map.add_unopened(5.into());
        map
    }

    fn initial_test_key() -> PrivateKey {
        PrivateKey::from(42)
    }
}
