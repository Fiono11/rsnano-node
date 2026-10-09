use std::{collections::BTreeMap, sync::Arc, time::Duration};

use rsnano_messages::CheckpointReply;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, ConsensusEpoch};

use super::chunk_window::ChunkWindow;
use crate::consensus::election::{EpochLedger, EpochValue};

/// RAI, checkpoint catch-up: the value a close certificate finalized and
/// the state it decided, fetched in chunks of one encoding by the value's
/// hash. A replica that holds the certificate but neither the proposal nor
/// the reports - leaders repeat their proposals for a few epochs only, and
/// the reports are released - learns both from a replica that decided the
/// epoch. The value must hash to the certified hash and the state to the
/// value's `d_e`.
pub(super) struct CheckpointFetch {
    epoch: ConsensusEpoch,
    value: BlockHash,
    total: Option<u32>,
    chunks: BTreeMap<u32, Vec<u8>>,
    window: ChunkWindow,
    started: Timestamp,
}

/// Chunks asked for at once
const WINDOW: usize = 8;

/// The encoding a holder serves: the value's length, the value, the state
pub(super) fn checkpoint_bytes(value: &EpochValue, state: &EpochLedger) -> Vec<u8> {
    let value = value.to_bytes();
    let state = state.to_bytes();
    let mut bytes = Vec::with_capacity(4 + value.len() + state.len());
    bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&value);
    bytes.extend_from_slice(&state);
    bytes
}

impl CheckpointFetch {
    pub fn new(epoch: ConsensusEpoch, value: BlockHash, now: Timestamp) -> Self {
        Self {
            epoch,
            value,
            total: None,
            chunks: BTreeMap::new(),
            window: ChunkWindow::new(WINDOW),
            started: now,
        }
    }

    /// The certified value's hash this fetch is for
    pub fn value(&self) -> BlockHash {
        self.value
    }

    pub fn started(&self) -> Timestamp {
        self.started
    }

    /// The chunk starts to ask for now; the first chunk tells the length
    pub fn due(&mut self, now: Timestamp, retry: Duration) -> Vec<u32> {
        let missing = match self.total {
            None => vec![0],
            Some(total) => (0..total)
                .step_by(CheckpointReply::MAX_DATA)
                .filter(|start| !self.chunks.contains_key(start))
                .collect(),
        };
        self.window.due(missing, now, retry)
    }

    /// Takes a chunk: the value and its state once every chunk arrived and
    /// both check out; Err for a chunk that contradicts the ones before, or
    /// a complete encoding that is not the certified value and its state
    pub fn take(
        &mut self,
        total: u32,
        from: u32,
        data: &[u8],
    ) -> Result<Option<(EpochValue, Arc<EpochLedger>)>, ()> {
        if *self.total.get_or_insert(total) != total
            || from as usize % CheckpointReply::MAX_DATA != 0
            || from >= total.max(1)
            || data.len() != (total - from).min(CheckpointReply::MAX_DATA as u32) as usize
        {
            return Err(());
        }
        self.window.received(from);
        self.chunks.insert(from, data.to_vec());
        let received: usize = self.chunks.values().map(|chunk| chunk.len()).sum();
        if received < total as usize {
            return Ok(None);
        }
        let bytes: Vec<u8> = self.chunks.values().flatten().copied().collect();
        self.decode(&bytes).map(Some).ok_or(())
    }

    fn decode(&self, bytes: &[u8]) -> Option<(EpochValue, Arc<EpochLedger>)> {
        let length = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
        let value = EpochValue::from_bytes(bytes.get(4..4 + length)?)?;
        if value.epoch != self.epoch || value.hash() != self.value {
            return None;
        }
        let state = EpochLedger::from_bytes(bytes.get(4 + length..)?)?;
        (state.state_hash() == value.state).then(|| (value, Arc::new(state)))
    }
}

#[cfg(test)]
mod tests {
    use rsnano_types::Account;

    use super::*;
    use crate::consensus::election::AccountSlot;

    #[test]
    fn a_value_and_its_state_arrive_in_chunks_and_are_checked() {
        let now = Timestamp::new_test_instance();
        let retry = Duration::from_millis(300);
        let state = large_state();
        let value = value_naming(&state);
        let bytes = checkpoint_bytes(&value, &state);
        assert!(bytes.len() > 2 * CheckpointReply::MAX_DATA);
        let mut fetch = CheckpointFetch::new(value.epoch, value.hash(), now);

        assert_eq!(fetch.due(now, retry), vec![0]);
        let total = bytes.len() as u32;
        let chunk = |from: usize| &bytes[from..(from + CheckpointReply::MAX_DATA).min(bytes.len())];
        assert_eq!(fetch.take(total, 0, chunk(0)), Ok(None));
        let rest = fetch.due(now, retry);
        assert_eq!(
            rest.len(),
            bytes.len().div_ceil(CheckpointReply::MAX_DATA) - 1
        );

        let mut result = Ok(None);
        for from in rest {
            result = fetch.take(total, from, chunk(from as usize));
        }
        let (fetched, fetched_state) = result.unwrap().unwrap();
        assert_eq!(fetched, value);
        assert_eq!(fetched_state.state_hash(), state.state_hash());
    }

    #[test]
    fn a_state_the_value_does_not_name_is_refused() {
        let state = large_state();
        let mut other = state.clone();
        other.finalize_genesis(AccountSlot::new(Account::from(1), 1), BlockHash::from(1));
        let value = value_naming(&state);
        let bytes = checkpoint_bytes(&value, &other);
        assert_eq!(fetch_all(value.hash(), &bytes), Err(()));
    }

    #[test]
    fn a_value_other_than_the_certified_one_is_refused() {
        let state = large_state();
        let bytes = checkpoint_bytes(&value_naming(&state), &state);
        assert_eq!(fetch_all(BlockHash::from(99), &bytes), Err(()));
    }

    #[test]
    fn a_chunk_that_contradicts_the_length_is_refused() {
        let mut fetch = CheckpointFetch::new(
            ConsensusEpoch::new(3),
            BlockHash::from(1),
            Timestamp::new_test_instance(),
        );
        assert_eq!(
            fetch.take(100_000, 0, &[0; CheckpointReply::MAX_DATA]),
            Ok(None)
        );
        assert_eq!(fetch.take(90_000, 60_000, &[0; 30_000]), Err(()));
        assert_eq!(fetch.take(100_000, 7, &[0; 10]), Err(()));
    }

    /*
     * Test helpers
     */

    /// Every chunk of `bytes` into a fetch for the certified value `value`
    fn fetch_all(
        value: BlockHash,
        bytes: &[u8],
    ) -> Result<Option<(EpochValue, Arc<EpochLedger>)>, ()> {
        let mut fetch = CheckpointFetch::new(
            ConsensusEpoch::new(3),
            value,
            Timestamp::new_test_instance(),
        );
        let mut result = Ok(None);
        for from in (0..bytes.len()).step_by(CheckpointReply::MAX_DATA) {
            let end = (from + CheckpointReply::MAX_DATA).min(bytes.len());
            result = fetch.take(bytes.len() as u32, from as u32, &bytes[from..end]);
        }
        result
    }

    fn large_state() -> EpochLedger {
        let mut state = EpochLedger::new();
        for i in 0..2_000u64 {
            state.finalize_genesis(
                AccountSlot::new(Account::from(i + 10), 1),
                BlockHash::from(i + 10),
            );
        }
        state
    }

    fn value_naming(state: &EpochLedger) -> EpochValue {
        EpochValue::from_parts(
            ConsensusEpoch::new(3),
            0,
            BlockHash::ZERO,
            Vec::new(),
            BlockHash::ZERO,
            state.state_hash(),
        )
    }
}
