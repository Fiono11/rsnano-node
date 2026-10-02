use rand::seq::IndexedRandom;

use rsnano_types::{Account, PublicKey};

/// RAI: the principal representatives of a run. Every account delegates
/// to one of them, so that the whole balance of the ledger is voting
/// weight and a send between two accounts of different representatives
/// moves weight between the two.
#[derive(Clone, Debug, Default)]
pub(crate) struct Representatives {
    reps: Vec<PublicKey>,
}

impl Representatives {
    pub(crate) fn new(reps: Vec<PublicKey>) -> Self {
        Self { reps }
    }

    pub(crate) fn len(&self) -> usize {
        self.reps.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &PublicKey> {
        self.reps.iter()
    }

    /// The representative an account delegates to, fixed by the account
    pub(crate) fn of(&self, account: &Account) -> Option<PublicKey> {
        if self.reps.is_empty() {
            return None;
        }
        let bytes = account.as_bytes();
        let index = u16::from_le_bytes([bytes[0], bytes[1]]) as usize % self.reps.len();
        Some(self.reps[index])
    }

    /// A representative other than the given one, for a fork: the fork
    /// moves the account's weight to another representative
    pub(crate) fn other_than(&self, rep: PublicKey) -> Option<PublicKey> {
        let index = self.reps.iter().position(|r| *r == rep)?;
        if self.reps.len() < 2 {
            return None;
        }
        Some(self.reps[(index + 1) % self.reps.len()])
    }

    /// The representative at the given index, wrapping around
    pub(crate) fn nth(&self, index: u64) -> Option<PublicKey> {
        if self.reps.is_empty() {
            return None;
        }
        Some(self.reps[(index % self.reps.len() as u64) as usize])
    }

    pub(crate) fn random(&self) -> Option<PublicKey> {
        self.reps.choose(&mut rand::rng()).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::PrivateKey;

    #[test]
    fn every_account_delegates_to_a_fixed_representative() {
        let reps = representatives(3);
        let account = Account::from(7);
        let rep = reps.of(&account).unwrap();
        assert_eq!(reps.of(&account), Some(rep));
        assert!(reps.iter().any(|r| *r == rep));
        assert!(Representatives::default().of(&account).is_none());
    }

    #[test]
    fn accounts_spread_over_the_representatives() {
        let reps = representatives(3);
        let mut seen = Vec::new();
        for i in 0..100u64 {
            let rep = reps.of(&PrivateKey::from(i).account()).unwrap();
            if !seen.contains(&rep) {
                seen.push(rep);
            }
        }
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn a_fork_names_another_representative() {
        let reps = representatives(3);
        let rep = reps.nth(0).unwrap();
        let other = reps.other_than(rep).unwrap();
        assert_ne!(other, rep);
        assert_eq!(reps.other_than(reps.nth(2).unwrap()), reps.nth(0));
        assert!(representatives(1).other_than(rep).is_none());
        assert!(reps.other_than(PublicKey::from(999)).is_none());
        assert_eq!(reps.nth(4), reps.nth(1));
        let random = reps.random().unwrap();
        assert!(reps.iter().any(|r| *r == random));
    }

    /*
     * Test helpers
     */

    fn representatives(count: u64) -> Representatives {
        Representatives::new(
            (1..=count)
                .map(|i| PrivateKey::from(i).public_key())
                .collect(),
        )
    }
}
