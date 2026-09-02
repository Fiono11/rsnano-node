use rand::{
    rng,
    seq::{IndexedRandom, IteratorRandom},
};
use rsnano_types::{Account, Amount, BlockHash, PrivateKey};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Default)]
pub(crate) struct AccountMap {
    pub account_states: FxHashMap<Account, AccountState>,
    all_accounts: Vec<Account>,
    active_accounts: FxHashSet<Account>,
    active_accounts_vec: Vec<Account>,
    confirmed_accounts: FxHashSet<Account>,

    /// Account => Send block hash + amount sent
    receivable: FxHashMap<Account, Vec<(BlockHash, Amount)>>,

    /// Accounts that can receive and the send is confirmed
    /// Receiving account + send hash => amount
    confirmed_receivable: FxHashMap<(Account, BlockHash), Amount>,
    unconfirmed: FxHashMap<BlockHash, UnconfirmedEntry>,
    winner_to_primary: FxHashMap<BlockHash, BlockHash>,
}

struct UnconfirmedEntry {
    pub(crate) source: Account,
    /// Is only set for send-blocks
    pub(crate) destination: Option<Account>,
    pub(crate) fork: Option<BlockHash>,
    pub(crate) amount: Amount,
    pub(crate) consumed_send: Option<BlockHash>,
    pub(crate) previous: BlockHash,
    pub(crate) winner: Option<BlockHash>,
}

pub(crate) struct AccountState {
    pub key: PrivateKey,
    pub confirmed_frontier: BlockHash,
    pub finalized_frontier: BlockHash,
    pub unconfirmed_frontier: BlockHash,
    pub balance: Amount,
}

impl AccountState {
    pub fn confirmed(&self) -> bool {
        self.confirmed_frontier == self.unconfirmed_frontier
    }
}

impl AccountMap {
    pub fn fill(&mut self, count: usize) {
        for _ in 0..count {
            let key = PrivateKey::new();
            self.add_unopened(key);
        }
    }

    pub fn private_keys(&self) -> impl Iterator<Item = &PrivateKey> {
        self.account_states.values().map(|s| &s.key)
    }

    pub fn initial_key(&self) -> &PrivateKey {
        &self.account_states.get(&self.all_accounts[0]).unwrap().key
    }

    pub fn accounts(&self) -> &Vec<Account> {
        &self.all_accounts
    }

    pub fn set_account_state(&mut self, account: Account, balance: Amount, frontier: BlockHash) {
        let state = self.account_states.get_mut(&account).unwrap();
        state.balance = balance;
        state.unconfirmed_frontier = frontier;
        state.confirmed_frontier = frontier;
        state.finalized_frontier = frontier;
        self.confirmed_accounts.insert(account);
        self.active_accounts.insert(account);
        self.active_accounts_vec.push(account);
    }

    pub fn add_confirmed_receivable(
        &mut self,
        destination: Account,
        send_hash: BlockHash,
        amount: Amount,
    ) {
        self.receivable
            .entry(destination)
            .or_default()
            .push((send_hash, amount));
        self.confirmed_receivable
            .insert((destination, send_hash), amount);
    }

    pub fn add_unopened(&mut self, key: PrivateKey) {
        let account = key.account();
        self.all_accounts.push(account);
        self.account_states.insert(
            account,
            AccountState {
                key,
                confirmed_frontier: BlockHash::ZERO,
                finalized_frontier: BlockHash::ZERO,
                unconfirmed_frontier: BlockHash::ZERO,
                balance: Amount::ZERO,
            },
        );
        self.confirmed_accounts.insert(account);
    }

    pub fn state(&self, account: &Account) -> Option<&AccountState> {
        self.account_states.get(account)
    }

    pub fn random_account(&self) -> Option<Account> {
        self.all_accounts.choose(&mut rand::rng()).cloned()
    }

    pub fn process_send(
        &mut self,
        source: Account,
        destination: Account,
        send_hash: BlockHash,
        amount: Amount,
        fork: Option<BlockHash>,
    ) {
        let previous = self
            .account_states
            .get(&source)
            .map(|state| state.confirmed_frontier)
            .unwrap_or_default();
        self.receivable
            .entry(destination)
            .or_default()
            .push((send_hash, amount));

        if let Some(state) = self.account_states.get_mut(&source) {
            state.unconfirmed_frontier = send_hash;
            state.balance -= amount;
        }
        self.unconfirmed.insert(
            send_hash,
            UnconfirmedEntry {
                source,
                destination: Some(destination),
                fork,
                amount,
                consumed_send: None,
                previous,
                winner: None,
            },
        );
        self.confirmed_accounts.remove(&source);

        if self.active_accounts.insert(destination) {
            self.active_accounts_vec.push(destination);
        }
    }

    pub fn process_receive(
        &mut self,
        receiver: Account,
        send_hash: BlockHash,
        receive_hash: BlockHash,
        fork: Option<BlockHash>,
    ) {
        let entries = self
            .receivable
            .get_mut(&receiver)
            .expect("no receivables found");

        let pos = entries
            .iter()
            .position(|(hash, _)| *hash == send_hash)
            .expect("no receivable entry found for given send hash");

        let (send_hash, amount) = entries.remove(pos);

        if entries.is_empty() {
            self.receivable.remove(&receiver);
        }
        self.confirmed_receivable.remove(&(receiver, send_hash));
        self.confirmed_accounts.remove(&receiver);

        let state = self.account_states.get_mut(&receiver).unwrap();
        let previous = state.confirmed_frontier;
        state.balance += amount;
        state.unconfirmed_frontier = receive_hash;
        self.unconfirmed.insert(
            receive_hash,
            UnconfirmedEntry {
                source: receiver,
                destination: None,
                fork,
                amount,
                consumed_send: Some(send_hash),
                previous,
                winner: None,
            },
        );
    }

    pub fn process_change(&mut self, account: Account, hash: BlockHash) {
        let state = self.account_states.get_mut(&account).unwrap();
        let previous = state.confirmed_frontier;
        state.unconfirmed_frontier = hash;
        self.confirmed_accounts.remove(&account);
        self.unconfirmed.insert(
            hash,
            UnconfirmedEntry {
                source: account,
                destination: None,
                fork: None,
                amount: Amount::ZERO,
                consumed_send: None,
                previous,
                winner: None,
            },
        );
    }

    #[cfg(not(feature = "rai_protocol"))]
    pub fn confirm(&mut self, hash: &BlockHash) {
        let Some(entry) = self.unconfirmed.remove(hash) else {
            return;
        };

        if let Some(fork) = entry.fork {
            self.unconfirmed.remove(&fork);
        }

        if let Some(dest) = entry.destination
            && let Some(entries) = self.receivable.get(&dest)
            && let Some((_, amount)) = entries.iter().find(|(h, _)| h == hash)
        {
            self.confirmed_receivable.insert((dest, *hash), *amount);
        }

        let Some(state) = self.account_states.get_mut(&entry.source) else {
            return;
        };
        state.confirmed_frontier = *hash;
        if state.confirmed() {
            self.confirmed_accounts.insert(entry.source);
        }
    }

    #[cfg(all(test, not(feature = "rai_protocol")))]
    pub fn terminate(&mut self, hash: &BlockHash) {
        self.confirm(hash);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn terminate(&mut self, hash: &BlockHash) {
        let Some(primary) = self.primary_hash(hash) else {
            return;
        };
        let entry = self.unconfirmed.get_mut(&primary).unwrap();
        if entry.winner.is_some() {
            return;
        }
        entry.winner = Some(*hash);
        self.winner_to_primary.insert(*hash, primary);

        if let Some(dest) = entry.destination
            && let Some(entries) = self.receivable.get_mut(&dest)
            && let Some((receivable_hash, amount)) = entries.iter_mut().find(|(h, _)| *h == primary)
        {
            *receivable_hash = *hash;
            self.confirmed_receivable.insert((dest, *hash), *amount);
        }

        let Some(state) = self.account_states.get_mut(&entry.source) else {
            return;
        };
        state.confirmed_frontier = *hash;
        state.unconfirmed_frontier = *hash;
        if state.confirmed() {
            self.confirmed_accounts.insert(entry.source);
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn finalize(&mut self, hash: &BlockHash) -> Vec<BlockHash> {
        let Some(primary) = self.primary_hash(hash) else {
            return Vec::new();
        };
        if self
            .unconfirmed
            .get(&primary)
            .is_some_and(|entry| entry.winner.is_none())
        {
            // A finalization certificate implies notarization. Websocket delivery can expose
            // finalization before the corresponding termination notification.
            self.terminate(hash);
        }
        let mut stack = vec![primary];
        let mut visited = FxHashSet::default();
        let mut finalized = Vec::new();
        while let Some(primary) = stack.pop() {
            if !visited.insert(primary) {
                continue;
            }
            let Some(entry) = self.unconfirmed.get(&primary) else {
                continue;
            };
            let Some(winner) = entry.winner else {
                continue;
            };
            finalized.push((primary, winner, entry.source));
            if let Some(previous) = self.winner_to_primary.get(&entry.previous) {
                stack.push(*previous);
            }
            if let Some(source_send) = entry
                .consumed_send
                .and_then(|send| self.winner_to_primary.get(&send))
            {
                stack.push(*source_send);
            }
        }

        for (primary, winner, source) in finalized.iter().rev() {
            self.unconfirmed.remove(primary);
            self.winner_to_primary.remove(winner);
            if let Some(state) = self.account_states.get_mut(source) {
                state.finalized_frontier = *winner;
            }
        }
        finalized.into_iter().map(|(_, winner, _)| winner).collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rollback(&mut self, hash: &BlockHash) {
        let Some(primary) = self.primary_hash(hash) else {
            return;
        };
        let entry = self.unconfirmed.remove(&primary).unwrap();

        if let Some(destination) = entry.destination {
            if let Some(receivables) = self.receivable.get_mut(&destination) {
                receivables.retain(|(send, _)| *send != primary);
                if receivables.is_empty() {
                    self.receivable.remove(&destination);
                }
            }
            self.confirmed_receivable.remove(&(destination, primary));
            if let Some(state) = self.account_states.get_mut(&entry.source) {
                state.balance += entry.amount;
            }
        } else if let Some(send_hash) = entry.consumed_send {
            self.receivable
                .entry(entry.source)
                .or_default()
                .push((send_hash, entry.amount));
            self.confirmed_receivable
                .insert((entry.source, send_hash), entry.amount);
            if let Some(state) = self.account_states.get_mut(&entry.source) {
                state.balance -= entry.amount;
            }
        }

        if let Some(state) = self.account_states.get_mut(&entry.source) {
            state.unconfirmed_frontier = state.confirmed_frontier;
        }
        self.confirmed_accounts.insert(entry.source);
    }

    fn primary_hash(&self, hash: &BlockHash) -> Option<BlockHash> {
        if self.unconfirmed.contains_key(hash) {
            Some(*hash)
        } else if let Some(primary) = self.winner_to_primary.get(hash) {
            Some(*primary)
        } else {
            self.unconfirmed.iter().find_map(|(primary, entry)| {
                (entry.fork.as_ref() == Some(hash)).then_some(*primary)
            })
        }
    }

    #[allow(dead_code)]
    pub fn contains(&self, account: &Account) -> bool {
        self.account_states.contains_key(account)
    }

    #[allow(dead_code)]
    pub fn get_receivable(&self, account: &Account) -> Option<(BlockHash, Amount)> {
        let entries = self.receivable.get(account)?;
        entries.first().cloned()
    }

    pub fn next_receivable(&self) -> Option<(Account, BlockHash, Amount)> {
        self.confirmed_receivable.iter().take(100).find_map(
            |((receiving_account, send_hash), amount)| {
                if self.confirmed_accounts.contains(receiving_account) {
                    Some((*receiving_account, *send_hash, *amount))
                } else {
                    None
                }
            },
        )
    }

    pub fn random_account_that_can_send(&self) -> Option<&AccountState> {
        for _ in 0..100 {
            let account = self.active_accounts_vec.iter().choose(&mut rng())?;
            let state = self.account_states.get(account).unwrap();
            if state.confirmed() && !state.balance.is_zero() {
                return Some(state);
            }
        }
        None
    }

    pub fn len(&self) -> usize {
        self.all_accounts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntest::assert_false;

    #[test]
    fn empty() {
        let map = AccountMap::default();
        assert_eq!(map.get_receivable(&1.into()), None);
        assert_eq!(map.next_receivable(), None);
        assert_false!(map.contains(&1.into()));
        assert_eq!(map.random_account(), None);
        assert!(map.state(&Account::from(1)).is_none());
        assert!(map.random_account_that_can_send().is_none());
    }

    #[test]
    fn add_one_account() {
        let mut map = AccountMap::default();
        let key = PrivateKey::from(1);

        map.add_unopened(key.clone());

        assert!(map.contains(&key.account()));
        assert_eq!(
            map.state(&key.account()).unwrap().key.account(),
            key.account()
        );
        assert_eq!(map.random_account(), Some(key.account()));
        assert!(map.random_account_that_can_send().is_none());
    }

    #[test]
    fn process_send() {
        let mut map = AccountMap::default();
        let send_hash = BlockHash::from(42);
        let dest_key = PrivateKey::from(100);
        let dest_account = dest_key.account();
        let amount = Amount::nano(12_345);
        map.add_unopened(dest_key.clone());

        map.process_send(TEST_GENESIS_ACCOUNT, dest_account, send_hash, amount, None);
        map.terminate(&send_hash);

        assert_eq!(map.get_receivable(&dest_account), Some((send_hash, amount)));
        assert_eq!(
            map.next_receivable(),
            Some((dest_account, send_hash, amount))
        );
        assert!(map.random_account_that_can_send().is_none());
        assert_eq!(map.state(&dest_account).unwrap().balance, Amount::ZERO);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn confirming_send_fork_uses_winning_hash() {
        let mut map = AccountMap::default();
        let send_hash = BlockHash::from(42);
        let fork_hash = BlockHash::from(43);
        let source_key = PrivateKey::from(101);
        let source_account = source_key.account();
        let dest_key = PrivateKey::from(100);
        let dest_account = dest_key.account();
        let amount = Amount::nano(12_345);
        map.add_unopened(dest_key);
        map.add_unopened(source_key);
        map.set_account_state(
            source_account,
            Amount::nano(100_000_000),
            BlockHash::from(41),
        );

        map.process_send(
            source_account,
            dest_account,
            send_hash,
            amount,
            Some(fork_hash),
        );
        map.terminate(&fork_hash);

        assert_eq!(map.get_receivable(&dest_account), Some((fork_hash, amount)));
        assert_eq!(
            map.state(&source_account).unwrap().confirmed_frontier,
            fork_hash
        );
    }

    #[test]
    fn process_send_reduces_balance_of_sender() {
        let mut map = AccountMap::default();
        let key = PrivateKey::from(100);

        map.add_unopened(key.clone());

        let send_genesis_hash = BlockHash::from(42);
        let send_hash = BlockHash::from(43);
        let receive_hash = BlockHash::from(44);

        let amount = Amount::nano(12_345);

        map.process_send(
            TEST_GENESIS_ACCOUNT,
            key.account(),
            send_genesis_hash,
            amount,
            None,
        );
        map.terminate(&send_genesis_hash);
        map.process_receive(key.account(), send_genesis_hash, receive_hash, None);
        map.terminate(&receive_hash);
        map.process_send(
            key.account(),
            key.account(),
            send_hash,
            Amount::nano(1),
            None,
        );
        map.terminate(&send_hash);

        assert_eq!(
            map.state(&key.account()).unwrap().balance,
            Amount::nano(12_344)
        );
        assert_eq!(
            map.state(&key.account()).unwrap().confirmed_frontier,
            send_hash
        );
    }

    #[test]
    fn process_receive() {
        let mut map = AccountMap::default();
        let send_hash = BlockHash::from(42);
        let receive_hash = BlockHash::from(43);
        let dest_key = PrivateKey::from(100);
        let dest_account = dest_key.account();
        let amount = Amount::nano(12_345);
        map.add_unopened(dest_key.clone());

        map.process_send(TEST_GENESIS_ACCOUNT, dest_account, send_hash, amount, None);
        map.terminate(&send_hash);
        map.process_receive(dest_account, send_hash, receive_hash, None);
        map.terminate(&receive_hash);

        assert!(map.next_receivable().is_none());
        assert_eq!(map.state(&dest_account).unwrap().balance, amount);
        assert_eq!(
            map.state(&dest_account).unwrap().confirmed_frontier,
            receive_hash
        );
        assert_eq!(
            map.random_account_that_can_send().unwrap().key.account(),
            dest_account
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn confirming_receive_fork_uses_winning_hash() {
        let mut map = AccountMap::default();
        let send_hash = BlockHash::from(42);
        let receive_hash = BlockHash::from(43);
        let fork_hash = BlockHash::from(44);
        let dest_key = PrivateKey::from(100);
        let dest_account = dest_key.account();
        let amount = Amount::nano(12_345);
        map.add_unopened(dest_key);

        map.process_send(TEST_GENESIS_ACCOUNT, dest_account, send_hash, amount, None);
        map.terminate(&send_hash);
        map.process_receive(dest_account, send_hash, receive_hash, Some(fork_hash));
        map.terminate(&fork_hash);

        assert_eq!(
            map.state(&dest_account).unwrap().confirmed_frontier,
            fork_hash
        );
        assert_eq!(map.state(&dest_account).unwrap().balance, amount);
        assert!(map.random_account_that_can_send().is_some());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn notarization_advances_slot_without_finalizing() {
        let mut map = AccountMap::default();
        let key = PrivateKey::from(100);
        let initial = BlockHash::from(41);
        let block = BlockHash::from(42);
        map.add_unopened(key.clone());
        map.set_account_state(key.account(), Amount::nano(1), initial);

        map.process_change(key.account(), block);
        map.terminate(&block);

        let state = map.state(&key.account()).unwrap();
        assert_eq!(state.confirmed_frontier, block);
        assert_eq!(state.finalized_frontier, initial);
        assert!(state.confirmed());
        assert!(map.finalize(&BlockHash::from(999)).is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn finalizing_descendant_finalizes_notarized_ancestors() {
        let mut map = AccountMap::default();
        let key = PrivateKey::from(100);
        let initial = BlockHash::from(41);
        let first = BlockHash::from(42);
        let second = BlockHash::from(43);
        map.add_unopened(key.clone());
        map.set_account_state(key.account(), Amount::nano(1), initial);

        map.process_change(key.account(), first);
        map.terminate(&first);
        map.process_change(key.account(), second);
        map.terminate(&second);

        assert_eq!(map.finalize(&second), vec![second, first]);
        assert_eq!(
            map.state(&key.account()).unwrap().finalized_frontier,
            second
        );
        assert!(map.finalize(&first).is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn finalizing_receive_finalizes_source_send() {
        let mut map = AccountMap::default();
        let source = PrivateKey::from(100);
        let destination = PrivateKey::from(101);
        let source_frontier = BlockHash::from(40);
        let send = BlockHash::from(41);
        let receive = BlockHash::from(42);
        let amount = Amount::nano(1);
        map.add_unopened(source.clone());
        map.add_unopened(destination.clone());
        map.set_account_state(source.account(), amount, source_frontier);

        map.process_send(source.account(), destination.account(), send, amount, None);
        map.terminate(&send);
        map.process_receive(destination.account(), send, receive, None);
        map.terminate(&receive);

        let finalized = map.finalize(&receive);
        assert_eq!(finalized.len(), 2);
        assert!(finalized.contains(&send));
        assert!(finalized.contains(&receive));
        assert_eq!(
            map.state(&source.account()).unwrap().finalized_frontier,
            send
        );
        assert_eq!(
            map.state(&destination.account())
                .unwrap()
                .finalized_frontier,
            receive
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn timeout_rolls_back_without_advancing_slot() {
        let mut map = AccountMap::default();
        let key = PrivateKey::from(100);
        let initial = BlockHash::from(41);
        let timed_out = BlockHash::from(42);
        map.add_unopened(key.clone());
        map.set_account_state(key.account(), Amount::nano(1), initial);

        map.process_change(key.account(), timed_out);
        map.rollback(&timed_out);

        let state = map.state(&key.account()).unwrap();
        assert_eq!(state.confirmed_frontier, initial);
        assert_eq!(state.finalized_frontier, initial);
        assert_eq!(state.unconfirmed_frontier, initial);
        assert!(state.confirmed());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn finalization_before_termination_advances_and_finalizes_slot() {
        let mut map = AccountMap::default();
        let key = PrivateKey::from(100);
        let initial = BlockHash::from(41);
        let block = BlockHash::from(42);
        map.add_unopened(key.clone());
        map.set_account_state(key.account(), Amount::nano(1), initial);
        map.process_change(key.account(), block);

        assert_eq!(map.finalize(&block), vec![block]);
        let state = map.state(&key.account()).unwrap();
        assert_eq!(state.confirmed_frontier, block);
        assert_eq!(state.finalized_frontier, block);
    }

    const TEST_GENESIS_ACCOUNT: Account = Account::from_bytes([1; 32]);
}
