use crate::server::Service;
use rsnano_core::Account;
use rsnano_ledger::DEV_GENESIS;
use rsnano_node::{BUILD_INFO, VERSION_STRING};
use serde::Serialize;
use serde_json::to_string_pretty;

#[derive(Serialize)]
struct AvailableSupply {
    available: String,
}

impl AvailableSupply {
    fn new(available: String) -> Self {
        Self { available }
    }
}

impl Service {
    pub(crate) async fn available_supply(&self) -> String {
        let mut tx = self.node.store.env.tx_begin_read();
        let genesis_balance = self
            .node
            .balance(
                &self
                    .node
                    .network_params
                    .ledger
                    .genesis
                    .account_field()
                    .unwrap(),
            )
            .number();

        let landing_balance = self
            .node
            .balance(
                &Account::decode_hex(
                    "059F68AAB29DE0D3A27443625C7EA9CDDB6517A8B76FE37727EF6A4D76832AD5",
                )
                .unwrap(),
            )
            .number();

        let faucet_balance = self
            .node
            .balance(
                &Account::decode_hex(
                    "8E319CE6F3025E5B2DF66DA7AB1467FE48F1679C13DD43BFDB29FA2E9FC40D3B",
                )
                .unwrap(),
            )
            .number();

        let burned_balance = self
            .node
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
}
