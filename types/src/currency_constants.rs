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

pub const PRECONFIGURED_REPRESENTATIVES_LIVE: [&'static str; 8] = [
    "nano_3arg3asgtigae3xckabaaewkx3bzsh7nwz7jkmjos79ihyaxwphhm6qgjps4",
    "nano_1stofnrxuz3cai7ze75o174bpm7scwj9jn3nxsn8ntzg784jf1gzn1jjdkou",
    "nano_1q3hqecaw15cjt7thbtxu3pbzr1eihtzzpzxguoc37bj1wc5ffoh7w74gi6p",
    "nano_3dmtrrws3pocycmbqwawk6xs7446qxa36fcncush4s1pejk16ksbmakis78m",
    "nano_3hd4ezdgsp15iemx7h81in7xz5tpxi43b6b41zn3qmwiuypankocw3awes5k",
    "nano_1awsn43we17c1oshdru4azeqjz9wii41dy8npubm4rg11so7dx3jtqgoeahy",
    "nano_1anrzcuwe64rwxzcco8dkhpyxpi8kd7zsjc1oeimpc3ppca4mrjtwnqposrs",
    "nano_1hza3f7wiiqa7ig3jczyxj5yo86yegcmqk3criaz838j91sxcckpfhbhhra1",
];

pub const PRECONFIGURED_REPRESENTATIVES_BETA: [&'static str; 1] =
    ["nano_1defau1t9off1ine9rep99999999999999999999999999999999wgmuzxxy"];

pub const PRECONFIGURED_PEERS_LIVE: [&'static str; 1] = ["peering.nano.org"];
pub const PRECONFIGURED_PEERS_BETA: [&'static str; 1] = ["peering-beta.nano.org"];
pub const PRECONFIGURED_PEERS_TEST: [&'static str; 1] = ["peering-test.nano.org"];
