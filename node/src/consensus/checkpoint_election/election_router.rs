//! Shared multi-epoch election ownership. Contexts are registered by the node,
//! never created from untrusted frame headers. Retain decided epochs to serve proofs.
use super::*;
use crate::consensus::{
    election::EpochValue,
    reports::{CheckpointDecision, CheckpointElection},
};
use rsnano_messages::CheckpointFrame;
use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub struct CheckpointElectionRouter<P: Ord, V> {
    adapters: BTreeMap<ConsensusEpoch, CheckpointElectionAdapter<P, V>>,
    routes: BTreeMap<BlockHash, ConsensusEpoch>,
    assembler: SlowFrameAssembler<P>,
}
impl<P: Ord + Clone + Send, V> CheckpointElectionRouter<P, V>
where
    V: FnMut(CheckpointCandidate) -> Result<ValidatedCheckpointCandidate, CheckpointError> + Send,
{
    pub fn new() -> Self {
        Self {
            adapters: BTreeMap::new(),
            routes: BTreeMap::new(),
            assembler: SlowFrameAssembler::new(
                SlowIoLimits::default().max_message_bytes + FastWireCodec::HEADER_BYTES,
            )
            .unwrap(),
        }
    }
    pub fn contains_epoch(&self, epoch: ConsensusEpoch) -> bool {
        self.adapters.contains_key(&epoch)
    }
    pub fn register(
        &mut self,
        context: CheckpointContext,
        key: PrivateKey,
        validate: V,
        proof_bytes: usize,
    ) -> Result<(), CheckpointError> {
        let epoch = context.instance().epoch;
        // Never replace an existing signer/session or discard its signing locks.
        if self.adapters.contains_key(&epoch) {
            return Err(CheckpointError::InvalidEvidence);
        }
        let fast = context.instance().digest();
        let slow = SlowContext::new(&context).instance();
        if self.routes.contains_key(&fast) || self.routes.contains_key(&slow) {
            return Err(CheckpointError::InvalidEvidence);
        }
        let adapter = CheckpointElectionAdapter::new(
            context,
            key,
            CheckpointSessionJournals::default(),
            validate,
            proof_bytes,
        )?;
        self.adapters.insert(epoch, adapter);
        self.routes.insert(fast, epoch);
        self.routes.insert(slow, epoch);
        Ok(())
    }
    pub fn receive(&mut self, peer: P, frame: &CheckpointFrame) -> Result<(), CheckpointError> {
        let Some(bytes) = self.assembler.receive(peer.clone(), frame.as_bytes())? else {
            return Ok(());
        };
        if bytes.len() < FastWireCodec::HEADER_BYTES {
            return Err(CheckpointError::InvalidSize);
        }
        let id = BlockHash::from_bytes(bytes[8..40].try_into().unwrap());
        let epoch = self.routes.get(&id).ok_or(CheckpointError::WrongInstance)?;
        self.adapters
            .get_mut(epoch)
            .unwrap()
            .receive_payload(peer, &bytes)
    }
    pub fn encode(
        &self,
        epoch: ConsensusEpoch,
        message: &CheckpointMessage,
    ) -> Result<Vec<CheckpointFrame>, CheckpointError> {
        self.adapters
            .get(&epoch)
            .ok_or(CheckpointError::WrongInstance)?
            .encode(message)
    }
    pub fn drain_effects(&mut self) -> Vec<(ConsensusEpoch, DriverEffect<P>)> {
        self.adapters
            .iter_mut()
            .flat_map(|(epoch, adapter)| adapter.drain_effects().map(|e| (*epoch, e)))
            .collect()
    }
    pub fn retry(&mut self) {
        for adapter in self.adapters.values_mut() {
            adapter.retry();
        }
    }
    pub fn forget_connection(&mut self, peer: &P) {
        self.assembler.forget_peer(peer);
        for adapter in self.adapters.values_mut() {
            adapter.forget_connection(peer);
        }
    }
}
impl<P: Ord + Clone + Send, V> CheckpointElection for CheckpointElectionRouter<P, V>
where
    V: FnMut(CheckpointCandidate) -> Result<ValidatedCheckpointCandidate, CheckpointError> + Send,
{
    fn evidence_updated(&mut self) {
        for a in self.adapters.values_mut() {
            a.evidence_updated();
        }
    }
    fn candidate(&mut self, value: EpochValue) {
        if let Some(a) = self.adapters.get_mut(&value.epoch) {
            a.candidate(value);
        }
    }
    fn decided(&mut self) -> Option<CheckpointDecision> {
        self.adapters.values_mut().find_map(|a| a.decided())
    }
    fn verify(&self, decision: &CheckpointDecision) -> bool {
        self.adapters
            .get(&decision.value.epoch)
            .is_some_and(|a| a.verify(decision))
    }
}
// The same owner can be retained by the inbound network handler and installed
// behind EpochDecisionService's boxed election interface.
impl<P: Ord + Clone + Send, V> CheckpointElection for Arc<Mutex<CheckpointElectionRouter<P, V>>>
where
    V: FnMut(CheckpointCandidate) -> Result<ValidatedCheckpointCandidate, CheckpointError> + Send,
{
    fn evidence_updated(&mut self) {
        self.lock().unwrap().evidence_updated();
    }
    fn candidate(&mut self, value: EpochValue) {
        self.lock().unwrap().candidate(value);
    }
    fn decided(&mut self) -> Option<CheckpointDecision> {
        self.lock().unwrap().decided()
    }
    fn verify(&self, decision: &CheckpointDecision) -> bool {
        self.lock().unwrap().verify(decision)
    }
}
