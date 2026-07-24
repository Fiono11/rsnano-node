use std::collections::BTreeMap;

use ed25519_dalek::{Signature as DalekSignature, Signer, SigningKey, VerifyingKey};

use crate::types::{AccountId, Hash32, ReplicaId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Signature(pub [u8; 64]);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Ed25519PublicKey(pub [u8; 32]);

impl Ed25519PublicKey {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }

    fn verifying_key(self) -> Option<VerifyingKey> {
        VerifyingKey::from_bytes(&self.0)
            .ok()
            .filter(|key| !key.is_weak())
    }

    pub fn is_valid(self) -> bool {
        self.verifying_key().is_some()
    }
}

pub trait CryptoProvider {
    fn sign(&self, signer: ReplicaId, message: &[u8]) -> Option<Signature>;
    fn verify(&self, signer: ReplicaId, message: &[u8], signature: &Signature) -> bool;
}

/// Ed25519 replica keys used to sign votes, reports, and other replica-authored
/// protocol statements.
///
/// A store may contain public keys without their corresponding signing keys,
/// allowing verifier-only replicas and package validators. `deterministic` is a
/// reproducible test/simulation constructor; deployments should provision
/// independent random 32-byte seeds through `insert` instead.
#[derive(Clone, Debug, Default)]
pub struct Ed25519KeyStore {
    signing_keys: BTreeMap<ReplicaId, SigningKey>,
    public_keys: BTreeMap<ReplicaId, Ed25519PublicKey>,
}

impl Ed25519KeyStore {
    pub fn deterministic(replicas: impl IntoIterator<Item = ReplicaId>) -> Self {
        let mut store = Self::default();
        for replica in replicas {
            let mut seed = Vec::with_capacity(24);
            seed.extend_from_slice(b"rai-replica-ed25519-v1");
            seed.extend_from_slice(&replica.to_be_bytes());
            store.insert(replica, sha256(&seed).0);
        }
        store
    }

    pub fn insert(&mut self, replica: ReplicaId, seed: [u8; 32]) {
        let signing_key = SigningKey::from_bytes(&seed);
        let public_key = Ed25519PublicKey(signing_key.verifying_key().to_bytes());
        self.signing_keys.insert(replica, signing_key);
        self.public_keys.insert(replica, public_key);
    }

    pub fn insert_public_key(&mut self, replica: ReplicaId, public_key: Ed25519PublicKey) -> bool {
        if public_key.verifying_key().is_none() {
            return false;
        }
        self.signing_keys.remove(&replica);
        self.public_keys.insert(replica, public_key);
        true
    }

    pub fn contains(&self, replica: ReplicaId) -> bool {
        self.public_keys.contains_key(&replica)
    }

    pub fn can_sign(&self, replica: ReplicaId) -> bool {
        self.signing_keys.contains_key(&replica)
    }

    pub fn public_key(&self, replica: ReplicaId) -> Option<Ed25519PublicKey> {
        self.public_keys.get(&replica).copied()
    }

    pub fn verifier(&self) -> Self {
        Self {
            signing_keys: BTreeMap::new(),
            public_keys: self.public_keys.clone(),
        }
    }

    /// Returns a store with every replica public key and only `replica`'s
    /// private signing key. This models one replica process without granting it
    /// authority to impersonate its peers.
    pub fn signer_view(&self, replica: ReplicaId) -> Option<Self> {
        let signing_key = self.signing_keys.get(&replica)?.clone();
        Some(Self {
            signing_keys: [(replica, signing_key)].into_iter().collect(),
            public_keys: self.public_keys.clone(),
        })
    }
}

impl CryptoProvider for Ed25519KeyStore {
    fn sign(&self, signer: ReplicaId, message: &[u8]) -> Option<Signature> {
        let key = self.signing_keys.get(&signer)?;
        Some(Signature(key.sign(message).to_bytes()))
    }

    fn verify(&self, signer: ReplicaId, message: &[u8], signature: &Signature) -> bool {
        let Some(key) = self
            .public_keys
            .get(&signer)
            .copied()
            .and_then(Ed25519PublicKey::verifying_key)
        else {
            return false;
        };
        key.verify_strict(message, &DalekSignature::from_bytes(&signature.0))
            .is_ok()
    }
}

/// Backwards-compatible name for the now-real Ed25519 replica key store.
pub type DemoKeyStore = Ed25519KeyStore;

/// Client-side Ed25519 keys. Account keys are intentionally separate from
/// replica keys: delegating stake to a replica does not authorize that replica
/// to mutate the account chain.
#[derive(Clone, Debug, Default)]
pub struct AccountKeyStore {
    signing_keys: BTreeMap<AccountId, SigningKey>,
}

impl AccountKeyStore {
    /// Reproducible keys for tests and deterministic simulations.
    pub fn deterministic(accounts: impl IntoIterator<Item = AccountId>) -> Self {
        let mut store = Self::default();
        for account in accounts {
            let mut material = Vec::with_capacity(40);
            material.extend_from_slice(b"rai-account-ed25519-v1");
            material.extend_from_slice(&account.to_be_bytes());
            store.insert(account, sha256(&material).0);
        }
        store
    }

    /// Installs an externally generated 32-byte Ed25519 signing seed.
    pub fn insert(&mut self, account: AccountId, seed: [u8; 32]) {
        self.signing_keys
            .insert(account, SigningKey::from_bytes(&seed));
    }

    pub fn contains(&self, account: AccountId) -> bool {
        self.signing_keys.contains_key(&account)
    }

    pub fn public_key(&self, account: AccountId) -> Option<Ed25519PublicKey> {
        self.signing_keys
            .get(&account)
            .map(|key| Ed25519PublicKey(key.verifying_key().to_bytes()))
    }

    pub fn sign(&self, account: AccountId, message: &[u8]) -> Option<Signature> {
        self.signing_keys
            .get(&account)
            .map(|key| Signature(key.sign(message).to_bytes()))
    }
}

pub fn verify_ed25519(public_key: Ed25519PublicKey, message: &[u8], signature: &Signature) -> bool {
    let Some(key) = public_key.verifying_key() else {
        return false;
    };
    key.verify_strict(message, &DalekSignature::from_bytes(&signature.0))
        .is_ok()
}

pub fn sha256(input: &[u8]) -> Hash32 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (input.len() as u64) * 8;
    let mut data = input.to_vec();
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());

    let mut h = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];

    for chunk in data.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let j = i * 4;
            *word = u32::from_be_bytes([chunk[j], chunk[j + 1], chunk[j + 2], chunk[j + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(big_s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let big_s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = big_s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    Hash32(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn replica_ed25519_signatures_reject_wrong_keys_and_tampering() {
        let keys = Ed25519KeyStore::deterministic([1, 2]);
        let signature = keys.sign(1, b"protocol message").unwrap();
        assert!(keys.verify(1, b"protocol message", &signature));
        assert!(!keys.verify(2, b"protocol message", &signature));
        assert!(!keys.verify(1, b"modified message", &signature));

        let mut corrupted = signature;
        corrupted.0[17] ^= 0x80;
        assert!(!keys.verify(1, b"protocol message", &corrupted));
    }

    #[test]
    fn replica_signer_view_has_only_its_own_private_key() {
        let keys = Ed25519KeyStore::deterministic([1, 2]);
        let replica_one = keys.signer_view(1).unwrap();
        assert!(replica_one.sign(1, b"one").is_some());
        assert!(replica_one.sign(2, b"two").is_none());
        let signature_two = keys.sign(2, b"two").unwrap();
        assert!(replica_one.verify(2, b"two", &signature_two));
    }

    #[test]
    fn account_and_replica_key_domains_are_distinct() {
        let replica_keys = Ed25519KeyStore::deterministic([1]);
        let account_keys = AccountKeyStore::deterministic([1]);
        assert_ne!(
            replica_keys.public_key(1).unwrap(),
            account_keys.public_key(1).unwrap()
        );
    }
}
