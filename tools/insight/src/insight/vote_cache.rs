use rsnano_types::{Amount, PublicKey};

#[derive(Default)]
pub(crate) struct VoteCacheViewModel {
    pub cached_blocks: usize,
    pub search: String,
    pub block_votes: Vec<VoteViewModel>,
}

pub(crate) struct VoteViewModel {
    pub rep_key: PublicKey,
    pub is_final: bool,
    pub weight: Amount,
}
