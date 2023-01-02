use std::{clone, ptr};
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;
use rsnano_core::Account;
use rsnano_node::config::NodeConfig;
use rsnano_node::online_reps::OnlineReps;
use crate::ledger::datastore::LedgerHandle;
use crate::{fill_node_config_dto, NodeConfigDto, U256ArrayDto};
use crate::core::BlockHandle;

pub struct OnlineRepsHandle(pub OnlineReps);

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_create(ledger_handle: *mut LedgerHandle, node_config_dto: NodeConfigDto) -> *mut OnlineRepsHandle {
    Box::into_raw(Box::new(OnlineRepsHandle(OnlineReps::new(
        (*ledger_handle).clone(), Arc::new(NodeConfig::try_from(&node_config_dto).unwrap()),
    ))))
}

/*#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_create1() -> *mut OnlineRepsHandle {
    Box::into_raw(Box::new(OnlineRepsHandle(OnlineReps::default())))
}*/

/*#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_clone(
    handle: *const OnlineRepsHandle,
) -> *mut OnlineRepsHandle {
    Box::into_raw(Box::new(OnlineRepsHandle((*handle).0.clone())))
}*/

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_destroy(handle: *mut OnlineRepsHandle) {
    drop(Box::from_raw(handle))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_ledger(
    handle: *const OnlineRepsHandle,
) -> *mut LedgerHandle {
    let ledger = (*handle).0.ledger.clone();
    Box::into_raw(Box::new(LedgerHandle::new(ledger)))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_config(
    handle: *const OnlineRepsHandle,
    config_dto: *mut NodeConfigDto,
) {
    let config = (*handle).0.node_config.clone();
    fill_node_config_dto(config_dto.as_mut().unwrap(), &*config);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_observe(handle: *mut OnlineRepsHandle, rep_a: *const u8) {
    let rep_a = Account::from_ptr(rep_a);
    (*handle).0.observe(rep_a)
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_sample(handle: *mut OnlineRepsHandle) -> *const u8 {
    (*handle).0.calculate_online().to_be_bytes().as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_trended(handle: *mut OnlineRepsHandle) -> *const u8 {
    (*handle).0.trended().to_be_bytes().as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_online(handle: *mut OnlineRepsHandle) -> *const u8 {
    (*handle).0.online().to_be_bytes().as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_delta(handle: *mut OnlineRepsHandle) -> *const u8 {
    (*handle).0.delta().to_be_bytes().as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_list(
    handle: *mut OnlineRepsHandle,
    result: *mut U256ArrayDto,
) {
    let accounts = (*handle).0.list();
    let data = Box::new(accounts.iter().map(|a| *a.as_bytes()).collect());
    (*result).initialize(data);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_clear(handle: *mut OnlineRepsHandle) {
    (*handle).0.clear();
}