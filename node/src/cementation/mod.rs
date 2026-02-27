mod conf_time_stats;
mod confirming_set;
mod ordered_entries;

pub(crate) use conf_time_stats::*;
pub use confirming_set::*;
use rsnano_types::BlockHash;

pub enum ConfirmingSetEvent {
    ConfirmationFailed(BlockHash),
    NearFull,
    Recovered,
}
