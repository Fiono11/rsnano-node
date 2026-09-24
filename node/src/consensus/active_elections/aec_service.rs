use std::{
    collections::{BTreeMap, HashMap},
    sync::RwLock,
    time::Duration,
};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Account, Amount, Block, BlockHash, ConsensusEpoch, PublicKey, QualifiedRoot, SavedBlock,
    VoteError,
};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};

use super::{
    ActiveElectionsConfig, ActiveElectionsContainer, ActiveElectionsInfo, AecCooldownReason,
    AecFact, AecInsertError, AecInsertRequest, ApplyVoteArgs, CommitteeInfo, EpochCloseInfo,
};
use crate::{
    consensus::{
        ElectionCandidateSource,
        election::{
            AccountFrontier, CertificateEvidence, ConfirmedElection, Election, ElectionBehavior,
            ElectionId, ElectionState, EpochSlot, EpochState, FinalStateHash, LocalSlotState,
        },
        vote_generation::VoteTarget,
        vote_rebroadcast::WalletRepsConsumer,
    },
    wallets::WalletRepresentatives,
};

pub struct AecService {
    aec: RwLock<ActiveElectionsContainer>,
    clock: SteadyClock,
}

impl AecService {
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::new(config, base_latency)),
            clock: SteadyClock::default(),
        }
    }

    pub fn new_null() -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::default()),
            clock: SteadyClock::new_null(),
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn retain_close_proposal(&self, prop: rsnano_messages::EpochProp, hash: BlockHash) {
        self.aec.write().unwrap().retain_close_proposal(prop, hash);
    }
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn close_proof(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<rsnano_messages::CloseProofReply> {
        self.aec.read().unwrap().close_proof(epoch)
    }
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn close_committees(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<crate::consensus::election::Committees> {
        self.aec.read().unwrap().close_committees(epoch)
    }
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn next_checkpoint(&self) -> ConsensusEpoch {
        self.aec.read().unwrap().next_checkpoint()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn epoch_decided_state(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<std::sync::Arc<crate::consensus::election::EpochLedger>> {
        self.aec.read().unwrap().epoch_decided_state(epoch)
    }

    // --- Read forwarding ---

    pub fn check_vacancy<T>(&self, source: &T) -> bool
    where
        T: ElectionCandidateSource,
    {
        self.aec.read().unwrap().check_vacancy(source)
    }

    /// The election of the newest epoch for this root
    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<Election> {
        self.aec.read().unwrap().election_for_root(root).cloned()
    }

    pub fn election(&self, id: &ElectionId) -> Option<Election> {
        self.aec.read().unwrap().election(id).cloned()
    }

    /// The elections of all epochs for this root, ascending by epoch
    pub fn elections_for_root(&self, root: &QualifiedRoot) -> Vec<Election> {
        self.aec
            .read()
            .unwrap()
            .elections_for_root(root)
            .cloned()
            .collect()
    }

    /// RAI: the epoch new elections are started in
    pub(crate) fn latest_checkpoint(
        &self,
    ) -> Option<std::sync::Arc<crate::consensus::election::EpochLedger>> {
        self.aec.read().unwrap().latest_checkpoint()
    }

    pub fn checkpoint_snapshot(
        &self,
    ) -> Option<(
        ConsensusEpoch,
        std::sync::Arc<crate::consensus::election::EpochLedger>,
    )> {
        self.aec.read().unwrap().checkpoint_snapshot()
    }

    pub fn checkpoint_locks(&self) -> Option<(ConsensusEpoch, Vec<(Account, u64, BlockHash)>)> {
        self.aec.read().unwrap().checkpoint_locks()
    }

    pub(crate) fn epoch_report_snapshot(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<super::EpochReport> {
        self.aec.read().unwrap().epoch_report(epoch)
    }

    /// RAI: whether this node may sign a new account vote in the epoch
    pub fn signs_account_votes_in(&self, epoch: ConsensusEpoch) -> bool {
        self.aec.read().unwrap().signs_account_votes_in(epoch)
    }

    pub fn current_epoch(&self) -> ConsensusEpoch {
        self.aec.read().unwrap().current_epoch()
    }

    pub fn set_current_epoch(&self, epoch: ConsensusEpoch) {
        self.aec.write().unwrap().set_current_epoch(epoch)
    }

    /// RAI: elections of the current epoch which got a certificate so far
    pub fn decided_in_current_epoch(&self) -> usize {
        self.aec.read().unwrap().decided_in_current_epoch()
    }

    /// RAI: whether this block was finalized explicitly in the given epoch
    pub fn finalized_in_epoch(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> bool {
        self.aec.read().unwrap().finalized_in_epoch(hash, epoch)
    }

    /// RAI: whether this block was finalized explicitly in any epoch
    pub fn is_finalized(&self, hash: &BlockHash) -> bool {
        self.aec.read().unwrap().is_finalized(hash)
    }

    /// RAI: whether this node cast its final vote for the block in the epoch
    pub fn final_voted_in_epoch(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> bool {
        self.aec.read().unwrap().final_voted_in_epoch(hash, epoch)
    }

    /// RAI: the explicitly finalized state per epoch
    pub fn finalized_by_epoch(&self) -> BTreeMap<ConsensusEpoch, FinalStateHash> {
        self.aec.read().unwrap().finalized_by_epoch().clone()
    }

    /// RAI: the blocks finalized explicitly in the given epoch
    pub fn finalized_in(&self, epoch: ConsensusEpoch) -> Vec<(Account, u64, BlockHash)> {
        self.aec.read().unwrap().finalized_in(epoch)
    }

    /// RAI: the committees known here, the genesis one first
    pub fn epoch_committees(&self) -> Vec<CommitteeInfo> {
        self.aec.read().unwrap().epoch_committees()
    }

    /// RAI: the close elections of the epochs this node has left
    pub fn epoch_closes(&self) -> Vec<EpochCloseInfo> {
        self.aec.read().unwrap().epoch_closes()
    }

    /// RAI: the final state of an epoch as it stands on this node
    pub fn epoch_state(&self, epoch: ConsensusEpoch) -> EpochState {
        self.aec.read().unwrap().epoch_state(epoch)
    }

    /// RAI: the frontiers of every account at the end of the setup: the
    /// genesis committee, and the base the epochs' committees are counted on
    pub fn set_genesis_committee(
        &self,
        frontiers: Vec<AccountFrontier>,
        history: crate::consensus::election::EpochLedger,
    ) {
        self.aec
            .write()
            .unwrap()
            .set_genesis_committee(frontiers, Some(history))
    }

    /// RAI: the setup is over, epoch 0 starts now
    pub fn start_epochs(&self) {
        let now = self.clock.now();
        self.aec.write().unwrap().start_epochs(now)
    }

    /// RAI: whether the epochs have started
    pub fn epochs_started(&self) -> bool {
        self.aec.read().unwrap().epochs_started()
    }

    /// RAI: end the current epoch now; it is left once it has drained
    pub fn leave_epoch(&self) {
        let now = self.clock.now();
        self.aec.write().unwrap().leave_epoch(now)
    }

    /// RAI: the current epoch's duration has ended and its instances drain
    pub fn is_draining(&self) -> bool {
        self.aec.read().unwrap().is_draining()
    }

    /// RAI: the epoch whose instances are draining, if any
    pub fn draining_epoch(&self) -> Option<ConsensusEpoch> {
        self.aec.read().unwrap().draining_epoch()
    }

    /// RAI: the close rounds to solicit evidence for now
    pub(crate) fn close_solicitations(&self, now: Timestamp) -> Vec<(ElectionId, BlockHash)> {
        self.aec.write().unwrap().close_solicitations(now)
    }

    /// RAI: start the instance of a block for a vote of its epoch
    pub fn insert_for_vote(&self, block: SavedBlock, epoch: ConsensusEpoch, now: Timestamp) {
        self.aec.write().unwrap().insert_for_vote(block, epoch, now)
    }

    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<Election> {
        self.aec
            .read()
            .unwrap()
            .election_for_block(block_hash)
            .cloned()
    }

    pub fn max_len(&self) -> usize {
        self.aec.read().unwrap().max_len()
    }

    pub fn len(&self) -> usize {
        self.aec.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.aec.read().unwrap().is_empty()
    }

    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.aec.read().unwrap().is_active_root(root)
    }

    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.aec.read().unwrap().is_active_hash(block_hash)
    }

    /// Whether any of the blocks is a candidate of a current election, in one
    /// pass under the lock
    pub fn is_any_active_hash<'a>(&self, hashes: impl Iterator<Item = &'a BlockHash>) -> bool {
        let guard = self.aec.read().unwrap();
        let mut hashes = hashes;
        hashes.any(|hash| guard.is_active_hash(hash))
    }

    pub fn is_priority_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.aec.read().unwrap().is_priority_active_hash(block_hash)
    }

    pub fn was_recently_confirmed(&self, block_hash: &BlockHash) -> bool {
        self.aec.read().unwrap().was_recently_confirmed(block_hash)
    }

    pub fn count_by_behavior(&self, behavior: ElectionBehavior) -> usize {
        self.aec.read().unwrap().count_by_behavior(behavior)
    }

    pub fn vacancy(&self) -> i64 {
        self.aec.read().unwrap().vacancy()
    }

    pub fn info(&self) -> ActiveElectionsInfo {
        let now = self.clock.now();
        self.aec.read().unwrap().info(now)
    }

    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    pub fn round_robin<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&mut dyn Iterator<Item = &Election>) -> T,
    {
        let guard = self.aec.read().unwrap();
        f(&mut guard.iter_round_robin())
    }

    /// Kudzu: the votes to broadcast now, see Protocol 1; `proposal_valid`
    /// tells whether a block may be first voted
    pub(crate) fn kudzu_votes_due(
        &self,
        proposal_valid: impl Fn(&BlockHash) -> Result<(), crate::consensus::Unattached>,
    ) -> Vec<VoteTarget> {
        self.aec.read().unwrap().kudzu_votes_due(proposal_valid)
    }

    /// Kudzu: the signed votes behind the certificates of a terminated election
    pub fn certificate_evidence(
        &self,
        hash: &BlockHash,
        epoch: ConsensusEpoch,
    ) -> Option<(ElectionId, CertificateEvidence)> {
        self.aec.read().unwrap().certificate_evidence(hash, epoch)
    }

    /// RAI: the certified block tree of one epoch as it stands here
    #[cfg(feature = "rai_protocol")]
    pub fn epoch_certified(
        &self,
        epoch: ConsensusEpoch,
    ) -> crate::consensus::election::CertifiedState {
        let inputs = self.aec.read().unwrap().epoch_report_projection(epoch);
        inputs.finish()
    }

    /// RAI: `S_{e-1}` for the close of an epoch, the state its derivation
    /// builds on
    #[cfg(feature = "rai_protocol")]
    /// RAI: the votes of one voter received for one epoch
    pub fn vote_records_of(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
    ) -> Vec<(
        crate::consensus::election::CertifiedBlock,
        crate::consensus::election::ResidualKind,
        BlockHash,
    )> {
        self.aec.read().unwrap().vote_records_of(epoch, voter)
    }

    /// RAI: how many votes of one voter this node holds for an epoch
    pub fn vote_record_count(&self, epoch: ConsensusEpoch, voter: &PublicKey) -> usize {
        self.aec.read().unwrap().vote_record_count(epoch, voter)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn report_block(&self, hash: &BlockHash) -> Option<Block> {
        self.aec.read().unwrap().report_block(hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn first_voted_elsewhere(
        &self,
        epoch: ConsensusEpoch,
        voters: &[PublicKey],
    ) -> Vec<(BlockHash, bool)> {
        self.aec
            .read()
            .unwrap()
            .first_voted_elsewhere(epoch, voters)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn unplaced_signed(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
    ) -> Vec<(BlockHash, crate::consensus::election::ResidualKind)> {
        self.aec.read().unwrap().unplaced_signed(epoch, voter)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn place_signed(
        &self,
        epoch: ConsensusEpoch,
        voter: PublicKey,
        votes: Vec<(
            crate::consensus::election::CertifiedBlock,
            crate::consensus::election::ResidualKind,
            BlockHash,
        )>,
    ) {
        self.aec.write().unwrap().place_signed(epoch, voter, votes);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn signed_votes_for(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
        hashes: &[BlockHash],
    ) -> Vec<std::sync::Arc<rsnano_types::Vote>> {
        self.aec
            .read()
            .unwrap()
            .signed_votes_for(epoch, voter, hashes)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn signed_votes_for_hashes(
        &self,
        epoch: ConsensusEpoch,
        hashes: &[BlockHash],
    ) -> Vec<std::sync::Arc<rsnano_types::Vote>> {
        self.aec
            .read()
            .unwrap()
            .signed_votes_for_hashes(epoch, hashes)
    }

    /// RAI, Rule 3: whether a block's epoch-e NC is predecessor-backed by
    /// the signed first votes retained here
    #[cfg(feature = "rai_protocol")]
    pub fn predecessor_backed(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> bool {
        self.aec.read().unwrap().predecessor_backed(epoch, hash)
    }

    /// RAI: the certificates assembled here from retained signed votes, per
    /// hash, for the votes of one epoch
    #[cfg(feature = "rai_protocol")]
    pub fn certificate_kinds(
        &self,
        epoch: ConsensusEpoch,
        hashes: &[BlockHash],
    ) -> Vec<(BlockHash, crate::consensus::election::CertificateKinds)> {
        self.aec.read().unwrap().certificate_kinds(epoch, hashes)
    }

    pub fn epoch_previous_state(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<std::sync::Arc<crate::consensus::election::EpochLedger>> {
        self.aec.read().unwrap().epoch_previous_state(epoch)
    }

    /// RAI: `O_e = C_{e-2}`, the committee an epoch's reports are counted in
    #[cfg(feature = "rai_protocol")]
    pub fn epoch_committee(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<std::sync::Arc<crate::consensus::election::Committee>> {
        self.aec.read().unwrap().epoch_committee(epoch)
    }

    /// RAI: this node holds what it takes to derive a value for an epoch's
    /// close
    #[cfg(feature = "rai_protocol")]
    pub fn set_close_ready(&self, epoch: ConsensusEpoch, ready: bool) {
        let now = self.clock.now();
        self.aec.write().unwrap().set_close_ready(epoch, ready, now);
    }

    /// RAI: a value this node derived and checked for itself. Deciding it
    /// decides the state derived.
    #[cfg(feature = "rai_protocol")]
    pub fn accept_epoch_value(
        &self,
        value: crate::consensus::election::EpochValue,
        state: std::sync::Arc<crate::consensus::election::EpochLedger>,
    ) -> Option<BlockHash> {
        let now = self.clock.now();
        self.aec
            .write()
            .unwrap()
            .accept_epoch_value(value, state, now)
    }

    /// RAI: whether this node already derived and checked a value
    #[cfg(feature = "rai_protocol")]
    pub fn holds_epoch_value(&self, epoch: ConsensusEpoch, value: &BlockHash) -> bool {
        self.aec.read().unwrap().holds_epoch_value(epoch, value)
    }

    /// RAI: this node proposed a value as the leader of a close round
    #[cfg(feature = "rai_protocol")]
    pub fn record_epoch_proposal(&self, epoch: ConsensusEpoch, round: u32, value: BlockHash) {
        self.aec
            .write()
            .unwrap()
            .record_epoch_proposal(epoch, round, value);
    }

    /// RAI: the close rounds this node leads and has not proposed into yet
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn epoch_proposals_due(&self) -> Vec<super::EpochProposalContext> {
        self.aec.read().unwrap().epoch_proposals_due()
    }

    pub fn is_terminated(&self, id: &ElectionId) -> bool {
        self.aec.read().unwrap().is_terminated(id)
    }

    pub fn slot_state(&self, slot: &EpochSlot) -> Option<LocalSlotState> {
        self.aec.read().unwrap().slot_state(slot).cloned()
    }

    // --- Write forwarding ---

    pub fn set_observer(&self, observer: Sender<AecFact>) {
        self.aec.write().unwrap().set_observer(observer)
    }

    pub fn insert(&self, request: AecInsertRequest, now: Timestamp) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert(request, now)
    }

    pub fn try_add_fork(&self, fork: &Block, fork_tally: Amount) -> bool {
        self.aec.write().unwrap().try_add_fork(fork, fork_tally)
    }

    pub fn apply_vote<'a>(
        &self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        self.aec.write().unwrap().apply_vote(args)
    }

    pub fn transition_time(&self, now: Timestamp) {
        self.aec.write().unwrap().transition_time(now)
    }

    /// Kudzu: record the votes that were handed to the vote generators
    pub(crate) fn mark_kudzu_voted(&self, targets: Vec<VoteTarget>) -> Vec<VoteTarget> {
        self.aec.write().unwrap().mark_kudzu_voted(targets)
    }

    pub fn transition_active(&self, block_hash: &BlockHash) -> bool {
        self.aec.write().unwrap().transition_active(block_hash)
    }

    pub fn refill<T>(&self, source: &mut T, now: Timestamp)
    where
        T: ElectionCandidateSource,
    {
        self.aec.write().unwrap().refill(source, now);
    }

    pub fn remove_votes<'a>(
        &self,
        root: &QualifiedRoot,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        self.aec.write().unwrap().remove_votes(root, voters)
    }

    pub fn erase(&self, root: &QualifiedRoot) -> bool {
        self.aec.write().unwrap().erase(root)
    }

    pub fn confirm_dependent_elections(
        &self,
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>)>,
        now: Timestamp,
    ) {
        self.aec
            .write()
            .unwrap()
            .confirm_dependent_elections(confirmed, now)
    }

    pub fn remove_recently_confirmed(&self, block_hash: &BlockHash) {
        self.aec
            .write()
            .unwrap()
            .remove_recently_confirmed(block_hash)
    }

    pub fn set_cooldown(&self, cool_down: bool, reason: AecCooldownReason) {
        self.aec.write().unwrap().set_cooldown(cool_down, reason)
    }

    pub fn cancel(&self, root: &QualifiedRoot) {
        self.aec.write().unwrap().cancel(root)
    }

    pub fn cancel_all(&self) {
        self.aec.write().unwrap().cancel_all()
    }

    pub fn clear_recently_confirmed(&self) {
        self.aec.write().unwrap().clear_recently_confirmed()
    }

    pub fn stop(&self) {
        self.aec.write().unwrap().stop()
    }

    pub fn force_confirm(&self, block_hash: &BlockHash, now: Timestamp) {
        self.aec.write().unwrap().force_confirm(block_hash, now)
    }

    pub fn simulate_event(&self, event: AecFact) {
        self.aec.read().unwrap().simulate_event(event)
    }

    pub fn snapshot(&self) -> AecSnapshot {
        let now = self.clock.now();
        self.aec.read().unwrap().snapshot(now)
    }
}

impl StatsSource for AecService {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.aec.read().unwrap().collect_stats(result)
    }
}

impl WalletRepsConsumer for AecService {
    fn update_wallet_reps(&self, reps: &WalletRepresentatives) {
        self.aec
            .write()
            .unwrap()
            .set_local_representatives(reps.rep_pub_keys().collect());
    }
}

impl ContainerInfoProvider for AecService {
    fn container_info(&self) -> ContainerInfo {
        self.aec.read().unwrap().container_info()
    }
}

#[derive(Default)]
pub struct AecSnapshot {
    pub buckets: Vec<BucketSnapshot>,
}

pub struct BucketSnapshot {
    pub bucket_index: usize,
    pub election_count: usize,
    pub elections: Vec<ElectionSnapshot>,
}

pub struct ElectionSnapshot {
    pub winner_hash: BlockHash,
    pub non_final_tally: Amount,
    pub final_tally: Amount,
    pub root: QualifiedRoot,
    pub account: Account,
    pub state: ElectionState,
    pub candidate_blocks: Vec<BlockHash>,
    pub is_final: bool,
    pub elapsed: Duration,
}
