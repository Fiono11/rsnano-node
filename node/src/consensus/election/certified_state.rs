use std::collections::BTreeMap;

use rsnano_types::{Account, Blake2HashBuilder, BlockHash, ConsensusEpoch, PublicKey};

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
    fn as_byte(self) -> u8 {
        match self {
            CertifiedStatus::Notarized => 0,
            CertifiedStatus::Finalized => 1,
            CertifiedStatus::FastFinalized => 2,
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
    entries: BTreeMap<CertifiedBlock, CertifiedStatus>,
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
        self.entries.get(block).copied()
    }

    pub fn entries(&self) -> impl Iterator<Item = (&CertifiedBlock, &CertifiedStatus)> {
        self.entries.iter()
    }

    /// Records a certified block, or upgrades the status of one already
    /// there. A status never weakens: a validator that has constructed a
    /// finalization certificate does not lose it.
    pub fn certify(&mut self, block: CertifiedBlock, status: CertifiedStatus) -> bool {
        match self.entries.get(&block).copied() {
            Some(held) if held >= status => false,
            held => {
                if let Some(held) = held {
                    self.toggle(&block, held);
                }
                self.toggle(&block, status);
                self.entries.insert(block, status);
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

    /// The additions and upgrades that take this state to the other one.
    /// A certified state only ever grows, so a difference that would have to
    /// remove an entry or weaken a status is not representable: the states
    /// are then unrelated and the caller has no bridge between them.
    pub fn difference(&self, target: &CertifiedState) -> Option<CertifiedDelta> {
        // The target has to hold everything this state holds, at least as
        // strongly; otherwise it does not descend from it and there is no
        // bridge between the two
        for (block, held) in &self.entries {
            match target.entries.get(block) {
                Some(status) if status >= held => {}
                _ => return None,
            }
        }
        let mut delta = CertifiedDelta::default();
        for (block, status) in &target.entries {
            if self.entries.get(block) != Some(status) {
                delta.entries.push((*block, *status));
            }
        }
        Some(delta)
    }

    /// Applies a difference. The caller checks the resulting root against the
    /// signed one, which is the whole of the reconciliation check.
    pub fn apply(&mut self, delta: &CertifiedDelta) {
        for (block, status) in &delta.entries {
            self.certify(*block, *status);
        }
    }

    fn toggle(&mut self, block: &CertifiedBlock, status: CertifiedStatus) {
        let entry = Blake2HashBuilder::new()
            .update(b"RAI certified entry")
            .update(block.account.as_bytes())
            .update(block.height.to_le_bytes())
            .update(block.hash.as_bytes())
            .update([status.as_byte()])
            .build();
        for (d, e) in self.digest.iter_mut().zip(entry.as_bytes()) {
            *d ^= e;
        }
    }
}

/// RAI: the reconstructive difference between two certified states
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CertifiedDelta {
    pub entries: Vec<(CertifiedBlock, CertifiedStatus)>,
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
    fn as_byte(self) -> u8 {
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
    entries: BTreeMap<(CertifiedBlock, ResidualKind), ()>,
    digest: [u8; 32],
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

    pub fn record(&mut self, block: CertifiedBlock, kind: ResidualKind) -> bool {
        if self.entries.contains_key(&(block, kind)) {
            return false;
        }
        let entry = Blake2HashBuilder::new()
            .update(b"RAI residual vote")
            .update(block.account.as_bytes())
            .update(block.height.to_le_bytes())
            .update(block.hash.as_bytes())
            .update([kind.as_byte()])
            .build();
        for (d, e) in self.digest.iter_mut().zip(entry.as_bytes()) {
            *d ^= e;
        }
        self.entries.insert((block, kind), ());
        true
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
            one.certify(block(i), CertifiedStatus::Notarized);
        }
        for i in (0..20).rev() {
            other.certify(block(i), CertifiedStatus::Notarized);
        }
        assert_eq!(one.root(), other.root());

        // A status upgrade changes the root
        other.certify(block(3), CertifiedStatus::Finalized);
        assert_ne!(one.root(), other.root());
        one.certify(block(3), CertifiedStatus::Finalized);
        assert_eq!(one.root(), other.root());
    }

    /// A status never weakens: the certificate stays constructed
    #[test]
    fn a_status_is_upgraded_and_never_weakened() {
        let mut state = CertifiedState::new();
        assert!(state.certify(block(1), CertifiedStatus::Notarized));
        assert!(state.certify(block(1), CertifiedStatus::Finalized));
        assert_eq!(state.status(&block(1)), Some(CertifiedStatus::Finalized));
        // Back to notarized changes nothing
        assert!(!state.certify(block(1), CertifiedStatus::Notarized));
        assert_eq!(state.status(&block(1)), Some(CertifiedStatus::Finalized));
        assert_eq!(state.len(), 1);
    }

    /// The reconciliation check: apply the difference to the source state and
    /// the root has to come out as the signed one
    #[test]
    fn a_difference_reconstructs_the_target_root() {
        let mut source = CertifiedState::new();
        for i in 0..20 {
            source.certify(block(i), CertifiedStatus::Notarized);
        }
        let mut target = source.clone();
        for i in 20..25 {
            target.certify(block(i), CertifiedStatus::Notarized);
        }
        target.certify(block(2), CertifiedStatus::FastFinalized);

        let delta = source.difference(&target).expect("target descends");
        assert_eq!(delta.entries.len(), 6);
        source.apply(&delta);
        assert_eq!(source.root(), target.root());
        assert_eq!(source.len(), target.len());
    }

    /// A state the target does not descend from has no difference: the
    /// responder has no bridge and does not answer
    #[test]
    fn an_unrelated_state_has_no_difference() {
        let mut source = CertifiedState::new();
        source.certify(block(1), CertifiedStatus::Notarized);
        source.certify(block(99), CertifiedStatus::Notarized);
        let mut target = CertifiedState::new();
        target.certify(block(1), CertifiedStatus::Notarized);
        assert!(source.difference(&target).is_none());

        // Nor does one whose status is ahead of the target's
        let mut ahead = CertifiedState::new();
        ahead.certify(block(1), CertifiedStatus::Finalized);
        let mut behind = CertifiedState::new();
        behind.certify(block(1), CertifiedStatus::Notarized);
        assert!(ahead.difference(&behind).is_none());
        assert!(behind.difference(&ahead).is_some());
    }

    /// A live state that has grown past the report keeps a bridge to it:
    /// the report's root is reconstructed from the difference
    #[test]
    fn a_grown_state_still_bridges_to_the_reported_one() {
        let mut reported = CertifiedState::new();
        for i in 0..10 {
            reported.certify(block(i), CertifiedStatus::Notarized);
        }
        let root = reported.root();
        let mut live = reported.clone();
        for i in 10..15 {
            live.certify(block(i), CertifiedStatus::Notarized);
        }
        // The live state is ahead, so it bridges the other way: a requester
        // whose own state is the smaller one reconstructs the report
        let mut requester = CertifiedState::new();
        for i in 0..8 {
            requester.certify(block(i), CertifiedStatus::Notarized);
        }
        let delta = requester.difference(&reported).unwrap();
        requester.apply(&delta);
        assert_eq!(requester.root(), root);
    }

    #[test]
    fn residual_votes_hash_their_entries() {
        let mut one = ResidualVotes::new();
        let mut other = ResidualVotes::new();
        assert_eq!(one.root(), other.root());
        one.record(block(1), ResidualKind::First);
        one.record(block(2), ResidualKind::Final);
        other.record(block(2), ResidualKind::Final);
        other.record(block(1), ResidualKind::First);
        assert_eq!(one.root(), other.root());
        assert_eq!(one.len(), 2);
        assert!(one.contains(&block(1), ResidualKind::First));
        assert!(!one.contains(&block(1), ResidualKind::Final));
        assert_eq!(one.supported().collect::<Vec<_>>(), vec![&block(1)]);

        // Recording twice changes nothing
        assert!(!one.record(block(1), ResidualKind::First));
        assert_eq!(one.len(), 2);
    }

    /// A first vote and second-look notarization support are separate
    /// entries: only the first vote can witness a hidden fast certificate
    #[test]
    fn a_first_vote_is_told_apart_from_notarization_support() {
        let mut votes = ResidualVotes::new();
        votes.record(block(1), ResidualKind::First);
        votes.record(block(2), ResidualKind::Notar);
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
        first_only.record(block(1), ResidualKind::First);
        let mut notar_only = ResidualVotes::new();
        notar_only.record(block(1), ResidualKind::Notar);
        assert_ne!(first_only.root(), notar_only.root());

        let mut both = ResidualVotes::new();
        both.record(block(1), ResidualKind::First);
        both.record(block(1), ResidualKind::Notar);
        assert_eq!(both.len(), 2);
        assert_ne!(both.root(), first_only.root());
        assert_eq!(both.first_votes().collect::<Vec<_>>(), vec![&block(1)]);
    }

    #[test]
    fn the_signed_payload_covers_both_roots() {
        let commitment = ReportCommitment {
            epoch: ConsensusEpoch::new(3),
            committee: BlockHash::from(7),
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
}
