use crate::consensus::election::Election;
use rsnano_types::{BlockHash, ElectionId};
use rsnano_utils::container_info::ContainerInfo;
use std::collections::HashMap;
#[derive(Default)]
pub(crate) struct VoteRouter {
    elections: HashMap<BlockHash, std::collections::BTreeMap<u64, ElectionId>>,
}
impl VoteRouter {
    pub fn connect_epoch(&mut self, hash: BlockHash, id: ElectionId) {
        self.elections.entry(hash).or_default().insert(id.epoch, id);
    }
    pub fn disconnect_election(&mut self, e: &Election) {
        for hash in e.candidate_blocks().keys() {
            if let Some(epochs) = self.elections.get_mut(hash) {
                epochs.remove(&e.epoch);
                if epochs.is_empty() {
                    self.elections.remove(hash);
                }
            }
        }
    }
    pub fn disconnect_epoch(&mut self, hash: &BlockHash, epoch: u64) {
        if let Some(epochs) = self.elections.get_mut(hash) {
            epochs.remove(&epoch);
            if epochs.is_empty() {
                self.elections.remove(hash);
            }
        }
    }
    pub fn id(&self, hash: &BlockHash) -> Option<&ElectionId> {
        self.elections
            .get(hash)?
            .first_key_value()
            .map(|(_, id)| id)
    }
    pub fn id_in_epoch(&self, hash: &BlockHash, epoch: u64) -> Option<&ElectionId> {
        self.elections.get(hash)?.get(&epoch)
    }
    pub fn is_active(&self, hash: &BlockHash) -> bool {
        self.id(hash).is_some()
    }
    pub fn container_info(&self) -> ContainerInfo {
        [("elections", self.elections.len(), size_of::<ElectionId>())].into()
    }
}
