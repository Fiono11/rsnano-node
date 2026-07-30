//! RAI consensus support.
//!
//! RAI deliberately extends the existing election and vote pipeline. It does
//! not introduce a parallel consensus subsystem; later feature-gated code is
//! rooted in this module.
mod election_id;
mod election_vote_state;
mod epoch;
mod report;

pub use election_id::{rai_close_cut_root, rai_close_record_root};
pub use election_vote_state::*;
pub use epoch::{RaiEpochManager, RaiEpochPhase};
pub use report::*;
pub use rsnano_types::RaiEpoch;
