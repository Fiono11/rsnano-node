use crate::server::Service;
use rsnano_core::Account;

impl Service {
    pub async fn account_block_count(&self, account_str: String) -> String {
        let tx = self.node.ledger.read_txn();
        match Account::decode_account(&account_str) {
            Ok(account) => match self.node.ledger.store.account.get(&tx, &account) {
                Some(account_info) => {
                    format!("block_count: {}", account_info.block_count).to_string()
                }
                None => "Account not found".to_string(),
            },
            Err(e) => e.to_string(),
        }
    }
}
