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
    /// RAI: the two committees of a joint epoch election notarized
    /// different values. The slot is abandoned like a timed out one: no
    /// joint certificate can form for either value, and the two
    /// notarization certificates are themselves the skip evidence.
    pub conflict: bool,
    /// Fast finalization certificate
    pub fast: Option<BlockHash>,
    /// Finalization certificate
    pub final_: Option<BlockHash>,
}

impl Certificates {
    /// Protocol 1, lines 9–13: the slot is done
    pub fn is_terminated(&self) -> bool {
        self.has_block() || self.timeout || self.conflict
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
    /// The same holds once the two committees of a joint election have
    /// notarized different values: neither can gather a joint certificate.
    pub fn explicit_finalization_possible(&self) -> bool {
        !self.timeout && !self.conflict
    }
}

/// The state of a Kudzu instance given its certificates and votes: finalized,
/// or terminated by a block in the tree or a timeout certificate, and settled
/// once no further notarization certificate can form
pub fn kudzu_state<'a>(
    current: ElectionState,
    votes: &SlotVotes,
    certs: &Certificates,
    candidates: impl IntoIterator<Item = &'a BlockHash> + Clone,
) -> ElectionState {
    if certs.is_finalized() {
        ElectionState::Confirmed
    } else if !certs.is_terminated() {
        current
    } else if votes.is_settled(certs, candidates) {
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
    /// allVotes(firstVote), the abstaining first votes for the timeout block included
    all_first: Amount,
    timeout_weight: Amount,
}

impl CommitteeTallies {
    fn calculate(committee: Arc<Committee>, reps: &HashMap<PublicKey, RepSlotVotes>) -> Self {
        let weighted = || {
            reps.iter()
                .map(|(voter, rep)| (rep, committee.weight(voter)))
        };
        let mut first_tallies = BlockTallies::new();
        first_tallies.calculate_from(weighted().filter_map(|(r, w)| r.first.map(|h| (h, w))));
        let mut notar_tallies = BlockTallies::new();
        notar_tallies
            .calculate_from(weighted().flat_map(|(r, w)| r.notar.iter().map(move |h| (*h, w))));
        let mut final_tallies = BlockTallies::new();
        final_tallies.calculate_from(weighted().filter_map(|(r, w)| r.final_.map(|h| (h, w))));
        let all_first = first_tallies.sum();
        let timeout_weight = weighted().filter(|(r, _)| r.timeout).map(|(_, w)| w).sum();
        Self {
            committee,
            first_tallies,
            notar_tallies,
            final_tallies,
            all_first,
            timeout_weight,
        }
    }

    fn thresholds(&self) -> &KudzuThresholds {
        self.committee.thresholds()
    }

    fn has_many(&self, hash: &BlockHash) -> bool {
        self.first_tallies.get(hash) >= self.thresholds().many
    }

    /// Protocol 1, line 32: allVotes(firstVote) − maxVotes(firstVote) ≥ f + p + 1.
    /// maxVotes is the most first votes on a *non-timeout* block (Section
    /// 4.7: "1, 2, 3, 4 first votes on B1, B2, B3, B_timeout: allVotes = 10,
    /// maxVotes = 3"). Counting the timeout block deadlocked a slot where more
    /// representatives abstained than proposed, short of a certificate either
    /// way: the proposers never timed out, because the abstains were the
    /// maximum they were measured against.
    fn should_timeout(&self) -> bool {
        let max_votes = self
            .first_tallies
            .iter()
            .filter(|(hash, _)| *hash != TIMEOUT_BLOCK)
            .map(|(_, tally)| *tally)
            .max()
            .unwrap_or_default();
        self.all_first - max_votes >= self.thresholds().many
    }

    fn notarizes(&self, hash: &BlockHash) -> bool {
        self.notar_tallies.get(hash) >= self.thresholds().certificate
    }

    fn times_out(&self) -> bool {
        self.timeout_weight >= self.thresholds().certificate
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
        // and those known without a first vote which have not exited. An
        // abstaining first vote for the timeout block counts as cast; a
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
        // A representative that timed out never casts a final vote (line 10)
        let able: Amount = reps
            .iter()
            .filter(|(_, r)| {
                !r.timeout
                    && r.notar.iter().all(|h| h == hash)
                    && r.final_.is_none_or(|h| h == *hash)
            })
            .map(|(voter, _)| self.committee.weight(voter))
            .sum();
        able + unknown >= self.thresholds().certificate
    }

    fn max_notar_weight(
        &self,
        reps: &HashMap<PublicKey, RepSlotVotes>,
        hash: &BlockHash,
        unvoted: Amount,
    ) -> Amount {
        // Any representative that may still first vote may first vote this
        // block, so the block may still reach many first votes
        let can_reach_many = self.first_tallies.get(hash) + unvoted >= self.thresholds().many;
        // Of that weight, what notarized the block already (a notarization
        // vote without a first vote: not an honest one) is in the tally and
        // does not count twice
        let notarized_without_first: Amount = reps
            .iter()
            .filter(|(_, r)| r.first.is_none() && r.final_.is_none() && r.notar.contains(hash))
            .map(|(voter, _)| self.committee.weight(voter))
            .sum();
        let could_first_vote = unvoted
            .checked_sub(notarized_without_first)
            .unwrap_or_default();
        let mut result = self.notar_tallies.get(hash) + could_first_vote;
        if can_reach_many {
            // Every representative that has first voted and not exited could
            // still take a second look (line 28), abstaining ones included
            result += reps
                .iter()
                .filter(|(_, r)| r.first.is_some() && r.final_.is_none() && !r.notar.contains(hash))
                .map(|(voter, _)| self.committee.weight(voter))
                .sum();
        }
        result
    }
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

    pub fn timeout_weight(&self) -> Amount {
        self.primary().map_or(Amount::ZERO, |t| t.timeout_weight)
    }

    /// manyVotes(firstVote): the blocks with at least f + p + 1 first votes in
    /// some committee. The timeout block is not looked at, its notarization is
    /// the timeout vote (lines 32-35).
    pub fn many_votes(&self) -> Vec<BlockHash> {
        let mut result = Vec::new();
        for tallies in &self.tallies {
            for (hash, _) in tallies.first_tallies.iter() {
                if *hash != TIMEOUT_BLOCK && tallies.has_many(hash) && !result.contains(hash) {
                    result.push(*hash);
                }
            }
        }
        result
    }

    /// Protocol 1, line 32: allVotes(firstVote) − maxVotes(firstVote) ≥ f + p + 1
    /// in some committee, maxVotes over the non-timeout blocks
    pub fn should_timeout(&self) -> bool {
        self.tallies.iter().any(|t| t.should_timeout())
    }

    /// Adds every certificate the current tallies support: a notarization or
    /// finalization certificate in every committee, a timeout certificate in
    /// one of them
    pub fn update_certificates(&self, certs: &mut Certificates) {
        let Some(primary) = self.primary() else {
            return;
        };
        for (hash, _) in primary.notar_tallies.iter() {
            if *hash != TIMEOUT_BLOCK
                && self.tallies.iter().all(|t| t.notarizes(hash))
                && !certs.notar.contains(hash)
            {
                certs.notar.push(*hash);
            }
        }
        // The notarization certificate of the timeout block
        if self.tallies.iter().any(|t| t.times_out()) {
            certs.timeout = true;
        }
        certs.conflict |= self.committees_notarize_different_values();
        if certs.fast.is_none() {
            certs.fast = primary
                .first_tallies
                .iter()
                .find(|(hash, _)| {
                    *hash != TIMEOUT_BLOCK && self.tallies.iter().all(|t| t.fast_finalizes(hash))
                })
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

    /// RAI, the cross-committee conflict clause of ET: two committees of a
    /// joint election hold notarization certificates for different values.
    /// The slot is abandoned, and the two certificates are the skip
    /// evidence.
    ///
    /// Nothing can be decided in such a slot, so abandoning it loses
    /// nothing. A value x is decided by a finalization certificate in every
    /// committee, and a validator final votes x only if it notarized x
    /// alone. A committee holding a certificate for some h != x therefore
    /// has q validators which can not final vote x, leaving at most
    /// N - q = f + p of them, short of the q a certificate needs. So a
    /// committee certifying h can only ever finalize h, and with two
    /// committees certifying different values no value can be finalized in
    /// both. A committee that certified a second value can not finalize
    /// either of them, which is the same argument once more.
    pub fn committees_notarize_different_values(&self) -> bool {
        if self.tallies.len() < 2 {
            return false;
        }
        let notarized = |tallies: &CommitteeTallies| -> Vec<BlockHash> {
            tallies
                .notar_tallies
                .iter()
                .filter(|(hash, _)| *hash != TIMEOUT_BLOCK && tallies.notarizes(hash))
                .map(|(hash, _)| *hash)
                .collect()
        };
        let certified: Vec<Vec<BlockHash>> = self.tallies.iter().map(notarized).collect();
        certified.iter().enumerate().any(|(i, ours)| {
            certified[i + 1..].iter().any(|theirs| {
                ours.iter()
                    .any(|hash| theirs.iter().any(|other| other != hash))
            })
        })
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
        if !certs.is_terminated() {
            return false;
        }
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
    fn one_first_vote_per_rep_and_it_counts_as_notarization() {
        let mut pool = SlotVotes::default();
        assert_eq!(pool.add(rep(1), hash(1), VoteKind::First), Ok(()));
        assert_eq!(
            pool.add(rep(1), hash(2), VoteKind::First),
            Err(VoteError::Replay)
        );
        pool.calculate(&committees(&[(1, 10)]));
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
        pool.calculate(&committees(&[(1, 10)]));
        assert_eq!(pool.timeout_weight(), Amount::raw(10));
        assert_eq!(pool.final_tallies().get(&hash(1)), Amount::raw(10));
        // A final vote supplies notarization weight but is not a first vote
        assert_eq!(pool.notar_tallies().get(&hash(1)), Amount::raw(10));
        assert_eq!(pool.all_first(), Amount::ZERO);
    }

    #[test]
    fn many_votes_and_timeout_rule() {
        // f + p + 1 = 39 of 100
        let mut pool = SlotVotes::default();
        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(2), VoteKind::First).unwrap();
        pool.add(rep(3), hash(3), VoteKind::First).unwrap();
        pool.calculate(&committees(&[(1, 40), (2, 25), (3, 14)]));

        assert_eq!(pool.many_votes(), vec![hash(1)]);
        // allVotes − maxVotes = 79 − 40 = 39 ≥ 39
        assert!(pool.should_timeout());

        pool.calculate(&committees(&[(1, 40), (2, 25), (3, 13)]));
        assert!(!pool.should_timeout());
    }

    /// The split that deadlocked a run with one representative silent: three
    /// abstained (52 of 100), two proposed (32), neither side a certificate.
    /// The abstains are the most first votes of all, but maxVotes counts the
    /// non-timeout blocks only, so the proposers time out and the timeout
    /// certificate forms.
    #[test]
    fn proposers_time_out_when_the_abstains_outnumber_them() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let committees = committees(&[(1, 20), (2, 16), (3, 16), (4, 16), (5, 16), (6, 16)]);
        pool.add(rep(1), hash(1), VoteKind::Abstain).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Abstain).unwrap();
        pool.add(rep(3), hash(1), VoteKind::Abstain).unwrap();
        pool.add(rep(4), hash(1), VoteKind::First).unwrap();
        pool.add(rep(5), hash(1), VoteKind::First).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert!(!certs.is_terminated());
        // allVotes 84 − maxVotes 32 = 52 ≥ 39
        assert!(pool.should_timeout());

        pool.add(rep(4), hash(1), VoteKind::Timeout).unwrap();
        pool.add(rep(5), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert!(certs.timeout);
        assert!(certs.is_terminated());
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

    #[test]
    fn final_and_timeout_certificates() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        pool.add(rep(1), hash(1), VoteKind::Final).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Final).unwrap();
        pool.add(rep(1), hash(1), VoteKind::Timeout).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&committees(&[(1, 50), (2, 17)]));
        pool.update_certificates(&mut certs);
        assert_eq!(certs.final_, Some(hash(1)));
        assert!(certs.timeout);
        assert!(!certs.explicit_finalization_possible());
    }

    #[test]
    fn settled_when_no_other_candidate_can_be_notarized() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let candidates = [hash(1), hash(2)];
        let committees = committees(&[(1, 40), (2, 27), (3, 33)]);

        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Notar).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert!(certs.is_notarized(&hash(1)));
        // 60 of weight has not first voted yet and could notarize anything
        assert!(!pool.is_settled(&certs, &candidates));

        pool.add(rep(3), hash(2), VoteKind::First).unwrap();
        pool.calculate(&committees);
        // hash 2 can still reach f + p + 1 first votes, so rep 1 could take a second look
        assert!(!pool.is_settled(&certs, &candidates));

        pool.add(rep(1), hash(1), VoteKind::Final).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert!(!certs.is_finalized());
        // rep 1 exited: hash 2 can gather at most 33 + 27 = 60 < 67
        assert!(pool.is_settled(&certs, &candidates));
    }

    /// RAI: a representative that abstained has left the instance and will
    /// not first vote any more, one that merely timed out may have first
    /// voted a block we do not hold
    #[test]
    fn abstained_weight_settles_a_timed_out_instance() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let committees = committees(&[(1, 30), (2, 32), (3, 38)]);

        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Abstain).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert!(!certs.is_terminated());
        // 38 of weight is still unknown and could notarize anything
        assert!(!pool.is_settled(&certs, &[hash(1)]));

        pool.add(rep(3), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert!(certs.timeout);
        // rep 3 timed out but may have first voted a block we have not seen
        assert!(!pool.is_settled(&certs, &[hash(1)]));

        let mut abstaining = SlotVotes::default();
        abstaining.add(rep(1), hash(1), VoteKind::First).unwrap();
        abstaining.add(rep(2), hash(1), VoteKind::Abstain).unwrap();
        abstaining.add(rep(3), hash(1), VoteKind::Abstain).unwrap();
        abstaining.calculate(&committees);
        // 70 of weight left for good: hash 1 stays at 30
        assert!(abstaining.is_settled(&certs, &[hash(1)]));
    }

    /// RAI: a representative that timed out never casts a final vote
    #[test]
    fn timed_out_weight_cannot_finalize() {
        let mut pool = SlotVotes::default();
        let committees = committees(&[(1, 40), (2, 27), (3, 33)]);
        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::First).unwrap();
        pool.add(rep(3), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&committees);
        assert!(pool.can_finalize(&hash(1)));

        pool.add(rep(1), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&committees);
        assert!(!pool.can_finalize(&hash(1)));
    }

    /// A 3-3 fork whose representatives all timed out settles once every
    /// representative has taken its second look at the other block (line 28)
    #[test]
    fn settled_once_every_representative_took_its_second_look() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let committees = committees(&[(1, 17), (2, 17), (3, 17), (4, 17), (5, 16), (6, 16)]);
        for rep_id in 1..=3 {
            pool.add(rep(rep_id), hash(1), VoteKind::First).unwrap();
        }
        for rep_id in 4..=6 {
            pool.add(rep(rep_id), hash(2), VoteKind::First).unwrap();
        }
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert!(!certs.is_terminated());
        assert!(!pool.is_settled(&certs, &[hash(1), hash(2)]));

        for rep_id in 1..=6 {
            pool.add(rep(rep_id), hash(1), VoteKind::Timeout).unwrap();
        }
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert!(certs.timeout);
        assert!(certs.notar.is_empty());
        // Both blocks have many first votes, every representative still looks
        assert!(!pool.is_settled(&certs, &[hash(1), hash(2)]));

        for rep_id in 1..=3 {
            pool.add(rep(rep_id), hash(2), VoteKind::Notar).unwrap();
        }
        for rep_id in 4..=6 {
            pool.add(rep(rep_id), hash(1), VoteKind::Notar).unwrap();
        }
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert_eq!(certs.notar.len(), 2);
        assert!(pool.is_settled(&certs, &[hash(1), hash(2)]));
    }

    #[test]
    fn not_settled_while_a_second_look_is_still_possible() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::First).unwrap();
        pool.add(rep(3), hash(2), VoteKind::First).unwrap();
        pool.calculate(&committees(&[(1, 40), (2, 27), (3, 39)]));
        pool.update_certificates(&mut certs);
        // hash 2 has f + p + 1 first votes, rep 1 and 2 may still take a second look
        assert!(!pool.is_settled(&certs, &[hash(1), hash(2)]));

        // Once they exited with a final vote they cannot
        pool.add(rep(1), hash(1), VoteKind::Final).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Final).unwrap();
        pool.calculate(&committees(&[(1, 40), (2, 27), (3, 39)]));
        pool.update_certificates(&mut certs);
        assert!(certs.is_finalized());
        assert!(pool.is_settled(&certs, &[hash(1), hash(2)]));
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
        pool.calculate(&committees(&[(1, 10)]));
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

    /// RAI: a joint instance times out once one committee does, and the
    /// rules with a liveness role (many votes, the timeout rule) hold once
    /// they hold in one committee
    #[test]
    fn joint_timeout_and_many_votes_need_one_committee() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let committees = joint(&[(1, 70), (2, 10), (3, 20)], &[(1, 10), (2, 70), (3, 20)]);
        pool.add(rep(1), hash(1), VoteKind::Timeout).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert!(certs.timeout);
        assert!(certs.is_terminated());

        // Many first votes for hash 2 in the previous committee only
        let mut looking = SlotVotes::default();
        looking.add(rep(1), hash(1), VoteKind::First).unwrap();
        looking.add(rep(2), hash(2), VoteKind::First).unwrap();
        looking.calculate(&committees);
        assert_eq!(looking.many_votes(), vec![hash(1), hash(2)]);
        // allVotes − maxVotes: 10 in the own committee, 10 in the previous: no timeout
        assert!(!looking.should_timeout());
        looking.add(rep(3), hash(3), VoteKind::First).unwrap();
        looking.calculate(&committees);
        // 30 ≥ 39 in neither committee
        assert!(!looking.should_timeout());
        let split = joint(&[(1, 40), (2, 25), (3, 14)], &[(1, 60), (2, 20), (3, 20)]);
        looking.calculate(&split);
        // 39 in the own committee is enough
        assert!(looking.should_timeout());
    }

    /// RAI: a joint instance settles once one committee can not notarize
    /// anything more, and can finalize only while both still can
    #[test]
    fn joint_settled_needs_one_committee_and_finalizable_both() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let candidates = [hash(1), hash(2)];
        // Own: rep 1 alone certifies; previous: reps 1 and 2 together
        let committees = joint(&[(1, 70), (2, 10), (3, 20)], &[(1, 40), (2, 40), (3, 20)]);
        pool.add(rep(1), hash(1), VoteKind::First).unwrap();
        pool.add(rep(2), hash(1), VoteKind::First).unwrap();
        pool.add(rep(1), hash(1), VoteKind::Final).unwrap();
        pool.add(rep(2), hash(1), VoteKind::Final).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert_eq!(certs.notar, vec![hash(1)]);
        assert!(certs.is_finalized());
        assert!(pool.is_settled(&certs, &candidates));

        // Without rep 2's final vote: the own committee alone finalizes, the
        // previous one still needs rep 2, which may still cast it
        let mut open = SlotVotes::default();
        let mut open_certs = Certificates::default();
        open.add(rep(1), hash(1), VoteKind::First).unwrap();
        open.add(rep(2), hash(1), VoteKind::First).unwrap();
        open.add(rep(1), hash(1), VoteKind::Final).unwrap();
        open.calculate(&committees);
        open.update_certificates(&mut open_certs);
        assert!(!open_certs.is_finalized());
        assert!(open.can_finalize(&hash(1)));
        // Rep 2 timed out: it will never final vote, the previous committee
        // can not finalize any more
        open.add(rep(2), hash(1), VoteKind::Timeout).unwrap();
        open.calculate(&committees);
        assert!(!open.can_finalize(&hash(1)));
        // Settled: in the own committee rep 1 exited and rep 3 alone can
        // not notarize hash 2, whatever the previous committee allows
        assert!(open.is_settled(&open_certs, &candidates));
    }

    /// A fork with one candidate notarized, the other short of a certificate,
    /// where a representative final voted the loser without a first vote:
    /// its weight is in the loser's tally already and does not count as
    /// weight that could still notarize it. Only a Byzantine representative
    /// does that; the instance settled nowhere until it was counted once.
    #[test]
    fn settled_when_the_only_weight_left_already_notarized_the_loser() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let committees = committees(&[(1, 20), (2, 16), (3, 16), (4, 16), (5, 16), (6, 16)]);
        let (winner, loser) = (hash(1), hash(2));
        for rep_id in 1..=3 {
            pool.add(rep(rep_id), winner, VoteKind::First).unwrap();
            pool.add(rep(rep_id), winner, VoteKind::Final).unwrap();
        }
        for rep_id in 4..=5 {
            pool.add(rep(rep_id), loser, VoteKind::First).unwrap();
            pool.add(rep(rep_id), winner, VoteKind::Notar).unwrap();
        }
        pool.add(rep(6), loser, VoteKind::Final).unwrap();
        pool.add(rep(6), loser, VoteKind::Timeout).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert_eq!(certs.notar, vec![winner]);
        assert!(!certs.is_finalized());
        // The loser holds 48: reps 4 and 5 first voted it, rep 6 final voted
        // it. Rep 6 never first voted, but it can not add its 16 again.
        assert!(pool.is_settled(&certs, &[winner, loser]));
    }

    /// A representative that final voted the winner without a first vote
    /// (line 11: the block was in its tree, it never proposed in this
    /// instance) has exited: its weight can not first vote the loser any
    /// more. Counting it kept a fork instance unsettled for good.
    #[test]
    fn settled_when_the_weight_without_a_first_vote_has_exited() {
        let mut pool = SlotVotes::default();
        let mut certs = Certificates::default();
        let committees = committees(&[(1, 16), (2, 16), (3, 20), (4, 16), (5, 16), (6, 16)]);
        let (winner, loser) = (hash(1), hash(2));
        // Reps 1 and 5 first voted the loser, then notarized the winner
        for rep_id in [1, 5] {
            pool.add(rep(rep_id), loser, VoteKind::First).unwrap();
            pool.add(rep(rep_id), winner, VoteKind::Notar).unwrap();
        }
        // Reps 2 and 4 first voted the winner and exited
        for rep_id in [2, 4] {
            pool.add(rep(rep_id), winner, VoteKind::First).unwrap();
            pool.add(rep(rep_id), winner, VoteKind::Final).unwrap();
        }
        // Rep 3 exited with a final vote for the winner, without a first vote
        pool.add(rep(3), winner, VoteKind::Final).unwrap();
        // Rep 6 first voted the winner and timed out
        pool.add(rep(6), winner, VoteKind::First).unwrap();
        pool.add(rep(6), winner, VoteKind::Timeout).unwrap();
        pool.calculate(&committees);
        pool.update_certificates(&mut certs);
        assert_eq!(certs.notar, vec![winner]);
        // The loser holds 32 of first votes; rep 3's 20 can not join them,
        // and 32 is short of many, so nobody takes a second look at it
        assert!(pool.is_settled(&certs, &[winner, loser]));
    }

    /*
     * Test helpers
     */

    /// A single committee for an online weight of 100: certificate 62,
    /// fast 81, many 39
    /// RAI: a joint epoch election abandons a slot when its two committees
    /// notarize different values. No joint certificate can form for either,
    /// and the two notarization certificates are the skip evidence.
    #[test]
    fn two_committees_notarizing_different_values_abandon_the_slot() {
        let mut votes = SlotVotes::default();
        // The old committee is representatives 1-3, the new one 4-6; each
        // committee notarizes a different value with its own 67 of weight
        for voter in [1, 2] {
            votes.add(rep(voter), hash(1), VoteKind::First).unwrap();
        }
        for voter in [4, 5] {
            votes.add(rep(voter), hash(2), VoteKind::First).unwrap();
        }
        let committees = joint(&[(1, 40), (2, 27), (3, 33)], &[(4, 40), (5, 27), (6, 33)]);
        votes.calculate(&committees);
        let mut certs = Certificates::default();
        votes.update_certificates(&mut certs);

        assert!(votes.committees_notarize_different_values());
        assert!(certs.conflict);
        // No joint notarization certificate formed for either value
        assert!(certs.notar.is_empty());
        assert!(certs.is_terminated());
        assert!(!certs.explicit_finalization_possible());
    }

    /// The same value in both committees is no conflict, and it notarizes
    #[test]
    fn two_committees_notarizing_the_same_value_do_not_conflict() {
        let mut votes = SlotVotes::default();
        for voter in [1, 2, 4, 5] {
            votes.add(rep(voter), hash(1), VoteKind::First).unwrap();
        }
        let committees = joint(&[(1, 40), (2, 27), (3, 33)], &[(4, 40), (5, 27), (6, 33)]);
        votes.calculate(&committees);
        let mut certs = Certificates::default();
        votes.update_certificates(&mut certs);

        assert!(!votes.committees_notarize_different_values());
        assert!(!certs.conflict);
        assert_eq!(certs.notar, vec![hash(1)]);
    }

    /// A committee that certified a second value can no longer finalize
    /// either of them: no validator of it notarized one alone, so the slot
    /// is abandoned even though the committees share a value
    #[test]
    fn a_committee_certifying_a_second_value_conflicts() {
        let mut votes = SlotVotes::default();
        // Both committees certify hash 1; the old one also certifies hash 2
        for voter in [1, 2, 4, 5] {
            votes.add(rep(voter), hash(1), VoteKind::First).unwrap();
        }
        for voter in [1, 2] {
            votes.add(rep(voter), hash(2), VoteKind::Notar).unwrap();
        }
        let committees = joint(&[(1, 40), (2, 27), (3, 33)], &[(4, 40), (5, 27), (6, 33)]);
        votes.calculate(&committees);
        let mut certs = Certificates::default();
        votes.update_certificates(&mut certs);

        assert!(votes.committees_notarize_different_values());
        assert!(certs.conflict);
        // The shared value is jointly notarized, but neither committee can
        // finalize it any more, so the slot is abandoned
        assert_eq!(certs.notar, vec![hash(1)]);
        assert!(!certs.explicit_finalization_possible());
    }

    /// One committee notarizing while the other has not yet is no conflict:
    /// the second may still notarize the same value
    #[test]
    fn one_committee_ahead_of_the_other_is_no_conflict() {
        let mut votes = SlotVotes::default();
        for voter in [1, 2] {
            votes.add(rep(voter), hash(1), VoteKind::First).unwrap();
        }
        votes.add(rep(4), hash(1), VoteKind::First).unwrap();
        let committees = joint(&[(1, 40), (2, 27), (3, 33)], &[(4, 40), (5, 27), (6, 33)]);
        votes.calculate(&committees);
        let mut certs = Certificates::default();
        votes.update_certificates(&mut certs);

        assert!(!votes.committees_notarize_different_values());
        assert!(!certs.conflict);
        assert!(certs.notar.is_empty());
    }

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
