use std::{collections::BTreeMap, sync::Arc, time::Duration};

use rsnano_messages::CheckpointReply;
use rsnano_nullable_clock::Timestamp;

use super::chunk_window::ChunkWindow;
use crate::consensus::election::{EpochLedger, EpochValue};

/// RAI, checkpoint catch-up: the decided state of an epoch being fetched
/// in chunks of its encoding, for the value a close certificate finalized.
/// The state is accepted only if it hashes to the `d_e` that value names.
pub(super) struct CheckpointFetch {
    value: EpochValue,
    total: Option<u32>,
    chunks: BTreeMap<u32, Vec<u8>>,
    window: ChunkWindow,
    started: Timestamp,
}

/// Chunks of a state asked for at once
const WINDOW: usize = 8;

impl CheckpointFetch {
    pub fn new(value: EpochValue, now: Timestamp) -> Self {
        Self {
            value,
            total: None,
            chunks: BTreeMap::new(),
            window: ChunkWindow::new(WINDOW),
            started: now,
        }
    }

    pub fn value(&self) -> &EpochValue {
        &self.value
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

    /// Takes a chunk: the state once every chunk arrived and it hashes to
    /// the value's `d_e`; Err for a chunk that contradicts the ones before
    /// or a complete encoding that is not the state named
    pub fn take(
        &mut self,
        total: u32,
        from: u32,
        data: &[u8],
    ) -> Result<Option<Arc<EpochLedger>>, ()> {
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
        match EpochLedger::from_bytes(&bytes) {
            Some(state) if state.state_hash() == self.value.state => Ok(Some(Arc::new(state))),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use rsnano_types::{Account, BlockHash, ConsensusEpoch};

    use super::*;
    use crate::consensus::election::AccountSlot;

    #[test]
    fn a_state_arrives_in_chunks_and_is_checked_against_the_value() {
        let now = Timestamp::new_test_instance();
        let retry = Duration::from_millis(300);
        let state = large_state();
        let bytes = state.to_bytes();
        assert!(bytes.len() > 2 * CheckpointReply::MAX_DATA);
        let mut fetch = CheckpointFetch::new(value_naming(&state), now);

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
        assert_eq!(result.unwrap().unwrap().state_hash(), state.state_hash());
    }

    #[test]
    fn a_state_the_value_does_not_name_is_refused() {
        let now = Timestamp::new_test_instance();
        let state = large_state();
        let mut other = state.clone();
        other.finalize_genesis(AccountSlot::new(Account::from(1), 1), BlockHash::from(1));
        let bytes = other.to_bytes();
        let mut fetch = CheckpointFetch::new(value_naming(&state), now);
        let total = bytes.len() as u32;

        let mut result = Ok(None);
        for from in (0..bytes.len()).step_by(CheckpointReply::MAX_DATA) {
            let end = (from + CheckpointReply::MAX_DATA).min(bytes.len());
            result = fetch.take(total, from as u32, &bytes[from..end]);
        }
        assert_eq!(result, Err(()));
    }

    #[test]
    fn a_chunk_that_contradicts_the_length_is_refused() {
        let mut fetch = CheckpointFetch::new(
            value_naming(&EpochLedger::new()),
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
