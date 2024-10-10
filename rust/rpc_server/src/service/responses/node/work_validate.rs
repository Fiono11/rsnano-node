use rsnano_core::{BlockDetails, BlockHash, DifficultyV1, WorkNonce, WorkVersion};
use rsnano_node::Node;
use rsnano_rpc_messages::WorkValidateDto;
use serde_json::to_string_pretty;
use std::sync::Arc;

pub async fn work_validate(node: Arc<Node>, work: WorkNonce, hash: BlockHash) -> String {
    let result_difficulty =
        node.network_params
            .work
            .difficulty(WorkVersion::Work1, &hash.into(), work.into());

    let default_difficulty = node.network_params.work.threshold_base(WorkVersion::Work1);

    let valid_all = result_difficulty >= default_difficulty;

    let receive_difficulty = node.network_params.work.threshold(&BlockDetails::new(
        rsnano_core::Epoch::Epoch2,
        false,
        true,
        false,
    ));
    let valid_receive = result_difficulty >= receive_difficulty;

    let result_multiplier = DifficultyV1::to_multiplier(result_difficulty, default_difficulty);

    let work_validate_dto = WorkValidateDto {
        valid_all,
        valid_receive,
        difficulty: result_difficulty,
        multiplier: result_multiplier,
    };

    to_string_pretty(&work_validate_dto).unwrap()
}
