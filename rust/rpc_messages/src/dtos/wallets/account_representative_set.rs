use rsnano_core::BlockHash;
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AccountRepresentativeSetDto {
    block: BlockHash,
}

impl AccountRepresentativeSetDto {
    pub fn new(block: BlockHash) -> Self {
        Self { block }
    }
}
