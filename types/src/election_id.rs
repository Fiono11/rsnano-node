use crate::QualifiedRoot;
/// Local consensus epoch; unrelated to Nano account upgrade versions.
pub type ConsensusEpoch = u64;
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ElectionId {
    pub root: QualifiedRoot,
    pub epoch: ConsensusEpoch,
}
impl ElectionId {
    pub fn new(root: QualifiedRoot, epoch: ConsensusEpoch) -> Self {
        Self { root, epoch }
    }
}
