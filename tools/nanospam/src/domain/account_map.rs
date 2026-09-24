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
    /// The second block of each fork => the block published first, which
    /// the account's bookkeeping follows
    forks: FxHashMap<BlockHash, BlockHash>,
}

struct UnconfirmedEntry {
    pub(crate) source: Account,
    /// Is only set for send-blocks
    pub(crate) destination: Option<Account>,
    pub(crate) fork: Option<BlockHash>,
}

pub(crate) struct AccountState {
    pub key: PrivateKey,
    pub confirmed_frontier: BlockHash,
    pub unconfirmed_frontier: BlockHash,
    pub balance: Amount,
    /// RAI: the account delegates to itself rather than to a principal
    /// representative: the initial spam account, whose balance would
    /// otherwise give one representative more weight than the others
    pub own_representative: bool,
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

    /// The account that received the whole spam amount and funds all others
    pub fn initial_account(&self) -> Account {
        self.all_accounts[0]
    }

    pub fn accounts(&self) -> &Vec<Account> {
        &self.all_accounts
    }

    pub fn set_account_state(&mut self, account: Account, balance: Amount, frontier: BlockHash) {
        let state = self.account_states.get_mut(&account).unwrap();
        state.balance = balance;
        state.unconfirmed_frontier = frontier;
        state.confirmed_frontier = frontier;
        self.confirmed_accounts.insert(account);
        if self.active_accounts.insert(account) {
            self.active_accounts_vec.push(account);
        }
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
                unconfirmed_frontier: BlockHash::ZERO,
                balance: Amount::ZERO,
                // The first account is the initial one, which funds the run:
                // it delegates to nobody, so that every principal
                // representative holds an equal share
                own_representative: self.all_accounts.len() == 1,
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
        if let Some(fork) = fork {
            self.forks.insert(fork, send_hash);
        }
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
        if let Some(fork) = fork {
            self.forks.insert(fork, receive_hash);
        }
    }

    pub fn process_change(&mut self, account: Account, hash: BlockHash) {
        let state = self.account_states.get_mut(&account).unwrap();
        state.unconfirmed_frontier = hash;
        self.confirmed_accounts.remove(&account);
        self.unconfirmed.insert(
            hash,
            UnconfirmedEntry {
                source: account,
                destination: None,
                fork: None,
            },
        );
    }

    /// RAI: a decided checkpoint kept `lock`, an unconfirmed frontier of
    /// this run's or the fork of one, as a lock the owner resolves with a
    /// fresh child. Returns the account whose chain now continues from
    /// `lock`, or None if `lock` is no such block or the account was
    /// extended already. When the lock is a fork's second block, the
    /// account's bookkeeping moves over to it: its hash is what confirms,
    /// and a send's receivable is the one the lock created. Also returns the
    /// block published first.
    pub fn adopt_lock(&mut self, lock: &BlockHash) -> Option<(Account, BlockHash)> {
        let first = *self.forks.get(lock).unwrap_or(lock);
        let source = self.unconfirmed.get(&first)?.source;
        let state = self.account_states.get_mut(&source)?;
        if state.unconfirmed_frontier != first {
            return None;
        }
        state.unconfirmed_frontier = *lock;
        if first != *lock {
            let mut entry = self.unconfirmed.remove(&first)?;
            entry.fork = Some(first);
            if let Some(destination) = entry.destination
                && let Some(receivable) = self.receivable.get_mut(&destination)
                && let Some(pending) = receivable.iter_mut().find(|(hash, _)| *hash == first)
            {
                pending.0 = *lock;
            }
            self.unconfirmed.insert(*lock, entry);
        }
        self.forks.remove(lock);
        Some((source, first))
    }

    pub fn confirm(&mut self, hash: &BlockHash) {
        // A directly confirmed alternative must take over bookkeeping even
        // when no checkpoint recovery child was needed.
        if self.forks.contains_key(hash) {
            self.adopt_lock(hash);
        }
        let Some(entry) = self.unconfirmed.remove(hash) else {
            return;
        };

        if let Some(fork) = entry.fork {
            self.unconfirmed.remove(&fork);
            self.forks.remove(&fork);
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
        // RAI: a lock's child cements with it and may be reported first
        if state.confirmed() {
            return;
        }
        state.confirmed_frontier = *hash;
        if state.confirmed() {
            self.confirmed_accounts.insert(entry.source);
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
    fn a_locked_first_block_of_a_fork_is_extended_once() {
        let (mut map, sender, _) = forked_send_fixture();

        assert_eq!(map.adopt_lock(&FIRST), Some((sender, FIRST)));
        map.process_change(sender, CHILD);
        assert_eq!(map.adopt_lock(&FIRST), None);
        map.confirm(&FIRST);
        map.confirm(&CHILD);

        let state = map.state(&sender).unwrap();
        assert!(state.confirmed());
        assert_eq!(state.confirmed_frontier, CHILD);
    }

    #[test]
    fn a_locked_second_block_of_a_fork_takes_over_the_bookkeeping() {
        let (mut map, sender, destination) = forked_send_fixture();

        assert_eq!(map.adopt_lock(&SECOND), Some((sender, FIRST)));
        map.process_change(sender, CHILD);
        map.confirm(&SECOND);
        map.confirm(&CHILD);

        assert!(map.state(&sender).unwrap().confirmed());
        assert_eq!(
            map.get_receivable(&destination),
            Some((SECOND, Amount::nano(1)))
        );
        assert_eq!(
            map.next_receivable(),
            Some((destination, SECOND, Amount::nano(1)))
        );
    }

    #[test]
    fn a_child_confirmed_before_its_lock_leaves_the_account_confirmed() {
        let (mut map, sender, _) = forked_send_fixture();

        map.adopt_lock(&FIRST);
        map.process_change(sender, CHILD);
        map.confirm(&CHILD);
        map.confirm(&FIRST);

        let state = map.state(&sender).unwrap();
        assert!(state.confirmed());
        assert_eq!(state.confirmed_frontier, CHILD);
    }

    #[test]
    fn a_locked_block_without_a_fork_is_extended_too() {
        let mut map = AccountMap::default();
        let sender = PrivateKey::from(100);
        map.add_unopened(sender.clone());
        map.set_account_state(sender.account(), Amount::nano(10), BlockHash::from(1));
        map.process_send(
            sender.account(),
            sender.account(),
            FIRST,
            Amount::nano(1),
            None,
        );

        assert_eq!(map.adopt_lock(&FIRST), Some((sender.account(), FIRST)));
    }

    #[test]
    fn a_confirmed_block_is_not_extended() {
        let (mut map, _, _) = forked_send_fixture();
        map.confirm(&FIRST);

        assert_eq!(map.adopt_lock(&FIRST), None);
        assert_eq!(map.adopt_lock(&SECOND), None);
    }

    #[test]
    fn directly_confirmed_alternative_updates_frontier_and_receivable() {
        let (mut map, sender, destination) = forked_send_fixture();
        map.confirm(&SECOND);
        assert!(map.state(&sender).unwrap().confirmed());
        assert_eq!(map.state(&sender).unwrap().confirmed_frontier, SECOND);
        assert!(
            map.confirmed_receivable
                .contains_key(&(destination, SECOND))
        );
        assert!(!map.confirmed_receivable.contains_key(&(destination, FIRST)));
        map.confirm(&FIRST); // late losing-hash observation cannot roll it back
        assert_eq!(map.state(&sender).unwrap().confirmed_frontier, SECOND);
    }

    /* Test helpers */

    const TEST_GENESIS_ACCOUNT: Account = Account::from_bytes([1; 32]);
    const FIRST: BlockHash = BlockHash::from_bytes([42; 32]);
    const SECOND: BlockHash = BlockHash::from_bytes([43; 32]);
    const CHILD: BlockHash = BlockHash::from_bytes([50; 32]);

    /// A funded account sends to another one with a fork: FIRST published
    /// first, SECOND its fork
    fn forked_send_fixture() -> (AccountMap, Account, Account) {
        let mut map = AccountMap::default();
        let sender = PrivateKey::from(100);
        let destination = PrivateKey::from(101);
        map.add_unopened(sender.clone());
        map.add_unopened(destination.clone());
        map.set_account_state(sender.account(), Amount::nano(10), BlockHash::from(1));
        map.process_send(
            sender.account(),
            destination.account(),
            FIRST,
            Amount::nano(1),
            Some(SECOND),
        );
        (map, sender.account(), destination.account())
    }
}
