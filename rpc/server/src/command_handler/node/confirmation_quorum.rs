use rsnano_node::representatives::RepresentativeTracker;
use rsnano_rpc_messages::{ConfirmationQuorumArgs, ConfirmationQuorumResponse, PeerDetailsDto};

use crate::command_handler::RpcCommandHandler;
use rsnano_network::{NULL_ENDPOINT, Network};
use std::sync::RwLock;

impl RpcCommandHandler {
    pub(crate) fn confirmation_quorum(
        &self,
        args: ConfirmationQuorumArgs,
    ) -> ConfirmationQuorumResponse {
        create_response(args, &self.node.rep_tracker, &self.node.network)
    }
}

fn create_response(
    args: ConfirmationQuorumArgs,
    rep_tracker: &RepresentativeTracker,
    network: &RwLock<Network>,
) -> ConfirmationQuorumResponse {
    let specs = rep_tracker.quorum_snapshot();
    let mut result = ConfirmationQuorumResponse {
        quorum_delta: specs.quorum_delta,
        online_weight_quorum_percent: specs.quorum_percent.into(),
        online_weight_minimum: specs.online_weight_minimum,
        online_stake_total: specs.online_weight,
        trended_stake_total: specs.trended_or_min_weight,
        peers_stake_total: specs.peered_weight,
        peers: None,
    };

    if args.include_peer_details() {
        let net = network.read().unwrap();
        let peers = rep_tracker
            .peered_reps()
            .into_iter()
            .map(|rep| PeerDetailsDto {
                account: rep.rep_key.into(),
                ip: net
                    .get(rep.channel_id)
                    .map(|c| c.peer_addr())
                    .unwrap_or(NULL_ENDPOINT),
                weight: rep.weight,
            })
            .collect();

        result.peers = Some(peers);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::create_response;
    use crate::command_handler::test_rpc_command;
    use rsnano_network::Network;
    use rsnano_node::representatives::RepresentativeTracker;
    use rsnano_rpc_messages::{ConfirmationQuorumArgs, ConfirmationQuorumResponse, RpcCommand};
    use rsnano_types::Amount;
    use std::sync::RwLock;

    #[test]
    fn confirmation_quorum_command() {
        let result: ConfirmationQuorumResponse =
            test_rpc_command(RpcCommand::confirmation_quorum());
        assert!(result.quorum_delta > Amount::ZERO);
    }

    #[test]
    fn quorum_response() {
        let tracker = RepresentativeTracker::new_null();
        let specs = tracker.quorum_snapshot();
        let network = RwLock::new(Network::new_null());
        let response = create_response(
            ConfirmationQuorumArgs { peer_details: None },
            &tracker,
            &network,
        );
        assert_eq!(
            response.quorum_delta,
            tracker.quorum_snapshot().quorum_delta
        );
        assert_eq!(
            response.online_weight_quorum_percent,
            specs.quorum_percent.into()
        );
        assert_eq!(response.online_weight_minimum, specs.online_weight_minimum);
        assert_eq!(response.online_stake_total, specs.online_weight);
        assert_eq!(response.trended_stake_total, specs.trended_or_min_weight);
        assert_eq!(response.peers_stake_total, specs.peered_weight);
        assert!(response.peers.is_none());
    }

    #[test]
    fn quorum_response_with_peers() {
        let online_reps = RepresentativeTracker::new_null();
        let network = RwLock::new(Network::new_null());
        let response = create_response(
            ConfirmationQuorumArgs {
                peer_details: Some(true.into()),
            },
            &online_reps,
            &network,
        );
        assert_eq!(
            response.quorum_delta,
            online_reps.quorum_snapshot().quorum_delta
        );
        let peers = response.peers.unwrap();
        assert_eq!(peers.len(), online_reps.peered_reps().len());
    }
}
