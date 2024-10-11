use crate::{BlockTypeDto, RpcCommand, WorkVersionDto};
use rsnano_core::{Account, Amount, BlockHash, JsonBlock, Link, RawKey, WalletId, WorkNonce};
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BlockCreateArgs {
    #[serde(rename = "type")]
    pub block_type: BlockTypeDto,
    pub balance: Amount,
    #[serde(flatten)]
    pub account_identifier: AccountIdentifier,
    #[serde(flatten)]
    pub transaction_info: TransactionInfo,
    pub representative: Account,
    pub previous: BlockHash,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work: Option<WorkNonce>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<WorkVersionDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<WorkNonce>,
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AccountIdentifier {
    WalletAccount { wallet: WalletId, account: Account },
    PrivateKey { key: RawKey },
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TransactionInfo {
    Send { destination: Account },
    Receive { source: BlockHash },
    Link { link: Link },
}

impl RpcCommand {
    pub fn block_create(block_create_args: BlockCreateArgs) -> Self {
        Self::BlockCreate(block_create_args)
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BlockCreateDto {
    pub hash: BlockHash,
    pub difficulty: WorkNonce,
    pub block: JsonBlock,
}

impl BlockCreateDto {
    pub fn new(hash: BlockHash, difficulty: WorkNonce, block: JsonBlock) -> Self {
        Self {
            hash,
            difficulty,
            block,
        }
    }
}

impl BlockCreateArgs {
    pub fn builder(block_type: BlockTypeDto, balance: Amount, account_identifier: AccountIdentifier, transaction_info: TransactionInfo, previous: BlockHash, representative: Account) -> BlockCreateArgsBuilder {
        BlockCreateArgsBuilder::new(block_type, balance, account_identifier, transaction_info, previous, representative)
    }
}

pub struct BlockCreateArgsBuilder {
    args: BlockCreateArgs,
}

impl BlockCreateArgsBuilder {
    fn new(block_type: BlockTypeDto, balance: Amount, account_identifier: AccountIdentifier, transaction_info: TransactionInfo, previous: BlockHash, representative: Account) -> Self {
        Self {
            args: BlockCreateArgs {
                block_type,
                balance,
                account_identifier,
                transaction_info,
                representative,
                previous,
                work: None,
                version: None,
                difficulty: None,
            },
        }
    }

    pub fn work(mut self, work: WorkNonce) -> Self {
        self.args.work = Some(work);
        self.args.version = None; 
        self.args.difficulty = None; 
        self
    }

    pub fn version(mut self, version: WorkVersionDto) -> Self {
        if self.args.work.is_none() {
            self.args.version = Some(version);
        }
        self
    }

    pub fn difficulty(mut self, difficulty: WorkNonce) -> Self {
        if self.args.work.is_none() {
            self.args.difficulty = Some(difficulty);
        }
        self
    }

    pub fn build(self) -> Result<BlockCreateArgs, &'static str> {
        Ok(self.args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_core::{Account, Amount, BlockHash, Link, RawKey, WalletId, WorkNonce};

    #[test]
    fn test_block_create_args_builder() {
        let wallet = WalletId::zero();
        let account = Account::zero();
        let balance = Amount::raw(1000);
        let representative = Account::zero();
        let previous = BlockHash::zero();
        let destination = Account::zero();

        let args = BlockCreateArgs::builder(BlockTypeDto::State, balance, AccountIdentifier::WalletAccount { wallet, account }, TransactionInfo::Send { destination }, previous, representative)
            .work(WorkNonce::from(10))
            .version(WorkVersionDto::Work1) // This should be ignored
            .difficulty(WorkNonce::from(0x1234567890abcdef)) // This should be ignored
            .build()
            .unwrap();

        assert_eq!(args.block_type, BlockTypeDto::State);
        assert_eq!(args.balance, balance);
        assert_eq!(args.account_identifier, AccountIdentifier::WalletAccount { wallet, account });
        assert_eq!(args.transaction_info, TransactionInfo::Send { destination });
        assert_eq!(args.representative, representative);
        assert_eq!(args.previous, previous);
        assert_eq!(args.work, Some(WorkNonce::from(10)));
        assert_eq!(args.version, None);
        assert_eq!(args.difficulty, None);

        let serialized = serde_json::to_string(&args).unwrap();
        let deserialized: BlockCreateArgs = serde_json::from_str(&serialized).unwrap();

        assert_eq!(args, deserialized);
    }

    #[test]
    fn test_block_create_args_builder_private_key() {
        let key = RawKey::zero();
        let balance = Amount::raw(1000);
        let representative = Account::zero();
        let previous = BlockHash::zero();
        let source = BlockHash::zero();

        let args = BlockCreateArgs::builder(BlockTypeDto::State, balance, AccountIdentifier::PrivateKey { key }, TransactionInfo::Receive { source }, previous, representative)
            .build()
            .unwrap();

        assert_eq!(args.account_identifier, AccountIdentifier::PrivateKey { key });
        assert_eq!(args.transaction_info, TransactionInfo::Receive { source });
        assert_eq!(args.representative, representative);
        assert_eq!(args.previous, previous);

        let serialized = serde_json::to_string(&args).unwrap();
        let deserialized: BlockCreateArgs = serde_json::from_str(&serialized).unwrap();

        assert_eq!(args, deserialized);
    }

    #[test]
    fn test_block_create_args_builder_link() {
        let wallet = WalletId::zero();
        let account = Account::zero();
        let balance = Amount::raw(1000);
        let representative = Account::zero();
        let previous = BlockHash::zero();
        let link = Link::zero();

        let args = BlockCreateArgs::builder(BlockTypeDto::State, balance, AccountIdentifier::WalletAccount { wallet, account }, TransactionInfo::Link { link }, previous, representative)
            .build()
            .unwrap();

        assert_eq!(args.account_identifier, AccountIdentifier::WalletAccount { wallet, account });
        assert_eq!(args.transaction_info, TransactionInfo::Link { link });
        assert_eq!(args.representative, representative);
        assert_eq!(args.previous, previous);

        let serialized = serde_json::to_string(&args).unwrap();
        let deserialized: BlockCreateArgs = serde_json::from_str(&serialized).unwrap();

        assert_eq!(args, deserialized);
    }

    #[test]
    fn test_serialize_block_create_args() {
        let wallet = WalletId::zero();
        let account = Account::zero();
        let balance = Amount::raw(1000);
        let representative = Account::zero();
        let previous = BlockHash::zero();
        let destination = Account::zero();

        let args = BlockCreateArgs::builder(BlockTypeDto::State, balance, AccountIdentifier::WalletAccount { wallet, account }, TransactionInfo::Send { destination }, previous, representative)
            .work(WorkNonce::from(10))
            .version(WorkVersionDto::Work1)
            .difficulty(WorkNonce::from(0x1234567890abcdef))
            .build()
            .unwrap();

        let serialized = serde_json::to_string(&args).unwrap();
        let deserialized: BlockCreateArgs = serde_json::from_str(&serialized).unwrap();

        assert_eq!(args, deserialized);
    }

    #[test]
    fn test_deserialize_block_create_args_send() {
        let json = r#"{
            "type": "state",
            "balance": "1000",
            "wallet": "0000000000000000000000000000000000000000000000000000000000000000",
            "account": "nano_1111111111111111111111111111111111111111111111111111hifc8npp",
            "destination": "nano_1111111111111111111111111111111111111111111111111111hifc8npp",
            "representative": "nano_1111111111111111111111111111111111111111111111111111hifc8npp",
            "previous": "0000000000000000000000000000000000000000000000000000000000000000",
            "work": "0000000000000000",
            "version": "work1",
            "difficulty": "1234567890abcdef"
        }"#;

        let args: BlockCreateArgs = serde_json::from_str(json).unwrap();

        assert_eq!(args.block_type, BlockTypeDto::State);
        assert_eq!(args.balance, Amount::raw(1000));
        assert!(matches!(args.account_identifier, AccountIdentifier::WalletAccount { .. }));
        assert!(matches!(args.transaction_info, TransactionInfo::Send { .. }));
        assert_eq!(args.representative, Account::zero());
        assert_eq!(args.previous, BlockHash::zero());
        assert_eq!(args.work, Some(WorkNonce::from(0)));
        assert_eq!(args.version, Some(WorkVersionDto::Work1));
        assert_eq!(args.difficulty, Some(WorkNonce::from(0x1234567890abcdef)));
    }

    #[test]
    fn test_deserialize_block_create_args_receive() {
        let json = r#"{
            "type": "state",
            "balance": "2000",
            "key": "0000000000000000000000000000000000000000000000000000000000000000",
            "source": "0000000000000000000000000000000000000000000000000000000000000000",
            "representative": "nano_1111111111111111111111111111111111111111111111111111hifc8npp",
            "previous": "0000000000000000000000000000000000000000000000000000000000000000"
        }"#;

        let args: BlockCreateArgs = serde_json::from_str(json).unwrap();

        assert_eq!(args.block_type, BlockTypeDto::State);
        assert_eq!(args.balance, Amount::raw(2000));
        assert!(matches!(args.account_identifier, AccountIdentifier::PrivateKey { .. }));
        assert!(matches!(args.transaction_info, TransactionInfo::Receive { .. }));
        assert_eq!(args.representative, Account::zero());
        assert_eq!(args.previous, BlockHash::zero());
        assert_eq!(args.work, None);
        assert_eq!(args.version, None);
        assert_eq!(args.difficulty, None);
    }

    #[test]
    fn test_deserialize_block_create_args_link() {
        let json = r#"{
            "type": "state",
            "balance": "3000",
            "wallet": "0000000000000000000000000000000000000000000000000000000000000000",
            "account": "nano_1111111111111111111111111111111111111111111111111111hifc8npp",
            "link": "0000000000000000000000000000000000000000000000000000000000000000",
            "representative": "nano_1111111111111111111111111111111111111111111111111111hifc8npp",
            "previous": "0000000000000000000000000000000000000000000000000000000000000000"
        }"#;

        let args: BlockCreateArgs = serde_json::from_str(json).unwrap();

        assert_eq!(args.block_type, BlockTypeDto::State);
        assert_eq!(args.balance, Amount::raw(3000));
        assert!(matches!(args.account_identifier, AccountIdentifier::WalletAccount { .. }));
        assert!(matches!(args.transaction_info, TransactionInfo::Link { .. }));
        assert_eq!(args.representative, Account::zero());
        assert_eq!(args.previous, BlockHash::zero());
        assert_eq!(args.work, None);
        assert_eq!(args.version, None);
        assert_eq!(args.difficulty, None);
    }

    #[test]
    fn test_block_create_args_builder_with_version_and_difficulty() {
        let wallet = WalletId::zero();
        let account = Account::zero();
        let balance = Amount::raw(1000);
        let representative = Account::zero();
        let previous = BlockHash::zero();
        let destination = Account::zero();

        let args = BlockCreateArgs::builder(BlockTypeDto::State, balance, AccountIdentifier::WalletAccount { wallet, account }, TransactionInfo::Send { destination }, previous, representative)
            .version(WorkVersionDto::Work1)
            .difficulty(WorkNonce::from(0x1234567890abcdef))
            .build()
            .unwrap();

        assert_eq!(args.work, None);
        assert_eq!(args.version, Some(WorkVersionDto::Work1));
        assert_eq!(args.difficulty, Some(WorkNonce::from(0x1234567890abcdef)));

        let serialized = serde_json::to_string(&args).unwrap();
        let deserialized: BlockCreateArgs = serde_json::from_str(&serialized).unwrap();

        assert_eq!(args, deserialized);
    }

    #[test]
    fn test_block_create_args_builder_default_version() {
        let wallet = WalletId::zero();
        let account = Account::zero();
        let balance = Amount::raw(1000);
        let representative = Account::zero();
        let previous = BlockHash::zero();
        let destination = Account::zero();

        let args = BlockCreateArgs::builder(BlockTypeDto::State, balance, AccountIdentifier::WalletAccount { wallet, account }, TransactionInfo::Send { destination }, previous, representative)
            .build()
            .unwrap();

        assert_eq!(args.work, None);
        assert_eq!(args.version, None);
        assert_eq!(args.difficulty, None);

        let serialized = serde_json::to_string(&args).unwrap();
        let deserialized: BlockCreateArgs = serde_json::from_str(&serialized).unwrap();

        assert_eq!(args, deserialized);
    }
}