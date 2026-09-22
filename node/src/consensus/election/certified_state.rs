use std::collections::BTreeMap;

use rsnano_types::{
    Account, Blake2HashBuilder, BlockHash, ConsensusEpoch, PrivateKey, PublicKey, Signature,
};

/// RAI: what a validator has locally constructed for a block of one epoch.
/// The statuses are ordered: a block enters the certified tree notarized and
/// may later gain a finalization status, never the other way round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CertifiedStatus {
    /// A vote-notarization certificate: the block is complete
    Notarized,
    /// A normal finalization certificate
    Finalized,
    /// A fast finalization certificate
    FastFinalized,
}

impl CertifiedStatus {
    pub fn as_byte(self) -> u8 {
        match self {
            CertifiedStatus::Notarized => 0,
            CertifiedStatus::Finalized => 1,
            CertifiedStatus::FastFinalized => 2,
        }
    }

    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(CertifiedStatus::Notarized),
            1 => Some(CertifiedStatus::Finalized),
            2 => Some(CertifiedStatus::FastFinalized),
            _ => None,
        }
    }

    pub fn is_finalized(self) -> bool {
        !matches!(self, CertifiedStatus::Notarized)
    }
}

/// A block of the certified tree: where it sits and which block it is
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CertifiedBlock {
    pub account: Account,
    pub height: u64,
    pub hash: BlockHash,
}

impl CertifiedBlock {
    pub fn new(account: Account, height: u64, hash: BlockHash) -> Self {
        Self {
            account,
            height,
            hash,
        }
    }
}

/// RAI: what a validator constructed for a block, and the parent the block
/// names. "The inventory includes the required account ancestry": two blocks
/// at one account slot are told apart by the branch each continues, and the
/// epoch derivation places every candidate by its parent. Carrying it here
/// makes `BuildState` a function of the selected reports alone, so two
/// validators that reconstructed the same reports derive the same state
/// whether or not each holds every body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Certification {
    pub status: CertifiedStatus,
    /// The parent the block names; zero when it opens the account
    pub previous: BlockHash,
}

/// RAI, "Certified-state reports and reconciliation": the canonical certified
/// block tree of one validator for one epoch. It holds every complete
/// notarized block the validator knows, with the finalization status it has
/// been able to construct for it, and nothing else: no votes, no bodies.
///
/// The root is an order-independent hash of the entries, so two validators
/// that have constructed the same certificates hold the same root whatever
/// order the votes reached them in. A report signs the root of a frozen
/// snapshot; the live tree goes on growing as gossip delivers more votes,
/// which is what lets a later common state bridge to a historical root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CertifiedState {
    entries: BTreeMap<CertifiedBlock, Certification>,
    /// The XOR of the entry digests, so that a status upgrade or an added
    /// block is a constant-time update of the root
    digest: [u8; 32],
}

impl CertifiedState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn status(&self, block: &CertifiedBlock) -> Option<CertifiedStatus> {
        self.entries.get(block).map(|held| held.status)
    }

    /// The status of a block and the parent it names
    pub fn certification(&self, block: &CertifiedBlock) -> Option<Certification> {
        self.entries.get(block).copied()
    }

    pub fn entries(&self) -> impl DoubleEndedIterator<Item = (&CertifiedBlock, &Certification)> {
        self.entries.iter()
    }

    /// Records a certified block, or upgrades the status of one already
    /// there. A status never weakens: a validator that has constructed a
    /// finalization certificate does not lose it. The parent is fixed by the
    /// block body, so the first one recorded stands.
    pub fn certify(
        &mut self,
        block: CertifiedBlock,
        previous: BlockHash,
        status: CertifiedStatus,
    ) -> bool {
        match self.entries.get(&block).copied() {
            Some(held) if held.status >= status => false,
            held => {
                let entry = Certification {
                    status,
                    previous: held.map(|held| held.previous).unwrap_or(previous),
                };
                if let Some(held) = held {
                    self.toggle(&block, held);
                }
                self.toggle(&block, entry);
                self.entries.insert(block, entry);
                true
            }
        }
    }

    /// The root the report signs
    pub fn root(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI certified state")
            .update(self.digest)
            .update(self.entries.len().to_le_bytes())
            .build()
    }

    /// RAI: "It may add or remove blocks and change status annotations."
    /// The canonical edits that take this state to the other one. A live
    /// certified state only ever grows, but two validators' states are not
    /// comparable in general - each has constructed certificates the other
    /// has not - so a difference between two states a responder knows has to
    /// be able to drop an entry as well as add one. Without that, two
    /// incomparable historical states have no bridge at all and every
    /// reconciliation between them falls back to a full transfer.
    pub fn difference(&self, target: &CertifiedState) -> CertifiedDelta {
        let mut delta = CertifiedDelta::default();
        for (block, entry) in &target.entries {
            if self.entries.get(block) != Some(entry) {
                delta.added.push((*block, *entry));
            }
        }
        for block in self.entries.keys() {
            if !target.entries.contains_key(block) {
                delta.removed.push(*block);
            }
        }
        delta
    }

    /// Applies a difference. The caller checks the resulting root against the
    /// signed one, which is the whole of the reconciliation check.
    pub fn apply(&mut self, delta: &CertifiedDelta) {
        for block in &delta.removed {
            self.remove(block);
        }
        for (block, entry) in &delta.added {
            self.set(*block, *entry);
        }
    }

    /// Records an entry exactly as given, weaker status included: a
    /// reconstruction rebuilds the reporter's state, which is not this
    /// node's own and is not required to grow
    pub fn set(&mut self, block: CertifiedBlock, entry: Certification) {
        if let Some(held) = self.entries.insert(block, entry) {
            self.toggle(&block, held);
        }
        self.toggle(&block, entry);
    }

    pub fn remove(&mut self, block: &CertifiedBlock) {
        if let Some(held) = self.entries.remove(block) {
            self.toggle(block, held);
        }
    }

    /// RAI: the digest one entry contributes to the state root
    fn entry_digest(block: &CertifiedBlock, entry: &Certification) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI certified entry")
            .update(block.account.as_bytes())
            .update(block.height.to_le_bytes())
            .update(block.hash.as_bytes())
            .update(entry.previous.as_bytes())
            .update([entry.status.as_byte()])
            .build()
    }

    fn toggle(&mut self, block: &CertifiedBlock, entry: Certification) {
        let digest = Self::entry_digest(block, &entry);
        for (d, e) in self.digest.iter_mut().zip(digest.as_bytes()) {
            *d ^= e;
        }
    }
}

/// RAI: the reconstructive difference between two certified states
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CertifiedDelta {
    /// Entries the target holds and the source does not, or holds otherwise
    pub added: Vec<(CertifiedBlock, Certification)>,
    /// Entries the source holds and the target does not
    pub removed: Vec<CertifiedBlock>,
}

impl CertifiedDelta {
    pub fn len(&self) -> usize {
        self.added.len() + self.removed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// RAI: which of its own votes a reporter records for a block whose
/// certified status does not yet summarize them
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResidualKind {
    /// A first vote, for a block with no notarization certificate in the
    /// certified state. Kept apart from `Notar` because only first votes
    /// build a fast finalization certificate, so only they can stand for a
    /// hidden one in the checkpoint recovery rule.
    First,
    /// Notarization support added under the second-look rule, for a block
    /// with no notarization certificate in the certified state
    Notar,
    /// A final vote for a block the certified state holds as notarized only
    Final,
}

impl ResidualKind {
    /// The encoding a fetch of the object uses
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(ResidualKind::First),
            1 => Some(ResidualKind::Notar),
            2 => Some(ResidualKind::Final),
            _ => None,
        }
    }

    pub fn as_byte(self) -> u8 {
        match self {
            ResidualKind::First => 0,
            ResidualKind::Notar => 1,
            ResidualKind::Final => 2,
        }
    }
}

/// RAI: the reporter-local vote evidence an epoch's certified state does not
/// yet reflect. It is what makes a block report-visible when no certificate
/// for it could be constructed before the report was signed (Lemma 3.7), and
/// it holds this validator's own votes only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResidualVotes {
    entries: BTreeMap<(CertifiedBlock, ResidualKind), ResidualRecord>,
    digest: [u8; 32],
}

/// RAI: one residual vote: the parent the voted block names, and the
/// reporter's signature over the record. "All claimed statuses are
/// independently verified": a residual vote is the reporter's own claim
/// that it cast the vote, and the claim is signed, so a fetched object
/// carries nothing a Byzantine replica could have put in the reporter's
/// name. A draft this node builds for itself is unsigned until its
/// representative's key signs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidualRecord {
    pub previous: BlockHash,
    pub signature: Option<Signature>,
}

impl ResidualVotes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, block: &CertifiedBlock, kind: ResidualKind) -> bool {
        self.entries.contains_key(&(*block, kind))
    }

    /// The blocks this reporter supported with a first or notarization vote
    /// that its certified state does not summarize: `M_Q` counts these
    /// The blocks this reporter supported with a first or a notarization
    /// vote: what `M_Q` counts for candidate membership
    pub fn supported(&self) -> impl Iterator<Item = &CertifiedBlock> {
        self.entries
            .keys()
            .filter(|(_, kind)| matches!(kind, ResidualKind::First | ResidualKind::Notar))
            .map(|(block, _)| block)
    }

    /// The blocks this reporter first voted: what `FirstCount_Q` counts, and
    /// the only votes a hidden fast finalization certificate can rest on
    pub fn first_votes(&self) -> impl Iterator<Item = &CertifiedBlock> {
        self.entries
            .keys()
            .filter(|(_, kind)| *kind == ResidualKind::First)
            .map(|(block, _)| block)
    }

    /// The votes recorded, in canonical order, each with the parent its
    /// block names and its signature: what a fetch of the object carries,
    /// and what its root commits to
    pub fn entries(
        &self,
    ) -> impl DoubleEndedIterator<Item = (CertifiedBlock, ResidualKind, &ResidualRecord)> + '_ {
        self.entries
            .iter()
            .map(|((block, kind), record)| (*block, *kind, record))
    }

    /// The parent a recorded block names, whichever vote recorded it
    pub fn previous(&self, block: &CertifiedBlock) -> Option<BlockHash> {
        self.entries
            .iter()
            .find(|((held, _), _)| held == block)
            .map(|(_, record)| record.previous)
    }

    /// Records a vote of this node's own, unsigned as yet
    pub fn record(
        &mut self,
        block: CertifiedBlock,
        previous: BlockHash,
        kind: ResidualKind,
    ) -> bool {
        self.record_with(
            block,
            kind,
            ResidualRecord {
                previous,
                signature: None,
            },
        )
    }

    /// Records a vote as fetched, with the signature the reporter put on it
    pub fn record_signed(
        &mut self,
        block: CertifiedBlock,
        previous: BlockHash,
        kind: ResidualKind,
        signature: Signature,
    ) -> bool {
        self.record_with(
            block,
            kind,
            ResidualRecord {
                previous,
                signature: Some(signature),
            },
        )
    }

    fn record_with(
        &mut self,
        block: CertifiedBlock,
        kind: ResidualKind,
        record: ResidualRecord,
    ) -> bool {
        if self.entries.contains_key(&(block, kind)) {
            return false;
        }
        let mut entry = Blake2HashBuilder::new()
            .update(b"RAI residual vote")
            .update(block.account.as_bytes())
            .update(block.height.to_le_bytes())
            .update(block.hash.as_bytes())
            .update(record.previous.as_bytes())
            .update([kind.as_byte()]);
        if let Some(signature) = &record.signature {
            entry = entry.update(signature.as_bytes());
        }
        let entry = entry.build();
        for (d, e) in self.digest.iter_mut().zip(entry.as_bytes()) {
            *d ^= e;
        }
        self.entries.insert((block, kind), record);
        true
    }

    /// What a reporter signs for one residual vote: the domain and the
    /// vote, so that a record can not be moved to another epoch
    pub fn vote_payload(
        epoch: ConsensusEpoch,
        block: &CertifiedBlock,
        previous: BlockHash,
        kind: ResidualKind,
    ) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI residual vote signed")
            .update(epoch.as_u64().to_le_bytes())
            .update(block.account.as_bytes())
            .update(block.height.to_le_bytes())
            .update(block.hash.as_bytes())
            .update(previous.as_bytes())
            .update([kind.as_byte()])
            .build()
    }

    /// The object as one representative reports it: every record signed by
    /// that key. The root commits to the signatures, so two reporters that
    /// recorded the same votes still sign different roots.
    pub fn signed(&self, epoch: ConsensusEpoch, key: &PrivateKey) -> ResidualVotes {
        let mut signed = ResidualVotes::new();
        for (block, kind, record) in self.entries() {
            let payload = Self::vote_payload(epoch, &block, record.previous, kind);
            signed.record_signed(block, record.previous, kind, key.sign(payload.as_bytes()));
        }
        signed
    }

    /// Whether a fetched record carries the reporter's signature over it
    pub fn verify_record(
        epoch: ConsensusEpoch,
        reporter: &PublicKey,
        block: &CertifiedBlock,
        previous: BlockHash,
        kind: ResidualKind,
        signature: &Signature,
    ) -> bool {
        let payload = Self::vote_payload(epoch, block, previous, kind);
        reporter.verify(payload.as_bytes(), signature).is_ok()
    }

    pub fn root(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI residual votes")
            .update(self.digest)
            .update(self.entries.len().to_le_bytes())
            .build()
    }
}

/// RAI: what a report commits to. The signed message binds the epoch, the
/// old committee, the reporter and the two roots; the contents behind them
/// are reconstructed separately and checked against the roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportCommitment {
    pub epoch: ConsensusEpoch,
    /// The digest of the old committee, which issued the epoch's votes
    pub committee: BlockHash,
    /// d_{e-1}: the hash of the closed predecessor checkpoint the report is
    /// signed against
    pub predecessor: BlockHash,
    /// r_i, the certified-state root
    pub certified: BlockHash,
    /// g_i, the residual-vote root
    pub residual: BlockHash,
    pub reporter: PublicKey,
}

impl ReportCommitment {
    /// What the reporter signs
    pub fn payload(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI report")
            .update(self.epoch.as_u64().to_le_bytes())
            .update(self.committee.as_bytes())
            .update(self.predecessor.as_bytes())
            .update(self.certified.as_bytes())
            .update(self.residual.as_bytes())
            .update(self.reporter.as_bytes())
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_state_has_a_root_of_its_own() {
        let state = CertifiedState::new();
        assert!(state.is_empty());
        assert_eq!(state.root(), CertifiedState::new().root());
        assert_ne!(state.root(), BlockHash::ZERO);
    }

    /// Two validators that constructed the same certificates hold the same
    /// root, whatever order the votes reached them in
    #[test]
    fn the_root_is_the_entries_and_not_their_order() {
        let mut one = CertifiedState::new();
        let mut other = CertifiedState::new();
        for i in 0..20 {
            one.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        for i in (0..20).rev() {
            other.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        assert_eq!(one.root(), other.root());

        // A status upgrade changes the root
        other.certify(block(3), parent(block(3)), CertifiedStatus::Finalized);
        assert_ne!(one.root(), other.root());
        one.certify(block(3), parent(block(3)), CertifiedStatus::Finalized);
        assert_eq!(one.root(), other.root());
    }

    /// A status never weakens: the certificate stays constructed
    #[test]
    fn a_status_is_upgraded_and_never_weakened() {
        let mut state = CertifiedState::new();
        assert!(state.certify(block(1), parent(block(1)), CertifiedStatus::Notarized));
        assert!(state.certify(block(1), parent(block(1)), CertifiedStatus::Finalized));
        assert_eq!(state.status(&block(1)), Some(CertifiedStatus::Finalized));
        // Back to notarized changes nothing
        assert!(!state.certify(block(1), parent(block(1)), CertifiedStatus::Notarized));
        assert_eq!(state.status(&block(1)), Some(CertifiedStatus::Finalized));
        assert_eq!(state.len(), 1);
    }

    /// The reconciliation check: apply the difference to the source state and
    /// the root has to come out as the signed one
    #[test]
    fn a_difference_reconstructs_the_target_root() {
        let mut source = CertifiedState::new();
        for i in 0..20 {
            source.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        let mut target = source.clone();
        for i in 20..25 {
            target.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        target.certify(block(2), parent(block(2)), CertifiedStatus::FastFinalized);

        let delta = source.difference(&target);
        assert_eq!(delta.added.len(), 6);
        assert!(delta.removed.is_empty());
        source.apply(&delta);
        assert_eq!(source.root(), target.root());
        assert_eq!(source.len(), target.len());
    }

    /// RAI: "It may add or remove blocks and change status annotations."
    /// Two validators' historical states are not comparable in general, so a
    /// difference has to drop what the target does not hold. Without that
    /// there is no bridge between them at all.
    #[test]
    fn a_difference_between_incomparable_states_removes_and_adds() {
        let mut source = CertifiedState::new();
        source.certify(block(1), parent(block(1)), CertifiedStatus::Notarized);
        source.certify(block(99), parent(block(99)), CertifiedStatus::Notarized);
        let mut target = CertifiedState::new();
        target.certify(block(1), parent(block(1)), CertifiedStatus::Finalized);
        target.certify(block(7), parent(block(7)), CertifiedStatus::Notarized);

        let delta = source.difference(&target);
        assert_eq!(delta.removed, vec![block(99)]);
        assert_eq!(delta.added.len(), 2);
        source.apply(&delta);
        assert_eq!(source.root(), target.root());

        // And back the other way, which a state that only grows could not do
        let mut ahead = CertifiedState::new();
        ahead.certify(block(1), parent(block(1)), CertifiedStatus::Finalized);
        let mut behind = CertifiedState::new();
        behind.certify(block(1), parent(block(1)), CertifiedStatus::Notarized);
        let delta = ahead.difference(&behind);
        ahead.apply(&delta);
        assert_eq!(ahead.root(), behind.root());
    }

    /// The parent a block names is part of what a report commits to: two
    /// validators that place one block on different branches hold different
    /// roots, so the derivation can not be fed two answers for one block
    #[test]
    fn the_root_commits_to_the_branch_a_block_continues() {
        let mut one = CertifiedState::new();
        one.certify(block(1), BlockHash::from(50), CertifiedStatus::Notarized);
        let mut other = CertifiedState::new();
        other.certify(block(1), BlockHash::from(51), CertifiedStatus::Notarized);
        assert_ne!(one.root(), other.root());
        assert_eq!(
            one.certification(&block(1)).unwrap().previous,
            BlockHash::from(50)
        );
    }

    /// A live state that has grown past the report keeps a bridge to it:
    /// the report's root is reconstructed from the difference
    #[test]
    fn a_grown_state_still_bridges_to_the_reported_one() {
        let mut reported = CertifiedState::new();
        for i in 0..10 {
            reported.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        let root = reported.root();
        let mut live = reported.clone();
        for i in 10..15 {
            live.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        // The live state is ahead, so it bridges the other way: a requester
        // whose own state is the smaller one reconstructs the report
        let mut requester = CertifiedState::new();
        for i in 0..8 {
            requester.certify(block(i), parent(block(i)), CertifiedStatus::Notarized);
        }
        let delta = requester.difference(&reported);
        requester.apply(&delta);
        assert_eq!(requester.root(), root);
    }

    #[test]
    fn residual_votes_hash_their_entries() {
        let mut one = ResidualVotes::new();
        let mut other = ResidualVotes::new();
        assert_eq!(one.root(), other.root());
        one.record(block(1), parent(block(1)), ResidualKind::First);
        one.record(block(2), parent(block(2)), ResidualKind::Final);
        other.record(block(2), parent(block(2)), ResidualKind::Final);
        other.record(block(1), parent(block(1)), ResidualKind::First);
        assert_eq!(one.root(), other.root());
        assert_eq!(one.len(), 2);
        assert!(one.contains(&block(1), ResidualKind::First));
        assert!(!one.contains(&block(1), ResidualKind::Final));
        assert_eq!(one.supported().collect::<Vec<_>>(), vec![&block(1)]);

        // Recording twice changes nothing
        assert!(!one.record(block(1), parent(block(1)), ResidualKind::First));
        assert_eq!(one.len(), 2);
    }

    /// A first vote and second-look notarization support are separate
    /// entries: only the first vote can witness a hidden fast certificate
    #[test]
    fn a_first_vote_is_told_apart_from_notarization_support() {
        let mut votes = ResidualVotes::new();
        votes.record(block(1), parent(block(1)), ResidualKind::First);
        votes.record(block(2), parent(block(2)), ResidualKind::Notar);
        assert_eq!(votes.len(), 2);

        // Both count as support for candidate membership
        assert_eq!(
            votes.supported().collect::<Vec<_>>(),
            vec![&block(1), &block(2)]
        );
        // Only the first vote counts for the recovery threshold
        assert_eq!(votes.first_votes().collect::<Vec<_>>(), vec![&block(1)]);
    }

    /// The same block first voted and then notarized is two entries, and the
    /// root distinguishes them from either alone
    #[test]
    fn the_two_support_kinds_hash_apart() {
        let mut first_only = ResidualVotes::new();
        first_only.record(block(1), parent(block(1)), ResidualKind::First);
        let mut notar_only = ResidualVotes::new();
        notar_only.record(block(1), parent(block(1)), ResidualKind::Notar);
        assert_ne!(first_only.root(), notar_only.root());

        let mut both = ResidualVotes::new();
        both.record(block(1), parent(block(1)), ResidualKind::First);
        both.record(block(1), parent(block(1)), ResidualKind::Notar);
        assert_eq!(both.len(), 2);
        assert_ne!(both.root(), first_only.root());
        assert_eq!(both.first_votes().collect::<Vec<_>>(), vec![&block(1)]);
    }

    /// A residual record is the reporter's signed claim: signing gives a
    /// root of the reporter's own, and a record verifies against its key
    /// in its epoch only
    #[test]
    fn a_residual_object_is_signed_per_reporter() {
        let epoch = ConsensusEpoch::new(2);
        let mut draft = ResidualVotes::new();
        draft.record(block(1), parent(block(1)), ResidualKind::First);
        draft.record(block(2), parent(block(2)), ResidualKind::Final);
        let one = draft.signed(epoch, &PrivateKey::from(1));
        let other = draft.signed(epoch, &PrivateKey::from(2));
        assert_ne!(one.root(), other.root());
        assert_ne!(one.root(), draft.root());
        assert_eq!(one.root(), draft.signed(epoch, &PrivateKey::from(1)).root());
        assert_eq!(one.len(), 2);
        for (block, kind, record) in one.entries() {
            let signature = record.signature.as_ref().unwrap();
            assert!(ResidualVotes::verify_record(
                epoch,
                &PrivateKey::from(1).public_key(),
                &block,
                record.previous,
                kind,
                signature
            ));
            assert!(!ResidualVotes::verify_record(
                epoch,
                &PrivateKey::from(2).public_key(),
                &block,
                record.previous,
                kind,
                signature
            ));
            assert!(!ResidualVotes::verify_record(
                ConsensusEpoch::new(3),
                &PrivateKey::from(1).public_key(),
                &block,
                record.previous,
                kind,
                signature
            ));
        }
    }

    #[test]
    fn the_signed_payload_covers_both_roots() {
        let commitment = ReportCommitment {
            epoch: ConsensusEpoch::new(3),
            committee: BlockHash::from(7),
            predecessor: BlockHash::from(6),
            certified: BlockHash::from(8),
            residual: BlockHash::from(9),
            reporter: PublicKey::from(1),
        };
        for changed in [
            ReportCommitment {
                epoch: ConsensusEpoch::new(4),
                ..commitment.clone()
            },
            ReportCommitment {
                predecessor: BlockHash::from(60),
                ..commitment.clone()
            },
            ReportCommitment {
                committee: BlockHash::from(70),
                ..commitment.clone()
            },
            ReportCommitment {
                certified: BlockHash::from(80),
                ..commitment.clone()
            },
            ReportCommitment {
                residual: BlockHash::from(90),
                ..commitment.clone()
            },
            ReportCommitment {
                reporter: PublicKey::from(2),
                ..commitment.clone()
            },
        ] {
            assert_ne!(commitment.payload(), changed.payload());
        }
    }

    /*
     * Test helpers
     */

    fn block(i: u64) -> CertifiedBlock {
        CertifiedBlock::new(Account::from(i), 1 + i % 4, BlockHash::from(i * 7 + 1))
    }

    /// The parent a test block names, distinct per block
    fn parent(block: CertifiedBlock) -> BlockHash {
        BlockHash::from(block.height * 100_000 + block.account.as_bytes()[31] as u64 + 3)
    }
}
