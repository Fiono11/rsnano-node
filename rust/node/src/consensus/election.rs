use super::ElectionStatus;
use crate::{stats::DetailType, utils::HardenedConstants};
use rsnano_core::{Account, Amount, BlockEnum, BlockHash, QualifiedRoot, Root};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant, SystemTime},
};

//TODO remove the many RwLocks
pub struct Election {
    pub id: usize,
    pub mutex: Mutex<ElectionData>,
    pub root: Root,
    pub qualified_root: QualifiedRoot,
    pub is_quorum: AtomicBool,
    pub confirmation_request_count: AtomicU32,
    // These are modified while not holding the mutex from transition_time only
    last_block: RwLock<Instant>,
    pub last_req: RwLock<Option<Instant>>,
    pub behavior: ElectionBehavior,
    pub election_start: Instant,
    pub confirmation_action: Box<dyn Fn(Arc<BlockEnum>)>,
    pub live_vote_action: Box<dyn Fn(Account)>,
}

impl Election {
    pub fn new(
        id: usize,
        block: Arc<BlockEnum>,
        behavior: ElectionBehavior,
        confirmation_action: Box<dyn Fn(Arc<BlockEnum>)>,
        live_vote_action: Box<dyn Fn(Account)>,
    ) -> Self {
        let root = block.root();
        let qualified_root = block.qualified_root();

        let data = ElectionData {
            status: ElectionStatus {
                winner: Some(Arc::clone(&block)),
                election_end: SystemTime::now(),
                block_count: 1,
                election_status_type: super::ElectionStatusType::Ongoing,
                ..Default::default()
            },
            last_votes: HashMap::from([(
                HardenedConstants::get().not_an_account,
                VoteInfo::new(0, block.hash()),
            )]),
            last_blocks: HashMap::from([(block.hash(), block)]),
            state: ElectionState::Passive,
            state_start: Instant::now(),
            last_tally: HashMap::new(),
            final_weight: Amount::zero(),
            last_vote: None,
            last_block_hash: BlockHash::zero(),
        };

        Self {
            id,
            mutex: Mutex::new(data),
            root,
            qualified_root,
            is_quorum: AtomicBool::new(false),
            confirmation_request_count: AtomicU32::new(0),
            last_block: RwLock::new(Instant::now()),
            behavior,
            election_start: Instant::now(),
            last_req: RwLock::new(None),
            confirmation_action,
            live_vote_action,
        }
    }

    pub fn set_last_req(&self) {
        *self.last_req.write().unwrap() = Some(Instant::now());
    }

    pub fn last_req_elapsed(&self) -> Duration {
        match self.last_req.read().unwrap().as_ref() {
            Some(i) => i.elapsed(),
            None => Duration::from_secs(60 * 60 * 24 * 365), // Duration::MAX caused problems with C++
        }
    }

    pub fn set_last_block(&self) {
        *self.last_block.write().unwrap() = Instant::now();
    }

    pub fn last_block_elapsed(&self) -> Duration {
        self.last_block.read().unwrap().elapsed()
    }
}

pub struct ElectionData {
    pub status: ElectionStatus,
    pub state: ElectionState,
    pub state_start: Instant,
    pub last_blocks: HashMap<BlockHash, Arc<BlockEnum>>,
    pub last_votes: HashMap<Account, VoteInfo>,
    pub final_weight: Amount,
    pub last_tally: HashMap<BlockHash, Amount>,
    /** The last time vote for this election was generated */
    pub last_vote: Option<Instant>,
    pub last_block_hash: BlockHash,
}

impl ElectionData {
    pub fn update_status_to_confirmed(&mut self, election: &Election) {
        self.status.election_end = SystemTime::now();
        self.status.election_duration = election.election_start.elapsed();
        self.status.confirmation_request_count =
            election.confirmation_request_count.load(Ordering::SeqCst);
        self.status.block_count = self.last_blocks.len() as u32;
        self.status.voter_count = self.last_votes.len() as u32;
    }

    pub fn state_change(
        &mut self,
        expected: ElectionState,
        desired: ElectionState,
    ) -> Result<(), ()> {
        if Self::valid_change(expected, desired) {
            if self.state == expected {
                self.state = desired;
                self.state_start = Instant::now();
                return Ok(());
            }
        }

        Err(())
    }

    fn valid_change(expected: ElectionState, desired: ElectionState) -> bool {
        match expected {
            ElectionState::Passive => match desired {
                ElectionState::Active
                | ElectionState::Confirmed
                | ElectionState::ExpiredUnconfirmed => true,
                _ => false,
            },
            ElectionState::Active => match desired {
                ElectionState::Confirmed | ElectionState::ExpiredUnconfirmed => true,
                _ => false,
            },
            ElectionState::Confirmed => match desired {
                ElectionState::ExpiredConfirmed => true,
                _ => false,
            },
            _ => false,
        }
    }

    pub fn set_last_vote(&mut self) {
        self.last_vote = Some(Instant::now());
    }

    pub fn last_vote_elapsed(&self) -> Duration {
        match &self.last_vote {
            Some(i) => i.elapsed(),
            None => Duration::from_secs(60 * 60 * 24 * 365), // Duration::MAX caused problems with C++
        }
    }
}

#[derive(Clone)]
pub struct VoteInfo {
    pub time: SystemTime, // TODO use Instant
    pub timestamp: u64,
    pub hash: BlockHash,
}

impl VoteInfo {
    pub fn new(timestamp: u64, hash: BlockHash) -> Self {
        Self {
            time: SystemTime::now(),
            timestamp,
            hash,
        }
    }
}

impl Default for VoteInfo {
    fn default() -> Self {
        Self::new(0, BlockHash::zero())
    }
}

#[derive(FromPrimitive, Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ElectionState {
    Passive,   // only listening for incoming votes
    Active,    // actively request confirmations
    Confirmed, // confirmed but still listening for votes
    ExpiredConfirmed,
    ExpiredUnconfirmed,
}

#[derive(FromPrimitive, Copy, Clone, Debug)]
#[repr(u8)]
pub enum ElectionBehavior {
    Normal,
    /**
     * Hinted elections:
     * - shorter timespan
     * - limited space inside AEC
     */
    Hinted,
    /**
     * Optimistic elections:
     * - shorter timespan
     * - limited space inside AEC
     * - more frequent confirmation requests
     */
    Optimistic,
}

impl From<ElectionBehavior> for DetailType {
    fn from(value: ElectionBehavior) -> Self {
        match value {
            ElectionBehavior::Normal => DetailType::Normal,
            ElectionBehavior::Hinted => DetailType::Hinted,
            ElectionBehavior::Optimistic => DetailType::Optimistic,
        }
    }
}