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

pub const BETA_PUBLIC_KEY_HEX: &str =
    "259A438A8F9F9226130C84D902C237AF3E57C0981C7D709C288046B110D8C8AC";

// nano_1jg8zygjg3pp5w644emqcbmjqpnzmubfni3kfe1s8pooeuxsw49fdq1mco9j
pub const TEST_PUBLIC_KEY_HEX: &str =
    "45C6FF9D1706D61F0821327752671BDA9F9ED2DA40326B01935AB566FB9E08ED";

pub const LIVE_GENESIS_JSON: &str = r###"{
	"type": "open",
	"source": "E89208DD038FBB269987689621D52292AE9C35941A7484756ECCED92A65093BA",
	"representative": "xrb_3t6k35gi95xu6tergt6p69ck76ogmitsa8mnijtpxm9fkcm736xtoncuohr3",
	"account": "xrb_3t6k35gi95xu6tergt6p69ck76ogmitsa8mnijtpxm9fkcm736xtoncuohr3",
	"work": "62f05417dd3fb691",
	"signature": "9F0C933C8ADE004D808EA1985FA746A7E95BA2A38F867640F53EC8F180BDFE9E2C1268DEAD7C2664F356E37ABA362BC58E46DBA03E523A7B5A19E4B6EB12BB02"
    }"###;

pub const BETA_GENESIS_JSON: &str = r###"{
	"type": "open",
	"source": "259A438A8F9F9226130C84D902C237AF3E57C0981C7D709C288046B110D8C8AC",
	"representative": "nano_1betag7az9wk6rbis38s1d35hdsycz1bi95xg4g4j148p6afjk7embcurda4",
	"account": "nano_1betag7az9wk6rbis38s1d35hdsycz1bi95xg4g4j148p6afjk7embcurda4",
	"work": "e87a3ce39b43b84c",
	"signature": "BC588273AC689726D129D3137653FB319B6EE6DB178F97421D11D075B46FD52B6748223C8FF4179399D35CB1A8DF36F759325BD2D3D4504904321FAFB71D7602"
    }"###;

pub const TEST_GENESIS_JSON: &str = r###"{
        "type": "open",
        "source": "45C6FF9D1706D61F0821327752671BDA9F9ED2DA40326B01935AB566FB9E08ED",
        "representative": "nano_1jg8zygjg3pp5w644emqcbmjqpnzmubfni3kfe1s8pooeuxsw49fdq1mco9j",
        "account": "nano_1jg8zygjg3pp5w644emqcbmjqpnzmubfni3kfe1s8pooeuxsw49fdq1mco9j",
        "work": "bc1ef279c1a34eb1",
        "signature": "15049467CAEE3EC768639E8E35792399B6078DA763DA4EBA8ECAD33B0EDC4AF2E7403893A5A602EB89B978DABEF1D6606BB00F3C0EE11449232B143B6E07170E"
        }"###;

pub const DEV_GENESIS_JSON: &str = r###"{
	"type": "open",
	"source": "B0311EA55708D6A53C75CDBF88300259C6D018522FE3D4D0A242E431F9E8B6D0",
	"representative": "xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"account": "xrb_3e3j5tkog48pnny9dmfzj1r16pg8t1e76dz5tmac6iq689wyjfpiij4txtdo",
	"work": "7b42a00ee91d5810",
	"signature": "ECDA914373A2F0CA1296475BAEE40500A7F0A7AD72A5A80C81D7FAB7F6C802B2CC7DB50F5DD0FB25B2EF11761FA7344A158DD5A700B21BD47DE5BD0F63153A02"
    }"###;
