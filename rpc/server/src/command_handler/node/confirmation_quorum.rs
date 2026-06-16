use rsnano_node::representatives::RepresentativeTracker;
use rsnano_rpc_messages::{ConfirmationQuorumArgs, ConfirmationQuorumResponse, PeerDetailsDto};

use crate::command_handler::RpcCommandHandler;

impl RpcCommandHandler {
    pub(crate) fn confirmation_quorum(
        &self,
        args: ConfirmationQuorumArgs,
    ) -> ConfirmationQuorumResponse {
        create_response(args, &self.node.rep_tracker)
    }
}

fn create_response(
    args: ConfirmationQuorumArgs,
    rep_tracker: &RepresentativeTracker,
) -> ConfirmationQuorumResponse {
    let specs = rep_tracker.quorum_specs();
    let mut result = ConfirmationQuorumResponse {
        quorum_delta: specs.quorum_delta,
        online_weight_quorum_percent: rep_tracker.quorum_percent().into(),
        online_weight_minimum: specs.online_weight_minimum,
        online_stake_total: specs.online_weight,
        trended_stake_total: specs.trended_weight,
        peers_stake_total: specs.peered_weight,
        peers: None,
    };

    if args.include_peer_details() {
        let peers = rep_tracker
            .peered_reps()
            .iter()
            .map(|rep| PeerDetailsDto {
                account: rep.rep_key.into(),
                ip: rep.channel.peer_addr(),
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
    use rsnano_node::representatives::RepresentativeTracker;
    use rsnano_rpc_messages::{ConfirmationQuorumArgs, ConfirmationQuorumResponse, RpcCommand};
    use rsnano_types::Amount;

    #[test]
    fn confirmation_quorum_command() {
        let result: ConfirmationQuorumResponse =
            test_rpc_command(RpcCommand::confirmation_quorum());
        assert!(result.quorum_delta > Amount::ZERO);
    }

    #[test]
    fn quorum_response() {
        let tracker = RepresentativeTracker::new_test_instance();
        let specs = tracker.quorum_specs();
        let response = create_response(ConfirmationQuorumArgs { peer_details: None }, &tracker);
        assert_eq!(response.quorum_delta, tracker.quorum_delta());
        assert_eq!(
            response.online_weight_quorum_percent,
            tracker.quorum_percent().into()
        );
        assert_eq!(response.online_weight_minimum, specs.online_weight_minimum);
        assert_eq!(response.online_stake_total, specs.online_weight);
        assert_eq!(response.trended_stake_total, specs.trended_weight);
        assert_eq!(response.peers_stake_total, specs.peered_weight);
        assert!(response.peers.is_none());
    }

    #[test]
    fn quorum_response_with_peers() {
        let online_reps = RepresentativeTracker::new_test_instance();
        let response = create_response(
            ConfirmationQuorumArgs {
                peer_details: Some(true.into()),
            },
            &online_reps,
        );
        assert_eq!(response.quorum_delta, online_reps.quorum_delta());
        let peers = response.peers.unwrap();
        assert_eq!(peers.len(), online_reps.peered_reps().len());
    }
}
