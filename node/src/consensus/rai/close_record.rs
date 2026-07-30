use std::collections::BTreeMap;

use blake2::{
    Blake2bVar,
    digest::{Update, VariableOutput},
};
use rsnano_types::{Account, BlockHash, ConfirmationHeightInfo, RaiEpoch};

const CLOSE_RECORD_DOMAIN: &[u8] = b"RAI/CloseRecord";

/// The authenticated ledger frontier at an epoch boundary.  Account ordering,
/// integer byte order and the entry count are consensus encoding rules.
pub type RaiFrontierMap = BTreeMap<Account, ConfirmationHeightInfo>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseRecord {
    pub epoch: RaiEpoch,
    pub previous: BlockHash,
    pub frontiers: RaiFrontierMap,
}

impl RaiCloseRecord {
    pub fn new(
        epoch: RaiEpoch,
        previous: BlockHash,
        frontiers: impl IntoIterator<Item = (Account, ConfirmationHeightInfo)>,
    ) -> Self {
        Self {
            epoch,
            previous,
            frontiers: frontiers.into_iter().collect(),
        }
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes =
            Vec::with_capacity(CLOSE_RECORD_DOMAIN.len() + 44 + self.frontiers.len() * 72);
        bytes.extend_from_slice(CLOSE_RECORD_DOMAIN);
        bytes.extend_from_slice(&self.epoch.number().to_be_bytes());
        bytes.extend_from_slice(self.previous.as_bytes());
        bytes.extend_from_slice(&(self.frontiers.len() as u32).to_be_bytes());
        for (account, info) in &self.frontiers {
            bytes.extend_from_slice(account.as_bytes());
            bytes.extend_from_slice(&info.height.to_be_bytes());
            bytes.extend_from_slice(info.frontier.as_bytes());
        }
        bytes
    }

    pub fn hash(&self) -> BlockHash {
        let mut out = [0; 32];
        let mut hasher = Blake2bVar::new(out.len()).expect("valid hash length");
        hasher.update(&self.canonical_bytes());
        hasher
            .finalize_variable(&mut out)
            .expect("valid output length");
        BlockHash::from_bytes(out)
    }
}

#[derive(Clone, Debug, Default)]
pub struct RaiCloseRecordStore(BTreeMap<BlockHash, RaiCloseRecord>);

impl RaiCloseRecordStore {
    pub fn insert(&mut self, record: RaiCloseRecord) -> BlockHash {
        let hash = record.hash();
        self.0.entry(hash).or_insert(record);
        hash
    }

    pub fn get(&self, hash: &BlockHash) -> Option<&RaiCloseRecord> {
        self.0.get(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(height: u64, hash: u64) -> ConfirmationHeightInfo {
        ConfirmationHeightInfo::new(height, BlockHash::from(hash))
    }

    #[test]
    fn frontier_encoding_is_deterministic() {
        let a = RaiCloseRecord::new(
            7.into(),
            6.into(),
            [(2.into(), info(4, 40)), (1.into(), info(3, 30))],
        );
        let b = RaiCloseRecord::new(
            7.into(),
            6.into(),
            [(1.into(), info(3, 30)), (2.into(), info(4, 40))],
        );
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn previous_close_hash_is_bound() {
        let frontiers = [(1.into(), info(3, 30))];
        assert_ne!(
            RaiCloseRecord::new(7.into(), 6.into(), frontiers.clone()).hash(),
            RaiCloseRecord::new(7.into(), 5.into(), frontiers).hash()
        );
    }
}
