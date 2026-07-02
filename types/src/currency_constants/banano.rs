// This file contains the constants that are specific to Banano.

use crate::Amount;

pub const CURRENCY_NAME: &str = "Banano";

/// Prefix for accounts in encoded form like:
/// nano_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmXXXiq689wyjfpiij4txtd1
pub const ACCOUNT_PREFIX: &str = "ban";

/// How many raw are in a single coin?
pub const RAW_PER_COIN: u128 = 10u128.pow(29);

/// Network identifier bytes
pub const NETWORK_IDENTIFIER_DEV: u16 = 0x4241; // 'B', 'A'
pub const NETWORK_IDENTIFIER_BETA: u16 = 0x4242; // 'B', 'B'
pub const NETWORK_IDENTIFIER_LIVE: u16 = 0x4258; // 'B', 'X'
pub const NETWORK_IDENTIFIER_TEST: u16 = 0x4243; // 'R', 'C'

pub const DEFAULT_PORT_NODE: u16 = 7071;
pub const DEFAULT_PORT_RPC: u16 = 7072;
pub const DEFAULT_PORT_WEBSOCKET: u16 = 7074;

pub const WORK_THRESHOLD_EPOCH1: u64 = 0xfffffe0000000000;
pub const WORK_THRESHOLD_EPOCH2: u64 = 0xfffffff000000000; // 32x higher than originally
pub const WORK_THRESHOLD_EPOCH2_RECEIVE: u64 = 0x0000000000000000; // remove receive work
// requirements

pub const WORKING_PATH_PREFIX: &str = "Banano";

pub const PRECONFIGURED_REPRESENTATIVES_LIVE: [&str; 7] = [
    "ban_1fomoz167m7o38gw4rzt7hz67oq6itejpt4yocrfywujbpatd711cjew8gjj",
    "ban_1cake36ua5aqcq1c5i3dg7k8xtosw7r9r7qbbf5j15sk75csp9okesz87nfn",
    "ban_1bananobh5rat99qfgt1ptpieie5swmoth87thi74qgbfrij7dcgjiij94xr",
    "ban_1creepi89mp48wkyg5fktgap9j6165d8yz6g1fbe5pneinz3by9o54fuq63m",
    "ban_1tipbotgges3ss8pso6xf76gsyqnb69uwcxcyhouym67z7ofefy1jz7kepoy",
    "ban_1ka1ium4pfue3uxtntqsrib8mumxgazsjf58gidh1xeo5te3whsq8z476goo",
    "ban_1je44e5srozqqbimy94r9uusasw7kjnfabxtqbznbtqnift4irkkrg7fhd9o",
];

pub const PRECONFIGURED_REPRESENTATIVES_BETA: [&str; 1] =
    ["ban_1defau1t9off1ine9rep99999999999999999999999999999999wgmuzxxy"];

// Disabled, because livenet.banano.cc still runs an obsolete version
pub const PRECONFIGURED_PEERS_LIVE: [&str; 2] = ["livenet.banano.cc", "banano.rsnano.com"];

pub const PRECONFIGURED_PEERS_BETA: [&str; 1] = ["livenet-beta.banano.cc"];
pub const PRECONFIGURED_PEERS_TEST: [&str; 1] = ["peering-test.banano.cc"];

pub const BETA_PUBLIC_KEY_HEX: &str =
    "259A438A8F9F9226130C84D902C237AF3E57C0981C7D709C288046B110D8C8AC";

// nano_1jg8zygjg3pp5w644emqcbmjqpnzmubfni3kfe1s8pooeuxsw49fdq1mco9j
pub const TEST_PUBLIC_KEY_HEX: &str =
    "45C6FF9D1706D61F0821327752671BDA9F9ED2DA40326B01935AB566FB9E08ED";

pub const LIVE_GENESIS_JSON: &str = r###"{
        "type": "open",
	"source": "2514452A978F08D1CF76BB40B6AD064183CF275D3CC5D3E0515DC96E2112AD4E",
	"representative": "ban_1bananobh5rat99qfgt1ptpieie5swmoth87thi74qgbfrij7dcgjiij94xr",
	"account": "ban_1bananobh5rat99qfgt1ptpieie5swmoth87thi74qgbfrij7dcgjiij94xr",
	"work": "fa055f79fa56abcf",
	"signature": "533DCAB343547B93C4128E779848DEA5877D3278CB5EA948BB3A9AA1AE0DB293DE6D9DA4F69E8D1DDFA385F9B4C5E4F38DFA42C00D7B183560435D07AFA18900"
    }"###;

pub const BETA_GENESIS_JSON: &str = r###"{
        "type": "open",
        "source": "259A43ABDB779E97452E188BA3EB951B41C961D3318CA6B925380F4D99F0577A",
        "representative": "ban_1betagoxpxwykx4kw86dnhosc8t3s7ix8eeentwkcg1hbpez1outjrcyg4n1",
        "account": "ban_1betagoxpxwykx4kw86dnhosc8t3s7ix8eeentwkcg1hbpez1outjrcyg4n1",
        "work": "79d4e27dc873c6f2",
        "signature": "4BD7F96F9ED2721BCEE5EAED400EA50AD00524C629AE55E9AFF11220D2C1B00C3D4B3BB770BF67D4F8658023B677F91110193B6C101C2666931F57046A6DB806"
    }"###;

pub const TEST_GENESIS_JSON: &str = r###"{
        "type": "open",
        "source": "45C6FF9D1706D61F0821327752671BDA9F9ED2DA40326B01935AB566FB9E08ED",
        "representative": "ban_1jg8zygjg3pp5w644emqcbmjqpnzmubfni3kfe1s8pooeuxsw49fdq1mco9j",
        "account": "ban_1jg8zygjg3pp5w644emqcbmjqpnzmubfni3kfe1s8pooeuxsw49fdq1mco9j",
        "work": "bc1ef279c1a34eb1",
        "signature": "15049467CAEE3EC768639E8E35792399B6078DA763DA4EBA8ECAD33B0EDC4AF2E7403893A5A602EB89B978DABEF1D6606BB00F3C0EE11449232B143B6E07170E"
        }"###;

pub const DEV_GENESIS_JSON: &str = r###"{
	"type": "open",
	"source": "B0311EA55708D6A53C75CDBF88300259C6D018522FE3D4D0A242E431F9E8B6D0",
	"representative": "ban_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"account": "ban_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"work": "7b42a00ee91d5810",
	"signature": "ECDA914373A2F0CA1296475BAEE40500A7F0A7AD72A5A80C81D7FAB7F6C802B2CC7DB50F5DD0FB25B2EF11761FA7344A158DD5A700B21BD47DE5BD0F63153A02"
    }"###;

pub const LIVE_EPOCH_V2_SIGNER: &str =
    "ban_3qb6o6i1tkzr6jwr5s7eehfxwg9x6eemitdinbpi7u8bjjwsgqfj4wzser3x";

pub const DEFAULT_ONLINE_WEIGHT_MINIMUM: Amount = Amount::nano(900_000_000);
