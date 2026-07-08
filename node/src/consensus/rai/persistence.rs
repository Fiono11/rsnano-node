use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::Arc,
};

use anyhow::Context;
use num_traits::FromPrimitive;
use rsnano_ledger::Ledger;
use rsnano_types::{
    Amount, BlockHash, DeserializationError, PublicKey, RaiElectionId, RaiElectionValue, RaiEpoch,
    RaiPendingReport, RaiSlot, RaiVoteKind, read_u8, read_u64_be,
};

use super::{
    RaiActiveElectionsSnapshot, RaiCloseEpochSnapshot, RaiCloseStateSnapshot,
    RaiCloseValueSnapshot, RaiClosedSlotSnapshot, RaiCommittee, RaiCommitteeMember,
    RaiCommitteeSnapshot, RaiCommitteeThresholds, RaiElectionSnapshot, RaiElectionStatus,
    RaiTallySnapshot, RaiVoteSummary,
};

const SNAPSHOT_VERSION: u8 = 1;
const CLOSE_STATE_KEY: &[u8] = b"close_state";
const ACTIVE_ELECTIONS_KEY: &[u8] = b"active_elections";
const COMMITTEES_KEY: &[u8] = b"committees";

pub trait RaiStatePersistence: Send + Sync {
    fn save_close_state(&self, _snapshot: &RaiCloseStateSnapshot) {}
    fn save_active_elections(&self, _snapshot: &RaiActiveElectionsSnapshot) {}
    fn save_active_and_close(
        &self,
        active_elections: &RaiActiveElectionsSnapshot,
        close_state: &RaiCloseStateSnapshot,
    ) {
        self.save_active_elections(active_elections);
        self.save_close_state(close_state);
    }
    fn save_committee_snapshot(&self, _epoch: RaiEpoch, _committee: &RaiCommittee) {}
}

pub struct NoopRaiStatePersistence;

impl RaiStatePersistence for NoopRaiStatePersistence {}

#[derive(Default)]
pub struct RaiPersistedState {
    pub close_state: Option<RaiCloseStateSnapshot>,
    pub active_elections: Option<RaiActiveElectionsSnapshot>,
    pub committees: Vec<(RaiEpoch, RaiCommittee)>,
}

pub struct LmdbRaiStatePersistence {
    ledger: Arc<Ledger>,
}

impl LmdbRaiStatePersistence {
    pub fn new(ledger: Arc<Ledger>) -> Self {
        Self { ledger }
    }

    pub fn load(&self) -> anyhow::Result<RaiPersistedState> {
        let txn = self.ledger.store.begin_read();
        let close_state = self
            .ledger
            .store
            .rai
            .get(&txn, CLOSE_STATE_KEY)
            .map(|bytes| deserialize_close_state(&bytes))
            .transpose()
            .context("could not deserialize RAI close state")?;
        let active_elections = self
            .ledger
            .store
            .rai
            .get(&txn, ACTIVE_ELECTIONS_KEY)
            .map(|bytes| deserialize_active_elections(&bytes))
            .transpose()
            .context("could not deserialize RAI active elections")?;
        let committees = self
            .ledger
            .store
            .rai
            .get(&txn, COMMITTEES_KEY)
            .map(|bytes| deserialize_committees(&bytes))
            .transpose()
            .context("could not deserialize RAI committee snapshots")?
            .unwrap_or_default();

        Ok(RaiPersistedState {
            close_state,
            active_elections,
            committees,
        })
    }

    fn put(&self, key: &[u8], value: &[u8]) {
        let mut txn = self.ledger.store.begin_write();
        self.ledger.store.rai.put(&mut txn, key, value);
        txn.commit();
    }
}

impl RaiStatePersistence for LmdbRaiStatePersistence {
    fn save_close_state(&self, snapshot: &RaiCloseStateSnapshot) {
        self.put(CLOSE_STATE_KEY, &serialize_close_state(snapshot));
    }

    fn save_active_elections(&self, snapshot: &RaiActiveElectionsSnapshot) {
        self.put(ACTIVE_ELECTIONS_KEY, &serialize_active_elections(snapshot));
    }

    fn save_active_and_close(
        &self,
        active_elections: &RaiActiveElectionsSnapshot,
        close_state: &RaiCloseStateSnapshot,
    ) {
        let mut txn = self.ledger.store.begin_write();
        self.ledger.store.rai.put(
            &mut txn,
            ACTIVE_ELECTIONS_KEY,
            &serialize_active_elections(active_elections),
        );
        self.ledger.store.rai.put(
            &mut txn,
            CLOSE_STATE_KEY,
            &serialize_close_state(close_state),
        );
        txn.commit();
    }

    fn save_committee_snapshot(&self, epoch: RaiEpoch, committee: &RaiCommittee) {
        let mut txn = self.ledger.store.begin_write();
        let mut committees: BTreeMap<_, _> = self
            .ledger
            .store
            .rai
            .get(&txn, COMMITTEES_KEY)
            .map(|bytes| deserialize_committees(&bytes))
            .transpose()
            .expect("could not deserialize RAI committee snapshots")
            .unwrap_or_default()
            .into_iter()
            .collect();
        committees.insert(epoch, committee.clone());
        let bytes = serialize_committees(committees.into_iter());
        self.ledger.store.rai.put(&mut txn, COMMITTEES_KEY, &bytes);
        txn.commit();
    }
}

fn serialize_close_state(snapshot: &RaiCloseStateSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_version(&mut bytes);
    write_u64(&mut bytes, snapshot.current_epoch);
    write_len(&mut bytes, snapshot.epochs.len());
    for epoch in &snapshot.epochs {
        write_close_epoch(&mut bytes, epoch);
    }
    bytes
}

fn deserialize_close_state(
    mut bytes: &[u8],
) -> Result<RaiCloseStateSnapshot, DeserializationError> {
    read_version(&mut bytes)?;
    let current_epoch = read_u64(&mut bytes)?;
    let epoch_count = read_len(&mut bytes)?;
    let mut epochs = Vec::with_capacity(epoch_count);
    for _ in 0..epoch_count {
        epochs.push(read_close_epoch(&mut bytes)?);
    }
    expect_finished(bytes)?;

    Ok(RaiCloseStateSnapshot {
        current_epoch,
        epochs,
    })
}

fn write_close_epoch(writer: &mut Vec<u8>, snapshot: &RaiCloseEpochSnapshot) {
    write_u64(writer, snapshot.epoch);
    write_epoch_phase(writer, snapshot.phase);
    write_len(writer, snapshot.pending_reports.len());
    for report in &snapshot.pending_reports {
        write_pending_report(writer, report);
    }
    write_slots(writer, &snapshot.visible_slots);
    write_len(writer, snapshot.close_values.len());
    for close_value in &snapshot.close_values {
        close_value.hash.serialize(writer).unwrap();
        write_slots(writer, &close_value.slots);
    }
    write_u64s(writer, &snapshot.started_close_attempts);
    write_u64s(writer, &snapshot.processed_close_attempts);
    write_optional_slots(writer, snapshot.cut_set.as_deref());
    write_len(writer, snapshot.closed_slots.len());
    for closed in &snapshot.closed_slots {
        closed.slot.serialize(writer).unwrap();
        closed.outcome.serialize(writer).unwrap();
    }
}

fn read_close_epoch(reader: &mut &[u8]) -> Result<RaiCloseEpochSnapshot, DeserializationError> {
    let epoch = read_u64(reader)?;
    let phase = read_epoch_phase(reader)?;
    let pending_report_count = read_len(reader)?;
    let mut pending_reports = Vec::with_capacity(pending_report_count);
    for _ in 0..pending_report_count {
        pending_reports.push(read_pending_report(reader)?);
    }
    let visible_slots = read_slots(reader)?;
    let close_value_count = read_len(reader)?;
    let mut close_values = Vec::with_capacity(close_value_count);
    for _ in 0..close_value_count {
        let hash = BlockHash::deserialize(reader)?;
        let slots = read_slots(reader)?;
        close_values.push(RaiCloseValueSnapshot { hash, slots });
    }
    let started_close_attempts = read_u64s(reader)?;
    let processed_close_attempts = read_u64s(reader)?;
    let cut_set = read_optional_slots(reader)?;
    let closed_slot_count = read_len(reader)?;
    let mut closed_slots = Vec::with_capacity(closed_slot_count);
    for _ in 0..closed_slot_count {
        let slot = RaiSlot::deserialize(reader)?;
        let outcome = RaiElectionValue::deserialize(reader)?;
        closed_slots.push(RaiClosedSlotSnapshot { slot, outcome });
    }

    Ok(RaiCloseEpochSnapshot {
        epoch,
        phase,
        pending_reports,
        visible_slots,
        close_values,
        started_close_attempts,
        processed_close_attempts,
        cut_set,
        closed_slots,
    })
}

fn serialize_active_elections(snapshot: &RaiActiveElectionsSnapshot) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_version(&mut bytes);
    write_len(&mut bytes, snapshot.elections.len());
    for election in &snapshot.elections {
        write_election(&mut bytes, election);
    }
    bytes
}

fn deserialize_active_elections(
    mut bytes: &[u8],
) -> Result<RaiActiveElectionsSnapshot, DeserializationError> {
    read_version(&mut bytes)?;
    let election_count = read_len(&mut bytes)?;
    let mut elections = Vec::with_capacity(election_count);
    for _ in 0..election_count {
        elections.push(read_election(&mut bytes)?);
    }
    expect_finished(bytes)?;

    Ok(RaiActiveElectionsSnapshot { elections })
}

fn write_election(writer: &mut Vec<u8>, snapshot: &RaiElectionSnapshot) {
    snapshot.id.serialize(writer).unwrap();
    write_election_status(writer, snapshot.status);
    write_len(writer, snapshot.votes.len());
    for vote in &snapshot.votes {
        write_vote_summary(writer, vote);
    }
    write_tallies(writer, &snapshot.tallies);
    write_tallies(writer, &snapshot.final_tallies);
    write_optional_election_value(writer, snapshot.winner.as_ref());
    write_optional_election_value(writer, snapshot.confirmed_value.as_ref());
}

fn read_election(reader: &mut &[u8]) -> Result<RaiElectionSnapshot, DeserializationError> {
    let id = RaiElectionId::deserialize(reader)?;
    let status = read_election_status(reader)?;
    let vote_count = read_len(reader)?;
    let mut votes = Vec::with_capacity(vote_count);
    for _ in 0..vote_count {
        votes.push(read_vote_summary(reader)?);
    }
    let tallies = read_tallies(reader)?;
    let final_tallies = read_tallies(reader)?;
    let winner = read_optional_election_value(reader)?;
    let confirmed_value = read_optional_election_value(reader)?;

    Ok(RaiElectionSnapshot {
        id,
        status,
        votes,
        tallies,
        final_tallies,
        winner,
        confirmed_value,
    })
}

fn serialize_committees(committees: impl IntoIterator<Item = (RaiEpoch, RaiCommittee)>) -> Vec<u8> {
    let mut bytes = Vec::new();
    let committees: Vec<_> = committees.into_iter().collect();
    write_version(&mut bytes);
    write_len(&mut bytes, committees.len());
    for (epoch, committee) in committees {
        write_u64(&mut bytes, epoch);
        write_committee(&mut bytes, &committee.snapshot());
    }
    bytes
}

fn deserialize_committees(
    mut bytes: &[u8],
) -> Result<Vec<(RaiEpoch, RaiCommittee)>, DeserializationError> {
    read_version(&mut bytes)?;
    let committee_count = read_len(&mut bytes)?;
    let mut committees = Vec::with_capacity(committee_count);
    for _ in 0..committee_count {
        let epoch = read_u64(&mut bytes)?;
        let committee = RaiCommittee::from_snapshot(read_committee(&mut bytes)?);
        committees.push((epoch, committee));
    }
    expect_finished(bytes)?;
    Ok(committees)
}

fn write_committee(writer: &mut Vec<u8>, snapshot: &RaiCommitteeSnapshot) {
    write_thresholds(writer, snapshot.thresholds);
    write_len(writer, snapshot.members.len());
    for member in &snapshot.members {
        member.account.serialize(writer).unwrap();
        member.balance.serialize(writer).unwrap();
    }
}

fn read_committee(reader: &mut &[u8]) -> Result<RaiCommitteeSnapshot, DeserializationError> {
    let thresholds = read_thresholds(reader)?;
    let member_count = read_len(reader)?;
    let mut members = Vec::with_capacity(member_count);
    for _ in 0..member_count {
        let account = PublicKey::deserialize(reader)?;
        let balance = Amount::deserialize(reader)?;
        members.push(RaiCommitteeMember { account, balance });
    }
    Ok(RaiCommitteeSnapshot {
        members,
        thresholds,
    })
}

fn write_vote_summary(writer: &mut Vec<u8>, vote: &RaiVoteSummary) {
    vote.voter.serialize(writer).unwrap();
    writer.write_all(&[vote.kind as u8]).unwrap();
    vote.value.serialize(writer).unwrap();
    write_usize(writer, vote.committee_votes);
}

fn read_vote_summary(reader: &mut &[u8]) -> Result<RaiVoteSummary, DeserializationError> {
    let voter = PublicKey::deserialize(reader)?;
    let kind = RaiVoteKind::from_u8(read_u8(reader)?).ok_or(DeserializationError::InvalidData)?;
    let value = RaiElectionValue::deserialize(reader)?;
    let committee_votes = read_usize(reader)?;
    Ok(RaiVoteSummary {
        voter,
        kind,
        value,
        committee_votes,
    })
}

fn write_tallies(writer: &mut Vec<u8>, tallies: &[RaiTallySnapshot]) {
    write_len(writer, tallies.len());
    for tally in tallies {
        tally.value.serialize(writer).unwrap();
        write_len(writer, tally.per_committee.len());
        for count in &tally.per_committee {
            write_usize(writer, *count);
        }
    }
}

fn read_tallies(reader: &mut &[u8]) -> Result<Vec<RaiTallySnapshot>, DeserializationError> {
    let tally_count = read_len(reader)?;
    let mut tallies = Vec::with_capacity(tally_count);
    for _ in 0..tally_count {
        let value = RaiElectionValue::deserialize(reader)?;
        let count = read_len(reader)?;
        let mut per_committee = Vec::with_capacity(count);
        for _ in 0..count {
            per_committee.push(read_usize(reader)?);
        }
        tallies.push(RaiTallySnapshot {
            value,
            per_committee,
        });
    }
    Ok(tallies)
}

fn write_pending_report(writer: &mut Vec<u8>, report: &RaiPendingReport) {
    write_len(writer, report.slots.len());
    report.serialize(writer).unwrap();
}

fn read_pending_report(reader: &mut &[u8]) -> Result<RaiPendingReport, DeserializationError> {
    let slot_count = read_len(reader)?;
    let mut bytes = vec![0; RaiPendingReport::serialized_size(slot_count)];
    reader.read_exact(&mut bytes)?;
    RaiPendingReport::deserialize(&bytes, slot_count)
}

fn write_slots(writer: &mut Vec<u8>, slots: &[RaiSlot]) {
    write_len(writer, slots.len());
    for slot in slots {
        slot.serialize(writer).unwrap();
    }
}

fn read_slots(reader: &mut &[u8]) -> Result<Vec<RaiSlot>, DeserializationError> {
    let slot_count = read_len(reader)?;
    let mut slots = Vec::with_capacity(slot_count);
    for _ in 0..slot_count {
        slots.push(RaiSlot::deserialize(reader)?);
    }
    Ok(slots)
}

fn write_optional_slots(writer: &mut Vec<u8>, slots: Option<&[RaiSlot]>) {
    match slots {
        Some(slots) => {
            writer.write_all(&[1]).unwrap();
            write_slots(writer, slots);
        }
        None => writer.write_all(&[0]).unwrap(),
    }
}

fn read_optional_slots(reader: &mut &[u8]) -> Result<Option<Vec<RaiSlot>>, DeserializationError> {
    match read_u8(reader)? {
        0 => Ok(None),
        1 => Ok(Some(read_slots(reader)?)),
        _ => Err(DeserializationError::InvalidData),
    }
}

fn write_optional_election_value(writer: &mut Vec<u8>, value: Option<&RaiElectionValue>) {
    match value {
        Some(value) => {
            writer.write_all(&[1]).unwrap();
            value.serialize(writer).unwrap();
        }
        None => writer.write_all(&[0]).unwrap(),
    }
}

fn read_optional_election_value(
    reader: &mut &[u8],
) -> Result<Option<RaiElectionValue>, DeserializationError> {
    match read_u8(reader)? {
        0 => Ok(None),
        1 => Ok(Some(RaiElectionValue::deserialize(reader)?)),
        _ => Err(DeserializationError::InvalidData),
    }
}

fn write_thresholds(writer: &mut Vec<u8>, thresholds: RaiCommitteeThresholds) {
    write_usize(writer, thresholds.size);
    write_usize(writer, thresholds.max_faulty);
    write_usize(writer, thresholds.max_offline);
    write_usize(writer, thresholds.notarization);
    write_usize(writer, thresholds.fast);
    write_usize(writer, thresholds.finalization);
}

fn read_thresholds(reader: &mut &[u8]) -> Result<RaiCommitteeThresholds, DeserializationError> {
    Ok(RaiCommitteeThresholds {
        size: read_usize(reader)?,
        max_faulty: read_usize(reader)?,
        max_offline: read_usize(reader)?,
        notarization: read_usize(reader)?,
        fast: read_usize(reader)?,
        finalization: read_usize(reader)?,
    })
}

fn write_epoch_phase(writer: &mut Vec<u8>, phase: super::RaiEpochPhase) {
    writer
        .write_all(&[match phase {
            super::RaiEpochPhase::Open => 0,
            super::RaiEpochPhase::Closing => 1,
            super::RaiEpochPhase::Closed => 2,
        }])
        .unwrap();
}

fn read_epoch_phase(reader: &mut &[u8]) -> Result<super::RaiEpochPhase, DeserializationError> {
    match read_u8(reader)? {
        0 => Ok(super::RaiEpochPhase::Open),
        1 => Ok(super::RaiEpochPhase::Closing),
        2 => Ok(super::RaiEpochPhase::Closed),
        _ => Err(DeserializationError::InvalidData),
    }
}

fn write_election_status(writer: &mut Vec<u8>, status: RaiElectionStatus) {
    writer
        .write_all(&[match status {
            RaiElectionStatus::Active => 0,
            RaiElectionStatus::Confirmed => 1,
        }])
        .unwrap();
}

fn read_election_status(reader: &mut &[u8]) -> Result<RaiElectionStatus, DeserializationError> {
    match read_u8(reader)? {
        0 => Ok(RaiElectionStatus::Active),
        1 => Ok(RaiElectionStatus::Confirmed),
        _ => Err(DeserializationError::InvalidData),
    }
}

fn write_u64s(writer: &mut Vec<u8>, values: &[u64]) {
    write_len(writer, values.len());
    for value in values {
        write_u64(writer, *value);
    }
}

fn read_u64s(reader: &mut &[u8]) -> Result<Vec<u64>, DeserializationError> {
    let count = read_len(reader)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(read_u64(reader)?);
    }
    Ok(values)
}

fn write_version(writer: &mut Vec<u8>) {
    writer.write_all(&[SNAPSHOT_VERSION]).unwrap();
}

fn read_version(reader: &mut &[u8]) -> Result<(), DeserializationError> {
    if read_u8(reader)? == SNAPSHOT_VERSION {
        Ok(())
    } else {
        Err(DeserializationError::InvalidData)
    }
}

fn write_len(writer: &mut Vec<u8>, value: usize) {
    write_u64(writer, value as u64);
}

fn read_len(reader: &mut &[u8]) -> Result<usize, DeserializationError> {
    read_usize(reader)
}

fn write_usize(writer: &mut Vec<u8>, value: usize) {
    write_u64(writer, value as u64);
}

fn read_usize(reader: &mut &[u8]) -> Result<usize, DeserializationError> {
    usize::try_from(read_u64(reader)?).map_err(|_| DeserializationError::InvalidData)
}

fn write_u64(writer: &mut Vec<u8>, value: u64) {
    writer.write_all(&value.to_be_bytes()).unwrap();
}

fn read_u64(reader: &mut &[u8]) -> Result<u64, DeserializationError> {
    Ok(read_u64_be(reader)?)
}

fn expect_finished(bytes: &[u8]) -> Result<(), DeserializationError> {
    if bytes.is_empty() {
        Ok(())
    } else {
        Err(DeserializationError::TooMuchData)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::RaiCommitteeDeriver;
    use rsnano_ledger::Ledger;
    use rsnano_types::{Account, BlockHash, PrivateKey};
    use std::sync::Arc;

    #[test]
    fn close_state_roundtrip() {
        let key = PrivateKey::from(1);
        let slot = RaiSlot::new(Account::from(2), 3);
        let outcome = RaiElectionValue::Block(BlockHash::from(4));
        let snapshot = RaiCloseStateSnapshot {
            current_epoch: 7,
            epochs: vec![RaiCloseEpochSnapshot {
                epoch: 7,
                phase: super::super::RaiEpochPhase::Closing,
                pending_reports: vec![RaiPendingReport::new(&key, 7, vec![slot])],
                visible_slots: vec![slot],
                close_values: vec![RaiCloseValueSnapshot {
                    hash: BlockHash::from(5),
                    slots: vec![slot],
                }],
                started_close_attempts: vec![0],
                processed_close_attempts: vec![1],
                cut_set: Some(vec![slot]),
                closed_slots: vec![RaiClosedSlotSnapshot { slot, outcome }],
            }],
        };

        assert_eq!(
            deserialize_close_state(&serialize_close_state(&snapshot)).unwrap(),
            snapshot
        );
    }

    #[test]
    fn active_elections_roundtrip() {
        let key = PrivateKey::from(1);
        let slot = RaiSlot::new(Account::from(2), 3);
        let value = RaiElectionValue::Block(BlockHash::from(4));
        let snapshot = RaiActiveElectionsSnapshot {
            elections: vec![RaiElectionSnapshot {
                id: RaiElectionId::Slot { slot, epoch: 7 },
                status: RaiElectionStatus::Confirmed,
                votes: vec![RaiVoteSummary {
                    voter: key.public_key(),
                    kind: RaiVoteKind::Final,
                    value: value.clone(),
                    committee_votes: 1,
                }],
                tallies: vec![RaiTallySnapshot {
                    value: value.clone(),
                    per_committee: vec![1],
                }],
                final_tallies: vec![RaiTallySnapshot {
                    value: value.clone(),
                    per_committee: vec![1],
                }],
                winner: Some(value.clone()),
                confirmed_value: Some(value),
            }],
        };

        assert_eq!(
            deserialize_active_elections(&serialize_active_elections(&snapshot)).unwrap(),
            snapshot
        );
    }

    #[test]
    fn committees_roundtrip() {
        let key = PrivateKey::from(1);
        let committee =
            RaiCommitteeDeriver::new().derive_committee([(key.public_key(), Amount::raw(100))]);

        let decoded =
            deserialize_committees(&serialize_committees([(3, committee.clone())])).unwrap();

        assert_eq!(decoded, vec![(3, committee)]);
    }

    #[test]
    fn lmdb_persistence_saves_and_loads_snapshots() {
        let key = PrivateKey::from(1);
        let slot = RaiSlot::new(Account::from(2), 3);
        let value = RaiElectionValue::Block(BlockHash::from(4));
        let close_state = RaiCloseStateSnapshot {
            current_epoch: 1,
            epochs: vec![RaiCloseEpochSnapshot {
                epoch: 0,
                phase: super::super::RaiEpochPhase::Closed,
                pending_reports: Vec::new(),
                visible_slots: vec![slot],
                close_values: Vec::new(),
                started_close_attempts: vec![0],
                processed_close_attempts: vec![0],
                cut_set: Some(vec![slot]),
                closed_slots: vec![RaiClosedSlotSnapshot {
                    slot,
                    outcome: value.clone(),
                }],
            }],
        };
        let active_elections = RaiActiveElectionsSnapshot {
            elections: vec![RaiElectionSnapshot {
                id: RaiElectionId::Slot { slot, epoch: 0 },
                status: RaiElectionStatus::Confirmed,
                votes: Vec::new(),
                tallies: Vec::new(),
                final_tallies: Vec::new(),
                winner: Some(value.clone()),
                confirmed_value: Some(value),
            }],
        };
        let committee =
            RaiCommitteeDeriver::new().derive_committee([(key.public_key(), Amount::raw(100))]);
        let persistence = LmdbRaiStatePersistence::new(Arc::new(Ledger::new_null()));

        persistence.save_close_state(&close_state);
        persistence.save_active_elections(&active_elections);
        persistence.save_committee_snapshot(0, &committee);
        let loaded = persistence.load().unwrap();

        assert_eq!(loaded.close_state, Some(close_state));
        assert_eq!(loaded.active_elections, Some(active_elections));
        assert_eq!(loaded.committees, vec![(0, committee)]);
    }
}
