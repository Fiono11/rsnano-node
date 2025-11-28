// This file contains the constants that are specific to Nano.
// If you would like to create a fork then change these:

/// Prefix for accounts in encoded form like:
/// nano_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmXXXiq689wyjfpiij4txtd1
pub(crate) const ACCOUNT_PREFIX: &str = "nano";

/// How many raw are in a single coin?
pub(crate) const RAW_PER_COIN: u128 = 10u128.pow(30);

/// Network identifier bytes
pub(crate) const NETWORK_IDENTIFIER_DEV: u16 = 0x5241; // 'R', 'A'
pub(crate) const NETWORK_IDENTIFIER_BETA: u16 = 0x5242; // 'R', 'B'
pub(crate) const NETWORK_IDENTIFIER_LIVE: u16 = 0x5243; // 'R', 'C'
pub(crate) const NETWORK_IDENTIFIER_TEST: u16 = 0x5258; // 'R', 'X'

pub const DEFAULT_PORT_NODE: u16 = 7075;
pub const DEFAULT_PORT_RPC: u16 = 7076;
pub const DEFAULT_PORT_WEBSOCKET: u16 = 7078;

pub const WORK_THRESHOLD_EPOCH1: u64 = 0xffffffc000000000;
pub const WORK_THRESHOLD_EPOCH2: u64 = 0xfffffff800000000; // 8x higher than epoch_1
pub const WORK_THRESHOLD_EPOCH2_RECEIVE: u64 = 0xfffffe0000000000; // 8x lower than epoch_1;

pub const WORKING_PATH_PREFIX: &str = "Nano";

pub const PEERING_LIVE: &str = "peering.nano.org";
pub const PEERING_BETA: &str = "peering-beta.nano.org";
pub const PEERING_TEST: &str = "peering-test.nano.org";
