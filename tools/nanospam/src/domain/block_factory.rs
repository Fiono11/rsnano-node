use rand::RngExt;

use rsnano_types::{Account, Amount, Block, BlockHash, Link, PublicKey, StateBlockArgs, WorkNonce};

use crate::domain::{AccountMap, AccountState, Representatives};

pub(crate) struct BlockFactory {
    max_blocks: usize,
    created: usize,
    account_map: AccountMap,
    strategy: SpamStrategy,
    /// RAI: the representatives the accounts delegate to; without any, an
    /// account is its own representative
    representatives: Representatives,
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
    pub(crate) fn new(
        account_map: AccountMap,
        max_blocks: usize,
        strategy: SpamStrategy,
        representatives: Representatives,
    ) -> Self {
        Self {
            max_blocks,
            created: 0,
            account_map,
            strategy,
            representatives,
        }
    }

    pub fn create_next(&mut self, is_fork: bool) -> Option<BlockResult> {
        if self.max_blocks_reached() {
            return None;
        }

        let block_result = match self.strategy {
            SpamStrategy::SendReceive => {
                create_send_or_receive_block(&mut self.account_map, is_fork, &self.representatives)
            }
            SpamStrategy::Change => {
                // TODO: use is_fork flag
                create_change_block(&mut self.account_map, &self.representatives)
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

/// The representative of an account's blocks: the one it delegates to, or
/// the account itself in a run without representatives and for the
/// initial account, whose balance counts for no representative
fn representative(state: &AccountState, representatives: &Representatives) -> PublicKey {
    if state.own_representative {
        return state.key.public_key();
    }
    representatives
        .of(&state.key.account())
        .unwrap_or_else(|| state.key.public_key())
}

/// A fork of a block is the same block with another representative: under
/// RAI it moves the account's weight to that representative if it wins
fn fork_representative(representative: PublicKey, representatives: &Representatives) -> PublicKey {
    representatives
        .other_than(representative)
        .unwrap_or_else(|| PublicKey::from(1))
}

fn create_send_or_receive_block(
    account_map: &mut AccountMap,
    is_fork: bool,
    representatives: &Representatives,
) -> BlockResult {
    if let Some((receiver, send_hash, amount_sent)) = account_map.next_receivable() {
        let state = account_map.state(&receiver).unwrap();
        assert!(state.confirmed());
        let is_fork = is_fork && can_fork(account_map, &receiver);
        let representative = representative(state, representatives);
        let receive: Block = StateBlockArgs {
            key: &state.key,
            previous: state.confirmed_frontier,
            representative,
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
                representative: fork_representative(representative, representatives),
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
        let is_fork = is_fork && can_fork(account_map, &state.key.account());
        let destination = account_map.random_account().unwrap();
        let new_balance: Amount = rand::rng().random_range(..state.balance.number()).into();
        let amount_sent = state.balance - new_balance;
        let representative = representative(state, representatives);

        let send: Block = StateBlockArgs {
            key: &state.key,
            previous: state.confirmed_frontier,
            representative,
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
                representative: fork_representative(representative, representatives),
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

/// The initial account funds every other account with its sends. A forked
/// root may never confirm (under Kudzu a 3-3 fork settles without a winner),
/// which would freeze the funds and with them the whole workload, so the
/// initial account's blocks are never forked.
fn can_fork(account_map: &AccountMap, account: &Account) -> bool {
    *account != account_map.initial_account()
}

/// A change to a random representative: under RAI pure delegation churn,
/// the balances stay where they are
fn create_change_block(
    account_map: &mut AccountMap,
    representatives: &Representatives,
) -> BlockResult {
    let Some(state) = account_map.random_account_that_can_send() else {
        return BlockResult::Waiting;
    };
    let representative = representatives
        .random()
        .unwrap_or_else(|| PublicKey::from_bytes(rand::rng().random()));
    let block: Block = StateBlockArgs {
        key: &state.key,
        previous: state.confirmed_frontier,
        representative,
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
        let mut block_factory = BlockFactory::new(
            test_account_map(),
            MAX_BLOCKS,
            SpamStrategy::SendReceive,
            Representatives::default(),
        );
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

    /// The initial account funds the whole run, so its blocks are never forked
    #[test]
    fn initial_account_is_never_forked() {
        let mut block_factory = BlockFactory::new(
            test_account_map(),
            MAX_BLOCKS,
            SpamStrategy::SendReceive,
            Representatives::default(),
        );
        let Some(BlockResult::Block(forks)) = block_factory.create_next(true) else {
            panic!("expected a block");
        };
        assert_eq!(
            forks.block.account_field().unwrap(),
            initial_test_key().account()
        );
        assert!(forks.fork.is_none());
        block_factory.confirm(&forks.block.hash());

        // The receiving account is an ordinary account and can be forked
        let Some(BlockResult::Block(forks)) = block_factory.create_next(true) else {
            panic!("expected a block");
        };
        assert_ne!(
            forks.block.account_field().unwrap(),
            initial_test_key().account()
        );
        assert!(forks.fork.is_some());
    }

    /// RAI: every block names the account's representative, a fork another one
    #[test]
    fn blocks_delegate_to_the_representatives_and_forks_to_another() {
        let representatives = Representatives::new(vec![
            PrivateKey::from(100).public_key(),
            PrivateKey::from(101).public_key(),
        ]);
        let mut block_factory = BlockFactory::new(
            test_account_map(),
            40,
            SpamStrategy::SendReceive,
            representatives.clone(),
        );
        // The initial account's sends are never forked and may go to itself
        let mut forked = 0;
        for _ in 0..20 {
            let Some(BlockResult::Block(forks)) = block_factory.create_next(true) else {
                panic!("expected a block");
            };
            let account = forks.block.account_field().unwrap();
            let rep = representatives.of(&account).unwrap();
            assert_eq!(forks.block.representative_field(), Some(rep));
            if let Some(fork) = &forks.fork {
                assert_eq!(fork.representative_field(), representatives.other_than(rep));
                assert_ne!(fork.representative_field(), Some(rep));
                forked += 1;
            }
            block_factory.confirm(&forks.block.hash());
        }
        assert!(forked > 0);
    }

    #[test]
    fn initial_receive() {
        let mut block_factory = BlockFactory::new(
            test_account_map(),
            MAX_BLOCKS,
            SpamStrategy::SendReceive,
            Representatives::default(),
        );
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

        let mut block_factory = BlockFactory::new(
            account_map,
            block_count,
            SpamStrategy::SendReceive,
            Representatives::default(),
        );

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
