use rsnano_utils::stats::{DetailType, StatType};
use strum_macros::{EnumCount, EnumIter};

#[derive(Copy, Clone, Debug, PartialEq, Eq, EnumCount, EnumIter)]
pub enum ElectionState {
    Passive, // only listening for incoming votes
    Active,  // actively request confirmations
    /// Kudzu: a notarization certificate exists, the winner is in the block tree
    Terminated,
    /// Kudzu: a timeout certificate exists but no notarization certificate
    TimedOut,
    /// Kudzu: terminated and no further notarization certificate can form
    Settled,
    Confirmed, // confirmed (Kudzu: finalized) but still listening for votes
    ExpiredConfirmed,
    ExpiredUnconfirmed,
    Cancelled,
}

impl ElectionState {
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed | Self::ExpiredConfirmed)
    }

    pub fn has_ended(&self) -> bool {
        matches!(
            self,
            ElectionState::Confirmed
                | ElectionState::Cancelled
                | ElectionState::ExpiredConfirmed
                | ElectionState::ExpiredUnconfirmed
        )
    }

    /// Kudzu: the slot is done (Protocol 1, lines 9–13)
    pub fn is_terminated(&self) -> bool {
        matches!(
            self,
            Self::Terminated | Self::TimedOut | Self::Settled | Self::Confirmed
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ElectionState::Passive => "passive",
            ElectionState::Active => "active",
            ElectionState::Terminated => "terminated",
            ElectionState::TimedOut => "timed_out",
            ElectionState::Settled => "settled",
            ElectionState::Confirmed => "confirmed",
            ElectionState::ExpiredConfirmed => "expired_confirmed",
            ElectionState::ExpiredUnconfirmed => "expired_unconfirmed",
            ElectionState::Cancelled => "cancelled",
        }
    }
}

impl From<ElectionState> for StatType {
    fn from(value: ElectionState) -> Self {
        match value {
            ElectionState::Passive
            | ElectionState::Active
            | ElectionState::Terminated
            | ElectionState::TimedOut
            | ElectionState::Settled => StatType::ActiveElectionsDropped,
            ElectionState::Confirmed | ElectionState::ExpiredConfirmed => {
                StatType::ActiveElectionsConfirmed
            }
            ElectionState::ExpiredUnconfirmed => StatType::ActiveElectionsTimeout,
            ElectionState::Cancelled => StatType::ActiveElectionsCancelled,
        }
    }
}

impl From<ElectionState> for DetailType {
    fn from(value: ElectionState) -> Self {
        match value {
            ElectionState::Passive => DetailType::Passive,
            ElectionState::Active => DetailType::Active,
            ElectionState::Terminated => DetailType::Terminated,
            ElectionState::TimedOut => DetailType::TimedOut,
            ElectionState::Settled => DetailType::Settled,
            ElectionState::Confirmed => DetailType::Confirmed,
            ElectionState::ExpiredConfirmed => DetailType::ExpiredConfirmed,
            ElectionState::ExpiredUnconfirmed => DetailType::ExpiredUnconfirmed,
            ElectionState::Cancelled => DetailType::Cancelled,
        }
    }
}
