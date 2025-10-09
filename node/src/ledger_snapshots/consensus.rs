use std::{collections::HashMap, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}};
use rsnano_messages::{Aggregatable, Message, Proposal, ProposalHash, ProposalVote};
use rsnano_network::TrafficType;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{Amount, PrivateKey};
use crate::{ledger_snapshots::Aggregator, representatives::OnlineReps, transport::MessageFlooder};

pub struct Consensus {
    /// For simplicity we currently assume that there is at most
    /// one representative key!
    /// TODO: We have to extend this later to multiple representatives per node.
    get_private_key: Arc<dyn Fn() -> Option<PrivateKey> + Send + Sync>,
    flooder: Arc<Mutex<MessageFlooder>>,
    online_reps: Arc<Mutex<OnlineReps>>,
    receive_proposal_listener: OutputListenerMt<Proposal>,
    proposal_aggregator: Mutex<Aggregator<Proposal>>,
    receive_proposal_vote_listener: OutputListenerMt<ProposalVote>,
    proposal_vote_aggregator: Mutex<Aggregator<ProposalVote>>,
    proposal_voted: AtomicBool,
}

impl Consensus {
    pub fn new(get_private_key: Arc<dyn Fn() -> Option<PrivateKey> + Send + Sync>, 
        flooder: Arc<Mutex<MessageFlooder>>,
        online_reps: Arc<Mutex<OnlineReps>>, 
        ) -> Self {
        Self { 
            get_private_key, 
            flooder, 
            online_reps, 
            receive_proposal_listener: OutputListenerMt::default(), 
            proposal_aggregator: Default::default(), 
            receive_proposal_vote_listener: OutputListenerMt::default(), 
            proposal_vote_aggregator: Default::default(), 
            proposal_voted: AtomicBool::new(false), 
        }
    }

    pub fn receive_proposal(&self, proposal: Proposal) {
        self.receive_proposal_listener.emit(proposal.clone());

        let (rep_weights, quorum_weight) = self.online_reps.lock().unwrap().get_consensus_params();

        let mut proposal_aggregator = self.proposal_aggregator.lock().unwrap();
        proposal_aggregator.add(proposal);

        if proposal_aggregator.has_quorum(&rep_weights, &quorum_weight)
            && !self.proposal_voted.load(Ordering::SeqCst)
        {
            if let Some(proposal_vote) = self.create_proposal_vote() {
                self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                    &Message::SnapshotProposalVote(proposal_vote),
                    TrafficType::LedgerSnapshots,
                    0.0,
                );
                self.proposal_voted.store(true, Ordering::SeqCst);
            }
        }
    }

    fn create_proposal_vote(&self) -> Option<ProposalVote> {
        Some(ProposalVote::new(
            self.proposal_aggregator.lock().unwrap().values().map(|p| p.hash()).max()?,
            &(self.get_private_key)().unwrap(),
        ))
    }

    pub fn track_received_proposals(&self) -> Arc<OutputTrackerMt<Proposal>> {
        self.receive_proposal_listener.track()
    }

    pub fn track_received_proposal_votes(&self) -> Arc<OutputTrackerMt<ProposalVote>> {
        self.receive_proposal_vote_listener.track()
    }

    pub fn receive_proposal_vote(&self, proposal_vote: ProposalVote) {
        self.receive_proposal_vote_listener
            .emit(proposal_vote.clone());

        let mut vote_aggregator = self.proposal_vote_aggregator.lock().unwrap();
        vote_aggregator.add(proposal_vote);

        if let Some(winner) = self.find_winner_proposal() {
            tracing::warn!(proposal_hash=?winner, "Found a winner!");
        }
    }

    pub(crate) fn find_winner_proposal<'a>(
        &self,
    ) -> Option<ProposalHash> {
        let aggregator = self.proposal_vote_aggregator.lock().unwrap();
        let votes = aggregator.values();
        let (rep_weights, quorum_weight) = self.online_reps.lock().unwrap().get_consensus_params();
        let mut tallies: HashMap<ProposalHash, Amount> = HashMap::new();
    
        for vote in votes.into_iter() {
            let weight: &mut Amount = tallies.entry(vote.proposal_hash).or_default();
            *weight += rep_weights.weight(&vote.voter);
        }
    
        tallies
            .into_iter()
            .find(|(p, w)| *w >= quorum_weight)
            .map(|(p, _)| p)
    }
}