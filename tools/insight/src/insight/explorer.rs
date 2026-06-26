use rsnano_ledger::{AnySet, Ledger};
use rsnano_types::{Account, BlockHash, DetailedBlock};

pub(crate) fn search_ledger(ledger: &Ledger, input: &str, view_model: &mut ExplorerViewModel) {
    if let Some(hash) = BlockHash::decode_hex(input.trim()) {
        let any = ledger.any();
        match any.detailed_block(&hash) {
            Some(block) => {
                view_model.show(&block);
            }
            None => {
                *view_model = Default::default();
            }
        };
    } else if let Some(account) = Account::parse(input) {
        let any = ledger.any();
        if let Some(head) = any.account_head(&account) {
            match any.detailed_block(&head) {
                Some(block) => {
                    view_model.show(&block);
                }
                None => {
                    *view_model = Default::default();
                }
            }
        } else {
            *view_model = Default::default()
        };
    }
}

#[derive(Default)]
pub(crate) struct ExplorerViewModel {
    pub rollback_hash: String,
    pub hash: String,
    pub block: String,
    pub amount: String,
    pub confirmed: String,
    pub balance: String,
    pub height: String,
    pub timestamp: String,
    pub subtype: &'static str,
    pub destination: String,
}

impl ExplorerViewModel {
    pub fn show(&mut self, block: &DetailedBlock) {
        self.hash = block.block.hash().to_string();
        self.block = serde_json::to_string_pretty(&block.block.json_representation()).unwrap();
        self.balance = block.block.balance().to_string_dec();
        self.height = block.block.height().to_string();
        self.amount = block.amount.unwrap_or_default().to_string_dec();
        self.confirmed = block.confirmed.to_string();
        self.timestamp = block.block.timestamp().utc().to_string();
        self.subtype = block.block.subtype().as_str();
        self.destination = match block.block.destination() {
            Some(dest) => dest.encode_account(),
            None => String::new(),
        }
    }
}
