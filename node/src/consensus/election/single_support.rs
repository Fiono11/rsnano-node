use std::{cmp::max, collections::HashMap, sync::Arc};

use rsnano_types::{Amount, BlockHash, PublicKey, VoteError, VoteKind};

use super::{Committee, Committees, ElectionState, block_tallies::BlockTallies};
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
    /// n − f − p: notarization and finalization certificates
    pub certificate: Amount,
    /// n − p: fast finalization certificate
    pub fast: Amount,
    /// f + p + 1: support needed to recover hidden finality from reports
    pub many: Amount,
    /// RAI: q_report = N − f, the reports an epoch proposal selects. Weights
    /// stand in for validator counts here as they do for every other
    /// threshold, so a selection has to carry n − f of the old committee's
    /// weight rather than a number of identities.
    pub report: Amount,
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
            report: online - f,
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
    /// Fast finalization certificate
    pub fast: Option<BlockHash>,
    /// Finalization certificate
    pub final_: Option<BlockHash>,
}

impl Certificates {
    /// Protocol 1, lines 9–13: the slot is done
    pub fn is_terminated(&self) -> bool {
        self.has_block()
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
}

/// The state of a Kudzu instance given its certificates and votes: finalized,
/// or terminated by a block in the tree, and settled
/// once no further notarization certificate can form. RAI, single-support
/// account voting: an account domain settles as soon as no certificate can
/// form, terminated or not - split first votes never terminate it, and
/// "may remain unresolved until checkpoint recovery or an uncontested child".
pub fn kudzu_state<'a>(
    current: ElectionState,
    votes: &SlotVotes,
    certs: &Certificates,
    candidates: impl IntoIterator<Item = &'a BlockHash> + Clone,
) -> ElectionState {
    if certs.is_finalized() {
        ElectionState::Confirmed
    } else if votes.is_settled(certs, candidates) {
        ElectionState::Settled
    } else if !certs.is_terminated() {
        current
    } else if certs.has_block() {
        ElectionState::Terminated
    } else {
        current
    }
}

/// First and final support received from one representative at an account slot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepSlotVotes {
    /// The first vote received from this representative.
    pub first: Option<BlockHash>,
    pub notar: Vec<BlockHash>,
    pub final_: Option<BlockHash>,
    /// RAI: the further first and final votes of an identity that signed
    /// more than one block in an account domain, an equivocator. Kept: a
    /// certificate is a set of signatures, and a signature this identity
    /// put on a block is evidence for it wherever it arrives, so every
    /// correct validator that received the votes constructs the same
    /// certificates. The proofs need only that a correct identity supports
    /// one block.
    pub also: Vec<(VoteKind, BlockHash)>,
}

impl RepSlotVotes {
    fn add(&mut self, hash: BlockHash, kind: VoteKind) -> Result<(), VoteError> {
        match kind {
            VoteKind::First => {
                if self.first.is_some() {
                    return self.add_equivocation(hash, kind);
                }
                self.first = Some(hash);
                self.ensure_notar(hash);
            }
            VoteKind::Final => {
                if self.final_.is_some() {
                    return self.add_equivocation(hash, kind);
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

    /// A second first or final vote of one identity: the same vote again is
    /// a replay; a vote for another block is an equivocation, kept in an
    /// account domain.
    fn add_equivocation(&mut self, hash: BlockHash, kind: VoteKind) -> Result<(), VoteError> {
        if self.voted(kind, &hash) {
            return Err(VoteError::Replay);
        }
        self.also.push((kind, hash));
        self.ensure_notar(hash);
        Ok(())
    }

    fn voted(&self, kind: VoteKind, hash: &BlockHash) -> bool {
        let held = match kind {
            VoteKind::First => self.first,
            VoteKind::Final => self.final_,
        };
        held == Some(*hash) || self.also.contains(&(kind, *hash))
    }

    /// Every block this identity first-voted: the one recorded first and,
    /// in an account domain, the ones it equivocated with
    pub fn first_votes(&self) -> impl Iterator<Item = BlockHash> + '_ {
        self.first.into_iter().chain(
            self.also
                .iter()
                .filter(|(kind, _)| *kind == VoteKind::First)
                .map(|(_, hash)| *hash),
        )
    }

    /// Every block this identity final-voted, likewise
    pub fn final_votes(&self) -> impl Iterator<Item = BlockHash> + '_ {
        self.final_.into_iter().chain(
            self.also
                .iter()
                .filter(|(kind, _)| *kind == VoteKind::Final)
                .map(|(_, hash)| *hash),
        )
    }

    fn ensure_notar(&mut self, hash: BlockHash) {
        if !self.notar.contains(&hash) {
            self.notar.push(hash);
        }
    }

    fn has_voted_for(&self, hash: &BlockHash) -> bool {
        self.first == Some(*hash)
            || self.notar.contains(hash)
            || self.final_ == Some(*hash)
            || self.also.iter().any(|(_, h)| h == hash)
    }

    fn remove_block(&mut self, hash: &BlockHash) {
        if self.first == Some(*hash) {
            self.first = None;
        }
        if self.final_ == Some(*hash) {
            self.final_ = None;
        }
        self.notar.retain(|h| h != hash);
        self.also.retain(|(_, h)| h != hash);
    }
}

/// The vote pool of one election (Section 4.2), plus the weighted tallies
/// derived from it in each committee the instance is counted in
#[derive(Clone, Default)]
pub struct SlotVotes {
    reps: HashMap<PublicKey, RepSlotVotes>,
    /// One per committee, the instance's own committee first
    tallies: Vec<CommitteeTallies>,
}

/// The tallies of the vote pool in one committee
#[derive(Clone)]
struct CommitteeTallies {
    committee: Arc<Committee>,
    first_tallies: BlockTallies,
    notar_tallies: BlockTallies,
    final_tallies: BlockTallies,
    /// Total weight of identities with at least one first vote.
    all_first: Amount,
}

impl CommitteeTallies {
    fn calculate(committee: Arc<Committee>, reps: &HashMap<PublicKey, RepSlotVotes>) -> Self {
        let weighted = || {
            reps.iter()
                .map(|(voter, rep)| (rep, committee.weight(voter)))
        };
        let mut first_tallies = BlockTallies::new();
        first_tallies
            .calculate_from(weighted().flat_map(|(r, w)| r.first_votes().map(move |h| (h, w))));
        let mut notar_tallies = BlockTallies::new();
        notar_tallies
            .calculate_from(weighted().flat_map(|(r, w)| r.notar.iter().map(move |h| (*h, w))));
        let mut final_tallies = BlockTallies::new();
        final_tallies
            .calculate_from(weighted().flat_map(|(r, w)| r.final_votes().map(move |h| (h, w))));
        // allVotes(firstVote) counts identities once: an equivocator's
        // weight is in the tally of each block it voted, not in the sum twice
        let all_first = weighted()
            .filter(|(r, _)| r.first.is_some())
            .map(|(_, w)| w)
            .sum();

        Self {
            committee,
            first_tallies,
            notar_tallies,
            final_tallies,
            all_first,
        }
    }

    fn thresholds(&self) -> &KudzuThresholds {
        self.committee.thresholds()
    }

    fn notarizes(&self, hash: &BlockHash) -> bool {
        self.notar_tallies.get(hash) >= self.thresholds().certificate
    }

    fn fast_finalizes(&self, hash: &BlockHash) -> bool {
        self.first_tallies.get(hash) >= self.thresholds().fast
    }

    fn finalizes(&self, hash: &BlockHash) -> bool {
        self.final_tallies.get(hash) >= self.thresholds().certificate
    }

    /// Whether no further notarization certificate can form in this
    /// committee, conservatively: a representative that exited silently is
    /// still counted as able to vote
    fn is_settled<'a>(
        &self,
        reps: &HashMap<PublicKey, RepSlotVotes>,
        certs: &Certificates,
        candidates: impl IntoIterator<Item = &'a BlockHash>,
    ) -> bool {
        // Weight that may still first vote (and thereby notarize) any block,
        // including one we have not seen: representatives never heard from,
        // and those known without a first vote which have not exited. A
        // final vote without a first vote (line 11, for a block in the tree
        // this replica never proposed) is an exit, nothing follows it.
        let known: Amount = reps.keys().map(|voter| self.committee.weight(voter)).sum();
        let unknown = self
            .thresholds()
            .online
            .checked_sub(known)
            .unwrap_or_default();
        let idle: Amount = reps
            .iter()
            .filter(|(_, r)| r.first.is_none() && r.final_.is_none())
            .map(|(voter, _)| self.committee.weight(voter))
            .sum();
        let unvoted = unknown + idle;
        if unvoted >= self.thresholds().many {
            return false;
        }
        candidates
            .into_iter()
            .filter(|hash| !certs.is_notarized(hash))
            .all(|hash| self.max_notar_weight(reps, hash, unvoted) < self.thresholds().certificate)
    }

    /// Whether a finalization certificate for `hash` can still form in this
    /// committee: the weight that has notarized nothing but `hash` (or
    /// nothing at all, known or unknown) may still cast a final vote for it
    /// (Protocol 1, line 10)
    fn can_finalize(&self, reps: &HashMap<PublicKey, RepSlotVotes>, hash: &BlockHash) -> bool {
        let known: Amount = reps.keys().map(|voter| self.committee.weight(voter)).sum();
        let unknown = self
            .thresholds()
            .online
            .checked_sub(known)
            .unwrap_or_default();
        let able: Amount = reps
            .iter()
            .filter(|(_, r)| {
                r.notar.iter().all(|h| h == hash) && r.final_.is_none_or(|h| h == *hash)
            })
            .map(|(voter, _)| self.committee.weight(voter))
            .sum();
        able + unknown >= self.thresholds().certificate
    }

    fn max_notar_weight(
        &self,
        _reps: &HashMap<PublicKey, RepSlotVotes>,
        hash: &BlockHash,
        unvoted: Amount,
    ) -> Amount {
        self.notar_tallies.get(hash) + unvoted
    }
}

impl SlotVotes {
    /// RAI: the vote pool of an account domain, which keeps an equivocator's
    /// every first and final vote
    pub fn account_domain() -> Self {
        Self::default()
    }

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
        for tallies in &mut self.tallies {
            tallies.first_tallies.remove(hash);
            tallies.notar_tallies.remove(hash);
            tallies.final_tallies.remove(hash);
        }
    }

    /// Recalculate all tallies in the given committees
    pub fn calculate(&mut self, committees: &Committees) {
        self.tallies = committees
            .iter()
            .map(|committee| CommitteeTallies::calculate(committee.clone(), &self.reps))
            .collect();
    }

    /// The tallies in the instance's own committee
    fn primary(&self) -> Option<&CommitteeTallies> {
        self.tallies.first()
    }

    pub fn first_tallies(&self) -> &BlockTallies {
        self.primary()
            .map_or(&BlockTallies::EMPTY, |t| &t.first_tallies)
    }

    pub fn notar_tallies(&self) -> &BlockTallies {
        self.primary()
            .map_or(&BlockTallies::EMPTY, |t| &t.notar_tallies)
    }

    pub fn final_tallies(&self) -> &BlockTallies {
        self.primary()
            .map_or(&BlockTallies::EMPTY, |t| &t.final_tallies)
    }

    pub fn all_first(&self) -> Amount {
        self.primary().map_or(Amount::ZERO, |t| t.all_first)
    }

    /// Adds every certificate the current tallies support: a notarization or
    /// finalization certificate in every committee.
    pub fn update_certificates(&self, certs: &mut Certificates) {
        let Some(primary) = self.primary() else {
            return;
        };
        for (hash, _) in primary.notar_tallies.iter() {
            if self.tallies.iter().all(|t| t.notarizes(hash)) && !certs.notar.contains(hash) {
                certs.notar.push(*hash);
            }
        }
        if certs.fast.is_none() {
            certs.fast = primary
                .first_tallies
                .iter()
                .find(|(hash, _)| self.tallies.iter().all(|t| t.fast_finalizes(hash)))
                .map(|(hash, _)| *hash);
        }
        if certs.final_.is_none() {
            certs.final_ = primary
                .final_tallies
                .iter()
                .find(|(hash, _)| self.tallies.iter().all(|t| t.finalizes(hash)))
                .map(|(hash, _)| *hash);
        }
    }

    /// An election is settled when no further notarization certificate can
    /// form: a certificate needs every committee, so it is enough that one
    /// of them can not produce it any more. This is a conservative, locally
    /// provable check: a representative that exited silently is still
    /// counted as able to vote.
    pub fn is_settled<'a>(
        &self,
        certs: &Certificates,
        candidates: impl IntoIterator<Item = &'a BlockHash> + Clone,
    ) -> bool {
        if certs.is_finalized() {
            return true;
        }
        // An account domain settles once no additional certificate can form.

        self.tallies
            .iter()
            .any(|t| t.is_settled(&self.reps, certs, candidates.clone()))
    }

    /// Whether a finalization certificate for `hash` can still form: in
    /// every committee (Protocol 1, line 10)
    pub fn can_finalize(&self, hash: &BlockHash) -> bool {
        !self.tallies.is_empty()
            && self
                .tallies
                .iter()
                .all(|t| t.can_finalize(&self.reps, hash))
    }
}

/// The votes this node has cast for one slot, i.e. one account height. It is
/// shared by all elections at that height (Protocol 1: firstVoted, notarized).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalSlotState {
    pub first_voted: Option<BlockHash>,
    pub final_voted: Option<BlockHash>,
    /// This node does not propose in this instance because it left the epoch.
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
            VoteKind::Final => self.final_voted = Some(hash),
        }
    }

    /// notarized ⊆ {hash}: the precondition for a final vote (line 11)
    pub fn notarized_only(&self, hash: &BlockHash) -> bool {
        self.first_voted.is_none_or(|h| h == *hash)
    }

    /// The statements this node made for the given candidates, to be re-signed
    /// for a replica which asks for them (Section 4.2: certificates are handed
    /// to every replica). A statement's identity is (representative, kind, hash),
    /// so re-signing it for a subset of hashes is the same statement.
    pub fn statements_for<'a>(
        &self,
        candidates: impl IntoIterator<Item = &'a BlockHash>,
    ) -> Vec<(VoteKind, Vec<BlockHash>)> {
        let mut first = Vec::new();

        let mut final_ = Vec::new();
        for hash in candidates {
            if self.first_voted == Some(*hash) {
                first.push(*hash);
            }

            if self.final_voted == Some(*hash) {
                final_.push(*hash);
            }
        }
        let mut result = Vec::new();
        if !first.is_empty() {
            result.push((VoteKind::First, first));
        }

        if !final_.is_empty() {
            result.push((VoteKind::Final, final_));
        }
        result
    }

    /// Everything cast so far, for re-broadcasting
    pub fn cast_votes(&self) -> impl Iterator<Item = (BlockHash, VoteKind)> + '_ {
        self.first_voted
            .map(|h| (h, VoteKind::First))
            .into_iter()
            .chain(self.final_voted.map(|h| (h, VoteKind::Final)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::PrivateKey;
    use rustc_hash::FxHashMap;

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
    fn duplicate_first_vote_is_a_replay_and_support_counts_once() {
        let mut pool = SlotVotes::default();
        assert_eq!(pool.add(rep(1), hash(1), VoteKind::First), Ok(()));
        assert_eq!(
            pool.add(rep(1), hash(1), VoteKind::First),
            Err(VoteError::Replay)
        );
        pool.calculate(&committees(&[(1, 10)]));
        assert_eq!(pool.first_tallies().get(&hash(1)), Amount::raw(10));
        assert_eq!(pool.notar_tallies().get(&hash(1)), Amount::raw(10));
        assert_eq!(pool.all_first(), Amount::raw(10));
    }

    /// RAI: an account domain keeps an equivocator's every first and final
    /// vote, so that the certificates it took part in form here too; the
    /// same vote twice is still a replay, and allVotes counts it once
    #[test]
    fn an_account_domain_keeps_an_equivocators_votes_for_every_block() {
        let mut pool = SlotVotes::account_domain();
        assert_eq!(pool.add(rep(1), hash(1), VoteKind::First), Ok(()));
        assert_eq!(pool.add(rep(1), hash(2), VoteKind::First), Ok(()));
        assert_eq!(
            pool.add(rep(1), hash(2), VoteKind::First),
            Err(VoteError::Replay)
        );
        assert_eq!(pool.add(rep(1), hash(1), VoteKind::Final), Ok(()));
        assert_eq!(pool.add(rep(1), hash(2), VoteKind::Final), Ok(()));
        assert_eq!(
            pool.add(rep(1), hash(1), VoteKind::Final),
            Err(VoteError::Replay)
        );
        pool.calculate(&committees(&[(1, 10)]));
        for h in [hash(1), hash(2)] {
            assert_eq!(pool.first_tallies().get(&h), Amount::raw(10));
            assert_eq!(pool.notar_tallies().get(&h), Amount::raw(10));
            assert_eq!(pool.final_tallies().get(&h), Amount::raw(10));
        }
        assert_eq!(pool.all_first(), Amount::raw(10));
        assert!(pool.has_voted_for(&rep(1), &hash(2)));

        // Removing a block drops the equivocating votes for it too
        pool.remove_block(&hash(2));
        assert!(!pool.has_voted_for(&rep(1), &hash(2)));
        assert!(pool.has_voted_for(&rep(1), &hash(1)));
    }

    #[test]
    fn certificates_form_at_their_thresholds_and_never_disappear() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();

        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::First).unwrap();
        pool.calculate(&committees(&[(1, 50), (2, 17), (3, 20)]));
        pool.update_certificates(&mut certs);
        assert_eq!(certs.notar, vec![hash(1)]);
        assert!(certs.is_terminated());
        assert!(!certs.is_finalized());

        pool.add(rep(3), hash(1), VoteKind::First).unwrap();
        pool.calculate(&committees(&[(1, 50), (2, 17), (3, 20)]));
        pool.update_certificates(&mut certs);
        assert_eq!(certs.fast, Some(hash(1)));
        assert_eq!(certs.finalized(), Some(hash(1)));

        // Weights dropping does not revoke a certificate
        pool.calculate(&committees(&[(1, 1), (2, 1), (3, 1)]));
        pool.update_certificates(&mut certs);
        assert_eq!(certs.notar, vec![hash(1)]);
        assert_eq!(certs.fast, Some(hash(1)));
    }

    /// RAI: a joint instance notarizes and finalizes only what both
    /// committees do, and reports the tallies of its own committee
    #[test]
    fn joint_certificates_need_both_committees() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        // Rep 1 carries the own committee, rep 2 the previous one
        let committees = joint(&[(1, 70), (2, 10), (3, 20)], &[(1, 10), (2, 70), (3, 20)]);
        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert_eq!(pool.notar_tallies().get(&hash(1)), Amount::raw(70));
        assert!(certs.notar.is_empty());
        assert!(!certs.is_terminated());

        pool.add(rep(2), hash(1), VoteKind::First).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert_eq!(certs.notar, vec![hash(1)]);
        assert!(certs.fast.is_none());

        // 90 of 100 first votes in both: fast finalized in both
        pool.add(rep(3), hash(1), VoteKind::First).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert_eq!(certs.fast, Some(hash(1)));

        let mut final_pool = SlotVotes::default();
        let mut final_certs = Certificates::default();
        final_pool.add(rep(1), hash(1), VoteKind::Final).unwrap();
        final_pool.calculate(&committees);
        final_pool.update_certificates(&mut final_certs);
        assert!(final_certs.final_.is_none());
        final_pool.add(rep(2), hash(1), VoteKind::Final).unwrap();
        final_pool.calculate(&committees);
        final_pool.update_certificates(&mut final_certs);
        assert_eq!(final_certs.final_, Some(hash(1)));
    }

    /*
     * Test helpers
     */

    fn committees(entries: &[(u64, u128)]) -> Committees {
        Committees::single(Arc::new(Committee::with_online(
            weights(entries),
            Amount::raw(100),
        )))
    }

    /// The instance's own committee and the one of the epoch before, both
    /// for an online weight of 100
    fn joint(own: &[(u64, u128)], previous: &[(u64, u128)]) -> Committees {
        Committees::joint(
            Arc::new(Committee::with_online(weights(own), Amount::raw(100))),
            Arc::new(Committee::with_online(weights(previous), Amount::raw(100))),
        )
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
