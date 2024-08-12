use rsnano_core::Account;
use rsnano_ledger::DEV_GENESIS;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct AvailableSupply {
    available: String,
}

impl AvailableSupply {
    fn new(available: String) -> Self {
        Self { available }
    }
}

pub(crate) async fn available_supply(node: Arc<Node>) -> String {
    let tx = node.store.env.tx_begin_read();
    let genesis_balance = node
        .balance(&node.network_params.ledger.genesis.account_field().unwrap())
        .number();

    let landing_balance = node
        .balance(
            &Account::decode_hex(
                "059F68AAB29DE0D3A27443625C7EA9CDDB6517A8B76FE37727EF6A4D76832AD5",
            )
            .unwrap(),
        )
        .number();

    let faucet_balance = node
        .balance(
            &Account::decode_hex(
                "8E319CE6F3025E5B2DF66DA7AB1467FE48F1679C13DD43BFDB29FA2E9FC40D3B",
            )
            .unwrap(),
        )
        .number();

    let burned_balance = node
        .ledger
        .account_receivable(
            &tx,
            &Account::decode_account(
                "nano_1111111111111111111111111111111111111111111111111111hifc8npp",
            )
            .unwrap(),
            false,
        )
        .number();

    let available = DEV_GENESIS.balance().number()
        - genesis_balance
        - landing_balance
        - faucet_balance
        - burned_balance;

    let available_supply = AvailableSupply::new(available.to_string());

    to_string_pretty(&available_supply).unwrap()
}
