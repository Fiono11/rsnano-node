use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RaiError {
    InvalidCommittee(String),
    UnknownCommittee(u64),
    UnknownElection(String),
    UnknownCandidate(String),
    InvalidSignature,
    InvalidVote(String),
    MissingVoteSupport(String),
    DuplicateFirstVote,
    DuplicateFinalVote,
    Inadmissible(String),
    UnsafeVote(String),
    Incomplete(String),
    InvalidClosePackage(String),
    SafetyFault(String),
    InvalidConfiguration(String),
    Io(String),
}

impl fmt::Display for RaiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCommittee(msg)
            | Self::InvalidVote(msg)
            | Self::MissingVoteSupport(msg)
            | Self::Inadmissible(msg)
            | Self::UnsafeVote(msg)
            | Self::Incomplete(msg)
            | Self::InvalidClosePackage(msg)
            | Self::SafetyFault(msg)
            | Self::InvalidConfiguration(msg)
            | Self::Io(msg) => write!(f, "{msg}"),
            Self::UnknownCommittee(id) => write!(f, "unknown committee {id}"),
            Self::UnknownElection(id) => write!(f, "unknown election {id}"),
            Self::UnknownCandidate(id) => write!(f, "unknown candidate {id}"),
            Self::InvalidSignature => write!(f, "invalid signature"),
            Self::DuplicateFirstVote => write!(f, "replica already cast its first vote"),
            Self::DuplicateFinalVote => write!(f, "replica already cast its final vote"),
        }
    }
}

impl std::error::Error for RaiError {}

pub type Result<T> = std::result::Result<T, RaiError>;
