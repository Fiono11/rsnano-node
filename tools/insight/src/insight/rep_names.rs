use std::collections::HashMap;

use rsnano_types::{Account, PublicKey};

pub(crate) fn well_known_rep_names() -> HashMap<PublicKey, &'static str> {
    let rep_names = {
        #[cfg(not(feature = "banano"))]
        {
            include_str!("../../rep_names_nano.txt")
        }
        #[cfg(feature = "banano")]
        {
            include_str!("../../rep_names_banano.txt")
        }
    };
    rep_names
        .lines()
        .filter_map(|l| {
            let trimmed = l.trim();
            if !trimmed.is_empty() {
                let (account, name) = trimmed.split_once(' ').unwrap();
                Some((Account::parse(account).unwrap().as_key(), name))
            } else {
                None
            }
        })
        .collect()
}
