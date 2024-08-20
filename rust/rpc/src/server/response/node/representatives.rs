use rsnano_core::Account;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::{collections::HashMap, sync::Arc};

#[derive(Serialize)]
struct Representatives {
    accounts: HashMap<String, u128>,
}

impl Representatives {
    fn new(accounts: HashMap<String, u128>) -> Self {
        Self { accounts }
    }

    pub fn sort_by_amount(&mut self) {
        self.accounts.sort_by(|a, b| b.1.cmp(&a.1)); // Sort descending by Amount
    }

    pub fn sort_by_account(&mut self) {
        self.accounts.sort_by(|a, b| a.0.cmp(&b.0)); // Sort ascending by Account
    }
}

pub(crate) async fn representatives(
    node: Arc<Node>,
    count: Option<String>,
    sorting: Option<String>,
) -> String {
    // Access the online representatives from the node
    let mut accounts = node.online_reps.clone(); // Assuming this is a cloneable collection

    // Sort the representatives if sorting is specified
    if let Some(sort_order) = sorting {
        match sort_order.as_str() {
            "amount" => accounts.sort_by(|a, b| b.1.cmp(&a.1)), // Sort descending by Amount
            "account" => accounts.sort_by(|a, b| a.0.cmp(&b.0)), // Sort ascending by Account
            _ => (),
        }
    }

    // Apply count limit if provided
    if let Some(count_str) = count {
        if let Ok(count) = count_str.parse::<usize>() {
            if count < accounts.len() {
                accounts.truncate(count);
            }
        }
    }

    // Convert the list of representatives into the desired JSON format
    let mut rep_map = HashMap::new();
    for (account, amount) in accounts {
        rep_map.insert(account, amount.number());
    }

    // Wrap the map in a JSON object
    let response = json!({
        "representatives": rep_map
    });

    // Convert to pretty JSON string
    serde_json::to_string_pretty(&response)
        .unwrap_or_else(|_| "Error converting to JSON".to_string())
}
