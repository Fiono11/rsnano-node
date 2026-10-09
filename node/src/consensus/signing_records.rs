use std::sync::Arc;

use rsnano_messages::Report;
use rsnano_nullable_lmdb::LmdbEnvironment;
use rsnano_store_lmdb::LmdbSigningStore;
use rsnano_types::{Account, Amount, BlockHash, ConsensusEpoch, PublicKey, Signature, Vote};

use crate::consensus::election::{
    AccountFrontier, AccountSlot, CertifiedState, EpochLedger, EpochSlot, Item, LocalSlotState,
    ResidualVotes,
};

/// RAI: one slot's signing record: what this node voted in the slot and
/// epoch, with the parent of every block it voted for
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRecord {
    pub slot: EpochSlot,
    pub state: LocalSlotState,
    pub parents: Vec<(BlockHash, BlockHash)>,
}

/// RAI: what this node voted in one round of an epoch's close election.
/// The close's votes are signed like account votes: a restart must not
/// vote anything else in a round it voted in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseRecord {
    pub epoch: ConsensusEpoch,
    pub round: u32,
    pub state: LocalSlotState,
}

/// RAI: a frozen report as persisted: the signed headers of this node's
/// representatives and the two frozen sets
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportRecord {
    pub epoch: ConsensusEpoch,
    pub signed: Vec<Report>,
    pub certified: CertifiedState,
    pub residual: ResidualVotes,
}

/// RAI, overlap certificates as retained evidence: the signed votes for
/// one block in one epoch that this node relied on for overlap finality
/// and an early final vote - the closing-epoch exclusion witness, the
/// current-epoch NC, a witness discharging a record the block bypasses -
/// with the block's position, which decides how long the record is kept
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceRecord {
    pub epoch: ConsensusEpoch,
    pub hash: BlockHash,
    pub slot: AccountSlot,
    pub votes: Vec<Arc<Vote>>,
}

/// RAI: how the epochs started on this node, which a restart replays: the
/// boundaries' origin and the genesis state and committee
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochsRecord {
    /// T0, the origin of the epoch boundaries, in unix milliseconds: the
    /// node's clock starts again from zero on a restart
    pub origin_unix_ms: u64,
    /// The setup's frontiers: the genesis committee and `S_{-1}`
    pub frontiers: Vec<AccountFrontier>,
    /// The setup's confirmed history, also in `S_{-1}`
    pub history: Vec<(AccountSlot, BlockHash, BlockHash)>,
}

/// RAI: an epoch decided here. Its decided state is kept for the latest
/// two epochs only; the frontiers it counted towards the committees are
/// kept for every epoch, since the weights are cumulative and a restart
/// derives the committees again from the genesis on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecidedRecord {
    pub epoch: ConsensusEpoch,
    /// The close round and value its certificate finalized
    pub closed: Option<(u32, BlockHash)>,
    /// `d_e`
    pub state_hash: BlockHash,
    /// `S_e`; on load None once dropped or if it does not hash to `d_e`
    pub state: Option<Arc<EpochLedger>>,
    pub frontiers: Vec<AccountFrontier>,
}

/// What a restarted node recovers
#[derive(Default)]
pub struct Recovered {
    pub slots: Vec<SlotRecord>,
    pub frozen: Vec<ConsensusEpoch>,
    pub reports: Vec<ReportRecord>,
    pub evidence: Vec<EvidenceRecord>,
    pub epochs: Option<EpochsRecord>,
    /// In epoch order
    pub decided: Vec<DecidedRecord>,
    pub closes: Vec<CloseRecord>,
}

/// RAI, "Participants, faults, and retained evidence": "Correct validators
/// persist their first vote and terminal state before releasing
/// signatures." This is the infrastructure side: the records go to an
/// LMDB environment of their own, one write transaction per batch of votes
/// (group commit) and one per frozen report, before the signatures are
/// broadcast; a restart reloads them into the election container and the
/// report exchange, which then refuse whatever the records forbid. The
/// environment is not the ledger's: LMDB has one writer per environment,
/// and the voter must not wait for block processing or cementing to
/// release it.
pub struct SigningRecords {
    env: LmdbEnvironment,
    store: LmdbSigningStore,
    /// RAI: the bulk records a restart can do without or fetch again - the
    /// decided states and the retained evidence - in an environment of
    /// their own, not synced: LMDB has one writer per environment, and the
    /// voter must not wait behind megabytes of them to record a vote
    bulk_env: LmdbEnvironment,
    bulk_store: LmdbSigningStore,
    drive_flush: DriveFlush,
    /// How long each kind of write took, waiting for the environment's one
    /// writer included, since the timings were last taken
    timings: std::sync::Mutex<std::collections::BTreeMap<&'static str, WriteTiming>>,
}

/// RAI: the writes of one kind since the timings were last taken
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteTiming {
    pub count: u64,
    pub total: std::time::Duration,
    pub max: std::time::Duration,
}

/// RAI: flushes the signing records' file through the drive's write cache
/// after a signing write. LMDB's own sync is an fsync, which on macOS hands
/// the data to the drive without waiting for its stable media; F_FULLFSYNC
/// waits. On other systems an fsync already does, and this is a second one.
pub struct DriveFlush {
    file: Option<std::fs::File>,
}

impl DriveFlush {
    /// Flushes the file at `path` (opened read-only: a flush is per file,
    /// whichever descriptor asks for it)
    pub fn new(path: &std::path::Path) -> std::io::Result<Self> {
        Ok(Self {
            file: Some(std::fs::File::open(path)?),
        })
    }

    /// Flushes nothing
    pub fn new_null() -> Self {
        Self { file: None }
    }

    fn flush(&self) {
        let Some(file) = &self.file else {
            return;
        };
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: fcntl on a descriptor this struct owns
            unsafe {
                libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = file.sync_data();
        }
    }
}

const SLOT: u8 = b'S';
const PARENT: u8 = b'P';
const FROZEN: u8 = b'F';
const REPORT: u8 = b'R';
const CERTIFIED: u8 = b'T';
const RESIDUAL: u8 = b'G';
const EVIDENCE: u8 = b'E';
const EVIDENCE_VOTE: u8 = b'V';
const EPOCHS: u8 = b'Z';
const DECIDED: u8 = b'W';
const DECIDED_STATE: u8 = b'D';
const CLOSE: u8 = b'C';
/// How many of the latest decided states are kept: the predecessor of the
/// oldest epoch still closing, and the one before it, which the overlap
/// gate checks against
const DECIDED_STATES_KEPT: u64 = 2;
/// Records of old epochs deleted per transaction
const FORGET_CHUNK: usize = 2000;

impl SigningRecords {
    pub fn new(env: LmdbEnvironment, bulk_env: LmdbEnvironment) -> anyhow::Result<Self> {
        let store = LmdbSigningStore::new(&env)?;
        let bulk_store = LmdbSigningStore::new(&bulk_env)?;
        Ok(Self {
            env,
            store,
            bulk_env,
            bulk_store,
            drive_flush: DriveFlush::new_null(),
            timings: Default::default(),
        })
    }

    /// How long each kind of write took since the last call
    pub fn take_timings(&self) -> Vec<(&'static str, WriteTiming)> {
        std::mem::take(&mut *self.timings.lock().unwrap())
            .into_iter()
            .collect()
    }

    fn timed<R>(&self, kind: &'static str, write: impl FnOnce() -> R) -> R {
        let started = std::time::Instant::now();
        let result = write();
        let took = started.elapsed();
        let mut timings = self.timings.lock().unwrap();
        let timing = timings.entry(kind).or_default();
        timing.count += 1;
        timing.total += took;
        timing.max = timing.max.max(took);
        result
    }

    /// The signing writes are also flushed through the drive's write cache
    pub fn with_drive_flush(mut self, drive_flush: DriveFlush) -> Self {
        self.drive_flush = drive_flush;
        self
    }

    pub fn new_null() -> Self {
        Self::new(LmdbEnvironment::new_null(), LmdbEnvironment::new_null())
            .expect("a nulled environment opens")
    }

    /// Persists the slot states of a batch of votes about to be released
    pub fn write_slots(&self, records: &[SlotRecord]) {
        self.write_signing(records, &[]);
    }

    /// Persists a batch of votes about to be released, account and close
    /// votes alike, in one transaction: the group commit of the batch
    pub fn write_signing(&self, records: &[SlotRecord], closes: &[CloseRecord]) {
        self.timed("signing", || {
            if records.is_empty() && closes.is_empty() {
                return;
            }
            let store = &self.store;
            let mut txn = self.env.begin_write();
            for close in closes {
                store.put(
                    &mut txn,
                    &close_key(close.epoch, close.round),
                    &encode_slot(&close.state),
                );
            }
            for record in records {
                store.put(
                    &mut txn,
                    &slot_key(&record.slot),
                    &encode_slot(&record.state),
                );
                for (hash, parent) in &record.parents {
                    store.put(&mut txn, &parent_key(hash), parent.as_bytes());
                }
            }
            txn.commit();
            self.drive_flush.flush();
        })
    }

    /// Persists that this node left an epoch: it signs nothing new in it
    pub fn write_frozen(&self, epoch: ConsensusEpoch) {
        self.timed("frozen", || {
            let mut txn = self.env.begin_write();
            self.store.put(&mut txn, &epoch_key(FROZEN, epoch), &[]);
            txn.commit();
            self.drive_flush.flush();
        })
    }

    /// Persists a frozen report before its headers are broadcast
    pub fn write_report(&self, record: &ReportRecord) {
        self.timed("report", || {
            let store = &self.store;
            let mut txn = self.env.begin_write();
            store.put(
                &mut txn,
                &epoch_key(REPORT, record.epoch),
                &encode_headers(&record.signed),
            );
            store.put(
                &mut txn,
                &epoch_key(CERTIFIED, record.epoch),
                &encode_items(record.certified.items()),
            );
            store.put(
                &mut txn,
                &epoch_key(RESIDUAL, record.epoch),
                &encode_items(record.residual.items()),
            );
            txn.commit();
            self.drive_flush.flush();
        })
    }

    /// Persists retained evidence, in one batch: a record replaces the one
    /// held for its block and epoch, as it holds every vote the earlier one
    /// did
    ///
    /// A vote is stored once, by epoch and signature: a vote batch covers up
    /// to 255 blocks, and the records of those blocks name it rather than
    /// hold a copy each. The signature names it without hashing its blocks.
    pub fn write_evidence(&self, records: &[EvidenceRecord]) {
        self.timed("evidence", || {
            if records.is_empty() {
                return;
            }
            let mut txn = self.bulk_env.begin_write();
            let mut written = std::collections::HashSet::new();
            for record in records {
                for vote in &record.votes {
                    let key = evidence_vote_key(record.epoch, vote);
                    if written.insert(key.clone()) && self.bulk_store.get(&txn, &key).is_none() {
                        let mut bytes = Vec::new();
                        vote.serialize(&mut bytes)
                            .expect("a vote serializes to memory");
                        self.bulk_store.put(&mut txn, &key, &bytes);
                    }
                }
                self.bulk_store.put(
                    &mut txn,
                    &evidence_key(record.epoch, &record.hash),
                    &encode_evidence(record),
                );
            }
            txn.commit();
        })
    }

    /// Drops the evidence of epochs before the given one, except where
    /// `keep` says it may still discharge a lock record of its epoch at its
    /// block's position
    pub fn forget_evidence_before(
        &self,
        epoch: ConsensusEpoch,
        keep: impl Fn(ConsensusEpoch, &AccountSlot) -> bool,
    ) {
        self.timed("forget_evidence", || {
            let mut txn = self.bulk_env.begin_write();
            let mut old = Vec::new();
            let mut referenced = std::collections::HashSet::new();
            for (key, value) in self.bulk_store.with_prefix(&txn, &[EVIDENCE]) {
                let kept = decode_evidence_key(&key).and_then(|(held, _)| {
                    let (slot, votes) = decode_evidence(&value)?;
                    (held >= epoch || keep(held, &slot)).then_some((held, votes))
                });
                match kept {
                    Some((held, votes)) => referenced.extend(
                        votes
                            .iter()
                            .map(|signature| evidence_vote_key_of(held, signature)),
                    ),
                    None => old.push(key),
                }
            }
            // A stored vote no remaining record names goes too
            old.extend(
                self.bulk_store
                    .with_prefix(&txn, &[EVIDENCE_VOTE])
                    .into_iter()
                    .map(|(key, _)| key)
                    .filter(|key| !referenced.contains(key)),
            );
            for key in old {
                self.bulk_store.delete(&mut txn, &key);
            }
            txn.commit();
        })
    }

    /// Syncs the bulk records to disk whatever the environment's flags: a
    /// decided state written there is about to be acknowledged as held
    pub fn sync_bulk(&self) {
        self.timed("sync_bulk", || {
            let _ = self.bulk_env.sync();
        })
    }

    /// Persists how the epochs started
    pub fn write_epochs(&self, record: &EpochsRecord) {
        self.timed("epochs", || {
            let mut txn = self.env.begin_write();
            self.store.put(&mut txn, &[EPOCHS], &encode_epochs(record));
            txn.commit();
        })
    }

    /// Persists decided epochs and drops all but the latest decided states
    pub fn write_decided(&self, records: &[DecidedRecord]) {
        self.timed("decided", || {
            let Some(latest) = records.iter().map(|record| record.epoch).max() else {
                return;
            };
            let mut txn = self.env.begin_write();
            for record in records {
                self.store.put(
                    &mut txn,
                    &epoch_key(DECIDED, record.epoch),
                    &encode_decided(record),
                );
            }
            txn.commit();
            let store = &self.bulk_store;
            let mut txn = self.bulk_env.begin_write();
            for record in records {
                if let Some(state) = &record.state {
                    store.put(
                        &mut txn,
                        &epoch_key(DECIDED_STATE, record.epoch),
                        &state.to_bytes(),
                    );
                }
            }
            let old: Vec<Vec<u8>> = store
                .with_prefix(&txn, &[DECIDED_STATE])
                .into_iter()
                .map(|(key, _)| key)
                .filter(|key| {
                    decode_epoch_key(key)
                        .is_some_and(|held| held.as_u64() + DECIDED_STATES_KEPT <= latest.as_u64())
                })
                .collect();
            for key in old {
                store.delete(&mut txn, &key);
            }
            txn.commit();
        })
    }

    /// Drops the records of epochs before the given one
    pub fn forget_before(&self, epoch: ConsensusEpoch) {
        self.timed("forget", || {
            let store = &self.store;
            let txn = self.env.begin_read();
            let old: Vec<Vec<u8>> = store
                .with_prefix(&txn, &[SLOT])
                .into_iter()
                .map(|(key, _)| key)
                .filter(|key| decode_slot_key(key).is_some_and(|slot| slot.epoch < epoch))
                .chain(
                    [FROZEN, REPORT, CERTIFIED, RESIDUAL]
                        .into_iter()
                        .flat_map(|prefix| store.with_prefix(&txn, &[prefix]))
                        .map(|(key, _)| key)
                        .filter(|key| decode_epoch_key(key).is_some_and(|held| held < epoch)),
                )
                .chain(
                    store
                        .with_prefix(&txn, &[CLOSE])
                        .into_iter()
                        .map(|(key, _)| key)
                        .filter(|key| decode_close_key(key).is_some_and(|(held, _)| held < epoch)),
                )
                .collect();
            drop(txn);
            // In short transactions: the voter waits for the one writer
            for chunk in old.chunks(FORGET_CHUNK) {
                let mut txn = self.env.begin_write();
                for key in chunk {
                    store.delete(&mut txn, key);
                }
                txn.commit();
            }
        })
    }

    /// Everything persisted, for a restart
    pub fn load(&self) -> Recovered {
        let store = &self.store;
        let txn = self.env.begin_read();
        let mut recovered = Recovered::default();
        let parents: Vec<(BlockHash, BlockHash)> = store
            .with_prefix(&txn, &[PARENT])
            .into_iter()
            .filter_map(|(key, value)| {
                Some((
                    BlockHash::from_slice(&key[1..])?,
                    BlockHash::from_slice(&value)?,
                ))
            })
            .collect();
        for (key, value) in store.with_prefix(&txn, &[SLOT]) {
            let Some(slot) = decode_slot_key(&key) else {
                continue;
            };
            let Some(state) = decode_slot(&value) else {
                continue;
            };
            let voted = state.voted();
            let parents = parents
                .iter()
                .filter(|(hash, _)| voted.contains(hash))
                .copied()
                .collect();
            recovered.slots.push(SlotRecord {
                slot,
                state,
                parents,
            });
        }
        for (key, _) in store.with_prefix(&txn, &[FROZEN]) {
            if let Some(epoch) = decode_epoch_key(&key) {
                recovered.frozen.push(epoch);
            }
        }
        for (key, value) in store.with_prefix(&txn, &[REPORT]) {
            let Some(epoch) = decode_epoch_key(&key) else {
                continue;
            };
            let Some(signed) = decode_headers(&value) else {
                continue;
            };
            let certified = store
                .get(&txn, &epoch_key(CERTIFIED, epoch))
                .and_then(|bytes| decode_items(&bytes))
                .map(|items| {
                    let mut state = CertifiedState::new();
                    for item in items {
                        if let Some((block, entry)) = CertifiedState::entry_of(&item) {
                            state.set(block, entry);
                        }
                    }
                    state
                })
                .unwrap_or_default();
            let residual = store
                .get(&txn, &epoch_key(RESIDUAL, epoch))
                .and_then(|bytes| decode_items(&bytes))
                .map(|items| {
                    let mut votes = ResidualVotes::new();
                    for item in items {
                        if let Some((block, kind, previous)) = ResidualVotes::record_of(&item) {
                            votes.record(block, previous, kind);
                        }
                    }
                    votes
                })
                .unwrap_or_default();
            recovered.reports.push(ReportRecord {
                epoch,
                signed,
                certified,
                residual,
            });
        }
        let mut votes: std::collections::HashMap<Vec<u8>, Arc<Vote>> =
            std::collections::HashMap::new();
        let bulk = &self.bulk_store;
        let bulk_txn = self.bulk_env.begin_read();
        for (key, value) in bulk.with_prefix(&bulk_txn, &[EVIDENCE_VOTE]) {
            if let Ok(vote) = Vote::deserialize(&value) {
                votes.insert(key, Arc::new(vote));
            }
        }
        for (key, value) in bulk.with_prefix(&bulk_txn, &[EVIDENCE]) {
            let Some((epoch, hash)) = decode_evidence_key(&key) else {
                continue;
            };
            let Some((slot, named)) = decode_evidence(&value) else {
                continue;
            };
            recovered.evidence.push(EvidenceRecord {
                epoch,
                hash,
                slot,
                votes: named
                    .iter()
                    .filter_map(|signature| {
                        votes.get(&evidence_vote_key_of(epoch, signature)).cloned()
                    })
                    .collect(),
            });
        }
        for (key, value) in store.with_prefix(&txn, &[CLOSE]) {
            let (Some((epoch, round)), Some(state)) = (decode_close_key(&key), decode_slot(&value))
            else {
                continue;
            };
            recovered.closes.push(CloseRecord {
                epoch,
                round,
                state,
            });
        }
        recovered.epochs = store
            .get(&txn, &[EPOCHS])
            .and_then(|bytes| decode_epochs(&bytes));
        for (key, value) in store.with_prefix(&txn, &[DECIDED]) {
            let Some(epoch) = decode_epoch_key(&key) else {
                continue;
            };
            let Some(mut record) = decode_decided(epoch, &value) else {
                continue;
            };
            record.state = bulk
                .get(&bulk_txn, &epoch_key(DECIDED_STATE, epoch))
                .and_then(|bytes| EpochLedger::from_bytes(&bytes))
                .filter(|state| state.state_hash() == record.state_hash)
                .map(Arc::new);
            recovered.decided.push(record);
        }
        recovered
    }
}

/* Encoding */

const FRONTIER_SIZE: usize = 32 + 8 + 32 + 32 + 16;
const HISTORY_SIZE: usize = 32 + 8 + 32 + 32;

fn encode_frontiers(bytes: &mut Vec<u8>, frontiers: &[AccountFrontier]) {
    bytes.extend_from_slice(&(frontiers.len() as u32).to_be_bytes());
    for frontier in frontiers {
        bytes.extend_from_slice(frontier.account.as_bytes());
        bytes.extend_from_slice(&frontier.height.to_be_bytes());
        bytes.extend_from_slice(frontier.hash.as_bytes());
        bytes.extend_from_slice(frontier.representative.as_bytes());
        bytes.extend_from_slice(&frontier.balance.to_be_bytes());
    }
}

/// The frontiers at the start of `bytes`, and what follows them
fn decode_frontiers(bytes: &[u8]) -> Option<(Vec<AccountFrontier>, &[u8])> {
    let count = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    let end = 4 + count.checked_mul(FRONTIER_SIZE)?;
    let frontiers = bytes
        .get(4..end)?
        .chunks(FRONTIER_SIZE)
        .map(|chunk| {
            Some(AccountFrontier {
                account: Account::from_slice(&chunk[..32])?,
                height: u64::from_be_bytes(chunk[32..40].try_into().ok()?),
                hash: BlockHash::from_slice(&chunk[40..72])?,
                representative: PublicKey::from_slice(&chunk[72..104])?,
                balance: Amount::from_be_bytes(chunk[104..120].try_into().ok()?),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((frontiers, &bytes[end..]))
}

/// T0, the frontiers, then the history
fn encode_epochs(record: &EpochsRecord) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(
        8 + 8 + FRONTIER_SIZE * record.frontiers.len() + HISTORY_SIZE * record.history.len(),
    );
    bytes.extend_from_slice(&record.origin_unix_ms.to_be_bytes());
    encode_frontiers(&mut bytes, &record.frontiers);
    bytes.extend_from_slice(&(record.history.len() as u32).to_be_bytes());
    for (slot, hash, previous) in &record.history {
        bytes.extend_from_slice(slot.account.as_bytes());
        bytes.extend_from_slice(&slot.height.to_be_bytes());
        bytes.extend_from_slice(hash.as_bytes());
        bytes.extend_from_slice(previous.as_bytes());
    }
    bytes
}

fn decode_epochs(bytes: &[u8]) -> Option<EpochsRecord> {
    let origin_unix_ms = u64::from_be_bytes(bytes.get(..8)?.try_into().ok()?);
    let (frontiers, rest) = decode_frontiers(&bytes[8..])?;
    let count = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    let history_bytes = rest.get(4..)?;
    if history_bytes.len() != count.checked_mul(HISTORY_SIZE)? {
        return None;
    }
    let history = history_bytes
        .chunks(HISTORY_SIZE)
        .map(|chunk| {
            Some((
                AccountSlot::new(
                    Account::from_slice(&chunk[..32])?,
                    u64::from_be_bytes(chunk[32..40].try_into().ok()?),
                ),
                BlockHash::from_slice(&chunk[40..72])?,
                BlockHash::from_slice(&chunk[72..104])?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(EpochsRecord {
        origin_unix_ms,
        frontiers,
        history,
    })
}

/// A flag and the closed round and value, `d_e`, then the frontiers
fn encode_decided(record: &DecidedRecord) -> Vec<u8> {
    let mut bytes =
        Vec::with_capacity(1 + 4 + 32 + 32 + 4 + FRONTIER_SIZE * record.frontiers.len());
    let (round, value) = record.closed.unwrap_or((0, BlockHash::ZERO));
    bytes.push(record.closed.is_some() as u8);
    bytes.extend_from_slice(&round.to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    bytes.extend_from_slice(record.state_hash.as_bytes());
    encode_frontiers(&mut bytes, &record.frontiers);
    bytes
}

fn decode_decided(epoch: ConsensusEpoch, bytes: &[u8]) -> Option<DecidedRecord> {
    let closed = match *bytes.first()? {
        0 => None,
        1 => Some((
            u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?),
            BlockHash::from_slice(bytes.get(5..37)?)?,
        )),
        _ => return None,
    };
    let state_hash = BlockHash::from_slice(bytes.get(37..69)?)?;
    let (frontiers, rest) = decode_frontiers(bytes.get(69..)?)?;
    if !rest.is_empty() {
        return None;
    }
    Some(DecidedRecord {
        epoch,
        closed,
        state_hash,
        state: None,
        frontiers,
    })
}

fn slot_key(slot: &EpochSlot) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + 32 + 8 + 8);
    key.push(SLOT);
    key.extend_from_slice(slot.account.as_bytes());
    key.extend_from_slice(&slot.height.to_be_bytes());
    key.extend_from_slice(&slot.epoch.as_u64().to_be_bytes());
    key
}

fn decode_slot_key(key: &[u8]) -> Option<EpochSlot> {
    if key.len() != 1 + 32 + 8 + 8 || key[0] != SLOT {
        return None;
    }
    Some(EpochSlot {
        account: Account::from_slice(&key[1..33])?,
        height: u64::from_be_bytes(key[33..41].try_into().ok()?),
        epoch: ConsensusEpoch::new(u64::from_be_bytes(key[41..49].try_into().ok()?)),
    })
}

fn evidence_key(epoch: ConsensusEpoch, hash: &BlockHash) -> Vec<u8> {
    let mut key = Vec::with_capacity(41);
    key.push(EVIDENCE);
    key.extend_from_slice(&epoch.as_u64().to_be_bytes());
    key.extend_from_slice(hash.as_bytes());
    key
}

fn decode_evidence_key(key: &[u8]) -> Option<(ConsensusEpoch, BlockHash)> {
    if key.len() != 41 {
        return None;
    }
    Some((
        ConsensusEpoch::new(u64::from_be_bytes(key[1..9].try_into().ok()?)),
        BlockHash::from_slice(&key[9..])?,
    ))
}

/// A stored vote: epoch, then its signature
fn evidence_vote_key(epoch: ConsensusEpoch, vote: &Vote) -> Vec<u8> {
    evidence_vote_key_of(epoch, vote.signature.as_bytes())
}

fn evidence_vote_key_of(epoch: ConsensusEpoch, signature: &[u8; 64]) -> Vec<u8> {
    let mut key = Vec::with_capacity(73);
    key.push(EVIDENCE_VOTE);
    key.extend_from_slice(&epoch.as_u64().to_be_bytes());
    key.extend_from_slice(signature);
    key
}

/// The position, then the signature of each vote
fn encode_evidence(record: &EvidenceRecord) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(40 + 64 * record.votes.len());
    bytes.extend_from_slice(record.slot.account.as_bytes());
    bytes.extend_from_slice(&record.slot.height.to_be_bytes());
    for vote in &record.votes {
        bytes.extend_from_slice(vote.signature.as_bytes());
    }
    bytes
}

fn decode_evidence(bytes: &[u8]) -> Option<(AccountSlot, Vec<[u8; 64]>)> {
    let account = Account::from_slice(bytes.get(..32)?)?;
    let height = u64::from_be_bytes(bytes.get(32..40)?.try_into().ok()?);
    let named = bytes
        .get(40..)?
        .chunks(64)
        .map(|chunk| chunk.try_into().ok())
        .collect::<Option<Vec<[u8; 64]>>>()?;
    Some((AccountSlot::new(account, height), named))
}

/// A close round's record: epoch, then round
fn close_key(epoch: ConsensusEpoch, round: u32) -> Vec<u8> {
    let mut key = Vec::with_capacity(13);
    key.push(CLOSE);
    key.extend_from_slice(&epoch.as_u64().to_be_bytes());
    key.extend_from_slice(&round.to_be_bytes());
    key
}

fn decode_close_key(key: &[u8]) -> Option<(ConsensusEpoch, u32)> {
    if key.len() != 13 || key[0] != CLOSE {
        return None;
    }
    Some((
        ConsensusEpoch::new(u64::from_be_bytes(key[1..9].try_into().ok()?)),
        u32::from_be_bytes(key[9..13].try_into().ok()?),
    ))
}

fn parent_key(hash: &BlockHash) -> Vec<u8> {
    let mut key = Vec::with_capacity(33);
    key.push(PARENT);
    key.extend_from_slice(hash.as_bytes());
    key
}

fn epoch_key(prefix: u8, epoch: ConsensusEpoch) -> Vec<u8> {
    let mut key = Vec::with_capacity(9);
    key.push(prefix);
    key.extend_from_slice(&epoch.as_u64().to_be_bytes());
    key
}

fn decode_epoch_key(key: &[u8]) -> Option<ConsensusEpoch> {
    if key.len() != 9 {
        return None;
    }
    Some(ConsensusEpoch::new(u64::from_be_bytes(
        key[1..9].try_into().ok()?,
    )))
}

/// The flags byte: bit 0 stale, bit 1 an early first vote
fn encode_slot(state: &LocalSlotState) -> Vec<u8> {
    let mut bytes = vec![state.stale as u8 | (state.first_early as u8) << 1];
    for hash in [state.first_voted, state.timeout_voted, state.final_voted] {
        match hash {
            Some(hash) => {
                bytes.push(1);
                bytes.extend_from_slice(hash.as_bytes());
            }
            None => bytes.push(0),
        }
    }
    bytes.push(state.notar_voted.len().min(255) as u8);
    for hash in state.notar_voted.iter().take(255) {
        bytes.extend_from_slice(hash.as_bytes());
    }
    bytes
}

fn decode_slot(bytes: &[u8]) -> Option<LocalSlotState> {
    let mut at = 0;
    let take = |at: &mut usize, n: usize| -> Option<&[u8]> {
        let slice = bytes.get(*at..*at + n)?;
        *at += n;
        Some(slice)
    };
    let flags = take(&mut at, 1)?[0];
    let stale = flags & 1 != 0;
    let first_early = flags & 2 != 0;
    let mut optional = |at: &mut usize| -> Option<Option<BlockHash>> {
        match take(at, 1)?[0] {
            0 => Some(None),
            _ => Some(Some(BlockHash::from_slice(take(at, 32)?)?)),
        }
    };
    let first_voted = optional(&mut at)?;
    let timeout_voted = optional(&mut at)?;
    let final_voted = optional(&mut at)?;
    let count = take(&mut at, 1)?[0] as usize;
    let mut notar_voted = Vec::with_capacity(count);
    for _ in 0..count {
        notar_voted.push(BlockHash::from_slice(take(&mut at, 32)?)?);
    }
    Some(LocalSlotState {
        first_voted,
        notar_voted,
        timeout_voted,
        final_voted,
        stale,
        first_early,
    })
}

/// committee, predecessor, certified root, residual root, then one
/// (reporter, signature) per header
fn encode_headers(signed: &[Report]) -> Vec<u8> {
    let Some(first) = signed.first() else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(first.committee.as_bytes());
    bytes.extend_from_slice(first.predecessor.as_bytes());
    bytes.extend_from_slice(first.certified.as_bytes());
    bytes.extend_from_slice(first.residual.as_bytes());
    bytes.extend_from_slice(&first.epoch.as_u64().to_le_bytes());
    for report in signed {
        bytes.extend_from_slice(report.reporter.as_bytes());
        bytes.extend_from_slice(report.signature.as_bytes());
    }
    bytes
}

fn decode_headers(bytes: &[u8]) -> Option<Vec<Report>> {
    if bytes.len() < 32 * 4 + 8 {
        return None;
    }
    let committee = BlockHash::from_slice(&bytes[0..32])?;
    let predecessor = BlockHash::from_slice(&bytes[32..64])?;
    let certified = BlockHash::from_slice(&bytes[64..96])?;
    let residual = BlockHash::from_slice(&bytes[96..128])?;
    let epoch = ConsensusEpoch::new(u64::from_le_bytes(bytes[128..136].try_into().ok()?));
    let mut signed = Vec::new();
    let mut rest = &bytes[136..];
    while rest.len() >= 32 + 64 {
        let reporter = PublicKey::from_slice(&rest[0..32])?;
        let signature = Signature::from_bytes(rest[32..96].try_into().ok()?);
        signed.push(Report {
            epoch,
            committee,
            predecessor,
            certified,
            residual,
            reporter,
            signature,
        });
        rest = &rest[96..];
    }
    Some(signed)
}

fn encode_items(items: impl Iterator<Item = Item>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for item in items {
        bytes.extend_from_slice(&item);
    }
    bytes
}

fn decode_items(bytes: &[u8]) -> Option<Vec<Item>> {
    let size = std::mem::size_of::<Item>();
    if bytes.len() % size != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(size)
            .map(|chunk| {
                let mut item: Item = [0; std::mem::size_of::<Item>()];
                item.copy_from_slice(chunk);
                item
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::{CertifiedBlock, CertifiedStatus, ResidualKind};
    use rsnano_types::{PrivateKey, VoteKind};

    /// RAI: the records survive closing the environment and opening it
    /// again from disk, as a restart does, with every sync of the default
    /// and the strictest mode on the way
    #[test]
    fn records_survive_reopening_the_environment_on_disk() {
        let dir = std::env::temp_dir().join(format!("rsnano-signing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("signing.ldb");
        let env = |path: std::path::PathBuf| {
            let options = rsnano_nullable_lmdb::EnvironmentOptions {
                path,
                max_dbs: 1,
                map_size: 64 * 1024 * 1024,
                flags: rsnano_store_lmdb::get_lmdb_flags(&rsnano_store_lmdb::LmdbConfig {
                    sync: rsnano_store_lmdb::SyncStrategy::Always,
                    ..Default::default()
                }),
            };
            rsnano_nullable_lmdb::LmdbEnvironmentFactory::default()
                .create(options)
                .unwrap()
        };
        let open = || {
            let records =
                SigningRecords::new(env(path.clone()), env(dir.join("epoch_records.ldb"))).unwrap();
            records.with_drive_flush(DriveFlush::new(&path).unwrap())
        };
        let mut state = LocalSlotState::default();
        state.mark_voted(BlockHash::from(7), VoteKind::First);
        let slot = SlotRecord {
            slot: EpochSlot {
                account: Account::from(1),
                height: 2,
                epoch: ConsensusEpoch::new(3),
            },
            state: state.clone(),
            parents: vec![(BlockHash::from(7), BlockHash::from(6))],
        };
        let close = CloseRecord {
            epoch: ConsensusEpoch::new(2),
            round: 1,
            state,
        };
        let epochs = EpochsRecord {
            origin_unix_ms: 42,
            frontiers: vec![frontier(1, 1)],
            history: Vec::new(),
        };

        {
            let records = open();
            records.write_signing(&[slot.clone()], &[close.clone()]);
            records.write_frozen(ConsensusEpoch::new(2));
            records.write_epochs(&epochs);
        }
        let recovered = open().load();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(recovered.slots, vec![slot]);
        assert_eq!(recovered.closes, vec![close]);
        assert_eq!(recovered.frozen, vec![ConsensusEpoch::new(2)]);
        assert_eq!(recovered.epochs, Some(epochs));
    }

    #[test]
    fn close_records_survive_a_reload_and_are_forgotten_with_their_epoch() {
        let records = SigningRecords::new_null();
        let mut state = LocalSlotState::default();
        state.mark_voted(BlockHash::from(7), VoteKind::First);
        state.mark_voted(BlockHash::from(7), VoteKind::Final);
        let close = |epoch: u64, round: u32| CloseRecord {
            epoch: ConsensusEpoch::new(epoch),
            round,
            state: state.clone(),
        };

        records.write_signing(&[], &[close(1, 0), close(2, 3)]);
        assert_eq!(records.load().closes, vec![close(1, 0), close(2, 3)]);

        records.forget_before(ConsensusEpoch::new(2));
        assert_eq!(records.load().closes, vec![close(2, 3)]);
    }

    #[test]
    fn writes_are_timed_by_kind() {
        let records = SigningRecords::new_null();
        records.write_frozen(ConsensusEpoch::new(1));
        records.write_frozen(ConsensusEpoch::new(2));

        let timings = records.take_timings();
        assert_eq!(timings.len(), 1);
        assert_eq!(timings[0].0, "frozen");
        assert_eq!(timings[0].1.count, 2);
        assert!(records.take_timings().is_empty());
    }

    #[test]
    fn the_epochs_record_survives_a_reload() {
        let records = SigningRecords::new_null();
        let record = EpochsRecord {
            origin_unix_ms: 1_791_550_000_000,
            frontiers: vec![frontier(1, 3), frontier(2, 1)],
            history: vec![(
                AccountSlot::new(Account::from(1), 2),
                BlockHash::from(12),
                BlockHash::from(11),
            )],
        };

        records.write_epochs(&record);

        assert_eq!(records.load().epochs, Some(record));
    }

    #[test]
    fn decided_epochs_keep_their_frontiers_and_the_latest_two_states() {
        let records = SigningRecords::new_null();
        let decided = |epoch: u64| {
            let mut state = EpochLedger::new();
            state.finalize_genesis(
                AccountSlot::new(Account::from(epoch), 1),
                BlockHash::from(epoch),
            );
            DecidedRecord {
                epoch: ConsensusEpoch::new(epoch),
                closed: Some((epoch as u32, BlockHash::from(100 + epoch))),
                state_hash: state.state_hash(),
                state: Some(Arc::new(state)),
                frontiers: vec![frontier(epoch, 2)],
            }
        };

        records.write_decided(&[decided(0)]);
        records.write_decided(&[decided(1), decided(2)]);

        let loaded = records.load().decided;
        assert_eq!(loaded.len(), 3);
        assert!(loaded[0].state.is_none());
        assert_eq!(loaded[0].frontiers, decided(0).frontiers);
        assert_eq!(loaded[0].closed, decided(0).closed);
        assert_eq!(loaded[1], decided(1));
        assert_eq!(loaded[2], decided(2));
    }

    #[test]
    fn slot_records_and_frozen_epochs_survive_a_reload() {
        let records = SigningRecords::new_null();
        let mut state = LocalSlotState::default();
        state.mark_voted(BlockHash::from(7), VoteKind::First);
        state.mark_voted(BlockHash::from(7), VoteKind::Final);
        // An early first vote stays early when re-signed after a restart
        state.first_early = true;
        let slot = EpochSlot {
            account: Account::from(1),
            height: 2,
            epoch: ConsensusEpoch::new(3),
        };
        let record = SlotRecord {
            slot,
            state,
            parents: vec![(BlockHash::from(7), BlockHash::from(6))],
        };
        records.write_slots(&[record.clone()]);
        records.write_frozen(ConsensusEpoch::new(3));

        let recovered = records.load();
        assert_eq!(recovered.slots, vec![record]);
        assert_eq!(recovered.frozen, vec![ConsensusEpoch::new(3)]);
        assert!(recovered.reports.is_empty());
    }

    /// Overlap-certificate evidence survives a reload, and old evidence is
    /// forgotten unless it may still discharge a record at its position.
    /// A vote's epoch is on the wire under `rai_protocol` only.
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn retained_evidence_survives_a_reload_while_it_may_discharge_a_record() {
        let records = SigningRecords::new_null();
        let hash = BlockHash::from(7);
        let vote = |key: u64, kind: VoteKind| {
            Arc::new(Vote::new_in_epoch(
                &PrivateKey::from(key),
                kind,
                ConsensusEpoch::new(1),
                vec![hash, BlockHash::from(8)],
            ))
        };
        let locked = AccountSlot::new(Account::from(1), 2);
        let shared = vote(1, VoteKind::First);
        let witness = EvidenceRecord {
            epoch: ConsensusEpoch::new(1),
            hash,
            slot: locked,
            votes: vec![shared.clone(), vote(2, VoteKind::Final)],
        };
        let other = EvidenceRecord {
            epoch: ConsensusEpoch::new(1),
            hash: BlockHash::from(9),
            slot: AccountSlot::new(Account::from(2), 2),
            // A batch vote shared with the other record is stored once
            votes: vec![shared.clone(), vote(3, VoteKind::First)],
        };
        records.write_evidence(&[witness.clone(), other.clone()]);
        let recovered = records.load();
        assert_eq!(recovered.evidence.len(), 2);
        assert!(recovered.evidence.contains(&witness));
        assert!(recovered.evidence.contains(&other));

        records.forget_evidence_before(ConsensusEpoch::new(5), |origin, slot| {
            origin == ConsensusEpoch::new(1) && *slot == locked
        });
        assert_eq!(records.load().evidence, vec![witness]);
        records.forget_evidence_before(ConsensusEpoch::new(5), |_, _| false);
        assert!(records.load().evidence.is_empty());
    }

    #[test]
    fn a_frozen_report_survives_a_reload() {
        let records = SigningRecords::new_null();
        let key = PrivateKey::from(1);
        let mut certified = CertifiedState::new();
        certified.certify(
            CertifiedBlock::new(Account::from(1), 1, BlockHash::from(2)),
            BlockHash::ZERO,
            CertifiedStatus::Notarized,
        );
        let mut residual = ResidualVotes::new();
        residual.record(
            CertifiedBlock::new(Account::from(1), 2, BlockHash::from(3)),
            BlockHash::from(2),
            ResidualKind::First,
        );
        let report = Report::new(
            &key,
            ConsensusEpoch::new(4),
            BlockHash::from(9),
            BlockHash::from(8),
            certified.root(),
            residual.root(),
            BlockHash::from(99),
        );
        let record = ReportRecord {
            epoch: ConsensusEpoch::new(4),
            signed: vec![report],
            certified,
            residual,
        };
        records.write_report(&record);

        let recovered = records.load();
        assert_eq!(recovered.reports, vec![record]);
    }

    #[test]
    fn records_of_old_epochs_are_forgotten() {
        let records = SigningRecords::new_null();
        for epoch in [1u64, 2, 3] {
            records.write_frozen(ConsensusEpoch::new(epoch));
            records.write_slots(&[SlotRecord {
                slot: EpochSlot {
                    account: Account::from(epoch),
                    height: 1,
                    epoch: ConsensusEpoch::new(epoch),
                },
                state: LocalSlotState::default(),
                parents: Vec::new(),
            }]);
        }
        records.forget_before(ConsensusEpoch::new(3));
        let recovered = records.load();
        assert_eq!(recovered.frozen, vec![ConsensusEpoch::new(3)]);
        assert_eq!(recovered.slots.len(), 1);
    }

    /*
     * Test helpers
     */

    fn frontier(account: u64, height: u64) -> AccountFrontier {
        AccountFrontier {
            account: Account::from(account),
            height,
            hash: BlockHash::from(account * 10 + height),
            representative: PublicKey::from(account + 50),
            balance: Amount::raw(1000 * account as u128),
        }
    }
}
