// This file contains the constants that are specific to Nano.
// If you would like to create a fork then change these:

/// Prefix for accounts in encoded form like:
/// nano_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmXXXiq689wyjfpiij4txtd1
pub(crate) const ACCOUNT_PREFIX: &str = "nano";

/// How many raw are in a single coin?
pub(crate) const RAW_PER_COIN: u128 = 10u128.pow(30);

pub(crate) const NETWORK_IDENTIFIER_DEV: u16 = 0x5241; // 'R', 'A'
pub(crate) const NETWORK_IDENTIFIER_BETA: u16 = 0x5242; // 'R', 'B'
pub(crate) const NETWORK_IDENTIFIER_LIVE: u16 = 0x5243; // 'R', 'C'
pub(crate) const NETWORK_IDENTIFIER_TEST: u16 = 0x5258; // 'R', 'X'
