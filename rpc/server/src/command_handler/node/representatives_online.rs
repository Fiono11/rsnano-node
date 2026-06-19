use std::collections::{HashMap, HashSet};

use rsnano_node::representatives::RegisteredRepSnapshot;
use rsnano_rpc_messages::{
    DetailedRepresentativesOnline, RepWeightDto, RepresentativesOnlineArgs,
    RepresentativesOnlineResponse, SimpleRepresentativesOnline,
};
use rsnano_types::{Account, PublicKey};

use crate::command_handler::RpcCommandHandler;

impl RpcCommandHandler {
    pub(crate) fn representatives_online(
        &self,
        args: RepresentativesOnlineArgs,
    ) -> RepresentativesOnlineResponse {
        self.node
            .rep_tracker
            .with_snapshot(|s| ResponseBuilder::new(args).create_response(s.iter()))
    }
}

struct ResponseBuilder {
    include_weight: bool,
    filter: Option<HashSet<PublicKey>>,
    simple_result: Vec<Account>,
    detailed_result: HashMap<Account, RepWeightDto>,
}

impl ResponseBuilder {
    pub fn new(args: RepresentativesOnlineArgs) -> Self {
        let filter = args
            .accounts
            .map(|accs| accs.iter().map(PublicKey::from).collect());

        Self {
            include_weight: args.weight.unwrap_or_default().inner(),
            filter,
            simple_result: Vec::new(),
            detailed_result: HashMap::new(),
        }
    }

    pub fn create_response(
        mut self,
        online_reps: impl IntoIterator<Item = RegisteredRepSnapshot>,
    ) -> RepresentativesOnlineResponse {
        for rep in online_reps {
            if self.should_include(&rep.public_key) {
                self.add_result(rep);
            }
        }
        self.take_result()
    }

    fn should_include(&self, rep_key: &PublicKey) -> bool {
        if let Some(filter) = &self.filter {
            filter.contains(rep_key)
        } else {
            true
        }
    }

    fn add_result(&mut self, rep: RegisteredRepSnapshot) {
        if self.include_weight {
            self.detailed_result
                .insert(rep.public_key.into(), RepWeightDto { weight: rep.weight });
        } else {
            self.simple_result.push(rep.public_key.into());
        };
    }

    fn take_result(self) -> RepresentativesOnlineResponse {
        if self.include_weight {
            RepresentativesOnlineResponse::Detailed(DetailedRepresentativesOnline {
                representatives: self.detailed_result,
            })
        } else {
            RepresentativesOnlineResponse::Simple(SimpleRepresentativesOnline {
                representatives: self.simple_result,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_handler::test_rpc_command;
    use rsnano_rpc_messages::{
        RepWeightDto, RepresentativesOnlineArgs, RepresentativesOnlineResponse, RpcCommand,
        SimpleRepresentativesOnline,
    };
    use rsnano_types::Account;
    use std::collections::HashMap;

    #[test]
    fn online_reps_command() {
        let result: RepresentativesOnlineResponse = test_rpc_command(
            RpcCommand::RepresentativesOnline(RepresentativesOnlineArgs::default()),
        );
        assert_eq!(
            result,
            RepresentativesOnlineResponse::Simple(SimpleRepresentativesOnline {
                representatives: Vec::new(),
            })
        );
    }

    mod simple_result {
        use super::*;
        use rsnano_node::representatives::RegisteredRep;
        use rsnano_nullable_clock::Timestamp;
        use rsnano_types::Amount;

        #[test]
        fn empty() {
            let response = ResponseBuilder::new(Default::default()).create_response([]);
            assert_eq!(simple_response(response).len(), 0);
        }

        #[test]
        fn one_rep() {
            let rep = RegisteredRepSnapshot::new_test_instance();
            let account = rep.public_key.as_account();
            let response = ResponseBuilder::new(Default::default()).create_response([rep]);
            assert_eq!(simple_response(response), [account]);
        }

        #[test]
        fn filter_all() {
            let response = ResponseBuilder::new(RepresentativesOnlineArgs {
                weight: None,
                accounts: Some(Vec::new()),
            })
            .create_response([RegisteredRepSnapshot {
                rep: RegisteredRep {
                    public_key: 1.into(),
                    channel_id: None,
                    last_seen: Timestamp::new_test_instance(),
                },
                weight: Amount::ZERO,
            }]);
            assert_eq!(simple_response(response), []);
        }

        #[test]
        fn filter_given_accounts() {
            let make_rep = |key: PublicKey| RegisteredRepSnapshot {
                rep: RegisteredRep {
                    public_key: key,
                    ..RegisteredRep::new_test_instance()
                },
                ..RegisteredRepSnapshot::new_test_instance()
            };

            let response = ResponseBuilder::new(RepresentativesOnlineArgs {
                weight: None,
                accounts: Some(vec![2.into(), 3.into()]),
            })
            .create_response([
                make_rep(1.into()),
                make_rep(2.into()),
                make_rep(3.into()),
                make_rep(4.into()),
            ]);
            assert_eq!(simple_response(response), [2.into(), 3.into()]);
        }
    }

    mod detailed_result {
        use super::*;

        #[test]
        fn empty() {
            let response = ResponseBuilder::new(RepresentativesOnlineArgs {
                weight: Some(true.into()),
                accounts: None,
            })
            .create_response([]);

            assert_eq!(detailed_response(response).len(), 0);
        }

        #[test]
        fn one_rep() {
            let args = RepresentativesOnlineArgs {
                weight: Some(true.into()),
                accounts: None,
            };
            let rep = RegisteredRepSnapshot::new_test_instance();
            let response = ResponseBuilder::new(args).create_response([rep.clone()]);

            let response = detailed_response(response);
            assert_eq!(response.len(), 1);
            assert_eq!(
                response.get(&rep.public_key.into()).unwrap().weight,
                rep.weight
            );
        }
    }

    fn simple_response(response: RepresentativesOnlineResponse) -> Vec<Account> {
        match response {
            RepresentativesOnlineResponse::Simple(i) => i.representatives,
            RepresentativesOnlineResponse::Detailed(_) => panic!("simple response expected"),
        }
    }

    fn detailed_response(
        response: RepresentativesOnlineResponse,
    ) -> HashMap<Account, RepWeightDto> {
        match response {
            RepresentativesOnlineResponse::Simple(_) => panic!("detailed response expected"),
            RepresentativesOnlineResponse::Detailed(i) => i.representatives,
        }
    }
}
