mod aggregator;
pub(crate) mod fork_detector;
mod state;

use crate::{
    consensus::{AecService, LocalVoteHistory},
    ledger_snapshots::{aggregator::Aggregator, state::State},
    representatives::OnlineReps,
    transport::MessageFlooder,
};
use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_messages::{Aggregatable, Message, Preproposal, Proposal, ProposalVote};
use rsnano_network::TrafficType;
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{
    Account, BlockHash, NetworkType, PrivateKey, PublicKey, SavedBlock, SnapshotNumber,
};
use rsnano_utils::{CancellationToken, ticker::Tickable};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::Duration;
use tracing::warn;

static CURRENT_RAI_EPOCH: AtomicU32 = AtomicU32::new(1);
static CURRENT_RAI_EPOCH_FROZEN: AtomicBool = AtomicBool::new(false);

pub(crate) fn current_rai_epoch() -> u32 {
    CURRENT_RAI_EPOCH.load(Ordering::SeqCst)
}

pub(crate) fn set_current_rai_epoch(epoch: u32) {
    CURRENT_RAI_EPOCH.store(epoch, Ordering::SeqCst);
    CURRENT_RAI_EPOCH_FROZEN.store(false, Ordering::SeqCst);
}

pub(crate) fn freeze_current_rai_epoch() {
    CURRENT_RAI_EPOCH_FROZEN.store(true, Ordering::SeqCst);
}

pub(crate) fn is_rai_epoch_frozen(epoch: u32) -> bool {
    current_rai_epoch() == epoch && CURRENT_RAI_EPOCH_FROZEN.load(Ordering::SeqCst)
}

pub(crate) struct LedgerSnapshotTicker {
    ledger_snapshots: Arc<LedgerSnapshots>,
    clock: Arc<SteadyClock>,
    epoch_duration: Duration,
    tracked_epoch: SnapshotNumber,
    epoch_started_at: Timestamp,
}

impl LedgerSnapshotTicker {
    pub fn new(
        ledger_snapshots: Arc<LedgerSnapshots>,
        clock: Arc<SteadyClock>,
        epoch_duration: Duration,
    ) -> Self {
        let now = clock.now();
        let tracked_epoch = ledger_snapshots.get_current_snapshot_number();
        Self {
            ledger_snapshots,
            clock,
            epoch_duration,
            tracked_epoch,
            epoch_started_at: now,
        }
    }
}

impl Tickable for LedgerSnapshotTicker {
    fn tick(&mut self, cancel_token: &CancellationToken) {
        if cancel_token.is_cancelled() || self.epoch_duration.is_zero() {
            return;
        }

        let now = self.clock.now();
        let epoch = self.ledger_snapshots.get_current_snapshot_number();

        if epoch != self.tracked_epoch {
            self.tracked_epoch = epoch;
            self.epoch_started_at = now;
        }

        if !is_rai_epoch_frozen(epoch) && self.epoch_started_at.elapsed(now) >= self.epoch_duration
        {
            self.ledger_snapshots.start_ledger_snapshot();
        }
    }
}

pub struct LedgerSnapshots {
    ledger: Arc<Ledger>,
    /// For simplicity we currently assume that there is at most
    /// one representative key!
    /// TODO: We have to extend this later to multiple representatives per node.
    get_private_key: Box<dyn Fn() -> Option<PrivateKey> + Send + Sync>,
    flooder: Mutex<MessageFlooder>,
    receive_preproposal_listener: OutputListenerMt<Preproposal>,
    receive_proposal_listener: OutputListenerMt<Proposal>,
    receive_vote_listener: OutputListenerMt<ProposalVote>,
    state: Mutex<State>,
    online_reps: Arc<Mutex<OnlineReps>>,
    vote_history: Arc<LocalVoteHistory>,
    active_elections: Arc<AecService>,
}

impl LedgerSnapshots {
    pub fn new(
        ledger: Arc<Ledger>,
        get_private_key: impl Fn() -> Option<PrivateKey> + Send + Sync + 'static,
        flooder: MessageFlooder,
        online_reps: Arc<Mutex<OnlineReps>>,
        vote_history: Arc<LocalVoteHistory>,
        active_elections: Arc<AecService>,
    ) -> Self {
        Self {
            ledger,
            get_private_key: Box::new(get_private_key),
            flooder: flooder.into(),
            receive_preproposal_listener: OutputListenerMt::new(),
            receive_proposal_listener: OutputListenerMt::new(),
            receive_vote_listener: OutputListenerMt::new(),
            state: Default::default(),
            online_reps,
            vote_history,
            active_elections,
        }
    }

    pub fn new_null() -> Self {
        Self::new(
            Ledger::new_null().into(),
            || None,
            MessageFlooder::new_null(),
            Mutex::new(OnlineReps::default()).into(),
            Arc::new(LocalVoteHistory::new(NetworkType::NanoDevNetwork)),
            Arc::new(AecService::new_null()),
        )
    }

    pub fn start_ledger_snapshot(&self) {
        if is_rai_epoch_frozen(self.get_current_snapshot_number()) {
            return;
        }

        freeze_current_rai_epoch();
        warn!(
            snapshot_number = self.get_current_snapshot_number(),
            "Preproposal generation triggered"
        );
        let Some(private_key) = (self.get_private_key)() else {
            warn!(
                snapshot_number = self.get_current_snapshot_number(),
                "Skipping local preproposal because no representative key is available"
            );
            return;
        };
        let preproposal = self.create_preproposal(&private_key);
        let message = Message::SnapshotPreproposal(preproposal);
        self.publish_message(&message);
    }

    fn create_preproposal(&self, private_key: &PrivateKey) -> Preproposal {
        let blocks = self.collect_final_voted_blocks(self.get_current_snapshot_number());
        Preproposal::new(blocks, private_key, self.get_current_snapshot_number())
    }

    fn collect_final_voted_blocks(&self, epoch: SnapshotNumber) -> Vec<(Account, BlockHash)> {
        let any = self.ledger.any();
        let mut blocks = Vec::new();

        for (_, hash, vote) in self.vote_history.final_votes_for_epoch(epoch) {
            let Some(block) = any.get_block(&hash) else {
                continue;
            };

            if !self.preproposal_block_is_valid_for_signer(&block, vote.voter, epoch) {
                continue;
            }

            let entry = (block.account(), hash);
            if !blocks.contains(&entry) {
                blocks.push(entry);
            }
        }

        blocks
    }

    #[cfg(test)]
    fn collect_frontiers(&self) -> Vec<(Account, BlockHash)> {
        self.collect_final_voted_blocks(self.get_current_snapshot_number())
    }

    fn validate_preproposal(&self, preproposal: &Preproposal) -> bool {
        preproposal.frontiers.iter().all(|(account, hash)| {
            let Some(block) = self.ledger.any().get_block(hash) else {
                return false;
            };

            block.account() == *account
                && self.preproposal_block_is_valid_for_signer(
                    &block,
                    preproposal.signer,
                    preproposal.snapshot_number,
                )
        })
    }

    fn preproposal_block_is_valid_for_signer(
        &self,
        block: &SavedBlock,
        signer: PublicKey,
        epoch: SnapshotNumber,
    ) -> bool {
        if block.height() > 1 && !self.ledger.confirmed().block_exists(&block.previous()) {
            return false;
        }

        let Some(election) = self.active_elections.election_for_block(&block.hash()) else {
            return self.ledger.confirmed().block_exists(&block.hash());
        };

        if election.epoch() != epoch || !election.has_quorum() {
            return false;
        }

        let Some(vote) = election.votes().get(&signer) else {
            return self.ledger.confirmed().block_exists(&block.hash());
        };

        vote.hash == block.hash() && vote.is_final_vote()
    }

    pub fn handle_preproposal(&self, preproposal: Preproposal) {
        warn!(snapshot_number = preproposal.snapshot_number, preproposal_hash= ?preproposal.hash(), "Snapshot preproposal received");
        self.receive_preproposal_listener.emit(preproposal.clone());
        let consensus_params = self.online_reps.lock().unwrap().get_consensus_params();

        if !self.validate_preproposal(&preproposal) {
            warn!(
                preproposal_hash = ?preproposal.hash(),
                snapshot_number = ?preproposal.snapshot_number,
                "Snapshot preproposal discarded because local Rai validation failed"
            );
            return;
        }

        let mut state = self.state.lock().unwrap();
        if !state.receive_preproposal(preproposal.clone()) {
            warn!(
                preproposal_hash= ?preproposal.hash(),
                snapshot_number= ?preproposal.snapshot_number,
                "Snapshot preproposal discarded because snapshot number is different than current");
            return;
        }

        warn!(
            snapshot_number = state.current_snapshot_number,
            preproposals_count = state.preproposal_aggregator.len(),
            "Current preproposal tally = {:?}",
            state.preproposal_aggregator.tally(&consensus_params)
        );

        let Some(rep_key) = (self.get_private_key)() else {
            warn!(
                snapshot_number = state.current_snapshot_number,
                "Skipping local proposal because no representative key is available"
            );
            return;
        };
        let proposal = state.try_create_proposal(&consensus_params, &rep_key);
        if proposal.is_some() {
            state.set_proposal_published(true);
            warn!(
                snapshot_number = state.current_snapshot_number,
                "Quorum on preproposals reached"
            );
        } else {
            warn!(
                snapshot_number = state.current_snapshot_number,
                "No quorum on preproposals yet or proposal already published"
            );
        }
        drop(state);

        if let Some(proposal) = proposal {
            warn!(snapshot_number = self.get_current_snapshot_number(), proposal_hash = ?proposal.hash(), "Created proposal. Flooding...");
            self.publish_message(&Message::SnapshotProposal(proposal));
        };
    }

    pub fn track_received_preproposals(&self) -> Arc<OutputTrackerMt<Preproposal>> {
        self.receive_preproposal_listener.track()
    }

    pub fn handle_proposal(&self, proposal: Proposal) {
        warn!(snapshot_number = proposal.snapshot_number, proposal_hash = ?proposal.hash(), "Snapshot proposal received");
        self.receive_proposal_listener.emit(proposal.clone());
        let consensus_params = self.online_reps.lock().unwrap().get_consensus_params();

        let mut state = self.state.lock().unwrap();
        if !state.receive_proposal(proposal.clone()) {
            warn!(
                proposal_hash= ?proposal.hash(),
                snapshot_number= ?proposal.snapshot_number,
                "Snapshot proposal discarded because snapshot number is different than current");
            return;
        }

        warn!(
            snapshot_number = state.current_snapshot_number,
            "Current proposal tally = {:?}",
            state.proposal_aggregator.tally(&consensus_params)
        );

        let Some(rep_key) = (self.get_private_key)() else {
            warn!(
                snapshot_number = state.current_snapshot_number,
                "Skipping local proposal vote because no representative key is available"
            );
            return;
        };
        if let Some(vote) = state.try_create_vote(&consensus_params, &rep_key) {
            warn!(
                snapshot_number = state.current_snapshot_number,
                "Quorum on proposal reached"
            );
            warn!(vote_hash = ?vote.hash(), "Flooding proposal vote");
            self.publish_message(&Message::SnapshotProposalVote(vote));
        }
    }

    pub fn track_received_proposals(&self) -> Arc<OutputTrackerMt<Proposal>> {
        self.receive_proposal_listener.track()
    }

    pub fn track_received_votes(&self) -> Arc<OutputTrackerMt<ProposalVote>> {
        self.receive_vote_listener.track()
    }

    pub fn handle_vote(&self, vote: ProposalVote) {
        self.receive_vote_listener.emit(vote.clone());

        let consensus_params = self.online_reps.lock().unwrap().get_consensus_params();
        let mut state = self.state.lock().unwrap();

        if !state.receive_vote(vote.clone()) {
            warn!(
                vote_hash= ?vote.hash(),
                snapshot_number= ?vote.snapshot_number,
                "Snapshot vote discarded because snapshot number is different than current");
            return;
        }

        warn!(
            snapshot_number = vote.snapshot_number,
            received_votes = state.vote_aggregator.len(),
            "Snapshot proposal vote received"
        );

        if let Some(winner) = state.find_winner_proposal(&consensus_params) {
            tracing::warn!(snapshot_number = state.current_snapshot_number, proposal_hash=?winner, "Found a winner!");
            let hashes_to_confirm = state.hashes_for_winner(&winner);
            state.advance_epoch();
            tracing::warn!(
                snapshot_number = state.current_snapshot_number,
                "Advanced epoch"
            );
            drop(state);
            for hash in hashes_to_confirm {
                self.ledger.confirm(hash);
            }
        }
    }

    fn get_current_snapshot_number(&self) -> SnapshotNumber {
        self.state.lock().unwrap().current_snapshot_number
    }

    fn publish_message(&self, message: &Message) {
        let flood_count = self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
            message,
            TrafficType::LedgerSnapshots,
            0.0,
        );
        tracing::warn!(
            "Flooded {:?} to {} nodes",
            message.message_type(),
            flood_count.principal_reps
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{representatives::ONLINE_WEIGHT_QUORUM, transport::FloodEvent};
    use rsnano_ledger::{AnySet, RepWeights};
    use rsnano_messages::{Aggregatable, Message, MessageType, ProposalHash, ProposalVote};
    use rsnano_network::TrafficType;
    use rsnano_nullable_clock::SteadyClock;
    use rsnano_output_tracker::OutputTrackerMt;
    use rsnano_types::{Amount, QualifiedRoot, SavedBlock};
    use rsnano_utils::CancellationToken;
    use std::sync::LazyLock;

    #[test]
    fn collect_one_frontier() {
        let account = Account::from(1);
        let frontier = BlockHash::from(2);
        let fixture = Fixture::builder().frontiers([(account, frontier)]).finish();
        assert_eq!(
            fixture.snapshots.collect_frontiers(),
            Vec::<(Account, BlockHash)>::new()
        );
    }

    #[test]
    fn collect_multiple_frontiers() {
        let account1 = Account::from(1);
        let frontier1 = BlockHash::from(100);
        let account2 = Account::from(2);
        let frontier2 = BlockHash::from(200);

        let fixture = Fixture::builder()
            .frontiers([(account1, frontier1), (account2, frontier2)])
            .finish();

        assert_eq!(
            fixture.snapshots.collect_frontiers(),
            Vec::<(Account, BlockHash)>::new()
        );
    }

    #[test]
    fn create_preproposal_with_one_frontier() {
        let account = Account::from(10);
        let frontier = BlockHash::from(2);
        let fixture = Fixture::builder().frontiers([(account, frontier)]).finish();

        let preproposal = fixture.snapshots.create_preproposal(&PrivateKey::from(1));

        assert!(preproposal.frontiers.is_empty());
        assert_eq!(
            preproposal.snapshot_number,
            fixture.snapshots.get_current_snapshot_number()
        );
    }

    #[test]
    fn publish_preproposal() {
        let fixture = Fixture::new();

        fixture.snapshots.start_ledger_snapshot();

        let flood_events = fixture.flood_tracker.output();
        assert_eq!(flood_events.len(), 1, "Should flood the message");

        let expected_preproposal = fixture
            .snapshots
            .create_preproposal(&fixture.rep_keys.local_rep);

        assert_eq!(
            flood_events[0],
            FloodEvent {
                message: Message::SnapshotPreproposal(expected_preproposal),
                // TODO: add new traffic type for snapshots
                traffic_type: TrafficType::LedgerSnapshots,
                scale: 0.0,
                all_prs: true,
            }
        );
    }

    #[test]
    fn can_track_received_preproposals() {
        let fixture = Fixture::new();
        let preproposal = Preproposal::new_test_instance();
        fixture.snapshots.handle_preproposal(preproposal.clone());

        let receive_events = fixture.receive_preproposal_tracker.output();
        assert_eq!(receive_events.len(), 1, "Should receive preproposal");
        assert_eq!(receive_events[0], preproposal);
    }

    #[test]
    fn a_received_preproposal_is_added_to_the_preproposal_aggregator() {
        let fixture = Fixture::new();
        let snapshots = &fixture.snapshots;
        let preproposal = fixture.create_preproposal(&fixture.rep_keys.rep2);

        snapshots.handle_preproposal(preproposal.clone());

        assert!(
            snapshots
                .state
                .lock()
                .unwrap()
                .preproposal_aggregator
                .contains(&preproposal.hash())
        );
    }

    #[test]
    fn a_received_preproposal_without_private_key_does_not_panic() {
        let fixture = Fixture::builder().finish_with_get_private_key(|| None);
        let preproposal = fixture.create_preproposal(&fixture.rep_keys.rep2);

        fixture.snapshots.handle_preproposal(preproposal.clone());

        assert!(
            fixture
                .snapshots
                .state
                .lock()
                .unwrap()
                .preproposal_aggregator
                .contains(&preproposal.hash())
        );
        assert!(fixture.flood_tracker.output().is_empty());
    }

    #[test]
    fn publish_proposal_when_quorum_of_preproposals_is_reached() {
        let fixture = Fixture::new();

        let preproposal1 = fixture.create_preproposal(&fixture.rep_keys.local_rep);
        fixture.snapshots.handle_preproposal(preproposal1.clone());

        let preproposal2 = fixture.create_preproposal(&fixture.rep_keys.rep2);
        fixture.snapshots.handle_preproposal(preproposal2.clone());

        let flood_events = fixture.flood_tracker.output();
        assert_eq!(
            flood_events.len(),
            0,
            "Should not flood any message before quorum reached"
        );

        let preproposal3 = fixture.create_preproposal(&fixture.rep_keys.rep3);
        fixture.snapshots.handle_preproposal(preproposal3.clone());

        let flood_events = fixture.flood_tracker.output();
        assert_eq!(flood_events.len(), 1, "Should flood the message");

        let snapshot_number = fixture.snapshots.get_current_snapshot_number();
        let expected_proposal = Proposal::new(
            &[preproposal1, preproposal2, preproposal3],
            &fixture.rep_keys.local_rep,
            snapshot_number,
        );

        assert_eq!(
            flood_events[0],
            FloodEvent {
                message: Message::SnapshotProposal(expected_proposal),
                traffic_type: TrafficType::LedgerSnapshots,
                scale: 0.0,
                all_prs: true,
            }
        );
    }

    #[test]
    fn can_track_received_proposals() {
        let fixture = Fixture::new();
        let proposal = Proposal::new_test_instance();
        fixture.snapshots.handle_proposal(proposal.clone());

        let receive_events = fixture.receive_proposal_tracker.output();
        assert_eq!(receive_events.len(), 1, "Should receive proposal");
        assert_eq!(receive_events[0], proposal);
    }

    #[test]
    fn a_received_proposal_is_added_to_the_proposal_aggregator() {
        let fixture = Fixture::new();
        let snapshots = &fixture.snapshots;
        let proposal = fixture.create_proposal(&fixture.rep_keys.rep2);

        snapshots.handle_proposal(proposal.clone());

        assert!(
            snapshots
                .state
                .lock()
                .unwrap()
                .proposal_aggregator
                .contains(&proposal.hash())
        );
    }

    #[test]
    fn a_received_proposal_without_private_key_does_not_panic() {
        let fixture = Fixture::builder().finish_with_get_private_key(|| None);
        let proposal = fixture.create_proposal(&fixture.rep_keys.rep2);

        fixture.snapshots.handle_proposal(proposal.clone());

        assert!(
            fixture
                .snapshots
                .state
                .lock()
                .unwrap()
                .proposal_aggregator
                .contains(&proposal.hash())
        );
        assert!(fixture.flood_tracker.output().is_empty());
    }

    #[test]
    fn publish_vote_when_quorum_of_proposals_is_reached() {
        let fixture = Fixture::new();
        let snapshot_number = fixture.snapshots.get_current_snapshot_number();

        let proposal1 = fixture.create_proposal(&fixture.rep_keys.local_rep);
        fixture.snapshots.handle_proposal(proposal1.clone());

        let proposal2 = fixture.create_proposal(&fixture.rep_keys.rep2);
        fixture.snapshots.handle_proposal(proposal2.clone());

        let proposal3 = fixture.create_proposal(&fixture.rep_keys.rep3);
        fixture.snapshots.handle_proposal(proposal3.clone());

        let flood_events = fixture.flood_tracker.output();
        assert_eq!(flood_events.len(), 1, "Should flood the message");

        let highest_hash = [proposal1.hash(), proposal2.hash(), proposal3.hash()]
            .into_iter()
            .max()
            .unwrap();

        let expected_vote =
            ProposalVote::new(highest_hash, &fixture.rep_keys.local_rep, snapshot_number);

        assert_eq!(
            flood_events[0],
            FloodEvent {
                message: Message::SnapshotProposalVote(expected_vote),
                traffic_type: TrafficType::LedgerSnapshots,
                scale: 0.0,
                all_prs: true,
            }
        );
    }

    #[test]
    fn publish_proposal_only_once() {
        let fixture = Fixture::new();

        let preproposal1 = fixture.create_preproposal(&fixture.rep_keys.local_rep);
        let preproposal2 = fixture.create_preproposal(&fixture.rep_keys.rep2);
        let preproposal3 = fixture.create_preproposal(&fixture.rep_keys.rep3);
        let preproposal4 = fixture.create_preproposal(&fixture.rep_keys.rep4);
        fixture.snapshots.handle_preproposal(preproposal1.clone());
        fixture.snapshots.handle_preproposal(preproposal2);
        fixture.snapshots.handle_preproposal(preproposal3);

        let flood_events = fixture.flood_tracker.output();
        assert_eq!(
            flood_events.len(),
            1,
            "Should flood only one proposal message"
        );

        fixture.snapshots.handle_preproposal(preproposal4);

        let flood_events = fixture.flood_tracker.output();
        assert_eq!(flood_events.len(), 1, "Should not flood another proposal");
    }

    #[test]
    // Ignored so that we always reach quorum on the proposal with the highest hash
    #[ignore]
    fn publish_vote_only_once() {
        let fixture = Fixture::new();
        let proposal1 = fixture.create_proposal(&fixture.rep_keys.local_rep);
        let proposal2 = fixture.create_proposal(&fixture.rep_keys.rep2);
        let proposal3 = fixture.create_proposal(&fixture.rep_keys.rep3);
        let proposal4 = fixture.create_proposal(&fixture.rep_keys.rep4);

        fixture.snapshots.handle_proposal(proposal1);
        fixture.snapshots.handle_proposal(proposal2);
        fixture.snapshots.handle_proposal(proposal3);

        let flood_events = fixture.flood_tracker.output();
        assert_eq!(flood_events.len(), 1, "Should flood only one vote message");

        fixture.snapshots.handle_proposal(proposal4);

        let flood_events = fixture.flood_tracker.output();
        assert_eq!(flood_events.len(), 1, "Should flood only one vote message");
    }

    #[test]
    fn can_track_received_votes() {
        let fixture = Fixture::new();
        let vote = ProposalVote::new_test_instance();
        fixture.snapshots.handle_vote(vote.clone());

        let receive_events = fixture.receive_vote_tracker.output();
        assert_eq!(receive_events.len(), 1, "Should receive proposal vote");
        assert_eq!(receive_events[0], vote);
    }

    #[test]
    fn a_received_vote_is_added_to_the_vote_aggregator() {
        let fixture = Fixture::new();
        let snapshots = &fixture.snapshots;
        let vote = ProposalVote::new(
            ProposalHash::from(1),
            &PrivateKey::from(1),
            snapshots.get_current_snapshot_number(),
        );

        snapshots.handle_vote(vote.clone());

        assert!(
            snapshots
                .state
                .lock()
                .unwrap()
                .vote_aggregator
                .contains(&vote.hash())
        );
    }

    #[test]
    fn initial_snapshot_number_should_be_one() {
        let ledger_snapshots = LedgerSnapshots::new_null();

        assert_eq!(ledger_snapshots.get_current_snapshot_number(), 1);
    }

    #[test]
    fn rollback_fork() {
        let fork_block = SavedBlock::new_test_instance();
        let root = fork_block.qualified_root();
        let snapshot_number = 0;
        let fixture = Fixture::builder()
            .marked_forks([(root, snapshot_number)])
            .finish();

        let mut tx = fixture.snapshots.ledger.store.begin_write();
        fixture.snapshots.ledger.store.successors.put(
            &mut tx,
            &fork_block.previous(),
            &fork_block.hash(),
        );
        tx.commit();

        let proposal = fixture.create_proposal(&LOCAL_REP_KEY);
        let proposal_hash = proposal.hash();
        let rollback_tracker = fixture.snapshots.ledger.track_rollbacks();

        fixture.snapshots.handle_proposal(proposal);

        let vote1 = fixture.create_vote(proposal_hash, &LOCAL_REP_KEY);
        let vote2 = fixture.create_vote(proposal_hash, &fixture.rep_keys.rep2);
        let vote3 = fixture.create_vote(proposal_hash, &fixture.rep_keys.rep3);

        fixture.snapshots.handle_vote(vote1);
        fixture.snapshots.handle_vote(vote2);
        fixture.snapshots.handle_vote(vote3);

        let output = rollback_tracker.output();

        assert!(output.is_empty());
        assert_eq!(
            fixture
                .snapshots
                .ledger
                .any()
                .is_forked(&fork_block.qualified_root()),
            true,
            "Closure no longer clears fork markers"
        );
    }

    #[test]
    fn ticker_triggers_snapshot_after_epoch_duration() {
        let fixture = Fixture::new();
        let clock = Arc::new(SteadyClock::new_null());
        let snapshots = Arc::new(fixture.snapshots);
        let mut ticker =
            LedgerSnapshotTicker::new(snapshots.clone(), clock.clone(), Duration::from_secs(5));

        ticker.tick(&CancellationToken::new_null());
        assert!(fixture.flood_tracker.output().is_empty());

        clock.advance(Duration::from_secs(5));
        ticker.tick(&CancellationToken::new_null());

        let floods = fixture.flood_tracker.output();
        assert_eq!(floods.len(), 1);
        assert_eq!(floods[0].message.message_type(), MessageType::Preproposal);
    }

    struct FixtureBuilder {
        frontiers: Vec<(Account, BlockHash)>,
        forked_roots: Vec<(QualifiedRoot, SnapshotNumber)>,
    }

    impl FixtureBuilder {
        fn new() -> Self {
            let frontiers = vec![
                (Account::from(1), BlockHash::from(100)),
                (Account::from(2), BlockHash::from(200)),
            ];

            Self {
                frontiers,
                forked_roots: Vec::new(),
            }
        }

        fn frontiers(mut self, frontiers: impl IntoIterator<Item = (Account, BlockHash)>) -> Self {
            self.frontiers = frontiers.into_iter().collect();
            self
        }

        fn marked_forks(
            mut self,
            forked_roots: impl IntoIterator<Item = (QualifiedRoot, SnapshotNumber)>,
        ) -> Self {
            self.forked_roots = forked_roots.into_iter().collect();
            self
        }

        fn finish(self) -> Fixture {
            self.finish_with_get_private_key(get_test_key)
        }

        fn finish_with_get_private_key(
            self,
            get_private_key: impl Fn() -> Option<PrivateKey> + Send + Sync + 'static,
        ) -> Fixture {
            let online_weight = Amount::nano(100_000_000);
            let quorum_weight = Amount::nano(67_000_000);
            let mut rep_weights = RepWeights::default();
            let rep_weight = online_weight / 4_u128;

            let rep_keys = RepKeys::default();
            rep_weights.put(rep_keys.local_rep.public_key(), rep_weight);
            rep_weights.put(rep_keys.rep2.public_key(), rep_weight);
            rep_weights.put(rep_keys.rep3.public_key(), rep_weight);
            rep_weights.put(rep_keys.rep4.public_key(), rep_weight);

            let ledger = Ledger::new_null_builder()
                .frontiers(self.frontiers)
                .forks(self.forked_roots)
                .finish();

            Self::with_ledger_and_weights(
                ledger.into(),
                rep_weights,
                quorum_weight,
                get_private_key,
            )
        }

        fn with_ledger_and_weights(
            ledger: Arc<Ledger>,
            rep_weights: RepWeights,
            quorum_weight: Amount,
            get_private_key: impl Fn() -> Option<PrivateKey> + Send + Sync + 'static,
        ) -> Fixture {
            let flooder = MessageFlooder::new_null();
            let flood_tracker = flooder.track_floods();

            let mut online_reps =
                OnlineReps::new(Arc::new(rep_weights.into()), Amount::ZERO, Amount::ZERO);
            online_reps.set_trended(quorum_weight / ONLINE_WEIGHT_QUORUM as u128 * 100);
            let online_reps = Arc::new(Mutex::new(online_reps));
            let vote_history = Arc::new(LocalVoteHistory::new(NetworkType::NanoDevNetwork));
            let active_elections = Arc::new(AecService::new_null());

            let snapshots = LedgerSnapshots::new(
                ledger.clone(),
                get_private_key,
                flooder,
                online_reps,
                vote_history,
                active_elections,
            );

            snapshots.state.lock().unwrap().current_snapshot_number = 10;

            let receive_preproposal_tracker = snapshots.track_received_preproposals();
            let receive_proposal_tracker = snapshots.track_received_proposals();
            let receive_vote_tracker = snapshots.track_received_votes();

            Fixture {
                snapshots,
                rep_keys: RepKeys::default(),
                flood_tracker,
                receive_preproposal_tracker,
                receive_proposal_tracker,
                receive_vote_tracker,
            }
        }
    }

    struct Fixture {
        snapshots: LedgerSnapshots,
        rep_keys: RepKeys,
        flood_tracker: Arc<OutputTrackerMt<FloodEvent>>,
        receive_preproposal_tracker: Arc<OutputTrackerMt<Preproposal>>,
        receive_proposal_tracker: Arc<OutputTrackerMt<Proposal>>,
        receive_vote_tracker: Arc<OutputTrackerMt<ProposalVote>>,
    }

    impl Fixture {
        fn new() -> Self {
            Self::builder().finish()
        }

        fn builder() -> FixtureBuilder {
            FixtureBuilder::new()
        }
        fn create_preproposal(&self, rep_key: &PrivateKey) -> Preproposal {
            Preproposal::new(
                vec![],
                rep_key,
                self.snapshots.get_current_snapshot_number(),
            )
        }

        fn create_proposal(&self, rep_key: &PrivateKey) -> Proposal {
            Proposal::new(
                vec![],
                rep_key,
                self.snapshots.get_current_snapshot_number(),
            )
        }

        fn create_vote(&self, proposal_hash: ProposalHash, rep_key: &PrivateKey) -> ProposalVote {
            ProposalVote::new(
                proposal_hash,
                rep_key,
                self.snapshots.get_current_snapshot_number(),
            )
        }
    }

    fn get_test_key() -> Option<PrivateKey> {
        Some(LOCAL_REP_KEY.clone())
    }

    static LOCAL_REP_KEY: LazyLock<PrivateKey> = LazyLock::new(|| RepKeys::default().local_rep);

    struct RepKeys {
        local_rep: PrivateKey,
        rep2: PrivateKey,
        rep3: PrivateKey,
        rep4: PrivateKey,
    }

    impl Default for RepKeys {
        fn default() -> Self {
            Self {
                local_rep: PrivateKey::from(123),
                rep2: PrivateKey::from(2),
                rep3: PrivateKey::from(3),
                rep4: PrivateKey::from(4),
            }
        }
    }
}
