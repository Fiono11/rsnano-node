mod account;
mod accounts;
mod accounts_with_amounts;
mod address_with_port;
mod amount;
mod block;
mod blocks;
mod count;
mod destroyed;
mod error;
mod exists;
mod frontiers;
mod hash;
mod hashes;
mod key_pair;
mod locked;
mod moved;
mod primitives;
mod public_key;
mod removed;
mod started;
mod success;
mod valid;
mod wallet;

pub use account::*;
pub use accounts::*;
pub use accounts_with_amounts::*;
pub use address_with_port::*;
pub use amount::*;
pub use block::*;
pub use blocks::*;
pub use count::*;
pub use destroyed::*;
pub use error::*;
pub use exists::*;
pub use frontiers::*;
pub use hash::*;
pub use hashes::*;
pub use key_pair::*;
pub use locked::*;
pub use moved::*;
pub use primitives::*;
pub use public_key::*;
pub use removed::*;
pub use started::*;
pub use success::*;
pub use valid::*;
pub use wallet::*;

use rsnano_types::{BlockSubType, BlockTypeId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkVersionDto {
    Work1,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockTypeDto {
    Send,
    Receive,
    Open,
    Change,
    State,
    Dummy,
    Unknown,
}

impl From<BlockTypeId> for BlockTypeDto {
    fn from(value: BlockTypeId) -> Self {
        match value {
            BlockTypeId::LegacySend => BlockTypeDto::Send,
            BlockTypeId::LegacyReceive => BlockTypeDto::Receive,
            BlockTypeId::LegacyOpen => BlockTypeDto::Open,
            BlockTypeId::LegacyChange => BlockTypeDto::Change,
            BlockTypeId::State => BlockTypeDto::State,
            BlockTypeId::Dummy => BlockTypeDto::Dummy,
            BlockTypeId::Epoch => BlockTypeDto::Unknown,
            BlockTypeId::Invalid | BlockTypeId::NotABlock => BlockTypeDto::Unknown,
        }
    }
}

impl From<BlockTypeDto> for BlockTypeId {
    fn from(value: BlockTypeDto) -> Self {
        match value {
            BlockTypeDto::Send => BlockTypeId::LegacySend,
            BlockTypeDto::Receive => BlockTypeId::LegacyReceive,
            BlockTypeDto::Open => BlockTypeId::LegacyOpen,
            BlockTypeDto::Change => BlockTypeId::LegacyChange,
            BlockTypeDto::State => BlockTypeId::State,
            BlockTypeDto::Dummy => BlockTypeId::Dummy,
            BlockTypeDto::Unknown => BlockTypeId::Invalid,
        }
    }
}

#[derive(PartialEq, Eq, Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockSubTypeDto {
    Send,
    Receive,
    Open,
    Change,
    Epoch,
    Dummy,
    Unknown,
}

impl From<BlockSubType> for BlockSubTypeDto {
    fn from(value: BlockSubType) -> Self {
        match value {
            BlockSubType::Send => Self::Send,
            BlockSubType::Receive => Self::Receive,
            BlockSubType::Open => Self::Open,
            BlockSubType::Change => Self::Change,
            BlockSubType::Epoch => Self::Epoch,
            BlockSubType::Dummy => Self::Dummy,
        }
    }
}
