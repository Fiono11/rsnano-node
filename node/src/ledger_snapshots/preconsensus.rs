use std::sync::{Arc, Mutex};
use rsnano_ledger::Ledger;
use rsnano_messages::{Message, Preproposal, Proposal};
use rsnano_network::TrafficType;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{Account, BlockHash, PrivateKey};
use crate::{ledger_snapshots::Aggregator, representatives::OnlineReps, transport::MessageFlooder};

pub struct PreConsensus {
    ledger: Arc<Ledger>,
    /// For simplicity we currently assume that there is at most
    /// one representative key!
    /// TODO: We have to extend this later to multiple representatives per node.
    get_private_key: Arc<dyn Fn() -> Option<PrivateKey> + Send + Sync>,
    flooder: Arc<Mutex<MessageFlooder>>,
    receive_preproposal_listener: OutputListenerMt<Preproposal>,
    online_reps: Arc<Mutex<OnlineReps>>,
    preproposal_aggregator: Mutex<Aggregator<Preproposal>>,
}

impl PreConsensus {
    pub fn new(ledger: Arc<Ledger>, get_private_key: Arc<dyn Fn() -> Option<PrivateKey> + Send + Sync>, 
        flooder: Arc<Mutex<MessageFlooder>>,
        online_reps: Arc<Mutex<OnlineReps>>, 
        ) -> Self {
        Self { 
            ledger,
            get_private_key, 
            flooder, 
            online_reps, 
            receive_preproposal_listener: OutputListenerMt::default(), 
            preproposal_aggregator: Default::default(), 
        }
    }

    fn collect_frontiers(&self) -> Vec<(Account, BlockHash)> {
        self.ledger.confirmed().frontiers().collect()
    }

    fn create_preproposal(&self, private_key: &PrivateKey) -> Preproposal {
        let frontiers = self.collect_frontiers();
        Preproposal::new(frontiers, private_key)
    }

    pub fn publish_preproposal(&self) {
        // TODO add test for no private key
        let private_key = (self.get_private_key)().unwrap();
        let preproposal = self.create_preproposal(&private_key);
        let message = Message::SnapshotPreproposal(preproposal);
        self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
            &message,
            TrafficType::LedgerSnapshots,
            0.0,
        );
    }

    pub fn receive_preproposal(&self, preproposal: Preproposal) {
        self.receive_preproposal_listener.emit(preproposal.clone());

        let proposal = {
            let mut preproposal_aggregator = self.preproposal_aggregator.lock().unwrap();
            preproposal_aggregator.add(preproposal);

            let (rep_weights, quorum_weight) = self.online_reps.lock().unwrap().get_consensus_params();

            if preproposal_aggregator.has_quorum(&rep_weights, &quorum_weight) {
                let proposal = Proposal::new(
                    preproposal_aggregator.values(),
                    &(self.get_private_key)().unwrap(),
                );
                Some(proposal)
            } else {
                None
            }
        };

        if let Some(proposal) = proposal {
            self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                &Message::SnapshotProposal(proposal),
                TrafficType::LedgerSnapshots,
                0.0,
            );
        };
    }

    pub fn track_received_preproposals(&self) -> Arc<OutputTrackerMt<Preproposal>> {
        self.receive_preproposal_listener.track()
    }
}