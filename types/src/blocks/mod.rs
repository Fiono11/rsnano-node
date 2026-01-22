mod block_details;
mod block_priority;

pub use block_details::BlockDetails;
pub use block_priority::block_priority;

mod block_sideband;
pub use block_sideband::BlockSideband;

mod change_block;
use change_block::JsonChangeBlock;
pub use change_block::{ChangeBlock, ChangeBlockArgs, valid_change_block_predecessor};

mod open_block;
pub use open_block::{JsonOpenBlock, OpenBlock, OpenBlockArgs};

mod receive_block;
pub use receive_block::{
    JsonReceiveBlock, ReceiveBlock, ReceiveBlockArgs, valid_receive_block_predecessor,
};

mod send_block;
use send_block::JsonSendBlock;
pub use send_block::{SendBlock, SendBlockArgs, valid_send_block_predecessor};

mod state_block;
pub use state_block::{EpochBlockArgs, JsonStateBlock, StateBlock, StateBlockArgs};

mod epoch_block;
pub use epoch_block::EpochBlock;
use epoch_block::JsonEpochBlock;

mod dummy_block;
use dummy_block::JsonDummyBlock;
pub use dummy_block::{DummyBlock, DummyBlockArgs};

mod builders;
pub use builders::*;

use crate::{
    Account, Amount, BlockHash, DeserializationError, Epoch, Epochs, Link, PrivateKey, PublicKey,
    QualifiedRoot, Root, Signature, UnixMillisTimestamp, WorkNonce, read_u8,
};
use num::FromPrimitive;
use std::{
    io::Read,
    ops::{Deref, DerefMut},
    sync::LazyLock,
};

#[repr(u8)]
#[derive(PartialEq, Eq, Debug, Clone, Copy, FromPrimitive)]
pub enum BlockTypeId {
    Invalid = 0,
    NotABlock = 1,
    LegacySend = 2,
    LegacyReceive = 3,
    LegacyOpen = 4,
    LegacyChange = 5,
    State = 6,
    Dummy = 7,
    Epoch = 8,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum BlockType {
    LegacySend,
    LegacyReceive,
    LegacyOpen,
    LegacyChange,
    State,
    Dummy,
    Epoch,
}

impl From<BlockType> for BlockTypeId {
    fn from(value: BlockType) -> Self {
        match value {
            BlockType::LegacySend => BlockTypeId::LegacySend,
            BlockType::LegacyReceive => BlockTypeId::LegacyReceive,
            BlockType::LegacyOpen => BlockTypeId::LegacyOpen,
            BlockType::LegacyChange => BlockTypeId::LegacyChange,
            BlockType::State => BlockTypeId::State,
            BlockType::Dummy => BlockTypeId::Dummy,
            BlockType::Epoch => BlockTypeId::Epoch,
        }
    }
}

impl TryFrom<BlockTypeId> for BlockType {
    type Error = ();

    fn try_from(value: BlockTypeId) -> Result<Self, Self::Error> {
        match value {
            BlockTypeId::LegacySend => Ok(BlockType::LegacySend),
            BlockTypeId::LegacyReceive => Ok(BlockType::LegacyReceive),
            BlockTypeId::LegacyOpen => Ok(BlockType::LegacyOpen),
            BlockTypeId::LegacyChange => Ok(BlockType::LegacyChange),
            BlockTypeId::State => Ok(BlockType::State),
            BlockTypeId::Dummy => Ok(BlockType::Dummy),
            BlockTypeId::Epoch => Ok(BlockType::Epoch),
            BlockTypeId::Invalid | BlockTypeId::NotABlock => Err(()),
        }
    }
}

impl From<BlockType> for BlockSubType {
    fn from(value: BlockType) -> Self {
        match value {
            BlockType::LegacySend => BlockSubType::Send,
            BlockType::LegacyReceive => BlockSubType::Receive,
            BlockType::LegacyOpen => BlockSubType::Open,
            BlockType::LegacyChange => BlockSubType::Change,
            BlockType::State => BlockSubType::Send,
            BlockType::Dummy => BlockSubType::Dummy,
            BlockType::Epoch => BlockSubType::Epoch,
        }
    }
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum BlockSubType {
    Send,
    Receive,
    Open,
    Change,
    Epoch,
    Dummy,
}

impl BlockSubType {
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockSubType::Send => "send",
            BlockSubType::Receive => "receive",
            BlockSubType::Open => "open",
            BlockSubType::Change => "change",
            BlockSubType::Epoch => "epoch",
            BlockSubType::Dummy => "dummy",
        }
    }
}

pub trait BlockBase {
    fn block_type(&self) -> BlockType;
    fn account_field(&self) -> Option<Account>;
    fn hash(&self) -> BlockHash;
    fn link_field(&self) -> Option<Link>;
    fn signature(&self) -> &Signature;
    fn set_signature(&mut self, signature: Signature);
    fn work(&self) -> WorkNonce;
    fn set_work(&mut self, work: WorkNonce);
    fn previous(&self) -> BlockHash;
    fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(&self.json_representation())
    }
    fn json_representation(&self) -> JsonBlock;
    fn root(&self) -> Root;
    fn balance_field(&self) -> Option<Amount>;
    /// Source block for open/receive blocks, zero otherwise.
    fn source_field(&self) -> Option<BlockHash>;
    fn representative_field(&self) -> Option<PublicKey>;
    fn destination_field(&self) -> Option<Account>;
    fn qualified_root(&self) -> QualifiedRoot {
        QualifiedRoot::new(self.root(), self.previous())
    }
    fn valid_predecessor(&self, block_type: BlockType) -> bool;
}

pub fn serialized_block_size(block_type: BlockType) -> usize {
    match block_type {
        BlockType::LegacySend => SendBlock::SERIALIZED_SIZE,
        BlockType::LegacyReceive => ReceiveBlock::SERIALIZED_SIZE,
        BlockType::LegacyOpen => OpenBlock::SERIALIZED_SIZE,
        BlockType::LegacyChange => ChangeBlock::SERIALIZED_SIZE,
        BlockType::State => StateBlock::SERIALIZED_SIZE,
        BlockType::Dummy => DummyBlock::SERIALIZED_SIZE,
        BlockType::Epoch => EpochBlock::serialized_size(0), // Minimum size with 0 representatives
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Block {
    LegacySend(SendBlock),
    LegacyReceive(ReceiveBlock),
    LegacyOpen(OpenBlock),
    LegacyChange(ChangeBlock),
    State(StateBlock),
    Dummy(DummyBlock),
    Epoch(EpochBlock),
}

impl Block {
    pub fn new_test_instance() -> Self {
        let key = PrivateKey::from(42);
        Self::new_test_instance_with_key(key)
    }

    pub fn new_test_open() -> Self {
        let key = PrivateKey::from(42);
        StateBlockArgs {
            key: &key,
            previous: BlockHash::ZERO,
            representative: 789.into(),
            balance: 420.into(),
            link: 111.into(),
            work: 69420.into(),
        }
        .into()
    }

    pub fn new_test_instance_with_key(key: impl Into<PrivateKey>) -> Self {
        let key = key.into();
        StateBlockArgs {
            key: &key,
            previous: BlockHash::from_bytes(*key.account().as_bytes()),
            representative: 789.into(),
            balance: 420.into(),
            link: 111.into(),
            work: 69420.into(),
        }
        .into()
    }

    pub fn block_type(&self) -> BlockType {
        self.as_block().block_type()
    }

    pub fn as_block_mut(&mut self) -> &mut dyn BlockBase {
        match self {
            Block::LegacySend(b) => b,
            Block::LegacyReceive(b) => b,
            Block::LegacyOpen(b) => b,
            Block::LegacyChange(b) => b,
            Block::State(b) => b,
            Block::Dummy(b) => b,
            Block::Epoch(b) => b,
        }
    }

    pub fn as_block(&self) -> &dyn BlockBase {
        match self {
            Block::LegacySend(b) => b,
            Block::LegacyReceive(b) => b,
            Block::LegacyOpen(b) => b,
            Block::LegacyChange(b) => b,
            Block::State(b) => b,
            Block::Dummy(b) => b,
            Block::Epoch(b) => b,
        }
    }

    pub fn is_open(&self) -> bool {
        match &self {
            Block::LegacyOpen(_) => true,
            Block::State(state) => state.previous().is_zero(),
            _ => false,
        }
    }

    pub fn is_legacy(&self) -> bool {
        !matches!(self, Block::State(_) | Block::Dummy(_) | Block::Epoch(_))
    }

    pub fn is_change(&self) -> bool {
        match self {
            Block::LegacyChange(_) => true,
            Block::State(state) => state.link().is_zero(),
            _ => false,
        }
    }

    pub fn source_or_link(&self) -> BlockHash {
        self.source_field()
            .unwrap_or_else(|| self.link_field().unwrap_or_default().into())
    }

    pub fn destination_or_link(&self) -> Account {
        self.destination_field()
            .unwrap_or_else(|| self.link_field().unwrap_or_default().into())
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        let block_type_id = BlockTypeId::from(self.block_type()) as u8;
        writer.write_all(&[block_type_id])?;
        self.serialize_without_block_type(writer)
    }

    pub fn serialize_without_block_type<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        match self {
            Block::LegacySend(b) => b.serialize_without_block_type(writer),
            Block::LegacyReceive(b) => b.serialize_without_block_type(writer),
            Block::LegacyOpen(b) => b.serialize_without_block_type(writer),
            Block::LegacyChange(b) => b.serialize_without_block_type(writer),
            Block::State(b) => b.serialize_without_block_type(writer),
            Block::Dummy(b) => b.serialize_without_block_type(writer),
            Block::Epoch(b) => b.serialize_without_block_type(writer),
        }
    }

    pub fn deserialize_block_type<T>(
        block_type: BlockType,
        reader: &mut T,
    ) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let block = match block_type {
            BlockType::LegacyReceive => Self::LegacyReceive(ReceiveBlock::deserialize(reader)?),
            BlockType::LegacyOpen => Self::LegacyOpen(OpenBlock::deserialize(reader)?),
            BlockType::LegacyChange => Self::LegacyChange(ChangeBlock::deserialize(reader)?),
            BlockType::State => Self::State(StateBlock::deserialize(reader)?),
            BlockType::LegacySend => Self::LegacySend(SendBlock::deserialize(reader)?),
            BlockType::Dummy => Self::Dummy(DummyBlock::deserialize(reader)?),
            BlockType::Epoch => Self::Epoch(EpochBlock::deserialize(reader)?),
        };
        Ok(block)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Block, DeserializationError>
    where
        T: Read,
    {
        let block_type_id =
            BlockTypeId::from_u8(read_u8(reader)?).ok_or(DeserializationError::InvalidData)?;

        let block_type =
            BlockType::try_from(block_type_id).map_err(|_| DeserializationError::InvalidData)?;

        Self::deserialize_block_type(block_type, reader)
    }
}

impl From<Block> for serde_json::Value {
    fn from(value: Block) -> Self {
        serde_json::to_value(value.json_representation()).unwrap()
    }
}

impl From<SavedBlock> for serde_json::Value {
    fn from(value: SavedBlock) -> Self {
        let mut result = serde_json::to_value(value.block.json_representation()).unwrap();
        if let serde_json::Value::Object(obj) = &mut result {
            obj.insert(
                "subtype".to_string(),
                serde_json::Value::String(value.subtype().as_str().to_owned()),
            );
        }
        result
    }
}

impl Deref for Block {
    type Target = dyn BlockBase;

    fn deref(&self) -> &Self::Target {
        match self {
            Block::LegacySend(b) => b,
            Block::LegacyReceive(b) => b,
            Block::LegacyOpen(b) => b,
            Block::LegacyChange(b) => b,
            Block::State(b) => b,
            Block::Dummy(b) => b,
            Block::Epoch(b) => b,
        }
    }
}

impl DerefMut for Block {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Block::LegacySend(b) => b,
            Block::LegacyReceive(b) => b,
            Block::LegacyOpen(b) => b,
            Block::LegacyChange(b) => b,
            Block::State(b) => b,
            Block::Dummy(b) => b,
            Block::Epoch(b) => b,
        }
    }
}

impl serde::Serialize for Block {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let json = self.as_block().json_representation();
        json.serialize(serializer)
    }
}

#[derive(PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JsonBlock {
    Open(JsonOpenBlock),
    Change(JsonChangeBlock),
    Receive(JsonReceiveBlock),
    Send(JsonSendBlock),
    State(JsonStateBlock),
    Dummy(JsonDummyBlock),
    Epoch(JsonEpochBlock),
}

impl<'de> serde::Deserialize<'de> for Block {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let json_block = JsonBlock::deserialize(deserializer)?;
        Ok(json_block.into())
    }
}

impl From<JsonBlock> for Block {
    fn from(value: JsonBlock) -> Self {
        match value {
            JsonBlock::Open(open) => Block::LegacyOpen(open.into()),
            JsonBlock::Change(change) => Block::LegacyChange(change.into()),
            JsonBlock::Receive(receive) => Block::LegacyReceive(receive.into()),
            JsonBlock::Send(send) => Block::LegacySend(send.into()),
            JsonBlock::State(state) => Block::State(state.into()),
            JsonBlock::Dummy(dummy) => Block::Dummy(dummy.into()),
            JsonBlock::Epoch(epoch) => Block::Epoch(epoch.into()),
        }
    }
}

impl From<Block> for JsonBlock {
    fn from(value: Block) -> Self {
        value.as_block().json_representation()
    }
}

impl From<&Block> for JsonBlock {
    fn from(value: &Block) -> Self {
        value.as_block().json_representation()
    }
}

/// A block with additional data about that block (the "sideband")
/// which is only known when the block is saved.
/// The sideband contains additional data like block height, block time, etc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedBlock {
    block: Block,
    sideband: BlockSideband,
}

impl SavedBlock {
    pub fn new(block: Block, sideband: BlockSideband) -> Self {
        Self { block, sideband }
    }

    pub fn new_test_open_block() -> Self {
        let block = Block::new_test_open();
        let sideband = BlockSideband {
            height: 1,
            details: BlockDetails::new(Epoch::Epoch2, false, true, false),
            ..BlockSideband::new_test_instance_for(&block)
        };
        Self::new(block, sideband)
    }

    pub fn new_test_change_block() -> Self {
        Self::new_test_block_with_details(BlockDetails::new(Epoch::Epoch2, false, false, false))
    }

    pub fn new_test_send_block() -> Self {
        Self::new_test_block_with_details(BlockDetails::new(Epoch::Epoch2, true, false, false))
    }

    pub fn new_test_receive_block() -> Self {
        Self::new_test_block_with_details(BlockDetails::new(Epoch::Epoch2, false, true, false))
    }

    fn new_test_block_with_details(details: BlockDetails) -> Self {
        let block = Block::new_test_instance();
        let sideband = BlockSideband {
            details,
            ..BlockSideband::new_test_instance_for(&block)
        };
        Self::new(block, sideband)
    }

    pub fn new_test_instance() -> Self {
        let block = Block::new_test_instance();
        Self::new_test_instance_with(block)
    }

    pub fn new_test_instance_with(block: Block) -> Self {
        let sideband = Self::test_sideband(&block);
        Self::new(block, sideband)
    }

    pub fn new_test_instance_with_key(key: impl Into<PrivateKey>) -> Self {
        let block = Block::new_test_instance_with_key(key);
        let sideband = Self::test_sideband(&block);
        Self::new(block, sideband)
    }

    fn test_sideband(block: &Block) -> BlockSideband {
        BlockSideband {
            height: 2,
            timestamp: 222222.into(),
            account: block.account_field().unwrap(),
            balance: block.balance_field().unwrap(),
            details: BlockDetails::new(Epoch::Epoch2, true, false, false),
            source_epoch: Epoch::Epoch0,
        }
    }

    pub fn account(&self) -> Account {
        match self.account_field() {
            Some(account) => account,
            None => self.sideband.account,
        }
    }

    pub fn height(&self) -> u64 {
        self.sideband.height
    }

    pub fn timestamp(&self) -> UnixMillisTimestamp {
        self.sideband.timestamp
    }

    pub fn subtype(&self) -> BlockSubType {
        self.sideband.details.subtype()
    }

    pub fn epoch(&self) -> Epoch {
        self.sideband.details.epoch
    }

    pub fn is_epoch(&self) -> bool {
        self.sideband.details.is_epoch
    }

    pub fn is_receive(&self) -> bool {
        self.sideband.details.is_receive
    }

    pub fn is_send(&self) -> bool {
        match &self.block {
            Block::LegacySend(_) => true,
            Block::State(_) => self.sideband.details.is_send,
            _ => false,
        }
    }

    pub fn source(&self) -> Option<BlockHash> {
        match &self.block {
            Block::LegacyOpen(i) => Some(i.source()),
            Block::LegacyReceive(i) => Some(i.source()),
            Block::State(i) => {
                if self.sideband.details.is_receive {
                    Some(i.link().into())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn source_epoch(&self) -> Epoch {
        self.sideband.source_epoch
    }

    pub fn destination(&self) -> Option<Account> {
        match &self.block {
            Block::LegacySend(i) => Some(i.destination()),
            Block::State(i) => {
                if self.sideband.details.is_send {
                    Some(i.link().into())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn serialize_with_sideband(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(300);
        self.block
            .serialize(&mut buffer)
            .expect("Should serialize block");
        self.sideband
            .serialize(self.block.block_type(), &mut buffer)
            .expect("Should serialize sideband");
        buffer
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let block = Block::deserialize(reader)?;
        let mut sideband = BlockSideband::deserialize(reader, block.block_type())?;
        // BlockSideband does not serialize all data depending on the block type.
        // That's why we fill in the missing data here:
        match &block {
            Block::LegacySend(i) => {
                sideband.balance = i.balance();
                sideband.details = BlockDetails::new(Epoch::Epoch0, true, false, false)
            }
            Block::LegacyOpen(open) => {
                sideband.account = open.account();
                sideband.details = BlockDetails::new(Epoch::Epoch0, false, true, false)
            }
            Block::LegacyReceive(_) => {
                sideband.details = BlockDetails::new(Epoch::Epoch0, false, true, false)
            }
            Block::LegacyChange(_) => {
                sideband.details = BlockDetails::new(Epoch::Epoch0, false, false, false)
            }
            Block::State(state) => {
                sideband.account = state.account();
                sideband.balance = state.balance();
            }
            Block::Dummy(_) => {
                sideband.details = BlockDetails::new(Epoch::Epoch0, false, false, false)
            }
            Block::Epoch(_) => {
                sideband.details = BlockDetails::new(Epoch::Epoch0, false, false, true)
            }
        }
        Ok(SavedBlock { block, sideband })
    }

    pub fn balance(&self) -> Amount {
        match &self.block {
            Block::LegacySend(b) => b.balance(),
            Block::State(b) => b.balance(),
            Block::Epoch(_) => self.sideband.balance,
            _ => self.sideband.balance,
        }
    }

    pub fn details(&self) -> &BlockDetails {
        &self.sideband.details
    }

    pub fn sideband(&self) -> &BlockSideband {
        &self.sideband
    }

    /// There can be at most two dependencies per block, namely "previous" and "link/source".
    pub fn dependent_blocks(&self, epochs: &Epochs, genesis_account: &Account) -> DependentBlocks {
        match &self.block {
            Block::LegacySend(b) => b.dependent_blocks(),
            Block::LegacyChange(b) => b.dependent_blocks(),
            Block::LegacyReceive(b) => b.dependent_blocks(),
            Block::LegacyOpen(b) => b.dependent_blocks(genesis_account),
            Block::State(state) => {
                let link_refers_to_block = !self.is_send() && !epochs.is_epoch_link(&state.link());
                let linked_block = if link_refers_to_block {
                    state.link().into()
                } else {
                    BlockHash::ZERO
                };
                DependentBlocks::new(self.previous(), linked_block)
            }
            Block::Dummy(b) => b.dependent_blocks(),
            Block::Epoch(b) => b.dependent_blocks(),
        }
    }
}

impl Deref for SavedBlock {
    type Target = Block;

    fn deref(&self) -> &Self::Target {
        &self.block
    }
}

impl DerefMut for SavedBlock {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.block
    }
}

impl From<SavedBlock> for Block {
    fn from(value: SavedBlock) -> Self {
        value.block
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaybeSavedBlock {
    Saved(SavedBlock),
    Unsaved(Block),
}

impl From<MaybeSavedBlock> for Block {
    fn from(value: MaybeSavedBlock) -> Self {
        match value {
            MaybeSavedBlock::Saved(b) => b.into(),
            MaybeSavedBlock::Unsaved(b) => b,
        }
    }
}

impl Deref for MaybeSavedBlock {
    type Target = Block;

    fn deref(&self) -> &Self::Target {
        match self {
            MaybeSavedBlock::Saved(b) => b,
            MaybeSavedBlock::Unsaved(b) => b,
        }
    }
}

static DEV_PRIVATE_KEY_DATA: &str =
    "34F0A37AAD20F4A260F0A5B3CB3D7FB50673212263E58A380BC10474BB039CE4";
pub static DEV_PUBLIC_KEY_DATA: &str =
    "B0311EA55708D6A53C75CDBF88300259C6D018522FE3D4D0A242E431F9E8B6D0"; // xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo
pub static DEV_GENESIS_KEY: LazyLock<PrivateKey> =
    LazyLock::new(|| PrivateKey::from_hex_str(DEV_PRIVATE_KEY_DATA).unwrap());

static DEV_GENESIS_DATA: &str = r###"{
	"type": "open",
	"source": "B0311EA55708D6A53C75CDBF88300259C6D018522FE3D4D0A242E431F9E8B6D0",
	"representative": "xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"account": "xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"work": "7b42a00ee91d5810",
	"signature": "ECDA914373A2F0CA1296475BAEE40500A7F0A7AD72A5A80C81D7FAB7F6C802B2CC7DB50F5DD0FB25B2EF11761FA7344A158DD5A700B21BD47DE5BD0F63153A02"
    }"###;

pub static DEV_GENESIS_BLOCK: LazyLock<Block> =
    LazyLock::new(|| serde_json::from_str(DEV_GENESIS_DATA).unwrap());

#[derive(Default)]
pub struct DependentBlocks {
    dependents: [BlockHash; 2],
}

impl DependentBlocks {
    pub fn new(previous: BlockHash, link: BlockHash) -> Self {
        Self {
            dependents: [previous, link],
        }
    }

    pub fn none() -> Self {
        Self::new(BlockHash::ZERO, BlockHash::ZERO)
    }

    pub fn previous(&self) -> Option<BlockHash> {
        self.get_index(0)
    }

    pub fn link(&self) -> Option<BlockHash> {
        self.get_index(1)
    }

    fn get_index(&self, index: usize) -> Option<BlockHash> {
        if self.dependents[index].is_zero() {
            None
        } else {
            Some(self.dependents[index])
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &BlockHash> {
        self.dependents
            .iter()
            .flat_map(|i| if i.is_zero() { None } else { Some(i) })
    }
}

pub struct DetailedBlock {
    pub block: SavedBlock,
    pub amount: Option<Amount>,
    pub confirmed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_legacy_open() {
        let block = TestBlockBuilder::legacy_open().build_saved();
        assert_serializable(block.into());
    }

    #[test]
    fn serialize_legacy_receive() {
        let block = TestBlockBuilder::legacy_receive().build();
        assert_serializable(block);
    }

    #[test]
    fn serialize_legacy_send() {
        let block = TestBlockBuilder::legacy_send().build();
        assert_serializable(block);
    }

    #[test]
    fn serialize_legacy_change() {
        let block = TestBlockBuilder::legacy_change().build();
        assert_serializable(block);
    }

    #[test]
    fn serialize_state() {
        let block = TestBlockBuilder::state().build();
        assert_serializable(block);
    }

    #[test]
    fn serialize_dummy() {
        let block = Block::Dummy(DummyBlock::new_test_instance());
        assert_serializable(block);
    }

    fn assert_serializable(block: Block) {
        let mut buffer = Vec::new();
        block.serialize(&mut buffer).unwrap();
        let deserialized = Block::deserialize(&mut buffer.as_slice()).unwrap();
        assert_eq!(deserialized, block);
    }
}
