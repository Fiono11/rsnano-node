//! RAI consensus support.
//!
//! RAI deliberately extends the existing election and vote pipeline. It does
//! not introduce a parallel consensus subsystem; later feature-gated code is
//! rooted in this module.
mod election_id;
mod epoch;

pub use election_id::{rai_close_cut_root, rai_close_record_root};
pub use epoch::{RaiEpoch, RaiEpochManager, RaiEpochPhase};
