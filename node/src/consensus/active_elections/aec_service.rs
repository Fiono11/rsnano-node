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

    /// RAI: whether this node may sign a new account vote in the epoch
    pub fn signs_account_votes_in(&self, epoch: ConsensusEpoch) -> bool {
        self.aec.read().unwrap().signs_account_votes_in(epoch)
    }

    /// RAI: the epoch new elections are started in
    pub fn current_epoch(&self) -> ConsensusEpoch {
        self.aec.read().unwrap().current_epoch()
    }

    /// RAI: the members of the committees around the current epoch
    pub fn committee_members(&self) -> std::collections::HashSet<rsnano_types::PublicKey> {
        self.aec.read().unwrap().committee_members()
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

    /// RAI: the latest checkpoint decided here
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn latest_checkpoint(
        &self,
    ) -> Option<std::sync::Arc<crate::consensus::election::EpochLedger>> {
        self.aec.read().unwrap().latest_checkpoint()
    }

    /// RAI: the close elections of the epochs this node has left
    pub fn epoch_closes(&self) -> Vec<EpochCloseInfo> {
        self.aec.read().unwrap().epoch_closes()
    }

    /// RAI: the close rounds to solicit evidence for now
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn close_solicitations(&self, now: Timestamp) -> Vec<(ElectionId, BlockHash)> {
        self.aec.write().unwrap().close_solicitations(now)
    }

    /// RAI: the committees known here, the genesis one first
    pub fn epoch_committees(&self) -> Vec<CommitteeInfo> {
        self.aec.read().unwrap().epoch_committees()
    }

    /// RAI: the final state of an epoch as it stands on this node
    pub fn epoch_state(&self, epoch: ConsensusEpoch) -> EpochState {
        self.aec.read().unwrap().epoch_state(epoch)
    }

    /// RAI: the frontiers of every account at the end of the setup: the
    /// genesis committee, and the base the epochs' committees are counted on
    pub fn set_genesis_committee(&self, frontiers: Vec<AccountFrontier>) {
        self.aec.write().unwrap().set_genesis_committee(frontiers)
    }

    /// RAI: the confirmed setup history, added to the genesis state
    pub fn set_genesis_history(
        &self,
        history: Vec<(
            crate::consensus::election::AccountSlot,
            BlockHash,
            BlockHash,
        )>,
    ) {
        self.aec.write().unwrap().set_genesis_history(history)
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

    /// RAI: the current epoch's duration has ended and its instances drain
    pub fn is_draining(&self) -> bool {
        self.aec.read().unwrap().is_draining()
    }

    /// RAI: the epoch whose instances are draining, if any
    pub fn draining_epoch(&self) -> Option<ConsensusEpoch> {
        self.aec.read().unwrap().draining_epoch()
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
        // Collected under the lock, built after it: building hashes every
        // entry of the cumulative state, which vote application must not
        // wait for
        let parts = self.aec.read().unwrap().epoch_certified_parts(epoch);
        parts.build()
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

    pub fn epoch_previous_state(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<std::sync::Arc<crate::consensus::election::EpochLedger>> {
        self.aec.read().unwrap().epoch_previous_state(epoch)
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
    /// RAI: whether this node has decided an epoch's state
    /// RAI, checkpoint catch-up: the value an epoch's close finalized here,
    /// with its state
    pub fn decided_value(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<(
        crate::consensus::election::EpochValue,
        std::sync::Arc<crate::consensus::election::EpochLedger>,
    )> {
        self.aec.read().unwrap().decided_value(epoch)
    }

    /// RAI, checkpoint catch-up: adopt a certified value with the state
    /// another replica sent for it
    pub fn adopt_certified_epoch_value(
        &self,
        value: crate::consensus::election::EpochValue,
        state: std::sync::Arc<crate::consensus::election::EpochLedger>,
    ) -> bool {
        let now = self.clock.now();
        self.aec
            .write()
            .unwrap()
            .adopt_certified_epoch_value(value, state, now)
    }

    pub fn epoch_decided(&self, epoch: ConsensusEpoch) -> bool {
        self.aec
            .read()
            .unwrap()
            .epoch_decided_state(epoch)
            .is_some()
    }

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

    /// RAI: the members whose usable reports of the epoch this node holds
    #[cfg(feature = "rai_protocol")]
    pub fn set_close_reporters(&self, epoch: ConsensusEpoch, reporters: Vec<PublicKey>) {
        self.aec
            .write()
            .unwrap()
            .set_close_reporters(epoch, reporters);
    }

    /// RAI: a leader's proposal is being checked here; its round's timeout
    /// waits for the check
    #[cfg(feature = "rai_protocol")]
    pub fn mark_epoch_proposal_checking(
        &self,
        epoch: ConsensusEpoch,
        round: u32,
        value: BlockHash,
    ) {
        self.aec
            .write()
            .unwrap()
            .mark_epoch_proposal_checking(epoch, round, value);
    }

    /// RAI: the check of a proposal ended without a value to vote for
    #[cfg(feature = "rai_protocol")]
    pub fn clear_epoch_proposal_checking(
        &self,
        epoch: ConsensusEpoch,
        round: u32,
        value: &BlockHash,
    ) {
        self.aec
            .write()
            .unwrap()
            .clear_epoch_proposal_checking(epoch, round, value);
    }

    /// RAI: the state a value with the same payload, checked here in another
    /// slot, decides
    #[cfg(feature = "rai_protocol")]
    pub fn epoch_state_for_payload(
        &self,
        value: &crate::consensus::election::EpochValue,
    ) -> Option<std::sync::Arc<crate::consensus::election::EpochLedger>> {
        self.aec.read().unwrap().epoch_state_for_payload(value)
    }

    /// RAI: a validated child of election genesis of an epoch's close, for
    /// its leader to propose again
    #[cfg(feature = "rai_protocol")]
    pub fn validated_epoch_genesis_child(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<(
        crate::consensus::election::EpochValue,
        std::sync::Arc<crate::consensus::election::EpochLedger>,
    )> {
        self.aec
            .read()
            .unwrap()
            .validated_epoch_genesis_child(epoch)
    }

    /// RAI: the close rounds this node leads and has not proposed into yet
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn epoch_proposals_due(&self) -> Vec<super::EpochProposalContext> {
        self.aec.read().unwrap().epoch_proposals_due()
    }

    /// RAI: the certificates the signed votes held here assemble for each
    /// block. A report check asks for every entry of an epoch, tens of
    /// thousands, so the lock is taken per chunk and vote application gets
    /// its turn in between. A later chunk can only see more: a committee
    /// once known stays known and certificates are never withdrawn.
    #[cfg(feature = "rai_protocol")]
    pub fn certificate_kinds(
        &self,
        epoch: ConsensusEpoch,
        hashes: &[BlockHash],
    ) -> Option<Vec<crate::consensus::election::CertificateKinds>> {
        const CHUNK: usize = 1024;
        let mut kinds = Vec::with_capacity(hashes.len());
        for chunk in hashes.chunks(CHUNK) {
            let aec = self.aec.read().unwrap();
            for hash in chunk {
                kinds.push(aec.certificate_kinds(epoch, hash)?);
            }
        }
        Some(kinds)
    }

    /// RAI: a boundary report completed with its epoch's inherited base
    #[cfg(feature = "rai_protocol")]
    pub fn complete_report(
        &self,
        epoch: ConsensusEpoch,
        certified: &crate::consensus::election::CertifiedState,
        residual: &crate::consensus::election::ResidualVotes,
    ) -> (
        crate::consensus::election::CertifiedState,
        crate::consensus::election::ResidualVotes,
    ) {
        self.aec
            .read()
            .unwrap()
            .complete_report(epoch, certified, residual)
    }

    /// RAI: how many first and final voters this node holds for a block
    #[cfg(feature = "rai_protocol")]
    pub fn support_counts(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> (usize, usize) {
        self.aec.read().unwrap().support_counts(epoch, hash)
    }

    /// RAI: the signed votes held for blocks of an epoch, to relay
    #[cfg(feature = "rai_protocol")]
    /// RAI: whether this node holds an exclusion witness for a block in an
    /// epoch: first votes and late notarizations of a certificate's weight
    pub fn holds_exclusion_witness(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> bool {
        self.aec
            .read()
            .unwrap()
            .holds_exclusion_witness(epoch, hash)
    }

    /// RAI: the overlap-certificate evidence to persist now
    pub fn take_evidence_records(&self) -> Vec<crate::consensus::EvidenceRecord> {
        self.aec.write().unwrap().take_evidence_records()
    }

    /// RAI: retained evidence of a restarted node
    pub fn restore_evidence(&self, records: Vec<crate::consensus::EvidenceRecord>) {
        self.aec.write().unwrap().restore_evidence(records)
    }

    /// RAI, durable epochs: how the epochs started, once, and the epochs
    /// decided since the last call
    pub fn take_epoch_records(
        &self,
        unix_now_ms: u64,
    ) -> (
        Option<crate::consensus::EpochsRecord>,
        Vec<crate::consensus::DecidedRecord>,
    ) {
        let now = self.clock.now();
        self.aec
            .write()
            .unwrap()
            .take_epoch_records(now, unix_now_ms)
    }

    /// RAI, durable epochs: a restarted node takes up the epochs
    pub fn restore_epochs(
        &self,
        started: Option<crate::consensus::EpochsRecord>,
        decided: Vec<crate::consensus::DecidedRecord>,
        unix_now_ms: u64,
    ) {
        let now = self.clock.now();
        self.aec
            .write()
            .unwrap()
            .restore_epochs(started, decided, now, unix_now_ms)
    }

    pub fn evidence_votes(
        &self,
        epoch: ConsensusEpoch,
        hashes: &[BlockHash],
    ) -> Vec<std::sync::Arc<rsnano_types::Vote>> {
        self.aec.read().unwrap().evidence_votes(epoch, hashes)
    }

    /// RAI: whether this node holds a voter's signed vote for a block

    /// RAI, durable signing records: the account slots and close rounds
    /// voted in since the last call, taken together for one write
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn take_signing_batch(
        &self,
    ) -> (
        Vec<crate::consensus::SlotRecord>,
        Vec<crate::consensus::CloseRecord>,
    ) {
        let mut aec = self.aec.write().unwrap();
        (aec.take_signing_records(), aec.take_close_records())
    }

    /// RAI, durable signing records: a restarted node's close votes
    pub fn restore_close_votes(&self, records: Vec<crate::consensus::CloseRecord>) {
        self.aec.write().unwrap().restore_close_votes(records)
    }

    /// RAI, durable signing records: what this node signed before a restart
    pub fn restore_signing(
        &self,
        records: Vec<crate::consensus::SlotRecord>,
        frozen: Vec<ConsensusEpoch>,
    ) {
        self.aec.write().unwrap().restore_signing(records, frozen)
    }

    /// RAI: a successor member installed an epoch's checkpoint; true when
    /// `N - f` of the successor committee's weight has, for the first time
    pub fn acknowledge_install(
        &self,
        epoch: ConsensusEpoch,
        state: BlockHash,
        member: PublicKey,
    ) -> bool {
        self.aec
            .write()
            .unwrap()
            .acknowledge_install(epoch, state, member)
    }

    /// RAI: the evidence manifest this node would commit to for the claims
    pub fn evidence_manifest(
        &self,
        claims: &[(ConsensusEpoch, BlockHash)],
    ) -> crate::consensus::election::Manifest {
        self.aec.read().unwrap().evidence_manifest(claims)
    }

    /// RAI: the blocks whose votes a manifest names and this node lacks
    pub fn missing_manifest_votes(
        &self,
        manifest: &crate::consensus::election::Manifest,
    ) -> Vec<(ConsensusEpoch, BlockHash)> {
        self.aec.read().unwrap().missing_manifest_votes(manifest)
    }

    pub fn has_votes(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
        votes: &[(BlockHash, crate::consensus::election::ResidualKind)],
    ) -> Vec<bool> {
        let aec = self.aec.read().unwrap();
        votes
            .iter()
            .map(|(hash, kind)| aec.has_vote(epoch, voter, hash, *kind))
            .collect()
    }

    /// RAI: `O_e = C_{e-2}`, the committee an epoch's reports are counted in
    #[cfg(feature = "rai_protocol")]
    pub fn epoch_committee(
        &self,
        epoch: ConsensusEpoch,
    ) -> Option<std::sync::Arc<crate::consensus::election::Committee>> {
        self.aec.read().unwrap().epoch_committee(epoch)
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

impl WalletRepsConsumer for AecService {
    fn update_wallet_reps(&self, reps: &WalletRepresentatives) {
        self.aec
            .write()
            .unwrap()
            .set_local_representatives(reps.rep_pub_keys().collect());
    }
}

impl StatsSource for AecService {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.aec.read().unwrap().collect_stats(result)
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
