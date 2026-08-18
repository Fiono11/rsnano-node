use rand::RngExt;
use std::collections::VecDeque;

use rsnano_types::{Amount, Block, BlockHash, Link, PublicKey, StateBlockArgs, WorkNonce};

use crate::domain::AccountMap;

pub(crate) struct BlockFactory {
    max_blocks: usize,
    created: usize,
    account_map: AccountMap,
    strategy: SpamStrategy,
    one_shot_accounts: VecDeque<rsnano_types::Account>,
    live_representatives: Vec<PublicKey>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpamStrategy {
    SendReceive,
    Change,
    OneChangePerAccount,
    OneSendPerAccount,
}

impl BlockFactory {
    #[cfg(test)]
    pub(crate) fn new(account_map: AccountMap, max_blocks: usize, strategy: SpamStrategy) -> Self {
        Self::new_with_live_representatives(account_map, max_blocks, strategy, Vec::new())
    }

    pub(crate) fn new_with_live_representatives(
        mut account_map: AccountMap,
        max_blocks: usize,
        strategy: SpamStrategy,
        live_representatives: Vec<PublicKey>,
    ) -> Self {
        // Ordinary send/receive traffic is partitioned into stable
        // representative shards. Assign unopened accounts up front using the
        // historical address-byte mapping; their eventual open block will keep
        // this assignment. Already-open accounts retain their ledger
        // representative, and special workload strategies are left unchanged.
        if strategy == SpamStrategy::SendReceive && !live_representatives.is_empty() {
            let accounts = account_map.accounts().clone();
            for account in accounts {
                let state = account_map.state(&account).unwrap();
                if state.confirmed_frontier.is_zero() {
                    let index = usize::from(account.as_bytes()[0]) % live_representatives.len();
                    account_map.set_representative(account, live_representatives[index]);
                }
            }
        }
        let one_shot_accounts = if matches!(
            strategy,
            SpamStrategy::OneChangePerAccount | SpamStrategy::OneSendPerAccount
        ) {
            account_map
                .accounts()
                .iter()
                .copied()
                .filter(|account| {
                    account_map
                        .state(account)
                        .is_some_and(|state| state.confirmed() && !state.balance.is_zero())
                })
                .collect()
        } else {
            VecDeque::new()
        };
        Self {
            max_blocks,
            created: 0,
            account_map,
            strategy,
            one_shot_accounts,
            live_representatives,
        }
    }

    pub fn create_next(&mut self, is_fork: bool) -> Option<BlockResult> {
        if self.max_blocks_reached() {
            return None;
        }

        let block_result = match self.strategy {
            SpamStrategy::SendReceive => create_send_or_receive_block(
                &mut self.account_map,
                is_fork,
                &self.live_representatives,
            ),
            SpamStrategy::Change => {
                // TODO: use is_fork flag
                create_change_block(&mut self.account_map)
            }
            SpamStrategy::OneChangePerAccount => {
                let account = self.one_shot_accounts.pop_front()?;
                create_change_block_for(&mut self.account_map, account, &self.live_representatives)
            }
            SpamStrategy::OneSendPerAccount => {
                let account = self.one_shot_accounts.pop_front()?;
                create_send_block_for(&mut self.account_map, account, is_fork)
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

fn create_send_or_receive_block(
    account_map: &mut AccountMap,
    is_fork: bool,
    live_representatives: &[PublicKey],
) -> BlockResult {
    if let Some((receiver, send_hash, amount_sent)) = account_map.next_receivable() {
        let state = account_map.state(&receiver).unwrap();
        assert!(state.confirmed());
        // Unopened accounts were assigned to a live-representative shard when
        // the ordinary workload was created. Receiving must preserve that
        // assignment so the sender's delegated weight stays inside its shard.
        let representative = state.representative;
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
        account_map.set_representative(receiver, representative);
        result
    } else if let Some(state) = account_map.random_account_that_can_send() {
        assert!(state.confirmed());
        let destination = if live_representatives.is_empty() {
            // Preserve the legacy workload when no live PR list is configured.
            account_map.random_account().unwrap()
        } else {
            let destination_representative = if live_representatives.contains(&state.representative)
            {
                state.representative
            } else {
                // Compatibility for a pre-existing database whose funded
                // sender predates live-PR setup. Current RAI setup always puts
                // funded senders in the configured list.
                let index =
                    usize::from(state.key.account().as_bytes()[0]) % live_representatives.len();
                live_representatives[index]
            };
            account_map
                .random_account_for_representative(destination_representative)
                .expect("every configured representative shard must have an assigned account")
        };
        let new_balance: Amount = rand::rng().random_range(..state.balance.number()).into();
        let amount_sent = state.balance - new_balance;

        let send: Block = StateBlockArgs {
            key: &state.key,
            previous: state.confirmed_frontier,
            // A state send must preserve the account's current delegation.
            // Changing it to the account key silently moves committee weight
            // to spam keys which the test nodes do not host as representatives.
            representative: state.representative,
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
    account_map.process_change(
        state.key.account(),
        block.hash(),
        block.representative_field().unwrap(),
    );
    BlockResult::Block(Forks::new(block))
}

fn create_change_block_for(
    account_map: &mut AccountMap,
    account: rsnano_types::Account,
    live_representatives: &[PublicKey],
) -> BlockResult {
    let state = account_map
        .state(&account)
        .expect("prepared account must exist");
    assert!(
        live_representatives.len() >= 2,
        "one-change workload requires at least two live representatives"
    );
    let current_index = live_representatives
        .iter()
        .position(|representative| *representative == state.representative)
        .expect("prepared account representative must be one of the live representatives");
    let representative = live_representatives[(current_index + 1) % live_representatives.len()];
    assert_ne!(
        representative, state.representative,
        "one-change workload requires distinct live representatives"
    );
    let block: Block = StateBlockArgs {
        key: &state.key,
        previous: state.confirmed_frontier,
        representative,
        balance: state.balance,
        link: Link::ZERO,
        work: WorkNonce::new(0),
    }
    .into();
    account_map.process_change(account, block.hash(), representative);
    BlockResult::Block(Forks::new(block))
}

fn create_send_block_for(
    account_map: &mut AccountMap,
    account: rsnano_types::Account,
    is_fork: bool,
) -> BlockResult {
    let account_index = account_map
        .accounts()
        .iter()
        .position(|candidate| *candidate == account)
        .expect("prepared account must be indexed");
    let destination = account_map.accounts()[(account_index + 1) % account_map.len()];
    let fork_destination = account_map.accounts()[(account_index + 2) % account_map.len()];
    let state = account_map
        .state(&account)
        .expect("prepared account must exist");
    assert!(state.confirmed());
    assert!(!state.balance.is_zero());

    let amount = Amount::raw(1);
    let new_balance = state.balance - amount;
    let send: Block = StateBlockArgs {
        key: &state.key,
        previous: state.confirmed_frontier,
        representative: state.representative,
        balance: new_balance,
        link: destination.into(),
        work: 0.into(),
    }
    .into();

    let fork: Option<Block> = if is_fork {
        Some(
            StateBlockArgs {
                key: &state.key,
                previous: state.confirmed_frontier,
                representative: state.representative,
                balance: new_balance,
                link: fork_destination.into(),
                work: 0.into(),
            }
            .into(),
        )
    } else {
        None
    };
    let send_hash = send.hash();
    let fork_hash = fork.as_ref().map(|block| block.hash());
    account_map.process_send(account, destination, send_hash, amount, fork_hash);

    match fork {
        Some(fork) => BlockResult::Block(Forks::new_fork(send, fork)),
        None => BlockResult::Block(Forks::new(send)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{RngExt, SeedableRng, rngs::StdRng};
    use rsnano_types::PrivateKey;
    use std::time::{Duration, Instant};

    const MAX_BLOCKS: usize = 4;

    fn delegated_weights(account_map: &AccountMap, representatives: &[PublicKey]) -> Vec<Amount> {
        representatives
            .iter()
            .map(|representative| {
                account_map
                    .account_states
                    .values()
                    .filter(|state| state.representative == *representative)
                    .fold(Amount::ZERO, |sum, state| sum + state.balance)
            })
            .collect()
    }

    #[test]
    fn initial_send_to_random_account() {
        let mut account_map = test_account_map();
        let representative = PublicKey::from(999);
        account_map.set_representative(initial_test_key().account(), representative);
        let mut block_factory =
            BlockFactory::new(account_map, MAX_BLOCKS, SpamStrategy::SendReceive);
        let block = block_factory.create_next(false).unwrap().unwrap();
        let account = block.account_field().unwrap();
        let destination = block.destination_or_link();

        assert_eq!(account, initial_test_key().account());
        assert_eq!(block.representative_field(), Some(representative));
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
    fn initial_receive_delegates_to_a_live_representative() {
        let representatives = vec![PublicKey::from(900), PublicKey::from(901)];
        let mut block_factory = BlockFactory::new_with_live_representatives(
            test_account_map(),
            MAX_BLOCKS,
            SpamStrategy::SendReceive,
            representatives.clone(),
        );
        let send = block_factory.create_next(false).unwrap().unwrap();
        block_factory.confirm(&send.hash());

        let receive = block_factory.create_next(false).unwrap().unwrap();
        let representative = receive.representative_field().unwrap();

        assert!(representatives.contains(&representative));
        assert_eq!(
            block_factory
                .account_map
                .state(&receive.account_field().unwrap())
                .unwrap()
                .representative,
            representative
        );
    }

    #[test]
    fn ordinary_sends_never_cross_live_representative_shards() {
        let representatives = (900..906).map(PublicKey::from).collect::<Vec<_>>();
        let mut account_map = AccountMap::default();
        for i in 1..=120 {
            let key = PrivateKey::from(i);
            let account = key.account();
            account_map.add_unopened(key);
            if i <= 6 {
                account_map.set_account_state(account, Amount::raw(1_000), BlockHash::from(i));
                account_map.set_representative(account, representatives[i as usize - 1]);
            }
        }
        let mut factory = BlockFactory::new_with_live_representatives(
            account_map,
            2_000,
            SpamStrategy::SendReceive,
            representatives,
        );

        while let Some(BlockResult::Block(forks)) = factory.create_next(false) {
            let block = &forks.block;
            // A send has an account destination as its link. Receives link to
            // a block hash, so identify sends from their pending receivable.
            let destination = block.destination_or_link();
            if factory.account_map.get_receivable(&destination).is_some() {
                let source_rep = block.representative_field().unwrap();
                let destination_rep = factory
                    .account_map
                    .state(&destination)
                    .unwrap()
                    .representative;
                assert_eq!(destination_rep, source_rep);
            }
            factory.confirm(&block.hash());
        }
    }

    #[test]
    fn ordinary_transfers_conserve_each_live_representatives_weight() {
        const BLOCKS: usize = 12_000;
        let representatives = (1_000..1_006).map(PublicKey::from).collect::<Vec<_>>();
        let mut account_map = AccountMap::default();
        let mut initially_unopened = std::collections::BTreeSet::new();
        for i in 1..=240 {
            let key = PrivateKey::from(i);
            let account = key.account();
            account_map.add_unopened(key);
            if i <= 6 {
                account_map.set_account_state(account, Amount::raw(10_000), BlockHash::from(i));
                account_map.set_representative(account, representatives[i as usize - 1]);
            } else {
                initially_unopened.insert(account);
            }
        }
        let mut factory = BlockFactory::new_with_live_representatives(
            account_map,
            BLOCKS,
            SpamStrategy::SendReceive,
            representatives.clone(),
        );
        let initial_weights = delegated_weights(&factory.account_map, &representatives);
        assert_eq!(initial_weights, vec![Amount::raw(10_000); 6]);
        let mut opened_receiver = false;

        while let Some(BlockResult::Block(forks)) = factory.create_next(false) {
            let block = forks.block;
            let account = block.account_field().unwrap();
            factory.confirm(&block.hash());

            if initially_unopened.contains(&account)
                && !factory
                    .account_map
                    .state(&account)
                    .unwrap()
                    .balance
                    .is_zero()
            {
                opened_receiver = true;
            }
            for state in factory.account_map.account_states.values() {
                if !state.balance.is_zero() {
                    assert!(representatives.contains(&state.representative));
                }
            }
            // Ordinary generation completes each confirmed send with its
            // receive before selecting another sender.
            if factory.account_map.next_receivable().is_none() {
                assert_eq!(
                    delegated_weights(&factory.account_map, &representatives),
                    initial_weights
                );
            }
        }

        assert!(opened_receiver);
        assert_eq!(
            delegated_weights(&factory.account_map, &representatives),
            initial_weights
        );
    }

    #[test]
    fn moving_all_sender_weight_stays_in_its_representative_shard() {
        let representatives = (2_000..2_006).map(PublicKey::from).collect::<Vec<_>>();
        let mut account_map = AccountMap::default();
        for i in 1..=12 {
            let key = PrivateKey::from(i);
            let account = key.account();
            account_map.add_unopened(key);
            if i <= 6 {
                account_map.set_account_state(account, Amount::raw(1), BlockHash::from(i));
                account_map.set_representative(account, representatives[i as usize - 1]);
            }
        }
        let mut factory = BlockFactory::new_with_live_representatives(
            account_map,
            2,
            SpamStrategy::SendReceive,
            representatives.clone(),
        );
        let initial = delegated_weights(&factory.account_map, &representatives);

        let send = factory.create_next(false).unwrap().unwrap();
        assert_eq!(send.balance_field(), Some(Amount::ZERO));
        factory.confirm(&send.hash());
        let receive = factory.create_next(false).unwrap().unwrap();
        factory.confirm(&receive.hash());

        assert_eq!(
            delegated_weights(&factory.account_map, &representatives),
            initial
        );
        assert!(representatives.contains(&receive.representative_field().unwrap()));
    }

    #[test]
    fn no_live_representatives_preserves_legacy_receiver_assignment() {
        let mut factory = BlockFactory::new(test_account_map(), 2, SpamStrategy::SendReceive);
        let send = factory.create_next(false).unwrap().unwrap();
        let destination = send.destination_or_link();
        let original_representative = factory
            .account_map
            .state(&destination)
            .unwrap()
            .representative;
        factory.confirm(&send.hash());
        let receive = factory.create_next(false).unwrap().unwrap();

        assert_eq!(
            receive.representative_field(),
            Some(original_representative)
        );
    }

    #[test]
    fn fixed_seed_regression_runs_beyond_1600_blocks() {
        const BLOCKS: usize = 6_000;
        const SEED: u64 = 0x5241_492d_4e41_4e4f;
        let mut account_map = AccountMap::default();
        for key in 1..=128 {
            account_map.add_unopened(PrivateKey::from(key));
        }
        account_map.set_account_state(
            PrivateKey::from(1).account(),
            Amount::nano(1_000_000),
            BlockHash::from(1),
        );
        let mut factory = BlockFactory::new(account_map, BLOCKS, SpamStrategy::SendReceive);
        let mut rng = StdRng::seed_from_u64(SEED);
        let deadline = Instant::now() + Duration::from_secs(30);

        while let Some(result) = factory.create_next(rng.random_bool(0.01)) {
            assert!(
                Instant::now() < deadline,
                "fixed-seed nanospam workload timed out"
            );
            match result {
                BlockResult::Block(forks) => factory.confirm(&forks.block.hash()),
                BlockResult::Waiting => std::thread::yield_now(),
            }
        }

        assert_eq!(factory.created(), BLOCKS);
    }

    #[test]
    fn one_change_per_account_uses_each_confirmed_frontier_once() {
        let live_representatives = vec![PublicKey::from(100), PublicKey::from(101)];
        let mut account_map = AccountMap::default();
        let mut expected = std::collections::BTreeSet::new();
        for i in 1..=3 {
            let key = PrivateKey::from(i);
            let account = key.account();
            account_map.add_unopened(key);
            account_map.set_account_state(account, Amount::raw(1), BlockHash::from(100 + i));
            account_map.set_representative(
                account,
                live_representatives[(i as usize - 1) % live_representatives.len()],
            );
            expected.insert(account);
        }
        let mut factory = BlockFactory::new_with_live_representatives(
            account_map,
            3,
            SpamStrategy::OneChangePerAccount,
            live_representatives,
        );
        let mut actual = std::collections::BTreeSet::new();
        while let Some(BlockResult::Block(forks)) = factory.create_next(false) {
            actual.insert(forks.block.account_field().unwrap());
        }

        assert_eq!(actual, expected);
        assert_eq!(factory.created(), 3);
    }

    #[test]
    fn one_change_per_account_keeps_all_weight_on_live_representatives() {
        let live_representatives = (100..106).map(PublicKey::from).collect::<Vec<_>>();
        let mut account_map = AccountMap::default();
        let mut original_representatives = std::collections::HashMap::new();
        let mut total_weight = Amount::ZERO;
        for i in 1..=12 {
            let key = PrivateKey::from(i);
            let account = key.account();
            let balance = Amount::raw(i as u128);
            let representative =
                live_representatives[(i as usize - 1) % live_representatives.len()];
            account_map.add_unopened(key);
            account_map.set_account_state(account, balance, BlockHash::from(100 + i));
            account_map.set_representative(account, representative);
            original_representatives.insert(account, representative);
            total_weight += balance;
        }
        let mut factory = BlockFactory::new_with_live_representatives(
            account_map,
            12,
            SpamStrategy::OneChangePerAccount,
            live_representatives.clone(),
        );

        while let Some(BlockResult::Block(forks)) = factory.create_next(false) {
            let account = forks.block.account_field().unwrap();
            let representative = forks.block.representative_field().unwrap();
            assert!(live_representatives.contains(&representative));
            assert_ne!(representative, original_representatives[&account]);
            assert_eq!(
                factory.account_map.state(&account).unwrap().representative,
                representative
            );
        }

        let delegated_to_live_representatives = factory
            .account_map
            .account_states
            .values()
            .filter(|state| live_representatives.contains(&state.representative))
            .fold(Amount::ZERO, |sum, state| sum + state.balance);
        assert_eq!(delegated_to_live_representatives, total_weight);
    }

    #[test]
    fn one_change_per_account_rotates_all_100_accounts_without_moving_pr_weight() {
        let live_representatives = (100..106).map(PublicKey::from).collect::<Vec<_>>();
        let mut account_map = AccountMap::default();
        let mut original_representatives = std::collections::BTreeMap::new();
        for i in 1..=100 {
            let key = PrivateKey::from(i);
            let account = key.account();
            let source_index = (i as usize - 1) % live_representatives.len();
            // 100 accounts do not divide evenly across six PRs. Give the 17
            // accounts in the first four buckets 16 raw each and the 16
            // accounts in the last two buckets 17 raw each, for exactly 272
            // raw of delegated weight per PR before the workload.
            let balance = if source_index < 4 {
                Amount::raw(16)
            } else {
                Amount::raw(17)
            };
            account_map.add_unopened(key);
            account_map.set_account_state(account, balance, BlockHash::from(100 + i));
            account_map.set_representative(account, live_representatives[source_index]);
            original_representatives.insert(account, source_index);
        }
        let mut factory = BlockFactory::new_with_live_representatives(
            account_map,
            100,
            SpamStrategy::OneChangePerAccount,
            live_representatives.clone(),
        );

        let mut changed = 0;
        let mut changed_accounts = std::collections::BTreeSet::new();
        while let Some(BlockResult::Block(forks)) = factory.create_next(false) {
            let block = forks.block;
            let account = block.account_field().unwrap();
            let source_index = original_representatives[&account];
            let expected_representative =
                live_representatives[(source_index + 1) % live_representatives.len()];
            assert_eq!(block.representative_field(), Some(expected_representative));
            assert_ne!(
                block.representative_field(),
                Some(live_representatives[source_index])
            );
            assert_eq!(block.link_field(), Some(Link::ZERO));
            assert_eq!(
                block.balance_field(),
                Some(factory.account_map.state(&account).unwrap().balance)
            );
            assert!(changed_accounts.insert(account));
            factory.confirm(&block.hash());
            changed += 1;
        }

        assert_eq!(changed, 100);
        assert_eq!(changed_accounts.len(), 100);
        let mut final_weights = std::collections::BTreeMap::<PublicKey, Amount>::new();
        for state in factory.account_map.account_states.values() {
            *final_weights.entry(state.representative).or_default() += state.balance;
        }
        assert_eq!(
            live_representatives
                .iter()
                .map(|representative| final_weights[representative])
                .collect::<Vec<_>>(),
            vec![Amount::raw(272); 6]
        );
    }

    #[test]
    fn one_send_per_account_uses_every_funded_account_once() {
        let mut account_map = AccountMap::default();
        let representative = PublicKey::from(900);
        let mut expected = std::collections::BTreeSet::new();
        for i in 1..=100 {
            let key = PrivateKey::from(i);
            let account = key.account();
            account_map.add_unopened(key);
            account_map.set_account_state(account, Amount::raw(10), BlockHash::from(100 + i));
            account_map.set_representative(account, representative);
            expected.insert(account);
        }
        let mut factory = BlockFactory::new(account_map, 100, SpamStrategy::OneSendPerAccount);
        let mut actual = std::collections::BTreeSet::new();

        while let Some(BlockResult::Block(forks)) = factory.create_next(false) {
            let block = forks.block;
            let account = block.account_field().unwrap();
            actual.insert(account);
            assert_eq!(block.representative_field(), Some(representative));
            assert_eq!(block.balance_field(), Some(Amount::raw(9)));
            factory.confirm(&block.hash());
        }

        assert_eq!(actual, expected);
        assert_eq!(factory.created(), 100);
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
