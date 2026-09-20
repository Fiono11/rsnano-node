use crate::command_handler::RpcCommandHandler;
use anyhow::anyhow;
use indexmap::IndexMap;
use rsnano_node::consensus::election::ElectionId;
use rsnano_rpc_messages::{
    ConfirmationBlockInfoDto, ConfirmationInfoArgs, ConfirmationInfoResponse, KudzuCertificatesDto,
};
use rsnano_types::{Account, Amount, ConsensusEpoch};

impl RpcCommandHandler {
    pub(crate) fn confirmation_info(
        &self,
        args: ConfirmationInfoArgs,
    ) -> anyhow::Result<ConfirmationInfoResponse> {
        let include_representatives = args.representatives.unwrap_or(false.into()).inner();
        let contents = args.contents.unwrap_or(true.into()).inner();
        let election = match args.epoch {
            Some(epoch) => self.node.aec.election(&ElectionId::new(
                args.root,
                ConsensusEpoch::new(epoch.inner()),
            )),
            None => self.node.aec.election_for_root(&args.root),
        }
        .ok_or_else(|| anyhow!("Active confirmation not found"))?;

        let announcements = 0; // not supported in RsNano
        let voters = election.votes().len();
        let last_winner = election.winner().hash();
        let final_tally = election.winner_final_tally();
        let mut total_tally = Amount::ZERO;
        let mut blocks = IndexMap::new();

        for block in election.candidate_blocks().values() {
            let tally = election.tallies().get(&block.hash());

            total_tally += tally;

            let contents = if contents {
                Some(block.json_representation())
            } else {
                None
            };

            let (representatives, representatives_final, representatives_kudzu) =
                if include_representatives {
                    let mut reps = IndexMap::new();
                    let mut reps_final = IndexMap::new();
                    for (representative, vote) in election.votes() {
                        if block.hash() == vote.hash {
                            let amount = self.node.ledger.rep_weights.weight(representative);

                            reps.insert(Account::from(representative), amount);

                            if vote.is_final_vote() {
                                reps_final.insert(Account::from(representative), amount);
                            }
                        }
                    }
                    reps.sort_by(|k1, _, k2, _| k2.cmp(k1));
                    reps_final.sort_by(|k1, _, k2, _| k2.cmp(k1));
                    let kudzu = cfg!(feature = "rai_protocol")
                        .then(|| kudzu_vote_kinds(&election, &block.hash()));
                    (Some(reps), Some(reps_final), kudzu)
                } else {
                    (None, None, None)
                };

            let first_tally = cfg!(feature = "rai_protocol")
                .then(|| election.kudzu_votes().first_tallies().get(&block.hash()));

            let entry = ConfirmationBlockInfoDto {
                tally,
                contents,
                representatives,
                representatives_final,
                representatives_kudzu,
                first_tally,
            };

            blocks.insert(block.hash(), entry);
        }

        let (state, certificates) = if cfg!(feature = "rai_protocol") {
            let certs = election.certificates();
            (
                Some(election.state().as_str().to_string()),
                Some(KudzuCertificatesDto {
                    notarized: certs.notar.clone(),
                    timeout: certs.timeout,
                    timeout_tally: election.kudzu_votes().timeout_weight(),
                    certificate_threshold: election
                        .committees()
                        .map(|c| c.primary().thresholds().certificate)
                        .unwrap_or_default(),
                    fast: certs.fast,
                    final_: certs.final_,
                    finalizable: election.kudzu_can_finalize(),
                }),
            )
        } else {
            (None, None)
        };

        Ok(ConfirmationInfoResponse {
            announcements: (announcements as u32).into(),
            voters: (voters as u32).into(),
            last_winner,
            total_tally,
            final_tally,
            blocks,
            state,
            epoch: cfg!(feature = "rai_protocol").then(|| election.epoch().as_u64().into()),
            certificates,
        })
    }
}

/// The Kudzu vote kinds each representative cast for a block, e.g. "first,final"
fn kudzu_vote_kinds(
    election: &rsnano_node::consensus::election::Election,
    hash: &rsnano_types::BlockHash,
) -> IndexMap<Account, String> {
    let mut result = IndexMap::new();
    for representative in election.votes().keys() {
        let Some(rep) = election.kudzu_votes().rep(representative) else {
            continue;
        };
        let mut kinds = Vec::new();
        if rep.first == Some(*hash) {
            kinds.push("first");
        }
        if rep.notar.contains(hash) && rep.first != Some(*hash) && rep.final_ != Some(*hash) {
            kinds.push("notar");
        }
        if rep.final_ == Some(*hash) {
            kinds.push("final");
        }
        if rep.abstained() {
            kinds.push("abstain");
        } else if rep.timeout {
            kinds.push("timeout");
        }
        if !kinds.is_empty() {
            result.insert(Account::from(representative), kinds.join(","));
        }
    }
    result.sort_by(|k1, _, k2, _| k2.cmp(k1));
    result
}
