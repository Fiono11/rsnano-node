use std::fmt::Display;

use rsnano_types::{ConsensusEpoch, QualifiedRoot};

/// RAI: an election is identified by its root and its consensus epoch. A
/// slot may be contested in several epochs, each epoch has its own election.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElectionId {
    pub root: QualifiedRoot,
    pub epoch: ConsensusEpoch,
}

impl ElectionId {
    pub fn new(root: QualifiedRoot, epoch: ConsensusEpoch) -> Self {
        Self { root, epoch }
    }

    /// The election of the legacy protocol's single implicit epoch
    pub fn legacy(root: QualifiedRoot) -> Self {
        Self::new(root, ConsensusEpoch::ZERO)
    }

    pub fn new_test_instance() -> Self {
        Self::legacy(QualifiedRoot::new_test_instance())
    }
}

impl Display for ElectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}@{}",
            self.root.root, self.root.previous, self.epoch
        )
    }
}
