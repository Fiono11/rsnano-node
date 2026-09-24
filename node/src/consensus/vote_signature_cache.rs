use std::sync::Mutex;

use rsnano_types::{BlockHash, PublicKey, Signature, Vote};

use super::bounded_hash_map::BoundedHashMap;

/// Successful verification of an exact signed message. Evidence retries must
/// still reach elections, but identical bytes need not repeat public-key work.
/// The key includes the signer and signature as well as the signed payload hash.
pub(crate) struct VoteSignatureCache {
    verified: Mutex<BoundedHashMap<(PublicKey, Signature, BlockHash), ()>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SignatureValidation {
    Cached,
    Verified,
    Invalid,
}

impl VoteSignatureCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            verified: Mutex::new(BoundedHashMap::new(capacity)),
        }
    }

    pub fn validate(&self, vote: &Vote) -> SignatureValidation {
        let hash = vote.hash();
        let key = (vote.voter, vote.signature.clone(), hash);
        if self.verified.lock().unwrap().contains_key(&key) {
            return SignatureValidation::Cached;
        }
        // Do expensive verification outside the cache lock. Concurrent misses
        // may verify twice, but cannot accept a vote without a valid signature.
        if vote.voter.verify(hash.as_bytes(), &vote.signature).is_err() {
            return SignatureValidation::Invalid;
        }
        self.verified.lock().unwrap().insert(key, ());
        SignatureValidation::Verified
    }
}

#[cfg(test)]
mod tests {
    use rsnano_types::{ConsensusEpoch, PrivateKey, VoteKind};

    use super::*;

    #[test]
    fn cached_success_cannot_authenticate_altered_votes() {
        let cache = VoteSignatureCache::new(10);
        let key = PrivateKey::from(1);
        let vote = Vote::new_in_epoch(&key, VoteKind::First, ConsensusEpoch::ZERO, vec![1.into()]);
        assert_eq!(cache.validate(&vote), SignatureValidation::Verified);
        assert_eq!(cache.validate(&vote), SignatureValidation::Cached);

        let mut bad_signature = vote.clone();
        bad_signature.signature = Signature::new();
        assert_eq!(cache.validate(&bad_signature), SignatureValidation::Invalid);
        let mut bad_signer = vote.clone();
        bad_signer.voter = PrivateKey::from(2).public_key();
        assert_eq!(cache.validate(&bad_signer), SignatureValidation::Invalid);
        let mut bad_payload = vote.clone();
        bad_payload.hashes.push(2.into());
        assert_eq!(cache.validate(&bad_payload), SignatureValidation::Invalid);
        let mut bad_kind =
            Vote::new_in_epoch(&key, VoteKind::Final, vote.epoch, vote.hashes.clone());
        bad_kind.signature = vote.signature.clone();
        assert_eq!(cache.validate(&bad_kind), SignatureValidation::Invalid);
        if cfg!(feature = "rai_protocol") {
            let mut bad_epoch = vote.clone();
            bad_epoch.epoch = vote.epoch.next();
            assert_eq!(cache.validate(&bad_epoch), SignatureValidation::Invalid);
        }
        assert_eq!(cache.verified.lock().unwrap().len(), 1);
    }

    #[test]
    fn evicted_valid_votes_are_verified_again() {
        let cache = VoteSignatureCache::new(2);
        let key = PrivateKey::from(1);
        let votes: Vec<_> = (1..=3)
            .map(|i| {
                Vote::new_in_epoch(&key, VoteKind::First, ConsensusEpoch::ZERO, vec![i.into()])
            })
            .collect();
        for vote in &votes {
            assert_eq!(cache.validate(vote), SignatureValidation::Verified);
        }
        assert_eq!(cache.verified.lock().unwrap().len(), 2);
        assert_eq!(cache.validate(&votes[0]), SignatureValidation::Verified);
        assert_eq!(cache.verified.lock().unwrap().len(), 2);
    }
}
