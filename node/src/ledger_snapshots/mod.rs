mod aggregator;
pub(crate) mod fork_detector;
pub mod rai;
mod state;

use crate::{
    ledger_snapshots::{
        aggregator::Aggregator,
        rai::{RaiOpenElectionOutput, RaiService},
        state::State,
    },
    representatives::OnlineReps,
    transport::MessageFlooder,
};
use rsnano_ledger::{AnySet, BlockError, Ledger, LedgerSet};
use rsnano_messages::{
    Aggregatable, Message, Preproposal, Proposal, ProposalVote, RaiElectionId, RaiMessage,
    RaiStopReport,
};
use rsnano_network::TrafficType;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{Account, Blake2HashBuilder, BlockHash};
use rsnano_types::{PrivateKey, SnapshotNumber};
use std::sync::{Arc, Mutex};
use tracing::warn;

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
    rai: RaiService,
    online_reps: Arc<Mutex<OnlineReps>>,
}

impl LedgerSnapshots {
    pub fn new(
        ledger: Arc<Ledger>,
        get_private_key: impl Fn() -> Option<PrivateKey> + Send + Sync + 'static,
        flooder: MessageFlooder,
        online_reps: Arc<Mutex<OnlineReps>>,
    ) -> Self {
        Self {
            ledger,
            get_private_key: Box::new(get_private_key),
            flooder: flooder.into(),
            receive_preproposal_listener: OutputListenerMt::new(),
            receive_proposal_listener: OutputListenerMt::new(),
            receive_vote_listener: OutputListenerMt::new(),
            state: Default::default(),
            rai: RaiService::new(),
            online_reps,
        }
    }

    pub fn new_null() -> Self {
        Self::new(
            Ledger::new_null().into(),
            || None,
            MessageFlooder::new_null(),
            Mutex::new(OnlineReps::default()).into(),
        )
    }

    pub fn start_ledger_snapshot(&self) {
        self.start_rai_epoch_transition();
    }

    pub fn start_rai_epoch_transition(&self) {
        let closing_epoch = self.rai.current_open_epoch();
        let head = self.stop_report_head();
        let started_elections = self.rai.started_elections(closing_epoch);
        let terminal_records = self.rai.terminal_records(closing_epoch);
        let opened_epoch = self.rai.open_next_epoch_with_close_head(head);
        self.state.lock().unwrap().reset_for_epoch(opened_epoch);

        warn!(
            closing_epoch,
            opened_epoch,
            started_elections = started_elections.len(),
            terminal_records = terminal_records.len(),
            "Rai epoch transition triggered"
        );

        let Some(private_key) = (self.get_private_key)() else {
            warn!(
                closing_epoch,
                opened_epoch,
                "Rai stop report not published because no representative key is available"
            );
            return;
        };

        let report = RaiStopReport::new(closing_epoch, head, started_elections, &private_key);
        self.publish_message(&Message::Rai(RaiMessage::StopReport(report)));
    }

    #[cfg(test)]
    fn create_preproposal(&self, private_key: &PrivateKey) -> Preproposal {
        let frontiers = self.collect_frontiers();
        Preproposal::new(frontiers, private_key, self.get_current_snapshot_number())
    }

    fn collect_frontiers(&self) -> Vec<(Account, BlockHash)> {
        self.ledger.confirmed().frontiers().collect()
    }

    fn stop_report_head(&self) -> BlockHash {
        let mut frontiers = self.collect_frontiers();
        frontiers.sort();

        let mut builder = Blake2HashBuilder::new().update(b"rai:stop_report_head");
        for (account, frontier) in frontiers {
            builder = builder
                .update(account.as_bytes())
                .update(frontier.as_bytes());
        }

        builder.build()
    }

    pub fn handle_preproposal(&self, preproposal: Preproposal) {
        warn!(snapshot_number = preproposal.snapshot_number, preproposal_hash= ?preproposal.hash(), "Snapshot preproposal received");
        self.receive_preproposal_listener.emit(preproposal.clone());
        let consensus_params = self.online_reps.lock().unwrap().get_consensus_params();

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

        let rep_key = (self.get_private_key)().unwrap();
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

        let rep_key = (self.get_private_key)().unwrap();
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
            state.advance_epoch();
            tracing::warn!(
                snapshot_number = state.current_snapshot_number,
                "Advanced epoch"
            );
            let snapshot_number = state.current_snapshot_number;
            drop(state);
            tracing::warn!("Calling roll_back_forks_older_than");
            self.ledger.roll_back_forks_older_than(snapshot_number - 1);
        }
    }

    fn get_current_snapshot_number(&self) -> SnapshotNumber {
        self.state.lock().unwrap().current_snapshot_number
    }

    pub fn rai(&self) -> &RaiService {
        &self.rai
    }

    pub fn handle_rai_message(&self, message: RaiMessage) {
        if let RaiMessage::Proposal(proposal) = &message
            && let Err(error) = self
                .ledger
                .validate_rai_proposal(&proposal.election, &proposal.block)
        {
            tracing::warn!(
                election = ?proposal.election,
                proposal_hash = ?proposal.proposal_hash(),
                ?error,
                "Rai proposal discarded because block validation failed"
            );
            return;
        }

        let private_key = (self.get_private_key)();
        let output = self.rai.process_message(message, private_key.as_ref());
        self.publish_rai_output(output);
    }

    pub(crate) fn handle_rai_block_conflict(
        &self,
        election: RaiElectionId,
        fork_hash: BlockHash,
        existing_successor: Option<BlockHash>,
    ) {
        let private_key = (self.get_private_key)();
        let output = self.rai.process_block_conflict(
            election,
            fork_hash,
            existing_successor,
            private_key.as_ref(),
        );
        self.publish_rai_output(output);
    }

    fn publish_rai_output(&self, output: RaiOpenElectionOutput) {
        for message in output.messages {
            self.publish_message(&Message::Rai(message));
        }

        if let Some(record) = output.terminal_record {
            self.complete_rai_terminal_record(record);
            tracing::warn!(
                election = ?record.election,
                outcome = ?record.outcome,
                "Rai open-epoch election completed"
            );
        }
    }

    fn complete_rai_terminal_record(&self, record: rsnano_messages::RaiTerminalRecord) {
        match record.outcome {
            rsnano_messages::RaiTerminalOutcome::Proposal(proposal_hash) => {
                self.complete_rai_terminal_proposal(record.election, proposal_hash);
            }
            rsnano_messages::RaiTerminalOutcome::Notarized(_) => {}
            rsnano_messages::RaiTerminalOutcome::Timeout => {}
        }
    }

    fn complete_rai_terminal_proposal(&self, election: RaiElectionId, proposal_hash: BlockHash) {
        let record = rsnano_messages::RaiTerminalRecord::new(
            election,
            rsnano_messages::RaiTerminalOutcome::Proposal(proposal_hash),
        );

        if let Some(saved_block) = self.ledger.any().get_block(&proposal_hash) {
            self.ledger.confirm(proposal_hash);
            self.ledger.clear_fork(&saved_block.qualified_root());
            self.ledger.store_rai_terminal_proposal(record);
            return;
        }

        let Some(block) = self.rai.proposal_block(&election, &proposal_hash) else {
            tracing::warn!(
                ?election,
                ?proposal_hash,
                "Cannot complete Rai terminal proposal because the proposal block is missing"
            );
            return;
        };

        self.ledger.roll_back_competitors([&block]);

        let saved_block = match self.ledger.process_one(&block) {
            Ok(saved_block) | Err(BlockError::Old(saved_block)) => Some(saved_block),
            Err(BlockError::Conflict) => self.ledger.any().get_block(&proposal_hash),
            Err(error) => {
                tracing::warn!(
                    ?election,
                    ?proposal_hash,
                    ?error,
                    "Cannot process Rai terminal proposal block"
                );
                None
            }
        };

        let Some(saved_block) = saved_block else {
            tracing::warn!(
                ?election,
                ?proposal_hash,
                "Rai terminal proposal block was not available after processing"
            );
            return;
        };

        if self.ledger.any().block_exists(&proposal_hash) {
            let confirmed = self.ledger.confirm(proposal_hash);
            self.ledger.clear_fork(&saved_block.qualified_root());
            self.ledger.store_rai_terminal_proposal(record);
            tracing::warn!(
                ?election,
                ?proposal_hash,
                confirmed_blocks = confirmed.len(),
                "Rai terminal proposal applied to ledger"
            );
        }
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
    use crate::{
        ledger_snapshots::rai::RaiCommittee, representatives::ONLINE_WEIGHT_QUORUM,
        transport::FloodEvent,
    };
    use rsnano_ledger::{AnySet, LedgerInserter, RepWeights};
    use rsnano_messages::{
        Aggregatable, Message, ProposalHash, ProposalVote, RaiElectionId, RaiMessage, RaiProposal,
        RaiSlot, RaiStopReport, RaiTerminalOutcome, RaiTerminalRecord, RaiVotePhase, RaiVoteTarget,
    };
    use rsnano_network::TrafficType;
    use rsnano_output_tracker::OutputTrackerMt;
    use rsnano_types::{
        Amount, Block, DEV_GENESIS_KEY, QualifiedRoot, SavedBlock, Signature, TestBlockBuilder,
    };
    use std::sync::LazyLock;

    #[test]
    fn collect_one_frontier() {
        let account = Account::from(1);
        let frontier = BlockHash::from(2);
        let fixture = Fixture::builder().frontiers([(account, frontier)]).finish();
        assert_eq!(fixture.snapshots.collect_frontiers(), [(account, frontier)]);
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
            [(account1, frontier1), (account2, frontier2)]
        );
    }

    #[test]
    fn create_preproposal_with_one_frontier() {
        let account = Account::from(10);
        let frontier = BlockHash::from(2);
        let fixture = Fixture::builder().frontiers([(account, frontier)]).finish();

        let preproposal = fixture.snapshots.create_preproposal(&PrivateKey::from(1));

        assert!(preproposal.frontiers.contains(&(account, frontier)));
        assert_eq!(
            preproposal.snapshot_number,
            fixture.snapshots.get_current_snapshot_number()
        );
    }

    #[test]
    fn start_ledger_snapshot_transitions_rai_epoch_and_publishes_stop_report() {
        let fixture = Fixture::new();
        let unfinished = RaiElectionId::new(RaiSlot::new(Account::from(42), 1), 0);
        let finished = RaiElectionId::new(RaiSlot::new(Account::from(99), 1), 0);
        let terminal_record = RaiTerminalRecord::new(finished, RaiTerminalOutcome::Timeout);
        assert!(fixture.snapshots.rai().start_election(unfinished));
        assert!(fixture.snapshots.rai().start_election(finished));
        assert!(fixture.snapshots.rai().complete_slot(terminal_record));
        let expected_head = fixture.snapshots.stop_report_head();

        fixture.snapshots.start_ledger_snapshot();

        let rai_snapshot = fixture.snapshots.rai().snapshot();
        assert_eq!(rai_snapshot.current_open_epoch, 1);
        assert_eq!(rai_snapshot.open_epochs, 1);
        assert_eq!(rai_snapshot.closing_epochs, 1);
        assert_eq!(rai_snapshot.carryover_elections, 1);
        assert_eq!(fixture.snapshots.get_current_snapshot_number(), 1);
        assert!(
            fixture
                .snapshots
                .rai()
                .election(&unfinished)
                .unwrap()
                .is_carryover
        );

        let flood_events = fixture.flood_tracker.output();
        assert_eq!(flood_events.len(), 1, "Should flood the message");

        let expected_report = RaiStopReport::new(
            0,
            expected_head,
            vec![unfinished, finished],
            &fixture.rep_keys.local_rep,
        );

        assert_eq!(
            flood_events[0],
            FloodEvent {
                message: Message::Rai(RaiMessage::StopReport(expected_report)),
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
    fn received_rai_proposal_floods_local_first_vote() {
        let ledger = Arc::new(Ledger::new_null());
        let send = LedgerInserter::new(&ledger)
            .genesis()
            .send(Account::from(99), Amount::raw(1));
        let fixture =
            FixtureBuilder::with_ledger_and_weights(ledger, RepWeights::default(), Amount::nano(1));
        let local_key = &fixture.rep_keys.local_rep;
        let election = RaiElectionId::new(RaiSlot::new(send.account(), send.height()), 0);
        let block: Block = send.into();
        let proposal_hash = block.hash();

        fixture.snapshots.rai().install_committee(RaiCommittee::new(
            0,
            [(local_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        fixture
            .snapshots
            .handle_rai_message(RaiMessage::Proposal(RaiProposal::new(election, block)));

        let flood_events = fixture.flood_tracker.output();
        assert!(
            !flood_events.is_empty(),
            "Should flood the Rai first vote and any derived Rai objects"
        );
        assert!(flood_events.iter().any(|event| matches!(
            event,
            FloodEvent {
                message: Message::Rai(RaiMessage::Vote(vote)),
                traffic_type: TrafficType::LedgerSnapshots,
                scale: 0.0,
                all_prs: true,
            } if vote.phase == RaiVotePhase::First
                && vote.election == election
                && vote.target == RaiVoteTarget::Proposal(proposal_hash)
                && vote.voter == local_key.public_key()
        )));
    }

    #[test]
    fn received_rai_proposal_with_wrong_slot_is_discarded() {
        let ledger = Arc::new(Ledger::new_null());
        let send = LedgerInserter::new(&ledger)
            .genesis()
            .send(Account::from(99), Amount::raw(1));
        let fixture =
            FixtureBuilder::with_ledger_and_weights(ledger, RepWeights::default(), Amount::nano(1));
        let local_key = &fixture.rep_keys.local_rep;
        let election = RaiElectionId::new(RaiSlot::new(Account::from(42), send.height()), 0);

        fixture.snapshots.rai().install_committee(RaiCommittee::new(
            0,
            [(local_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        fixture
            .snapshots
            .handle_rai_message(RaiMessage::Proposal(RaiProposal::new(
                election,
                send.into(),
            )));

        assert!(fixture.flood_tracker.output().is_empty());
        assert!(fixture.snapshots.rai().election(&election).is_none());
    }

    #[test]
    fn received_rai_proposal_with_invalid_block_is_discarded() {
        let ledger = Arc::new(Ledger::new_null());
        let send = LedgerInserter::new(&ledger)
            .genesis()
            .send(Account::from(99), Amount::raw(1));
        let fixture =
            FixtureBuilder::with_ledger_and_weights(ledger, RepWeights::default(), Amount::nano(1));
        let local_key = &fixture.rep_keys.local_rep;
        let election = RaiElectionId::new(RaiSlot::new(send.account(), send.height()), 0);
        let mut block: Block = send.into();
        block.set_signature(Signature::new());

        fixture.snapshots.rai().install_committee(RaiCommittee::new(
            0,
            [(local_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        fixture
            .snapshots
            .handle_rai_message(RaiMessage::Proposal(RaiProposal::new(election, block)));

        assert!(fixture.flood_tracker.output().is_empty());
        assert!(fixture.snapshots.rai().election(&election).is_none());
    }

    #[test]
    fn received_rai_open_proposal_for_existing_account_is_discarded() {
        let ledger = Arc::new(Ledger::new_null());
        let account_key = PrivateKey::from(1);
        let first_send = LedgerInserter::new(&ledger)
            .genesis()
            .send(account_key.account(), Amount::raw(1));
        LedgerInserter::new(&ledger)
            .account(&account_key)
            .receive(first_send.hash());
        let second_send = LedgerInserter::new(&ledger)
            .genesis()
            .send(account_key.account(), Amount::raw(1));
        let fixture = FixtureBuilder::with_ledger_and_weights(
            ledger.clone(),
            RepWeights::default(),
            Amount::nano(1),
        );
        let local_key = &fixture.rep_keys.local_rep;
        let election = RaiElectionId::new(RaiSlot::new(account_key.account(), 1), 0);
        let block = TestBlockBuilder::state()
            .key(&account_key)
            .account(account_key.account())
            .previous(BlockHash::ZERO)
            .representative(account_key.public_key())
            .balance(Amount::raw(1))
            .link(second_send.hash())
            .work(u64::MAX)
            .is_receive()
            .build();

        fixture.snapshots.rai().install_committee(RaiCommittee::new(
            0,
            [(local_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        fixture
            .snapshots
            .handle_rai_message(RaiMessage::Proposal(RaiProposal::new(election, block)));

        assert!(fixture.flood_tracker.output().is_empty());
        assert!(fixture.snapshots.rai().election(&election).is_none());
    }

    #[test]
    fn terminal_rai_proposal_confirms_existing_ledger_block() {
        let ledger = Arc::new(Ledger::new_null());
        let send = LedgerInserter::new(&ledger)
            .genesis()
            .send(Account::from(99), Amount::raw(1));
        let proposal_hash = send.hash();
        let election = RaiElectionId::new(RaiSlot::new(send.account(), send.height()), 0);
        let fixture = FixtureBuilder::with_ledger_and_weights(
            ledger.clone(),
            RepWeights::default(),
            Amount::nano(1),
        );
        let local_key = &fixture.rep_keys.local_rep;

        assert!(!ledger.confirmed().block_exists(&proposal_hash));
        fixture.snapshots.rai().install_committee(RaiCommittee::new(
            0,
            [(local_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        fixture
            .snapshots
            .handle_rai_message(RaiMessage::Proposal(RaiProposal::new(
                election,
                send.into(),
            )));

        assert!(ledger.confirmed().block_exists(&proposal_hash));
    }

    #[test]
    fn terminal_rai_proposal_replaces_ledger_fork_competitor() {
        let ledger = Arc::new(Ledger::new_null());
        let competitor = LedgerInserter::new(&ledger)
            .genesis()
            .send(Account::from(99), Amount::raw(1));
        let winner = TestBlockBuilder::state()
            .key(&DEV_GENESIS_KEY)
            .previous(ledger.genesis().hash())
            .representative(competitor.representative_field().unwrap())
            .balance(competitor.balance())
            .link(Account::from(100))
            .is_send()
            .build();
        let proposal_hash = winner.hash();
        let root = winner.qualified_root();
        ledger.mark_fork(&root, 0);
        let election =
            RaiElectionId::new(RaiSlot::new(competitor.account(), competitor.height()), 0);

        let fixture = FixtureBuilder::with_ledger_and_weights(
            ledger.clone(),
            RepWeights::default(),
            Amount::nano(1),
        );
        let local_key = &fixture.rep_keys.local_rep;

        assert!(ledger.any().block_exists(&competitor.hash()));
        assert!(!ledger.any().block_exists(&proposal_hash));
        fixture.snapshots.rai().install_committee(RaiCommittee::new(
            0,
            [(local_key.public_key(), Amount::nano(1))],
            0,
            0,
        ));

        fixture
            .snapshots
            .handle_rai_message(RaiMessage::Proposal(RaiProposal::new(election, winner)));

        assert!(!ledger.any().block_exists(&competitor.hash()));
        assert!(ledger.confirmed().block_exists(&proposal_hash));
        assert!(!ledger.any().is_forked(&root));
    }

    #[test]
    fn terminal_rai_timeout_is_not_stored_as_a_slot_closure() {
        let fixture = Fixture::new();
        let election = RaiElectionId::new(RaiSlot::new(Account::from(42), 1), 0);
        let record = RaiTerminalRecord::new(election, RaiTerminalOutcome::Timeout);

        fixture.snapshots.complete_rai_terminal_record(record);

        assert_eq!(
            fixture.snapshots.ledger.rai_terminal_record(&election.slot),
            None
        );
    }

    #[test]
    fn rai_proposal_validation_ignores_stored_timeout_for_same_slot() {
        let slot = RaiSlot::new(DEV_GENESIS_KEY.account(), 2);
        let timed_out =
            RaiTerminalRecord::new(RaiElectionId::new(slot, 0), RaiTerminalOutcome::Timeout);
        let ledger = Ledger::new_null_builder()
            .rai_terminal_record(timed_out)
            .finish();
        let send = LedgerInserter::new(&ledger)
            .genesis()
            .send(Account::from(99), Amount::raw(1));
        assert_eq!(slot, RaiSlot::new(send.account(), send.height()));

        let block: Block = send.into();
        let retry = RaiElectionId::new(slot, 1);

        assert!(ledger.validate_rai_proposal(&retry, &block).is_ok());
    }

    #[test]
    fn initial_snapshot_number_should_be_zero() {
        let ledger_snapshots = LedgerSnapshots::new_null();

        assert_eq!(ledger_snapshots.get_current_snapshot_number(), 0);
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

        assert_eq!(output, vec![fork_block.hash()]);
        assert_eq!(
            fixture
                .snapshots
                .ledger
                .any()
                .is_forked(&fork_block.qualified_root()),
            false,
            "Should delete the fork from the forks table"
        );
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

            Self::with_ledger_and_weights(ledger.into(), rep_weights, quorum_weight)
        }

        fn with_ledger_and_weights(
            ledger: Arc<Ledger>,
            rep_weights: RepWeights,
            quorum_weight: Amount,
        ) -> Fixture {
            let flooder = MessageFlooder::new_null();
            let flood_tracker = flooder.track_floods();

            let mut online_reps =
                OnlineReps::new(Arc::new(rep_weights.into()), Amount::ZERO, Amount::ZERO);
            online_reps.set_trended(quorum_weight / ONLINE_WEIGHT_QUORUM as u128 * 100);
            let online_reps = Arc::new(Mutex::new(online_reps));

            let snapshots =
                LedgerSnapshots::new(ledger.clone(), get_test_key, flooder, online_reps);

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
