//! Bounded transport chunks for slow payloads. Aggregate retained storage is
//! intentionally unbounded. The caller owns peer lifetimes and retry scheduling.
use super::CheckpointError;
use rsnano_messages::CheckpointFrame;
use rsnano_types::{Blake2HashBuilder, BlockHash};
use std::collections::BTreeMap;

/// Fits within a 65535-byte packet including the existing eight-byte Nano header.
/// Carried by Message::CheckpointFrame; the live node handler remains pending.
pub const SLOW_FRAME_BYTES: usize = CheckpointFrame::MAX_BYTES;
const HEADER: usize = CheckpointFrame::HEADER_BYTES; // magic/version, content digest, total length, offset
const CHUNK: usize = SLOW_FRAME_BYTES - HEADER;
const MAGIC: &[u8; 8] = CheckpointFrame::MAGIC;

fn digest(bytes: &[u8]) -> BlockHash {
    Blake2HashBuilder::new()
        .update(b"RAI slow chunks v1")
        .update(bytes)
        .build()
}

pub fn frame_slow_payload(bytes: &[u8], max_bytes: usize) -> Result<Vec<Vec<u8>>, CheckpointError> {
    if bytes.is_empty() || bytes.len() > max_bytes || bytes.len() > u32::MAX as usize {
        return Err(CheckpointError::InvalidSize);
    }
    let id = digest(bytes);
    Ok(bytes
        .chunks(CHUNK)
        .enumerate()
        .map(|(index, chunk)| {
            let mut frame = Vec::with_capacity(HEADER + chunk.len());
            frame.extend_from_slice(MAGIC);
            frame.extend_from_slice(id.as_bytes());
            frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            frame.extend_from_slice(&((index * CHUNK) as u32).to_le_bytes());
            frame.extend_from_slice(chunk);
            frame
        })
        .collect())
}

struct Assembly {
    total: usize,
    chunks: BTreeMap<usize, Vec<u8>>,
}

/// Peer identifiers must identify a connection/session, not an untrusted field
/// in a packet. A malformed peer cannot overwrite another peer's assembly.
pub struct SlowFrameAssembler<P: Ord> {
    max_bytes: usize,
    pending: BTreeMap<(P, BlockHash), Assembly>,
}
impl<P: Ord + Clone> SlowFrameAssembler<P> {
    pub fn new(max_bytes: usize) -> Result<Self, CheckpointError> {
        if max_bytes == 0 || max_bytes > u32::MAX as usize {
            return Err(CheckpointError::InvalidSize);
        }
        Ok(Self {
            max_bytes,
            pending: BTreeMap::new(),
        })
    }

    pub fn forget_peer(&mut self, peer: &P) {
        self.pending.retain(|(p, _), _| p != peer);
    }

    /// Validates fixed header and canonical chunk boundaries before copying any
    /// body bytes. No buffer of the advertised total size is reserved on arrival.
    /// Completion checks the digest; the caller must then decode and verify proofs.
    pub fn receive(&mut self, peer: P, frame: &[u8]) -> Result<Option<Vec<u8>>, CheckpointError> {
        if frame.len() <= HEADER || frame.len() > SLOW_FRAME_BYTES {
            return Err(CheckpointError::InvalidSize);
        }
        if &frame[..8] != MAGIC {
            return Err(CheckpointError::InvalidEvidence);
        }
        let id = BlockHash::from_bytes(frame[8..40].try_into().unwrap());
        let total = u32::from_le_bytes(frame[40..44].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(frame[44..48].try_into().unwrap()) as usize;
        if total == 0
            || total > self.max_bytes
            || offset >= total
            || offset % CHUNK != 0
            || frame.len() - HEADER != CHUNK.min(total - offset)
        {
            return Err(CheckpointError::InvalidSize);
        }
        let key = (peer, id);
        let assembly = self.pending.entry(key.clone()).or_insert_with(|| Assembly {
            total,
            chunks: BTreeMap::new(),
        });
        if assembly.total != total {
            return Err(CheckpointError::InvalidEvidence);
        }
        if let Some(existing) = assembly.chunks.get(&offset) {
            if existing != &frame[HEADER..] {
                return Err(CheckpointError::InvalidEvidence);
            }
        } else {
            assembly.chunks.insert(offset, frame[HEADER..].to_vec());
        }
        if assembly.chunks.len() != total.div_ceil(CHUNK) {
            return Ok(None);
        }
        let assembly = self.pending.remove(&key).unwrap();
        let mut bytes = Vec::with_capacity(total);
        for chunk in assembly.chunks.into_values() {
            bytes.extend_from_slice(&chunk);
        }
        if digest(&bytes) != id {
            return Err(CheckpointError::InvalidEvidence);
        }
        // No completed-message suppression: explicit retransmission must work.
        Ok(Some(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reordered_duplicate_chunks_and_retries() {
        let bytes: Vec<_> = (0..CHUNK * 2 + 13).map(|i| (i % 251) as u8).collect();
        let frames = frame_slow_payload(&bytes, bytes.len()).unwrap();
        assert_eq!(frames.len(), 3);
        let mut receiver = SlowFrameAssembler::new(bytes.len()).unwrap();
        for _ in 0..2 {
            assert_eq!(receiver.receive(1, &frames[2]).unwrap(), None);
            assert_eq!(receiver.receive(1, &frames[2]).unwrap(), None);
            assert_eq!(receiver.receive(1, &frames[0]).unwrap(), None);
            assert_eq!(
                receiver.receive(1, &frames[1]).unwrap(),
                Some(bytes.clone())
            );
            assert!(receiver.pending.is_empty());
        }
    }
    #[test]
    fn peer_isolation_corruption_and_disconnect() {
        let bytes = vec![9; CHUNK + 1];
        let frames = frame_slow_payload(&bytes, bytes.len()).unwrap();
        let mut receiver = SlowFrameAssembler::new(bytes.len()).unwrap();
        let mut corrupt = frames[0].clone();
        corrupt[HEADER] ^= 1;
        assert!(receiver.receive(1, &corrupt).unwrap().is_none());
        assert!(receiver.receive(2, &frames[1]).unwrap().is_none());
        assert_eq!(
            receiver.receive(1, &frames[0]),
            Err(CheckpointError::InvalidEvidence)
        );
        assert_eq!(
            receiver.receive(1, &frames[1]),
            Err(CheckpointError::InvalidEvidence)
        );
        assert_eq!(
            receiver.receive(2, &frames[0]).unwrap(),
            Some(bytes.clone())
        );
        assert!(receiver.receive(1, &frames[0]).unwrap().is_none());
        receiver.forget_peer(&1);
        assert!(receiver.pending.is_empty());
        assert!(receiver.receive(1, &frames[1]).unwrap().is_none());
        assert_eq!(receiver.receive(1, &frames[0]).unwrap(), Some(bytes));
    }
    #[test]
    fn rejects_sizes_versions_and_noncanonical_offsets_before_retention() {
        let bytes = vec![1; CHUNK + 1];
        let frames = frame_slow_payload(&bytes, bytes.len()).unwrap();
        let mut receiver = SlowFrameAssembler::new(bytes.len()).unwrap();
        let mut bad = frames[0].clone();
        bad[7] = 2;
        assert!(receiver.receive(1, &bad).is_err());
        bad = frames[0].clone();
        bad[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(receiver.receive(1, &bad).is_err());
        bad = frames[0].clone();
        bad[44..48].copy_from_slice(&1u32.to_le_bytes());
        assert!(receiver.receive(1, &bad).is_err());
        for end in [0, 8, HEADER, HEADER + 1, frames[0].len() - 1] {
            assert!(receiver.receive(1, &frames[0][..end]).is_err());
        }
        bad = frames[0].clone();
        bad.push(0);
        assert!(receiver.receive(1, &bad).is_err());
        assert!(receiver.pending.is_empty());
        assert!(frame_slow_payload(&[], 1).is_err());
        assert!(frame_slow_payload(&bytes, bytes.len() - 1).is_err());
    }
}
