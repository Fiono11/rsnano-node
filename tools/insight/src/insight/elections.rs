use rsnano_types::{Amount, QualifiedRoot};

#[derive(Default)]
pub(crate) struct ElectionsViewModel {
    pub bucket_col1: Vec<BucketViewModel>,
    pub bucket_col2: Vec<BucketViewModel>,
}

pub(crate) struct BucketViewModel {
    pub name: String,
    pub election_count: usize,
    pub elections: Vec<ElectionViewModel>,
}

pub(crate) struct ElectionViewModel {
    pub hash: String,
    pub non_final_tally: u16,
    pub final_tally: u16,
    pub root: QualifiedRoot,
}

pub(crate) struct ElectionDetailsViewModel {
    pub winner_hash: String,
    pub non_final_tally: Amount,
    pub final_tally: Amount,
    pub root: String,
    pub behavior: &'static str,
    pub account: String,
    pub state: &'static str,
    pub candidate_blocks: Vec<String>,
    pub vote_count: String,
    pub phase: &'static str,
    pub elapsed: String,
    pub non_final_votes: Vec<RepVoteViewModel>,
}

pub(crate) struct RepVoteViewModel {
    pub rep: String,
    pub weight: Amount,
    pub voted: bool,
    pub is_final: bool,
}
