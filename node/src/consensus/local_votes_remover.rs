use super::{AecService, LocalVoteHistory};
use rsnano_types::{BlockHash, QualifiedRoot};
use std::sync::Arc;

pub(crate) struct LocalVotesRemover {
    pub(crate) vote_history: Arc<LocalVoteHistory>,
    pub(crate) active_elections: Arc<AecService>,
}

impl LocalVotesRemover {
    /// Removes votes that were created by this node from an election
    /// if the election winner has changed
    pub fn remove_local_votes_in_epoch(
        &self,
        previous_winner: &BlockHash,
        root: &QualifiedRoot,
        epoch: u64,
    ) {
        let votes: Vec<_> = self
            .vote_history
            .votes(&root.root, previous_winner, false)
            .into_iter()
            .filter(|v| v.epoch == epoch)
            .collect();

        self.active_elections
            .remove_votes_in_epoch(root, epoch, votes.iter().map(|i| &i.voter));

        self.vote_history.erase_in_epoch(&root.root, epoch);
    }
}
