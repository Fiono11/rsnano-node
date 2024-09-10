use rsnano_node::stats::StatCategory;
use serde::{Deserialize, Serialize};
use crate::RpcCommand;

impl RpcCommand {
    pub fn stats(stats_category: StatCategory) -> Self {
        Self::Stats(StatsArgs::new(stats_category))
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]

pub struct StatsArgs {
    #[serde(rename = "type")]
    pub stat_category: StatCategory
}

impl StatsArgs {
    pub fn new(stat_category: StatCategory) -> Self {
        Self { stat_category }
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StatsDto<T> {
    #[serde(rename = "type")]
    pub stat_category: StatCategory,
    pub created: u64,
    pub entries: T,
}

impl<T: Serialize> StatsDto<T> {
    pub fn new(stat_category: StatCategory, entries: T, created: u64) -> Self {
        Self {
            stat_category,
            created,
            entries,
        }
    }
}