//! Local admission/retransmission budgets, not a wire format or synchronizer.
use super::{BValue, CheckpointError, SlowEntry, SlowMessage, SlowPhase, SlowResponse, SlowValue};
use rsnano_types::{BlockHash, PublicKey, Signature};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Write},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlowIoLimits {
    /// Accounting uses the local serde JSON representation. A future wire
    /// decoder must enforce its own frame/reassembly bounds before allocation.
    pub max_message_bytes: usize,
    pub max_retry_responses: usize,
    pub max_retry_response_bytes: usize,
}
impl Default for SlowIoLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 1024 * 1024,
            max_retry_responses: 64,
            max_retry_response_bytes: 1024 * 1024,
        }
    }
}
impl SlowIoLimits {
    /// A replica must be able to relay every locally generated response shape,
    /// including later two-value states and the longest encoded rank number.
    pub(super) fn validate_for_replica(self) -> Result<(), CheckpointError> {
        self.validate()?;
        let shapes = [
            (
                SlowPhase::R,
                vec![SlowValue::R {
                    rank: u64::MAX,
                    value: BlockHash::ZERO,
                }],
            ),
            (
                SlowPhase::A,
                vec![
                    SlowValue::A(BlockHash::ZERO),
                    SlowValue::A(BlockHash::from(1)),
                ],
            ),
            (
                SlowPhase::B,
                vec![
                    SlowValue::B(BValue {
                        flag: false,
                        value: BlockHash::ZERO,
                    }),
                    SlowValue::B(BValue {
                        flag: false,
                        value: BlockHash::from(1),
                    }),
                ],
            ),
        ];
        for (phase, values) in shapes {
            SlowMessage::Response(SlowResponse {
                instance: BlockHash::ZERO,
                rank: u64::MAX,
                phase,
                request: BlockHash::ZERO,
                signer: PublicKey::default(),
                entries: values
                    .into_iter()
                    .map(|value| SlowEntry {
                        value,
                        origin: BlockHash::ZERO,
                    })
                    .collect(),
                signature: Signature::default(),
            })
            .encoded_len(self.max_message_bytes)?;
        }
        Ok(())
    }

    pub fn validate(self) -> Result<(), CheckpointError> {
        if self.max_message_bytes == 0
            || self.max_retry_responses == 0
            || self.max_retry_response_bytes < self.max_message_bytes
        {
            return Err(CheckpointError::InvalidSize);
        }
        Ok(())
    }
}

struct ByteCounter {
    bytes: usize,
    limit: usize,
}
impl Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.limit - self.bytes {
            return Err(io::Error::other("slow message exceeds admission budget"));
        }
        self.bytes += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl SlowMessage {
    /// Count without allocating a second copy of the encoded message, stopping
    /// serialization as soon as it exceeds the configured admission budget.
    pub fn encoded_len(&self, limit: usize) -> Result<usize, CheckpointError> {
        encoded_len(self, limit)
    }
}

pub(super) fn encoded_len(
    value: &impl serde::Serialize,
    limit: usize,
) -> Result<usize, CheckpointError> {
    let mut writer = ByteCounter { bytes: 0, limit };
    serde_json::to_writer(&mut writer, value).map_err(|_| CheckpointError::InvalidSize)?;
    Ok(writer.bytes)
}

/// Newly retained responses join the tail. A batch rotates only the records it
/// emits, so new arrivals cannot overtake older records awaiting retransmission.
#[derive(Default)]
pub(super) struct ResponseReplay {
    queue: VecDeque<BlockHash>,
}
impl ResponseReplay {
    /// Called exactly once when a new hash is inserted in relay history.
    pub fn remember(&mut self, id: BlockHash) {
        self.queue.push_back(id);
    }

    pub fn batch(
        &mut self,
        history: &BTreeMap<BlockHash, SlowResponse>,
        limits: SlowIoLimits,
    ) -> Result<Vec<SlowMessage>, CheckpointError> {
        limits.validate()?;
        let mut bytes = 0;
        let mut batch = Vec::new();
        for id in self.queue.iter().take(limits.max_retry_responses) {
            let response = history
                .get(id)
                .ok_or(CheckpointError::MissingEvidence(*id))?;
            let message = SlowMessage::Response(response.clone());
            let size = message.encoded_len(limits.max_message_bytes)?;
            if size > limits.max_retry_response_bytes - bytes {
                break;
            }
            bytes += size;
            batch.push(message);
        }
        // Never advance past evidence that could not be emitted. Errors above
        // leave the entire cursor unchanged, and no batch repeats a record.
        self.queue.rotate_left(batch.len());
        Ok(batch)
    }
}
