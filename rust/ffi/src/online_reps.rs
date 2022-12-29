use std::clone;
use std::ops::Deref;
use std::time::Duration;
use rsnano_node::config::NodeConfig;
use rsnano_node::online_reps::OnlineReps;
use crate::ledger::datastore::LedgerHandle;
use crate::NodeConfigDto;

pub struct OnlineRepsHandle(OnlineReps);

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_create(ledger_handle: *mut LedgerHandle, node_config_dto: *mut NodeConfigDto) -> *mut OnlineRepsHandle {
    Box::into_raw(Box::new(OnlineRepsHandle(OnlineReps::new(
        (*ledger_handle).clone(), NodeConfig::try_from(&(*node_config_dto)).unwrap(),
    ))))
}