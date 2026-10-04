//! Opt-in, all-online benchmark wiring. Not a round synchronizer.
use super::*;
use crate::{
    consensus::{
        election::EpochValue,
        reports::{CheckpointDecision, CheckpointElection, EpochDecisionService},
    },
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};
use rsnano_messages::{CheckpointFrame, Message};
use rsnano_network::{ChannelId, TrafficType};
use rsnano_types::BlockHash;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

type Validator = Box<
    dyn FnMut(CheckpointCandidate) -> Result<ValidatedCheckpointCandidate, CheckpointError> + Send,
>;
struct State {
    router: CheckpointElectionRouter<ChannelId, Validator>,
    last_retry: Instant,
    identities: std::collections::BTreeMap<rsnano_types::ConsensusEpoch, rsnano_types::PublicKey>,
}
pub(crate) struct BenchmarkCheckpointService {
    state: Mutex<State>,
    decisions: Weak<EpochDecisionService>,
    reps: Arc<Mutex<WalletRepresentatives>>,
    flooder: Mutex<MessageFlooder>,
    session: BlockHash,
}
impl BenchmarkCheckpointService {
    pub fn new(
        decisions: &Arc<EpochDecisionService>,
        reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        session: BlockHash,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                router: CheckpointElectionRouter::new(),
                last_retry: Instant::now(),
                identities: Default::default(),
            }),
            decisions: Arc::downgrade(decisions),
            reps,
            flooder: Mutex::new(flooder),
            session,
        })
    }
    fn prepare(&self, state: &mut State) {
        let Some(decisions) = self.decisions.upgrade() else {
            return;
        };
        for context in decisions.checkpoint_contexts(self.session) {
            if state.router.contains_epoch(context.instance().epoch) {
                continue;
            }
            let mut keys = Vec::new();
            self.reps.lock().unwrap().rep_priv_keys(&mut keys);
            keys.sort_by_key(|k| k.public_key());
            let Some(key) = keys
                .into_iter()
                .find(|k| !context.committee.weight(&k.public_key()).is_zero())
            else {
                continue;
            };
            let weak = self.decisions.clone();
            let session = self.session;
            let validate: Validator = Box::new(move |candidate| {
                weak.upgrade()
                    .ok_or(CheckpointError::InvalidEvidence)?
                    .validate_candidate(session, candidate)
            });
            let epoch = context.instance().epoch;
            let identity = key.public_key();
            match state
                .router
                .register(context, key, validate, 16 * 1024 * 1024)
            {
                Ok(()) => {
                    state.identities.insert(epoch, identity);
                    crate::utils::diagnostic!(
                        "CHECKPOINT_BENCH_START epoch={} node={}",
                        epoch,
                        identity
                    );
                }
                Err(error) => crate::utils::diagnostic!(
                    "CHECKPOINT_BENCH_REJECT epoch={} reason={:?}",
                    epoch,
                    error
                ),
            }
        }
    }
    fn flush(&self, state: &mut State) {
        let mut flooder = self.flooder.lock().unwrap();
        for (epoch, effect) in state.router.drain_effects() {
            let (peer, message) = match effect {
                DriverEffect::Broadcast(message) => (None, message),
                DriverEffect::Reply(peer, message) => (Some(peer), message),
                DriverEffect::Decided(value) => {
                    crate::utils::diagnostic!(
                        "CHECKPOINT_BENCH_DECIDED epoch={} value={}",
                        epoch,
                        value
                    );
                    continue;
                }
                DriverEffect::Rejected(error) => {
                    crate::utils::diagnostic!(
                        "CHECKPOINT_BENCH_REJECT epoch={} reason={:?}",
                        epoch,
                        error
                    );
                    continue;
                }
            };
            match state.router.encode(epoch, &message) {
                Ok(frames) => {
                    for frame in frames {
                        let message = Message::CheckpointFrame(frame);
                        if let Some(peer) = peer {
                            flooder.try_send_channel_id(peer, &message, TrafficType::Generic);
                        } else {
                            flooder.flood_prs_and_some_non_prs(&message, TrafficType::Generic, 1.0);
                        }
                    }
                }
                Err(error) => crate::utils::diagnostic!(
                    "CHECKPOINT_BENCH_ENCODE epoch={} reason={:?}",
                    epoch,
                    error
                ),
            }
        }
    }
    pub fn receive(&self, peer: ChannelId, frame: &CheckpointFrame) {
        let mut state = self.state.lock().unwrap();
        self.prepare(&mut state);
        if let Err(error) = state.router.receive(peer, frame) {
            crate::utils::diagnostic!("CHECKPOINT_BENCH_RECEIVE reason={:?}", error);
        }
        self.flush(&mut state);
    }
}
impl CheckpointElection for Arc<BenchmarkCheckpointService> {
    fn evidence_updated(&mut self) {
        let mut state = self.state.lock().unwrap();
        self.prepare(&mut state);
        state.router.evidence_updated();
        if state.last_retry.elapsed() >= Duration::from_secs(1) {
            state.router.retry();
            state.last_retry = Instant::now();
        }
        self.flush(&mut state);
    }
    fn candidate(&mut self, value: EpochValue) {
        let mut state = self.state.lock().unwrap();
        state.router.candidate(value);
        self.flush(&mut state);
    }
    fn decided(&mut self) -> Option<CheckpointDecision> {
        let mut state = self.state.lock().unwrap();
        let decision = state.router.decided()?;
        let path = match CheckpointDecisionProof::decode(&decision.proof, 16 * 1024 * 1024) {
            Ok(CheckpointDecisionProof::Fast(_)) => "fast",
            Ok(CheckpointDecisionProof::Slow(_)) => "slow",
            Err(_) => "invalid",
        };
        crate::utils::diagnostic!(
            "CHECKPOINT_BENCH_PROOF epoch={} node={} value={} path={} bytes={}",
            decision.value.epoch,
            state.identities[&decision.value.epoch],
            decision.value.hash(),
            path,
            decision.proof.len()
        );
        Some(decision)
    }
    fn verify(&self, decision: &CheckpointDecision) -> bool {
        self.state.lock().unwrap().router.verify(decision)
    }
}

impl rsnano_utils::EventHandler<rsnano_network::ChannelEvent> for BenchmarkCheckpointService {
    fn handle(&self, event: &rsnano_network::ChannelEvent) {
        if let rsnano_network::ChannelEvent::Removed(peer) = event {
            self.state.lock().unwrap().router.forget_connection(peer);
        }
    }
}
