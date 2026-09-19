use std::{cmp::max, collections::HashMap};

use rsnano_types::{Amount, BlockHash, PublicKey, VoteError, VoteKind};
use rustc_hash::FxHashMap;

use super::{ElectionState, block_tallies::BlockTallies};
use crate::representatives::QuorumSnapshot;

/// Weight thresholds of the Kudzu voting rules for one election.
///
/// f (Byzantine weight) and p (weight the fast path may do without) are fixed
/// shares of the online weight n. With f = p = 19%: certificate 62%, fast 81%,
/// and 3f + 2p = 95% < n. For six equal representatives this means four for a
/// certificate and five for fast finalization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KudzuThresholds {
    /// n: the online weight the thresholds are derived from
    pub online: Amount,
    /// f: the weight that may be faulty
    pub f: Amount,
    /// n − f − p: notarization, finalization and timeout certificates
    pub certificate: Amount,
    /// n − p: fast finalization certificate
    pub fast: Amount,
    /// f + p + 1: enough first votes for a second look or a timeout vote
    pub many: Amount,
}

impl KudzuThresholds {
    pub const F_PERCENT: u128 = 19;
    pub const P_PERCENT: u128 = 19;

    pub fn new(online: Amount) -> Self {
        let f = percent_of(online, Self::F_PERCENT);
        let p = percent_of(online, Self::P_PERCENT);
        Self {
            online,
            f,
            certificate: online - f - p,
            fast: online - p,
            many: f + p + Amount::raw(1),
        }
    }

    pub fn from_quorum(quorum: &QuorumSnapshot) -> Self {
        Self::new(max(quorum.online_weight, quorum.trended_or_min_weight))
    }
}

fn percent_of(amount: Amount, percent: u128) -> Amount {
    // Quotient/remainder arithmetic avoids overflowing even for Amount::MAX
    let n = amount.number();
    Amount::raw((n / 100) * percent + ((n % 100) * percent) / 100)
}

/// The certificates one election has collected so far. Certificates never
/// disappear, so this only ever grows.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Certificates {
    /// Notarization certificates, in the order they were observed locally
    pub notar: Vec<BlockHash>,
    /// Timeout certificate
    pub timeout: bool,
    /// Fast finalization certificate
    pub fast: Option<BlockHash>,
    /// Finalization certificate
    pub final_: Option<BlockHash>,
}

impl Certificates {
    /// Protocol 1, lines 9–13: the slot is done
    pub fn is_terminated(&self) -> bool {
        self.has_block() || self.timeout
    }

    /// Some block for this slot is in the complete block tree
    pub fn has_block(&self) -> bool {
        !self.notar.is_empty()
    }

    pub fn is_notarized(&self, hash: &BlockHash) -> bool {
        self.notar.contains(hash)
    }

    pub fn finalized(&self) -> Option<BlockHash> {
        self.fast.or(self.final_)
    }

    pub fn is_finalized(&self) -> bool {
        self.finalized().is_some()
    }

    /// Lemma 5.7: once a timeout certificate exists no block at this height
    /// can be (fast) finalized explicitly, only implicitly through a descendant.
    pub fn explicit_finalization_possible(&self) -> bool {
        !self.timeout
    }
}

/// The state of a Kudzu instance given its certificates and votes: finalized,
/// or terminated by a block in the tree or a timeout certificate, and settled
/// once no further notarization certificate can form
pub fn kudzu_state<'a>(
    current: ElectionState,
    votes: &SlotVotes,
    certs: &Certificates,
    thresholds: &KudzuThresholds,
    candidates: impl IntoIterator<Item = &'a BlockHash>,
) -> ElectionState {
    if certs.is_finalized() {
        ElectionState::Confirmed
    } else if !certs.is_terminated() {
        current
    } else if votes.is_settled(thresholds, certs, candidates) {
        ElectionState::Settled
    } else if certs.has_block() {
        ElectionState::Terminated
    } else {
        ElectionState::TimedOut
    }
}

/// The timeout block B_timeout (Definition 4.1): the first vote of a replica
/// that abstains from proposing goes to it, and the timeout certificate is its
/// notarization certificate
pub const TIMEOUT_BLOCK: BlockHash = BlockHash::from_bytes([0xFF; 32]);

/// The votes one representative has cast in one slot (Section 4.2: at most one
/// first vote, one timeout vote, one finalization vote and three notarization votes)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepSlotVotes {
    /// The first vote; `TIMEOUT_BLOCK` if the representative abstained (line 24)
    pub first: Option<BlockHash>,
    pub notar: Vec<BlockHash>,
    pub timeout: bool,
    pub final_: Option<BlockHash>,
    pub weight: Amount,
}

impl RepSlotVotes {
    const MAX_NOTAR_VOTES: usize = 3;

    fn add(&mut self, hash: BlockHash, kind: VoteKind) -> Result<(), VoteError> {
        match kind {
            VoteKind::First => {
                if self.first.is_some() {
                    return Err(VoteError::Replay);
                }
                self.first = Some(hash);
                self.ensure_notar(hash);
            }
            VoteKind::Notar => {
                if self.notar.contains(&hash) {
                    return Err(VoteError::Replay);
                }
                if self.notar.len() >= Self::MAX_NOTAR_VOTES {
                    return Err(VoteError::Ignored);
                }
                self.notar.push(hash);
            }
            VoteKind::Timeout => {
                if self.timeout {
                    return Err(VoteError::Replay);
                }
                self.timeout = true;
            }
            // Line 24: FirstVote(NotarVote(B_timeout)): a first vote spent on the
            // timeout block, which is a timeout vote as well
            VoteKind::Abstain => {
                if self.first.is_some() || self.timeout {
                    return Err(VoteError::Replay);
                }
                self.first = Some(TIMEOUT_BLOCK);
                self.timeout = true;
                self.ensure_notar(TIMEOUT_BLOCK);
            }
            VoteKind::Final => {
                if self.final_.is_some() {
                    return Err(VoteError::Replay);
                }
                self.final_ = Some(hash);
                // An honest replica final votes only if it notarized nothing else in
                // this slot and casts no votes afterwards, so a final vote can safely
                // supply notarization weight as well.
                self.ensure_notar(hash);
            }
        }
        Ok(())
    }

    fn ensure_notar(&mut self, hash: BlockHash) {
        if !self.notar.contains(&hash) {
            self.notar.push(hash);
        }
    }

    fn has_voted_for(&self, hash: &BlockHash) -> bool {
        self.first == Some(*hash) || self.notar.contains(hash) || self.final_ == Some(*hash)
    }

    /// RAI: the representative abstained from proposing in this instance
    pub fn abstained(&self) -> bool {
        self.first == Some(TIMEOUT_BLOCK)
    }

    fn remove_block(&mut self, hash: &BlockHash) {
        if self.first == Some(*hash) {
            self.first = None;
        }
        if self.final_ == Some(*hash) {
            self.final_ = None;
        }
        self.notar.retain(|h| h != hash);
    }
}

/// The vote pool of one election (Section 4.2), plus the weighted tallies
/// derived from it
#[derive(Clone, Default)]
pub struct SlotVotes {
    reps: HashMap<PublicKey, RepSlotVotes>,
    first_tallies: BlockTallies,
    notar_tallies: BlockTallies,
    final_tallies: BlockTallies,
    /// allVotes(firstVote), the abstaining first votes for the timeout block included
    all_first: Amount,
    timeout_weight: Amount,
}

impl SlotVotes {
    pub fn add(
        &mut self,
        voter: PublicKey,
        hash: BlockHash,
        kind: VoteKind,
    ) -> Result<(), VoteError> {
        self.reps.entry(voter).or_default().add(hash, kind)
    }

    pub fn rep(&self, voter: &PublicKey) -> Option<&RepSlotVotes> {
        self.reps.get(voter)
    }

    pub fn has_voted_for(&self, voter: &PublicKey, hash: &BlockHash) -> bool {
        self.reps.get(voter).is_some_and(|r| r.has_voted_for(hash))
    }

    pub fn len(&self) -> usize {
        self.reps.len()
    }

    pub fn remove_rep(&mut self, voter: &PublicKey) {
        self.reps.remove(voter);
    }

    pub fn remove_block(&mut self, hash: &BlockHash) {
        for rep in self.reps.values_mut() {
            rep.remove_block(hash);
        }
        self.first_tallies.remove(hash);
        self.notar_tallies.remove(hash);
        self.final_tallies.remove(hash);
    }

    /// Recalculate all tallies with the given representative weights
    pub fn calculate(&mut self, rep_weights: &FxHashMap<PublicKey, Amount>) {
        for (voter, rep) in self.reps.iter_mut() {
            rep.weight = rep_weights.get(voter).copied().unwrap_or_default();
        }
        let reps = self.reps.values();
        self.first_tallies
            .calculate_from(reps.clone().filter_map(|r| r.first.map(|h| (h, r.weight))));
        self.notar_tallies.calculate_from(
            reps.clone()
                .flat_map(|r| r.notar.iter().map(move |h| (*h, r.weight))),
        );
        self.final_tallies
            .calculate_from(reps.clone().filter_map(|r| r.final_.map(|h| (h, r.weight))));
        self.all_first = self.first_tallies.sum();
        self.timeout_weight = reps.filter(|r| r.timeout).map(|r| r.weight).sum();
    }

    pub fn first_tallies(&self) -> &BlockTallies {
        &self.first_tallies
    }

    pub fn notar_tallies(&self) -> &BlockTallies {
        &self.notar_tallies
    }

    pub fn final_tallies(&self) -> &BlockTallies {
        &self.final_tallies
    }

    pub fn all_first(&self) -> Amount {
        self.all_first
    }

    pub fn timeout_weight(&self) -> Amount {
        self.timeout_weight
    }

    /// manyVotes(firstVote): the blocks with at least f + p + 1 first votes. The
    /// timeout block is not looked at, its notarization is the timeout vote
    /// (lines 32-35).
    pub fn many_votes<'a>(
        &'a self,
        thresholds: &'a KudzuThresholds,
    ) -> impl Iterator<Item = BlockHash> + 'a {
        self.first_tallies
            .iter()
            .filter(|(hash, tally)| *hash != TIMEOUT_BLOCK && *tally >= thresholds.many)
            .map(|(hash, _)| *hash)
    }

    /// Protocol 1, line 32: allVotes(firstVote) − maxVotes(firstVote) ≥ f + p + 1
    pub fn should_timeout(&self, thresholds: &KudzuThresholds) -> bool {
        let max_votes = self
            .first_tallies
            .winner()
            .map(|(_, tally)| *tally)
            .unwrap_or_default();
        self.all_first - max_votes >= thresholds.many
    }

    /// Adds every certificate the current tallies support
    pub fn update_certificates(&self, thresholds: &KudzuThresholds, certs: &mut Certificates) {
        for (hash, tally) in self.notar_tallies.iter() {
            if *hash != TIMEOUT_BLOCK
                && *tally >= thresholds.certificate
                && !certs.notar.contains(hash)
            {
                certs.notar.push(*hash);
            }
        }
        // The notarization certificate of the timeout block
        if self.timeout_weight >= thresholds.certificate {
            certs.timeout = true;
        }
        if certs.fast.is_none() {
            certs.fast = self
                .first_tallies
                .iter()
                .find(|(hash, tally)| *hash != TIMEOUT_BLOCK && *tally >= thresholds.fast)
                .map(|(hash, _)| *hash);
        }
        if certs.final_.is_none() {
            certs.final_ = self
                .final_tallies
                .iter()
                .find(|(_, tally)| *tally >= thresholds.certificate)
                .map(|(hash, _)| *hash);
        }
    }

    /// An election is settled when no further notarization certificate can form.
    /// This is a conservative, locally provable check: a representative that exited
    /// silently is still counted as able to vote.
    pub fn is_settled<'a>(
        &self,
        thresholds: &KudzuThresholds,
        certs: &Certificates,
        candidates: impl IntoIterator<Item = &'a BlockHash>,
    ) -> bool {
        if certs.is_finalized() {
            return true;
        }
        if !certs.is_terminated() {
            return false;
        }
        // Weight that has not first voted yet, known or unknown. It can still first
        // vote (and thereby notarize) any block, including one we have not seen.
        // An abstaining first vote for the timeout block counts as cast.
        let unvoted = thresholds
            .online
            .checked_sub(self.all_first)
            .unwrap_or_default();
        if unvoted >= thresholds.many {
            return false;
        }
        candidates
            .into_iter()
            .filter(|hash| !certs.is_notarized(hash))
            .all(|hash| self.max_notar_weight(hash, unvoted, thresholds) < thresholds.certificate)
    }

    /// Whether a finalization certificate for `hash` can still form: the weight
    /// that has notarized nothing but `hash` (or nothing at all, known or
    /// unknown) may still cast a final vote for it (Protocol 1, line 10)
    pub fn can_finalize(&self, thresholds: &KudzuThresholds, hash: &BlockHash) -> bool {
        let known: Amount = self.reps.values().map(|r| r.weight).sum();
        let unknown = thresholds.online.checked_sub(known).unwrap_or_default();
        // A representative that timed out never casts a final vote (line 10)
        let able: Amount = self
            .reps
            .values()
            .filter(|r| {
                !r.timeout
                    && r.notar.iter().all(|h| h == hash)
                    && r.final_.is_none_or(|h| h == *hash)
            })
            .map(|r| r.weight)
            .sum();
        able + unknown >= thresholds.certificate
    }

    fn max_notar_weight(
        &self,
        hash: &BlockHash,
        unvoted: Amount,
        thresholds: &KudzuThresholds,
    ) -> Amount {
        let can_reach_many = self.first_tallies.get(hash) + unvoted >= thresholds.many;
        let mut result = self.notar_tallies.get(hash) + unvoted;
        if can_reach_many {
            // Every representative that has first voted and not exited could
            // still take a second look (line 28), abstaining ones included
            result += self
                .reps
                .values()
                .filter(|r| r.first.is_some() && r.final_.is_none() && !r.notar.contains(hash))
                .map(|r| r.weight)
                .sum();
        }
        result
    }
}

/// The votes this node has cast for one slot, i.e. one account height. It is
/// shared by all elections at that height (Protocol 1: firstVoted, notarized).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalSlotState {
    pub first_voted: Option<BlockHash>,
    /// Blocks notarized on a second look
    pub notar_voted: Vec<BlockHash>,
    /// The candidate hash the timeout vote was routed through
    pub timeout_voted: Option<BlockHash>,
    pub final_voted: Option<BlockHash>,
    /// RAI: this node does not propose in this instance. The instance belongs
    /// to an epoch this node has already left; it only casts its timeout vote
    /// so that the instance can terminate, and collects the certificates.
    pub stale: bool,
}

impl LocalSlotState {
    pub fn stale() -> Self {
        Self {
            stale: true,
            ..Default::default()
        }
    }

    pub fn mark_voted(&mut self, hash: BlockHash, kind: VoteKind) {
        match kind {
            VoteKind::First => self.first_voted = Some(hash),
            VoteKind::Notar => {
                if !self.notar_voted.contains(&hash) {
                    self.notar_voted.push(hash)
                }
            }
            VoteKind::Timeout => self.timeout_voted = Some(hash),
            // Line 23-25: the first vote is spent on the timeout block, `hash` is
            // the candidate the statement is routed through
            VoteKind::Abstain => {
                self.first_voted = Some(TIMEOUT_BLOCK);
                self.timeout_voted = Some(hash);
            }
            VoteKind::Final => self.final_voted = Some(hash),
        }
    }

    /// Line 24: this node abstained from proposing in the instance
    pub fn abstained(&self) -> bool {
        self.first_voted == Some(TIMEOUT_BLOCK)
    }

    /// notarized ⊆ {hash}: the precondition for a final vote (line 11)
    pub fn notarized_only(&self, hash: &BlockHash) -> bool {
        self.timeout_voted.is_none()
            && self.first_voted.is_none_or(|h| h == *hash)
            && self.notar_voted.iter().all(|h| h == hash)
    }

    /// A second look was already taken (line 28: B ∉ secondLook) or the block
    /// was first voted, in which case a second look changes nothing
    pub fn looked_at(&self, hash: &BlockHash) -> bool {
        self.first_voted == Some(*hash) || self.notar_voted.contains(hash)
    }

    /// The statements this node made for the given candidates, to be re-signed
    /// for a replica which asks for them (Section 4.2: certificates are handed
    /// to every replica). A statement's identity is (representative, kind, hash),
    /// so re-signing it for a subset of hashes is the same statement.
    pub fn statements_for<'a>(
        &self,
        candidates: impl IntoIterator<Item = &'a BlockHash>,
        timeout_routing: BlockHash,
    ) -> Vec<(VoteKind, Vec<BlockHash>)> {
        let mut first = Vec::new();
        let mut notar = Vec::new();
        let mut final_ = Vec::new();
        for hash in candidates {
            if self.first_voted == Some(*hash) && *hash != TIMEOUT_BLOCK {
                first.push(*hash);
            }
            if self.notar_voted.contains(hash) {
                notar.push(*hash);
            }
            if self.final_voted == Some(*hash) {
                final_.push(*hash);
            }
        }
        let mut result = Vec::new();
        if !first.is_empty() {
            result.push((VoteKind::First, first));
        }
        if !notar.is_empty() {
            result.push((VoteKind::Notar, notar));
        }
        if self.timeout_voted.is_some() {
            result.push((self.timeout_kind(), vec![timeout_routing]));
        }
        if !final_.is_empty() {
            result.push((VoteKind::Final, final_));
        }
        result
    }

    /// Everything cast so far, for re-broadcasting
    pub fn cast_votes(&self) -> impl Iterator<Item = (BlockHash, VoteKind)> + '_ {
        self.first_voted
            .filter(|h| *h != TIMEOUT_BLOCK)
            .map(|h| (h, VoteKind::First))
            .into_iter()
            .chain(self.notar_voted.iter().map(|h| (*h, VoteKind::Notar)))
            .chain(self.timeout_voted.map(|h| (h, self.timeout_kind())))
            .chain(self.final_voted.map(|h| (h, VoteKind::Final)))
    }

    /// The abstaining first vote for the timeout block, or the timeout vote of
    /// a replica that proposed
    fn timeout_kind(&self) -> VoteKind {
        if self.abstained() {
            VoteKind::Abstain
        } else {
            VoteKind::Timeout
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::PrivateKey;

    #[test]
    fn thresholds_from_online_weight() {
        let t = KudzuThresholds::new(Amount::raw(100));
        assert_eq!(t.certificate, Amount::raw(62));
        assert_eq!(t.fast, Amount::raw(81));
        assert_eq!(t.many, Amount::raw(39));
        // many + certificate = n + 1: two certificates cannot be disjoint
        assert_eq!(t.many + t.certificate, Amount::raw(101));
    }

    #[test]
    fn six_equal_representatives_need_four_for_a_certificate_and_five_for_fast() {
        let t = KudzuThresholds::new(Amount::raw(600));
        assert!(Amount::raw(300) < t.certificate && Amount::raw(400) >= t.certificate);
        assert!(Amount::raw(400) < t.fast && Amount::raw(500) >= t.fast);
    }

    #[test]
    fn thresholds_do_not_overflow_for_max_weight() {
        let t = KudzuThresholds::new(Amount::MAX);
        assert!(t.fast > t.certificate);
        assert!(t.many < t.certificate);
    }

    #[test]
    fn thresholds_from_quorum_snapshot_use_the_larger_online_estimate() {
        let mut quorum = QuorumSnapshot::new_test_instance();
        quorum.online_weight = Amount::nano(50_000_000);
        quorum.trended_or_min_weight = Amount::nano(100_000_000);
        let t = KudzuThresholds::from_quorum(&quorum);
        assert_eq!(t, KudzuThresholds::new(Amount::nano(100_000_000)));
    }

    #[test]
    fn one_first_vote_per_rep_and_it_counts_as_notarization() {
        let mut pool = SlotVotes::default();
        assert_eq!(pool.add(rep(1), hash(1), VoteKind::First), Ok(()));
        assert_eq!(
            pool.add(rep(1), hash(2), VoteKind::First),
            Err(VoteError::Replay)
        );
        pool.calculate(&weights(&[(1, 10)]));
        assert_eq!(pool.first_tallies().get(&hash(1)), Amount::raw(10));
        assert_eq!(pool.notar_tallies().get(&hash(1)), Amount::raw(10));
        assert_eq!(pool.all_first(), Amount::raw(10));
    }

    #[test]
    fn at_most_three_notarization_votes_per_rep() {
        let mut pool = SlotVotes::default();
        for i in 1..=3 {
            assert_eq!(pool.add(rep(1), hash(i), VoteKind::Notar), Ok(()));
        }
        assert_eq!(
            pool.add(rep(1), hash(1), VoteKind::Notar),
            Err(VoteError::Replay)
        );
        assert_eq!(
            pool.add(rep(1), hash(4), VoteKind::Notar),
            Err(VoteError::Ignored)
        );
    }

    #[test]
    fn timeout_and_final_votes_are_unique_per_rep() {
        let mut pool = SlotVotes::default();
        assert_eq!(pool.add(rep(1), hash(1), VoteKind::Timeout), Ok(()));
        assert_eq!(
            pool.add(rep(1), hash(2), VoteKind::Timeout),
            Err(VoteError::Replay)
        );
        assert_eq!(pool.add(rep(1), hash(1), VoteKind::Final), Ok(()));
        assert_eq!(
            pool.add(rep(1), hash(1), VoteKind::Final),
            Err(VoteError::Replay)
        );
        pool.calculate(&weights(&[(1, 10)]));
        assert_eq!(pool.timeout_weight(), Amount::raw(10));
        assert_eq!(pool.final_tallies().get(&hash(1)), Amount::raw(10));
        // A final vote supplies notarization weight but is not a first vote
        assert_eq!(pool.notar_tallies().get(&hash(1)), Amount::raw(10));
        assert_eq!(pool.all_first(), Amount::ZERO);
    }

    #[test]
    fn many_votes_and_timeout_rule() {
        // f + p + 1 = 39 of 100
        let t = thresholds();
        let mut pool = SlotVotes::default();
        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(2), VoteKind::First).unwrap();
        pool.add(rep(3), hash(3), VoteKind::First).unwrap();
        pool.calculate(&weights(&[(1, 40), (2, 25), (3, 14)]));

        assert_eq!(pool.many_votes(&t).collect::<Vec<_>>(), vec![hash(1)]);
        // allVotes − maxVotes = 79 − 40 = 39 ≥ 39
        assert!(pool.should_timeout(&t));

        pool.calculate(&weights(&[(1, 40), (2, 25), (3, 13)]));
        assert!(!pool.should_timeout(&t));
    }

    #[test]
    fn certificates_form_at_their_thresholds_and_never_disappear() {
        let t = thresholds();
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();

        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::First).unwrap();
        pool.calculate(&weights(&[(1, 50), (2, 17), (3, 20)]));
        pool.update_certificates(&t, &mut certs);
        assert_eq!(certs.notar, vec![hash(1)]);
        assert!(certs.is_terminated());
        assert!(!certs.is_finalized());

        pool.add(rep(3), hash(1), VoteKind::First).unwrap();
        pool.calculate(&weights(&[(1, 50), (2, 17), (3, 20)]));
        pool.update_certificates(&t, &mut certs);
        assert_eq!(certs.fast, Some(hash(1)));
        assert_eq!(certs.finalized(), Some(hash(1)));

        // Weights dropping does not revoke a certificate
        pool.calculate(&weights(&[(1, 1), (2, 1), (3, 1)]));
        pool.update_certificates(&t, &mut certs);
        assert_eq!(certs.notar, vec![hash(1)]);
        assert_eq!(certs.fast, Some(hash(1)));
    }

    #[test]
    fn final_and_timeout_certificates() {
        let t = thresholds();
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        pool.add(rep(1), hash(1), VoteKind::Final).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Final).unwrap();
        pool.add(rep(1), hash(1), VoteKind::Timeout).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&weights(&[(1, 50), (2, 17)]));
        pool.update_certificates(&t, &mut certs);
        assert_eq!(certs.final_, Some(hash(1)));
        assert!(certs.timeout);
        assert!(!certs.explicit_finalization_possible());
    }

    #[test]
    fn settled_when_no_other_candidate_can_be_notarized() {
        let t = thresholds();
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let candidates = [hash(1), hash(2)];
        let weights = weights(&[(1, 40), (2, 27), (3, 33)]);

        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Notar).unwrap();
        pool.calculate(&weights);
        pool.update_certificates(&t, &mut certs);
        assert!(certs.is_notarized(&hash(1)));
        // 60 of weight has not first voted yet and could notarize anything
        assert!(!pool.is_settled(&t, &certs, &candidates));

        pool.add(rep(3), hash(2), VoteKind::First).unwrap();
        pool.calculate(&weights);
        // hash 2 can still reach f + p + 1 first votes, so rep 1 could take a second look
        assert!(!pool.is_settled(&t, &certs, &candidates));

        pool.add(rep(1), hash(1), VoteKind::Final).unwrap();
        pool.calculate(&weights);
        pool.update_certificates(&t, &mut certs);
        assert!(!certs.is_finalized());
        // rep 1 exited: hash 2 can gather at most 33 + 27 = 60 < 67
        assert!(pool.is_settled(&t, &certs, &candidates));
    }

    /// RAI: a representative that abstained has left the instance and will
    /// not first vote any more, one that merely timed out may have first
    /// voted a block we do not hold
    #[test]
    fn abstained_weight_settles_a_timed_out_instance() {
        let t = thresholds();
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let weights = weights(&[(1, 30), (2, 32), (3, 38)]);

        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Abstain).unwrap();
        pool.calculate(&weights);
        pool.update_certificates(&t, &mut certs);
        assert!(!certs.is_terminated());
        // 38 of weight is still unknown and could notarize anything
        assert!(!pool.is_settled(&t, &certs, &[hash(1)]));

        pool.add(rep(3), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&weights);
        pool.update_certificates(&t, &mut certs);
        assert!(certs.timeout);
        // rep 3 timed out but may have first voted a block we have not seen
        assert!(!pool.is_settled(&t, &certs, &[hash(1)]));

        let mut abstaining = SlotVotes::default();
        abstaining.add(rep(1), hash(1), VoteKind::First).unwrap();
        abstaining.add(rep(2), hash(1), VoteKind::Abstain).unwrap();
        abstaining.add(rep(3), hash(1), VoteKind::Abstain).unwrap();
        abstaining.calculate(&weights);
        // 70 of weight left for good: hash 1 stays at 30
        assert!(abstaining.is_settled(&t, &certs, &[hash(1)]));
    }

    /// RAI: a representative that timed out never casts a final vote
    #[test]
    fn timed_out_weight_cannot_finalize() {
        let t = thresholds();
        let mut pool = SlotVotes::default();
        let weights = weights(&[(1, 40), (2, 27), (3, 33)]);
        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::First).unwrap();
        pool.add(rep(3), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&weights);
        assert!(pool.can_finalize(&t, &hash(1)));

        pool.add(rep(1), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&weights);
        assert!(!pool.can_finalize(&t, &hash(1)));
    }

    /// A 3-3 fork whose representatives all timed out settles once every
    /// representative has taken its second look at the other block (line 28)
    #[test]
    fn settled_once_every_representative_took_its_second_look() {
        let t = thresholds();
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let weights = weights(&[(1, 17), (2, 17), (3, 17), (4, 17), (5, 16), (6, 16)]);
        for rep_id in 1..=3 {
            pool.add(rep(rep_id), hash(1), VoteKind::First).unwrap();
        }
        for rep_id in 4..=6 {
            pool.add(rep(rep_id), hash(2), VoteKind::First).unwrap();
        }
        pool.calculate(&weights);
        pool.update_certificates(&t, &mut certs);
        assert!(!certs.is_terminated());
        assert!(!pool.is_settled(&t, &certs, &[hash(1), hash(2)]));

        for rep_id in 1..=6 {
            pool.add(rep(rep_id), hash(1), VoteKind::Timeout).unwrap();
        }
        pool.calculate(&weights);
        pool.update_certificates(&t, &mut certs);
        assert!(certs.timeout);
        assert!(certs.notar.is_empty());
        // Both blocks have many first votes, every representative still looks
        assert!(!pool.is_settled(&t, &certs, &[hash(1), hash(2)]));

        for rep_id in 1..=3 {
            pool.add(rep(rep_id), hash(2), VoteKind::Notar).unwrap();
        }
        for rep_id in 4..=6 {
            pool.add(rep(rep_id), hash(1), VoteKind::Notar).unwrap();
        }
        pool.calculate(&weights);
        pool.update_certificates(&t, &mut certs);
        assert_eq!(certs.notar.len(), 2);
        assert!(pool.is_settled(&t, &certs, &[hash(1), hash(2)]));
    }

    #[test]
    fn not_settled_while_a_second_look_is_still_possible() {
        let t = thresholds();
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::First).unwrap();
        pool.add(rep(3), hash(2), VoteKind::First).unwrap();
        pool.calculate(&weights(&[(1, 40), (2, 27), (3, 39)]));
        pool.update_certificates(&t, &mut certs);
        // hash 2 has f + p + 1 first votes, rep 1 and 2 may still take a second look
        assert!(!pool.is_settled(&t, &certs, &[hash(1), hash(2)]));

        // Once they exited with a final vote they cannot
        pool.add(rep(1), hash(1), VoteKind::Final).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Final).unwrap();
        pool.calculate(&weights(&[(1, 40), (2, 27), (3, 39)]));
        pool.update_certificates(&t, &mut certs);
        assert!(certs.is_finalized());
        assert!(pool.is_settled(&t, &certs, &[hash(1), hash(2)]));
    }

    #[test]
    fn statements_for_an_election_cover_only_its_candidates() {
        let mut slot = LocalSlotState::default();
        slot.mark_voted(hash(1), VoteKind::First);
        slot.mark_voted(hash(2), VoteKind::Notar);
        slot.mark_voted(hash(1), VoteKind::Timeout);
        slot.mark_voted(hash(1), VoteKind::Final);

        let statements = slot.statements_for(&[hash(1), hash(2)], hash(1));
        assert_eq!(
            statements,
            vec![
                (VoteKind::First, vec![hash(1)]),
                (VoteKind::Notar, vec![hash(2)]),
                (VoteKind::Timeout, vec![hash(1)]),
                (VoteKind::Final, vec![hash(1)]),
            ]
        );
        // A sibling election at the same height only gets what concerns it
        assert_eq!(
            slot.statements_for(&[hash(3)], hash(3)),
            vec![(VoteKind::Timeout, vec![hash(3)])]
        );
    }

    #[test]
    fn removing_a_block_drops_its_votes() {
        let mut pool = SlotVotes::default();
        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(1), hash(2), VoteKind::Notar).unwrap();
        pool.calculate(&weights(&[(1, 10)]));
        pool.remove_block(&hash(2));
        assert_eq!(pool.notar_tallies().get(&hash(2)), Amount::ZERO);
        assert_eq!(pool.rep(&rep(1)).unwrap().notar, vec![hash(1)]);
    }

    #[test]
    fn local_slot_state_tracks_the_final_vote_precondition() {
        let mut slot = LocalSlotState::default();
        assert!(slot.notarized_only(&hash(1)));
        slot.mark_voted(hash(1), VoteKind::First);
        assert!(slot.notarized_only(&hash(1)));
        assert!(slot.looked_at(&hash(1)));
        assert!(!slot.looked_at(&hash(2)));

        slot.mark_voted(hash(2), VoteKind::Notar);
        assert!(!slot.notarized_only(&hash(1)));
        assert!(slot.looked_at(&hash(2)));

        let mut timed_out = LocalSlotState::default();
        timed_out.mark_voted(hash(1), VoteKind::First);
        timed_out.mark_voted(hash(1), VoteKind::Timeout);
        assert!(!timed_out.notarized_only(&hash(1)));

        assert_eq!(
            slot.cast_votes().collect::<Vec<_>>(),
            vec![(hash(1), VoteKind::First), (hash(2), VoteKind::Notar)]
        );
    }

    /*
     * Test helpers
     */

    /// Thresholds for an online weight of 100: certificate 62, fast 81, many 39
    fn thresholds() -> KudzuThresholds {
        KudzuThresholds::new(Amount::raw(100))
    }

    fn rep(i: u64) -> PublicKey {
        PrivateKey::from(i).public_key()
    }

    fn hash(i: u64) -> BlockHash {
        BlockHash::from(i)
    }

    fn weights(entries: &[(u64, u128)]) -> FxHashMap<PublicKey, Amount> {
        entries
            .iter()
            .map(|(r, w)| (rep(*r), Amount::raw(*w)))
            .collect()
    }
}
