use rsnano_core::{Account, BlockHash, JsonBlock, WorkNonce, WorkVersion};
use serde::{Deserialize, Serialize};

use crate::RpcCommand;

impl RpcCommand {
    pub fn work_generate(hash: BlockHash, use_peers: Option<bool>, difficulty: Option<u64>, multiplier: Option<u64>, account: Option<Account>, version: Option<WorkVersion>, block: Option<JsonBlock>) -> Self {
        Self::WorkGenerate(WorkGenerateArgs::new(
            hash,
            use_peers,
            difficulty,
            multiplier,
            account,
            version,
            block,
        ))
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]

pub struct WorkGenerateArgs {
    pub hash: BlockHash,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_peers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<WorkVersion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<JsonBlock>,
}

impl WorkGenerateArgs {
    pub fn new(
        hash: BlockHash,
        use_peers: Option<bool>,
        difficulty: Option<u64>,
        multiplier: Option<u64>,
        account: Option<Account>,
        version: Option<WorkVersion>,
        block: Option<JsonBlock>,
    ) -> Self {
        Self {
            hash,
            use_peers,
            difficulty,
            multiplier,
            account,
            version,
            block,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkGenerateDto {
    pub work: WorkNonce,
    pub difficulty: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier: Option<f64>,
    pub hash: BlockHash,
}

impl WorkGenerateDto {
    pub fn new(work: WorkNonce, difficulty: u64, multiplier: Option<f64>, hash: BlockHash) -> Self {
        Self {
            work,
            difficulty,
            multiplier,
            hash,
        }
    }
}
