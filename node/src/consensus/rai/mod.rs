//! RAI consensus support.
//!
//! RAI deliberately extends the existing election and vote pipeline. It does
//! not introduce a parallel consensus subsystem; later feature-gated code is
//! rooted in this module.
mod close_record;
mod close_round;
mod election_id;
mod election_vote_state;
mod epoch;
mod report;

pub use close_record::*;
pub use close_round::*;
pub use election_id::{rai_close_cut_root, rai_close_record_root};
pub use election_vote_state::*;
pub use epoch::{
    CloseRecordDecisionError, RaiClosingEpoch, RaiClosingPhase, RaiDurableCloseRoundState,
    RaiDurableCloseState, RaiEpochManager, RaiEpochState,
};
pub use report::*;
pub use rsnano_types::RaiEpoch;
