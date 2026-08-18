use rand::{RngExt, rng, seq::IndexedRandom};
use rsnano_types::{Account, Amount, BlockHash, PrivateKey, PublicKey};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Default)]
pub(crate) struct AccountMap {
    pub account_states: FxHashMap<Account, AccountState>,
    all_accounts: Vec<Account>,
    active_accounts: FxHashSet<Account>,
    active_accounts_vec: Vec<Account>,
    sendable_accounts: Vec<Account>,
    sendable_positions: FxHashMap<Account, usize>,
    confirmed_accounts: FxHashSet<Account>,

    /// Account => Send block hash + amount sent
    receivable: FxHashMap<Account, Vec<(BlockHash, Amount)>>,

    /// Accounts that can receive and the send is confirmed
    /// Receiving account + send hash => amount
    confirmed_receivable: FxHashMap<(Account, BlockHash), Amount>,
    unconfirmed: FxHashMap<BlockHash, UnconfirmedEntry>,
}

struct UnconfirmedEntry {
    pub(crate) source: Account,
    /// Is only set for send-blocks
    pub(crate) destination: Option<Account>,
    pub(crate) fork: Option<BlockHash>,
}

pub(crate) struct AccountState {
    pub key: PrivateKey,
    pub representative: PublicKey,
    pub confirmed_frontier: BlockHash,
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

    #[cfg(test)]
    pub fn initial_key(&self) -> &PrivateKey {
        &self.account_states.get(&self.all_accounts[0]).unwrap().key
    }

    pub fn accounts(&self) -> &Vec<Account> {
        &self.all_accounts
    }

    pub fn set_account_state(&mut self, account: Account, balance: Amount, frontier: BlockHash) {
        {
            let state = self.account_states.get_mut(&account).unwrap();
            state.balance = balance;
            state.unconfirmed_frontier = frontier;
            state.confirmed_frontier = frontier;
        }
        self.confirmed_accounts.insert(account);
        if self.active_accounts.insert(account) {
            self.active_accounts_vec.push(account);
        }
        self.set_sendable(account, !balance.is_zero());
    }

    pub fn set_representative(&mut self, account: Account, representative: PublicKey) {
        self.account_states
            .get_mut(&account)
            .unwrap()
            .representative = representative;
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
                representative: key.public_key(),
                key,
                confirmed_frontier: BlockHash::ZERO,
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

    /// Selects an account from one representative shard with a bounded scan.
    ///
    /// Starting at a random offset retains the ordinary workload's random
    /// destination selection without retrying until a matching account happens
    /// to be found. In particular, a shard containing only the sender returns
    /// that account immediately instead of looping forever.
    pub fn random_account_for_representative(&self, representative: PublicKey) -> Option<Account> {
        if self.all_accounts.is_empty() {
            return None;
        }

        let start = rand::rng().random_range(0..self.all_accounts.len());
        self.all_accounts
            .iter()
            .cycle()
            .skip(start)
            .take(self.all_accounts.len())
            .find(|account| {
                self.account_states
                    .get(account)
                    .is_some_and(|state| state.representative == representative)
            })
            .copied()
    }

    pub fn process_send(
        &mut self,
        source: Account,
        destination: Account,
        send_hash: BlockHash,
        amount: Amount,
        fork: Option<BlockHash>,
    ) {
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
            },
        );
        self.confirmed_accounts.remove(&source);
        self.remove_sendable(source);

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
        self.remove_sendable(receiver);

        let state = self.account_states.get_mut(&receiver).unwrap();
        state.balance += amount;
        state.unconfirmed_frontier = receive_hash;
        self.unconfirmed.insert(
            receive_hash,
            UnconfirmedEntry {
                source: receiver,
                destination: None,
                fork,
            },
        );
    }

    pub fn process_change(&mut self, account: Account, hash: BlockHash, representative: PublicKey) {
        let state = self.account_states.get_mut(&account).unwrap();
        state.unconfirmed_frontier = hash;
        state.representative = representative;
        self.confirmed_accounts.remove(&account);
        self.remove_sendable(account);
        self.unconfirmed.insert(
            hash,
            UnconfirmedEntry {
                source: account,
                destination: None,
                fork: None,
            },
        );
    }

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
        let confirmed = state.confirmed();
        let sendable = confirmed && !state.balance.is_zero();
        if confirmed {
            self.confirmed_accounts.insert(entry.source);
        }
        self.set_sendable(entry.source, sendable);
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
        let account = self.sendable_accounts.choose(&mut rng())?;
        Some(self.account_states.get(account).unwrap())
    }

    fn set_sendable(&mut self, account: Account, sendable: bool) {
        if sendable {
            if self.sendable_positions.contains_key(&account) {
                return;
            }

            let position = self.sendable_accounts.len();
            self.sendable_accounts.push(account);
            self.sendable_positions.insert(account, position);
        } else {
            self.remove_sendable(account);
        }
    }

    fn remove_sendable(&mut self, account: Account) {
        let Some(position) = self.sendable_positions.remove(&account) else {
            return;
        };

        let last_position = self.sendable_accounts.len() - 1;
        self.sendable_accounts.swap_remove(position);
        if position != last_position {
            let moved = self.sendable_accounts[position];
            *self.sendable_positions.get_mut(&moved).unwrap() = position;
        }
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
    fn representative_shard_with_one_account_returns_without_retrying() {
        let mut map = AccountMap::default();
        let key = PrivateKey::from(1);
        let representative = PublicKey::from(900);
        map.add_unopened(key.clone());
        map.set_representative(key.account(), representative);

        assert_eq!(
            map.random_account_for_representative(representative),
            Some(key.account())
        );
        assert_eq!(
            map.random_account_for_representative(PublicKey::from(901)),
            None
        );
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
        map.confirm(&send_hash);

        assert_eq!(map.get_receivable(&dest_account), Some((send_hash, amount)));
        assert_eq!(
            map.next_receivable(),
            Some((dest_account, send_hash, amount))
        );
        assert!(map.random_account_that_can_send().is_none());
        assert_eq!(map.state(&dest_account).unwrap().balance, Amount::ZERO);
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
        map.confirm(&send_genesis_hash);
        map.process_receive(key.account(), send_genesis_hash, receive_hash, None);
        map.confirm(&receive_hash);
        map.process_send(
            key.account(),
            key.account(),
            send_hash,
            Amount::nano(1),
            None,
        );
        map.confirm(&send_hash);

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
        map.confirm(&send_hash);
        map.process_receive(dest_account, send_hash, receive_hash, None);
        map.confirm(&receive_hash);

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

    #[test]
    fn sender_selection_is_exact_with_one_eligible_among_thousands() {
        let mut map = AccountMap::default();
        let eligible = PrivateKey::from(1).account();

        for i in 1..=4_096 {
            let key = PrivateKey::from(i);
            let account = key.account();
            map.add_unopened(key);
            let balance = if account == eligible {
                Amount::raw(1)
            } else {
                Amount::ZERO
            };
            map.set_account_state(account, balance, BlockHash::from(10_000 + i));
        }

        assert_eq!(map.active_accounts_vec.len(), 4_096);
        assert_eq!(map.sendable_accounts, vec![eligible]);
        assert_eq!(map.sendable_positions.get(&eligible), Some(&0));
        for _ in 0..256 {
            assert_eq!(
                map.random_account_that_can_send().unwrap().key.account(),
                eligible
            );
        }
    }

    #[test]
    fn sendable_index_tracks_account_state_transitions() {
        let mut map = AccountMap::default();
        let source_key = PrivateKey::from(100);
        let source = source_key.account();
        let destination_key = PrivateKey::from(101);
        let destination = destination_key.account();
        map.add_unopened(source_key);
        map.add_unopened(destination_key);

        map.set_account_state(source, Amount::raw(10), BlockHash::from(1));
        map.set_account_state(source, Amount::raw(10), BlockHash::from(1));
        map.set_account_state(destination, Amount::raw(1), BlockHash::from(10));
        assert_eq!(map.active_accounts_vec, vec![source, destination]);
        assert_eq!(map.sendable_accounts, vec![source, destination]);
        assert_eq!(map.sendable_positions.get(&source), Some(&0));
        assert_eq!(map.sendable_positions.get(&destination), Some(&1));

        let send_hash = BlockHash::from(2);
        map.process_send(source, destination, send_hash, Amount::raw(10), None);
        assert_eq!(map.sendable_accounts, vec![destination]);
        assert_eq!(map.sendable_positions.get(&destination), Some(&0));

        map.confirm(&send_hash);
        assert!(map.state(&source).unwrap().confirmed());
        assert_eq!(map.state(&source).unwrap().balance, Amount::ZERO);
        assert!(!map.sendable_positions.contains_key(&source));
        assert_eq!(map.sendable_accounts, vec![destination]);

        let receive_hash = BlockHash::from(3);
        map.process_receive(destination, send_hash, receive_hash, None);
        assert!(map.random_account_that_can_send().is_none());
        map.confirm(&receive_hash);
        assert_eq!(
            map.random_account_that_can_send().unwrap().key.account(),
            destination
        );

        let change_hash = BlockHash::from(4);
        map.process_change(destination, change_hash, PublicKey::from(200));
        assert!(map.random_account_that_can_send().is_none());
        map.confirm(&change_hash);
        assert_eq!(map.sendable_accounts, vec![destination]);
        assert_eq!(map.sendable_positions.get(&destination), Some(&0));
    }

    const TEST_GENESIS_ACCOUNT: Account = Account::from_bytes([1; 32]);
}
