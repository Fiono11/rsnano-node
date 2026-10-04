//! Per-checkpoint transport adapter. Own one per active instance; recreate it on
//! instance replacement and forget connection state on disconnect. Decoded
//! messages remain untrusted until the fast/slow consensus verifier accepts them.
use super::*;
use rsnano_messages::CheckpointFrame;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointMessage {
    Fast(FastMessage),
    Application(ApplicationMessage),
    Slow(SlowMessage),
}

pub struct CheckpointTransport<P: Ord> {
    fast: FastWireCodec,
    application: ApplicationWireCodec,
    slow: SlowWireCodec,
    assembler: SlowFrameAssembler<P>,
    max_payload_bytes: usize,
}
impl<P: Ord + Clone> CheckpointTransport<P> {
    pub fn new(
        context: &CheckpointContext,
        max_body_bytes: usize,
    ) -> Result<Self, CheckpointError> {
        let max_payload_bytes = max_body_bytes
            .checked_add(FastWireCodec::HEADER_BYTES)
            .ok_or(CheckpointError::InvalidSize)?;
        Ok(Self {
            fast: FastWireCodec::new(context.instance(), max_body_bytes)?,
            application: ApplicationWireCodec::new(context.instance(), max_body_bytes)?,
            slow: SlowWireCodec::new(SlowContext::new(context).instance(), max_body_bytes)?,
            assembler: SlowFrameAssembler::new(max_payload_bytes)?,
            max_payload_bytes,
        })
    }

    pub fn encode(
        &self,
        message: &CheckpointMessage,
    ) -> Result<Vec<CheckpointFrame>, CheckpointError> {
        let bytes = match message {
            CheckpointMessage::Fast(message) => self.fast.encode(message)?,
            CheckpointMessage::Application(message) => self.application.encode(message)?,
            CheckpointMessage::Slow(message) => self.slow.encode(message)?,
        };
        frame_slow_payload(&bytes, self.max_payload_bytes)?
            .into_iter()
            .map(|bytes| CheckpointFrame::new(bytes).map_err(|_| CheckpointError::InvalidEvidence))
            .collect()
    }

    pub fn receive(
        &mut self,
        connection: P,
        frame: &CheckpointFrame,
    ) -> Result<Option<CheckpointMessage>, CheckpointError> {
        let Some(bytes) = self.assembler.receive(connection, frame.as_bytes())? else {
            return Ok(None);
        };
        self.decode_payload(&bytes).map(Some)
    }

    pub(super) fn decode_payload(
        &self,
        bytes: &[u8],
    ) -> Result<CheckpointMessage, CheckpointError> {
        // Select by the explicit namespace, never by trying multiple parsers.
        // Each decoder also checks the active instance and nested message fields.
        if bytes.starts_with(FastWireCodec::MAGIC) {
            Ok(CheckpointMessage::Fast(self.fast.decode(bytes)?))
        } else if bytes.starts_with(ApplicationWireCodec::MAGIC) {
            Ok(CheckpointMessage::Application(
                self.application.decode(bytes)?,
            ))
        } else if bytes.starts_with(SlowWireCodec::MAGIC) {
            Ok(CheckpointMessage::Slow(self.slow.decode(bytes)?))
        } else {
            Err(CheckpointError::InvalidEvidence)
        }
    }

    pub fn forget_connection(&mut self, connection: &P) {
        self.assembler.forget_peer(connection);
    }
}
