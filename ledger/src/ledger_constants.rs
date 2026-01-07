use std::sync::LazyLock;

use rsnano_nullable_env::get_env_or_default_string;
use rsnano_types::{
    Account, Amount, Block, BlockDetails, BlockHash, BlockSideband, DEV_GENESIS_KEY, Epoch, Epochs,
    Networks, PublicKey, SavedBlock, UnixMillisTimestamp,
    currency_constants::{
        BETA_GENESIS_JSON, BETA_PUBLIC_KEY_HEX, DEV_GENESIS_JSON, LIVE_GENESIS_JSON,
        TEST_GENESIS_JSON, TEST_PUBLIC_KEY_HEX,
    },
    epoch_v1_link, epoch_v2_link,
};
use rsnano_work::{WORK_THRESHOLDS_STUB, WorkThresholds};

static TEST_PUBLIC_KEY_DATA: LazyLock<String> =
    LazyLock::new(|| get_env_or_default_string("NANO_TEST_GENESIS_PUB", TEST_PUBLIC_KEY_HEX));

static TEST_GENESIS_DATA: LazyLock<String> =
    LazyLock::new(|| get_env_or_default_string("NANO_TEST_GENESIS_BLOCK", TEST_GENESIS_JSON));

pub static LEDGER_CONSTANTS_STUB: LazyLock<LedgerConstants> =
    LazyLock::new(|| LedgerConstants::new(WorkThresholds::none(), Networks::NanoDevNetwork));

#[cfg(test)]
pub static IMPOSSIBLE_WORK: LazyLock<WorkThresholds> =
    LazyLock::new(|| WorkThresholds::impossible());

pub static DEV_GENESIS_BLOCK: LazyLock<SavedBlock> =
    LazyLock::new(|| LEDGER_CONSTANTS_STUB.genesis_block.clone());

pub static DEV_GENESIS_ACCOUNT: LazyLock<Account> =
    LazyLock::new(|| DEV_GENESIS_BLOCK.account_field().unwrap());
#[allow(dead_code)]
pub static DEV_GENESIS_PUB_KEY: LazyLock<PublicKey> =
    LazyLock::new(|| DEV_GENESIS_BLOCK.account_field().unwrap().into());
pub static DEV_GENESIS_HASH: LazyLock<BlockHash> = LazyLock::new(|| DEV_GENESIS_BLOCK.hash());

fn parse_block_from_genesis_data(genesis_data: &str) -> anyhow::Result<Block> {
    let block = serde_json::from_str(genesis_data)?;
    Ok(block)
}

#[cfg(test)]
mod tests {
    use rsnano_types::BlockType;

    use super::*;

    #[test]
    fn test_parse_block() {
        let block_str = r###"{"type": "open", "source": "37FCEA4DA94F1635484EFCBA57483C4C654F573B435C09D8AACE1CB45E63FFB1", "representative": "nano_1fzwxb8tkmrp8o66xz7tcx65rm57bxdmpitw39ecomiwpjh89zxj33juzt6p", "account": "nano_1fzwxb8tkmrp8o66xz7tcx65rm57bxdmpitw39ecomiwpjh89zxj33juzt6p", "work": "ef0547d86748c71b", "signature": "13E33D1ADA50A79B64741C5159C0C0AFE0515581B47ABD73676FE02A1D600CDB637050D37BF92C9629649AE92949814BB57C6B5B0A44BF76E2F33043A3DF2D01"}"###;
        let block = parse_block_from_genesis_data(block_str).unwrap();
        assert_eq!(block.block_type(), BlockType::LegacyOpen);
    }
}

#[derive(Clone)]
pub struct LedgerConstants {
    pub work: WorkThresholds,
    pub genesis_block: SavedBlock,
    pub genesis_account: Account,
    pub genesis_amount: Amount,
    pub burn_account: Account,
    pub epochs: Epochs,
}

pub fn genesis_sideband(genesis_account: Account) -> BlockSideband {
    BlockSideband {
        height: 1,
        timestamp: UnixMillisTimestamp::ZERO,
        account: genesis_account,
        balance: Amount::MAX,
        details: BlockDetails::new(Epoch::Epoch0, false, false, false),
        source_epoch: Epoch::Epoch0,
    }
}

impl LedgerConstants {
    pub fn new(work: WorkThresholds, network: Networks) -> Self {
        let dev_genesis_block = parse_block_from_genesis_data(DEV_GENESIS_JSON).unwrap();
        let beta_genesis_block = parse_block_from_genesis_data(BETA_GENESIS_JSON).unwrap();
        let live_genesis_block = parse_block_from_genesis_data(LIVE_GENESIS_JSON).unwrap();
        let test_genesis_block = parse_block_from_genesis_data(TEST_GENESIS_DATA.as_str()).unwrap();

        let genesis_block = match network {
            Networks::NanoDevNetwork => dev_genesis_block,
            Networks::NanoBetaNetwork => beta_genesis_block,
            Networks::NanoTestNetwork => test_genesis_block,
            Networks::NanoLiveNetwork => live_genesis_block,
            Networks::Invalid => panic!("invalid network"),
        };
        let genesis_account = genesis_block.account_field().unwrap();

        let nano_beta_account = Account::decode_hex(BETA_PUBLIC_KEY_HEX).unwrap();
        let nano_test_account = Account::decode_hex(TEST_PUBLIC_KEY_DATA.as_str()).unwrap();

        let mut epochs = Epochs::new();

        let epoch_1_signer = PublicKey::from(genesis_account);
        let epoch_link_v1 = epoch_v1_link();

        let nano_live_epoch_v2_signer =
            Account::parse("nano_3qb6o6i1tkzr6jwr5s7eehfxwg9x6eemitdinbpi7u8bjjwsgqfj4wzser3x")
                .unwrap();
        let epoch_2_signer = match network {
            Networks::NanoDevNetwork => DEV_GENESIS_KEY.public_key(),
            Networks::NanoBetaNetwork => nano_beta_account.into(),
            Networks::NanoLiveNetwork => nano_live_epoch_v2_signer.into(),
            Networks::NanoTestNetwork => nano_test_account.into(),
            _ => panic!("invalid network"),
        };
        let epoch_link_v2 = epoch_v2_link();

        epochs.add(Epoch::Epoch1, epoch_1_signer, epoch_link_v1);
        epochs.add(Epoch::Epoch2, epoch_2_signer, epoch_link_v2);

        let genesis_block = SavedBlock::new(genesis_block, genesis_sideband(genesis_account));

        Self {
            work,
            genesis_block,
            genesis_account,
            genesis_amount: Amount::raw(u128::MAX),
            burn_account: Account::ZERO,
            epochs,
        }
    }

    pub fn live() -> Self {
        Self::new(
            WorkThresholds::publish_full().clone(),
            Networks::NanoLiveNetwork,
        )
    }

    pub fn beta() -> Self {
        Self::new(
            WorkThresholds::publish_beta().clone(),
            Networks::NanoBetaNetwork,
        )
    }

    pub fn test() -> Self {
        Self::new(
            WorkThresholds::publish_test().clone(),
            Networks::NanoTestNetwork,
        )
    }

    pub fn dev() -> Self {
        Self::new(
            WorkThresholds::publish_dev().clone(),
            Networks::NanoDevNetwork,
        )
    }

    pub fn unit_test() -> Self {
        Self::new(WORK_THRESHOLDS_STUB.clone(), Networks::NanoDevNetwork)
    }
}
