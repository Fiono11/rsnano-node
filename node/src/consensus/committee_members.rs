#[cfg(feature = "rai_protocol")]
use std::sync::Arc;
use std::{collections::HashSet, sync::Mutex};

use rsnano_types::PublicKey;
#[cfg(feature = "rai_protocol")]
use rsnano_utils::{CancellationToken, ticker::Tickable};

#[cfg(feature = "rai_protocol")]
use crate::{consensus::AecService, wallets::WalletRepresentatives};

/// RAI: the members of the committees around the current epoch. A member
/// votes and is heard by its membership, not by the ledger weight it holds
/// now: committees lag the finalized state by two epochs, so a member that
/// moved its weight away is still a member of the next two epochs, and one
/// that just gained weight is not one yet. The node's representative tiers
/// and its own voting keys take these members in.
#[derive(Default)]
pub struct CommitteeMembers(Mutex<HashSet<PublicKey>>);

impl CommitteeMembers {
    pub fn get(&self) -> HashSet<PublicKey> {
        self.0.lock().unwrap().clone()
    }

    pub fn contains(&self, key: &PublicKey) -> bool {
        self.0.lock().unwrap().contains(key)
    }

    /// True if the members changed
    pub fn set(&self, members: HashSet<PublicKey>) -> bool {
        let mut held = self.0.lock().unwrap();
        if *held == members {
            return false;
        }
        *held = members;
        true
    }
}

/// RAI: copies the committee members from the active elections into the
/// shared set and the wallet's representatives, recomputing which local
/// keys vote when the members change
#[cfg(feature = "rai_protocol")]
pub(crate) struct CommitteeMembersSync {
    aec: Arc<AecService>,
    members: Arc<CommitteeMembers>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
}

#[cfg(feature = "rai_protocol")]
impl CommitteeMembersSync {
    pub fn new(
        aec: Arc<AecService>,
        members: Arc<CommitteeMembers>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    ) -> Self {
        Self {
            aec,
            members,
            wallet_reps,
        }
    }
}

#[cfg(feature = "rai_protocol")]
impl Tickable for CommitteeMembersSync {
    fn tick(&mut self, _: &CancellationToken) {
        let members = self.aec.committee_members();
        if self.members.set(members.clone()) {
            let count = members.len();
            let mut wallet_reps = self.wallet_reps.lock().unwrap();
            wallet_reps.set_committee_members(members);
            wallet_reps.compute_reps();
            crate::utils::diagnostic!(
                "COMMITTEE_MEMBERS members={} voting_keys={}",
                count,
                wallet_reps
                    .rep_pub_keys()
                    .map(|key| key.to_string()[..8].to_owned())
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_the_same_members_is_no_change() {
        let members = CommitteeMembers::default();
        let set: HashSet<PublicKey> = [PublicKey::from(1)].into();
        assert!(members.set(set.clone()));
        assert!(!members.set(set.clone()));
        assert_eq!(members.get(), set);
        assert!(members.contains(&PublicKey::from(1)));
        assert!(!members.contains(&PublicKey::from(2)));
    }
}
