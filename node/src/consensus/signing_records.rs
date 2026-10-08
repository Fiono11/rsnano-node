use std::sync::Arc;

use rsnano_messages::Report;
use rsnano_nullable_lmdb::LmdbEnvironment;
use rsnano_store_lmdb::LmdbSigningStore;
use rsnano_types::{Account, BlockHash, ConsensusEpoch, PublicKey, Signature, Vote};

use crate::consensus::election::{
    AccountSlot, CertifiedState, EpochSlot, Item, LocalSlotState, ResidualVotes,
};

/// RAI: one slot's signing record: what this node voted in the slot and
/// epoch, with the parent of every block it voted for
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRecord {
    pub slot: EpochSlot,
    pub state: LocalSlotState,
    pub parents: Vec<(BlockHash, BlockHash)>,
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

/// What a restarted node recovers
#[derive(Default)]
pub struct Recovered {
    pub slots: Vec<SlotRecord>,
    pub frozen: Vec<ConsensusEpoch>,
    pub reports: Vec<ReportRecord>,
    pub evidence: Vec<EvidenceRecord>,
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
}

const SLOT: u8 = b'S';
const PARENT: u8 = b'P';
const FROZEN: u8 = b'F';
const REPORT: u8 = b'R';
const CERTIFIED: u8 = b'T';
const RESIDUAL: u8 = b'G';
const EVIDENCE: u8 = b'E';
const EVIDENCE_VOTE: u8 = b'V';

impl SigningRecords {
    pub fn new(env: LmdbEnvironment) -> anyhow::Result<Self> {
        let store = LmdbSigningStore::new(&env)?;
        Ok(Self { env, store })
    }

    pub fn new_null() -> Self {
        Self::new(LmdbEnvironment::new_null()).expect("a nulled environment opens")
    }

    /// Persists the slot states of a batch of votes about to be released
    pub fn write_slots(&self, records: &[SlotRecord]) {
        if records.is_empty() {
            return;
        }
        let store = &self.store;
        let mut txn = self.env.begin_write();
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
    }

    /// Persists that this node left an epoch: it signs nothing new in it
    pub fn write_frozen(&self, epoch: ConsensusEpoch) {
        let mut txn = self.env.begin_write();
        self.store.put(&mut txn, &epoch_key(FROZEN, epoch), &[]);
        txn.commit();
    }

    /// Persists a frozen report before its headers are broadcast
    pub fn write_report(&self, record: &ReportRecord) {
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
    }

    /// Persists retained evidence, in one batch: a record replaces the one
    /// held for its block and epoch, as it holds every vote the earlier one
    /// did
    ///
    /// A vote is stored once, by epoch and signature: a vote batch covers up
    /// to 255 blocks, and the records of those blocks name it rather than
    /// hold a copy each. The signature names it without hashing its blocks.
    pub fn write_evidence(&self, records: &[EvidenceRecord]) {
        if records.is_empty() {
            return;
        }
        let mut txn = self.env.begin_write();
        let mut written = std::collections::HashSet::new();
        for record in records {
            for vote in &record.votes {
                let key = evidence_vote_key(record.epoch, vote);
                if written.insert(key.clone()) && self.store.get(&txn, &key).is_none() {
                    let mut bytes = Vec::new();
                    vote.serialize(&mut bytes)
                        .expect("a vote serializes to memory");
                    self.store.put(&mut txn, &key, &bytes);
                }
            }
            self.store.put(
                &mut txn,
                &evidence_key(record.epoch, &record.hash),
                &encode_evidence(record),
            );
        }
        txn.commit();
    }

    /// Drops the evidence of epochs before the given one, except where
    /// `keep` says it may still discharge a lock record of its epoch at its
    /// block's position
    pub fn forget_evidence_before(
        &self,
        epoch: ConsensusEpoch,
        keep: impl Fn(ConsensusEpoch, &AccountSlot) -> bool,
    ) {
        let mut txn = self.env.begin_write();
        let mut old = Vec::new();
        let mut referenced = std::collections::HashSet::new();
        for (key, value) in self.store.with_prefix(&txn, &[EVIDENCE]) {
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
            self.store
                .with_prefix(&txn, &[EVIDENCE_VOTE])
                .into_iter()
                .map(|(key, _)| key)
                .filter(|key| !referenced.contains(key)),
        );
        for key in old {
            self.store.delete(&mut txn, &key);
        }
        txn.commit();
    }

    /// Drops the records of epochs before the given one
    pub fn forget_before(&self, epoch: ConsensusEpoch) {
        let store = &self.store;
        let mut txn = self.env.begin_write();
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
            .collect();
        for key in old {
            store.delete(&mut txn, &key);
        }
        txn.commit();
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
        for (key, value) in store.with_prefix(&txn, &[EVIDENCE_VOTE]) {
            if let Ok(vote) = Vote::deserialize(&value) {
                votes.insert(key, Arc::new(vote));
            }
        }
        for (key, value) in store.with_prefix(&txn, &[EVIDENCE]) {
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
        recovered
    }
}

/* Encoding */

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
}
