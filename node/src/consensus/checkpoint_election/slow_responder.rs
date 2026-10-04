//! Mutable, origin-preserving responder state. Validation precedes mutation.
use super::*;
use rsnano_types::{BlockHash, PrivateKey, PublicKey};
use std::collections::BTreeMap;

pub struct SlowResponder {
    instance: BlockHash,
    signer: PublicKey,
    r: Option<SlowEntry>,
    a: BTreeMap<u64, (AState, BTreeMap<BlockHash, BlockHash>)>,
    b: BTreeMap<u64, (BState, BTreeMap<BValue, BlockHash>)>,
}
impl SlowResponder {
    pub fn new(context: &SlowContext, signer: PublicKey) -> Result<Self, CheckpointError> {
        if !context.members().contains(&signer) {
            return Err(CheckpointError::NonMember);
        }
        Ok(Self {
            instance: context.instance(),
            signer,
            r: None,
            a: BTreeMap::new(),
            b: BTreeMap::new(),
        })
    }

    /// Merge R state already checked by the proposer certificate verifier.
    pub(super) fn merge_verified_r(&mut self, entry: SlowEntry) {
        debug_assert!(matches!(entry.value, SlowValue::R { .. }));
        if self.r.as_ref().is_none_or(|old| entry.value > old.value) {
            self.r = Some(entry);
        }
    }

    pub fn deliver(
        &mut self,
        slow: &SlowContext,
        fast: &CheckpointContext,
        application: &impl CheckpointEvidence,
        store: &SlowProofStore,
        request: BlockHash,
        key: &PrivateKey,
        limits: VerificationLimits,
    ) -> Result<SlowResponse, CheckpointError> {
        if slow.instance() != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        if key.public_key() != self.signer {
            return Err(CheckpointError::NonMember);
        }
        store
            .verifier(slow, fast, application, limits)
            .verify_request(request)?;
        let object = store.request(request)?;
        let entries = match object.value {
            SlowValue::R { .. } => {
                self.merge_verified_r(SlowEntry {
                    value: object.value,
                    origin: request,
                });
                vec![self.r.clone().unwrap()]
            }
            SlowValue::A(value) => {
                let (state, origins) = self.a.entry(object.rank).or_default();
                origins.entry(value).or_insert(request);
                let values = state.deliver(value);
                origins.retain(|value, _| values.contains(value));
                values
                    .into_iter()
                    .map(|value| SlowEntry {
                        value: SlowValue::A(value),
                        origin: origins[&value],
                    })
                    .collect()
            }
            SlowValue::B(value) => {
                let (state, origins) = self.b.entry(object.rank).or_default();
                // BState rejects conflicting true values without changing state.
                let values = state.deliver(value)?;
                origins.entry(value).or_insert(request);
                origins.retain(|value, _| values.contains(value));
                values
                    .into_iter()
                    .map(|value| SlowEntry {
                        value: SlowValue::B(value),
                        origin: origins[&value],
                    })
                    .collect()
            }
        };
        SlowResponse::sign(slow, object.rank, object.phase(), request, entries, key)
    }
}
