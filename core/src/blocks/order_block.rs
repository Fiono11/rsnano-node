use super::{Block, BlockBase, BlockType};
use crate::{
    private_key::TEST_KEY,
    utils::{BufferWriter, Deserialize, FixedSizeSerialize, Serialize, Stream},
    Account, Amount, BlockHash, BlockHashBuilder, JsonBlock, Link, PrivateKey, PublicKey, Root,
    Signature, WorkNonce,
};
use anyhow::Result;

#[derive(Clone, Default, Debug)]
pub struct OrderBlock {
    hashables: OrderHashables,
    signature: Signature,
    hash: BlockHash,
    work: WorkNonce,
}

impl OrderBlock {
    pub fn verify_signature(&self) -> anyhow::Result<()> {
        self.account()
            .as_key()
            .verify(self.hash().as_bytes(), self.signature())
    }

    pub fn account(&self) -> Account {
        self.hashables.account
    }

    pub fn link(&self) -> Link {
        self.hashables.link
    }

    pub fn balance(&self) -> Amount {
        self.hashables.balance
    }

    pub fn source(&self) -> BlockHash {
        BlockHash::zero()
    }

    pub fn representative(&self) -> PublicKey {
        self.hashables.representative
    }

    pub fn destination(&self) -> Account {
        Account::zero()
    }

    pub fn serialized_size() -> usize {
        Account::serialized_size() // Account
            + BlockHash::serialized_size() // Previous
            + Account::serialized_size() // Representative
            + Amount::serialized_size() // Balance
            + Link::serialized_size() // Link
            + Signature::serialized_size()
            + std::mem::size_of::<u64>() // Work
    }

    pub fn deserialize(stream: &mut dyn Stream) -> Result<Self> {
        let account = Account::deserialize(stream)?;
        let previous = BlockHash::deserialize(stream)?;
        let representative = PublicKey::deserialize(stream)?;
        let balance = Amount::deserialize(stream)?;
        let link = Link::deserialize(stream)?;
        let signature = Signature::deserialize(stream)?;
        let mut work_bytes = [0u8; 8];
        stream.read_bytes(&mut work_bytes, 8)?;
        let work = u64::from_be_bytes(work_bytes).into();
        let hashables = OrderHashables {
            account,
            previous,
            representative,
            balance,
            link,
        };
        let hash = hashables.hash();
        Ok(Self {
            work,
            signature,
            hashables,
            hash,
        })
    }
}

impl PartialEq for OrderBlock {
    fn eq(&self, other: &Self) -> bool {
        self.work == other.work
            && self.signature == other.signature
            && self.hashables == other.hashables
    }
}

impl Eq for OrderBlock {}

impl BlockBase for OrderBlock {
    fn block_type(&self) -> BlockType {
        BlockType::Order
    }

    fn account_field(&self) -> Option<Account> {
        Some(self.hashables.account)
    }

    fn hash(&self) -> BlockHash {
        self.hash
    }

    fn link_field(&self) -> Option<Link> {
        Some(self.hashables.link)
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn set_signature(&mut self, signature: Signature) {
        self.signature = signature;
    }

    fn set_work(&mut self, work: WorkNonce) {
        self.work = work;
    }

    fn work(&self) -> WorkNonce {
        self.work
    }

    fn previous(&self) -> BlockHash {
        self.hashables.previous
    }

    fn serialize_without_block_type(&self, writer: &mut dyn BufferWriter) {
        self.hashables.account.serialize(writer);
        self.hashables.previous.serialize(writer);
        self.hashables.representative.serialize(writer);
        self.hashables.balance.serialize(writer);
        self.hashables.link.serialize(writer);
        self.signature.serialize(writer);
        writer.write_bytes_safe(&self.work.0.to_be_bytes());
    }

    fn root(&self) -> Root {
        if !self.previous().is_zero() {
            self.previous().into()
        } else {
            self.hashables.account.into()
        }
    }

    fn balance_field(&self) -> Option<Amount> {
        Some(self.hashables.balance)
    }

    fn source_field(&self) -> Option<BlockHash> {
        None
    }

    fn representative_field(&self) -> Option<PublicKey> {
        Some(self.hashables.representative)
    }

    fn valid_predecessor(&self, _block_type: BlockType) -> bool {
        true
    }

    fn destination_field(&self) -> Option<Account> {
        None
    }

    fn json_representation(&self) -> JsonBlock {
        JsonBlock::Order(JsonOrderBlock {
            account: self.hashables.account,
            previous: self.hashables.previous,
            representative: self.hashables.representative.into(),
            balance: self.hashables.balance,
            link: self.hashables.link,
            link_as_account: Some(self.hashables.link.into()),
            signature: self.signature.clone(),
            work: self.work.into(),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
struct OrderHashables {
    // Account# / public key that operates this account
    // Uses:
    // Bulk signature validation in advance of further ledger processing
    // Arranging uncomitted transactions by account
    account: Account,

    // Previous transaction in this chain
    previous: BlockHash,

    // Representative of this account
    representative: PublicKey,

    // Current balance of this account
    // Allows lookup of account balance simply by looking at the head block
    balance: Amount,

    // Link field contains source block_hash if receiving, destination account if sending
    link: Link,
}

impl OrderHashables {
    fn hash(&self) -> BlockHash {
        let mut preamble = [0u8; 32];
        preamble[31] = BlockType::Order as u8;
        BlockHashBuilder::new()
            .update(preamble)
            .update(self.account.as_bytes())
            .update(self.previous.as_bytes())
            .update(self.representative.as_bytes())
            .update(self.balance.to_be_bytes())
            .update(self.link.as_bytes())
            .build()
    }
}

#[derive(Clone)]
pub struct OrderBlockArgs<'a> {
    pub key: &'a PrivateKey,
    pub previous: BlockHash,
    pub representative: PublicKey,
    pub balance: Amount,
    pub link: Link,
    pub work: WorkNonce,
}

impl<'a> OrderBlockArgs<'a> {
    pub fn new_test_instance() -> Self {
        Self {
            key: &TEST_KEY,
            previous: 1.into(),
            representative: 2.into(),
            balance: 3.into(),
            link: 4.into(),
            work: 5.into(),
        }
    }
}

impl From<JsonOrderBlock> for OrderBlock {
    fn from(value: JsonOrderBlock) -> Self {
        let hashables = OrderHashables {
            account: value.account,
            previous: value.previous,
            representative: value.representative.into(),
            balance: value.balance,
            link: value.link,
        };

        let hash = hashables.hash();

        Self {
            work: value.work.into(),
            signature: value.signature,
            hashables,
            hash,
        }
    }
}

#[derive(PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct JsonOrderBlock {
    pub account: Account,
    pub previous: BlockHash,
    pub representative: Account,
    pub balance: Amount,
    pub link: Link,
    pub link_as_account: Option<Account>,
    pub signature: Signature,
    pub work: WorkNonce,
}