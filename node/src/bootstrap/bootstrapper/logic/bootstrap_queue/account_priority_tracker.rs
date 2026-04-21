use rustc_hash::FxHashMap;

use rsnano_types::Account;

use crate::bootstrap::bootstrapper::logic::Priority;
use std::cmp::max;

#[derive(Debug, PartialEq, Eq)]
pub enum PriorityUpResult {
    Inserted(Priority),
    Upgraded(Priority, Priority),
    InvalidAccount,
    /// If the account is not in the download queue, we don't change its priority
    Unchanged,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PriorityDownResult {
    Deprioritized(Priority, Priority),
    /// The priority got too low, so the account got erased
    Removed,
    AccountNotFound,
}

#[derive(PartialEq, Eq)]
enum ChangePriorityResult {
    Updated(Priority, Priority),
    Removed,
    NotFound,
    Unchanged,
}

#[derive(Default)]
pub(super) struct AccountPriorityTracker {
    priorities: FxHashMap<Account, Priority>,
}

impl AccountPriorityTracker {
    pub fn priority_up(&mut self, account: &Account) -> PriorityUpResult {
        if account.is_zero() {
            return PriorityUpResult::InvalidAccount;
        }

        let result = self.modify_priority(account, |prio| prio.increase());

        match result {
            ChangePriorityResult::NotFound => {
                let prio = Priority::INITIAL;
                self.priorities.insert(*account, prio);
                PriorityUpResult::Inserted(prio)
            }
            ChangePriorityResult::Updated(old, new) => PriorityUpResult::Upgraded(old, new),
            ChangePriorityResult::Removed => {
                unreachable!()
            }
            ChangePriorityResult::Unchanged => PriorityUpResult::Unchanged,
        }
    }

    pub fn priority_up_to(
        &mut self,
        account: &Account,
        new_priority: Priority,
    ) -> PriorityUpResult {
        if account.is_zero() {
            return PriorityUpResult::InvalidAccount;
        }

        let result = self.modify_priority(account, |old_prio| max(old_prio, new_priority));

        match result {
            ChangePriorityResult::Updated(old, new) => PriorityUpResult::Upgraded(old, new),
            ChangePriorityResult::Removed => unreachable!(),
            ChangePriorityResult::Unchanged => PriorityUpResult::Unchanged,
            ChangePriorityResult::NotFound => {
                self.priorities
                    .insert(*account, max(new_priority, new_priority));
                PriorityUpResult::Inserted(new_priority)
            }
        }
    }

    pub fn priority_down(&mut self, account: &Account) -> PriorityDownResult {
        let change_result = self.modify_priority(account, |prio| prio / Priority::DIVIDE);

        match change_result {
            ChangePriorityResult::Updated(old, new) => PriorityDownResult::Deprioritized(old, new),
            ChangePriorityResult::Removed => PriorityDownResult::Removed,
            ChangePriorityResult::NotFound => PriorityDownResult::AccountNotFound,
            ChangePriorityResult::Unchanged => {
                unreachable!("the account is ether downgraded, removed or not found")
            }
        }
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.priorities.contains_key(account)
    }

    pub fn get(&self, account: &Account) -> Option<Priority> {
        self.priorities.get(account).copied()
    }

    pub fn len(&self) -> usize {
        self.priorities.len()
    }

    pub fn remove(&mut self, account: &Account) -> Option<Priority> {
        self.priorities.remove(account)
    }

    fn modify_priority<F>(&mut self, account: &Account, f: F) -> ChangePriorityResult
    where
        F: Fn(Priority) -> Priority,
    {
        let Some(old_prio) = self.priorities.get_mut(account) else {
            return ChangePriorityResult::NotFound;
        };

        let new_prio = f(*old_prio);
        if new_prio == *old_prio {
            return ChangePriorityResult::Unchanged;
        }

        *old_prio = new_prio;

        if new_prio < Priority::CUTOFF {
            self.priorities.remove(account);
            return ChangePriorityResult::Removed;
        }

        ChangePriorityResult::Updated(*old_prio, new_prio)
    }
}
