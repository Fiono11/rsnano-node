use crate::core::BlockHandle;
use crate::ledger::datastore::{LedgerHandle, TransactionHandle};
use crate::ledger::RepWeightsHandle;
use crate::{copy_amount_bytes, fill_node_config_dto, NodeConfigDto, U256ArrayDto};
use rsnano_core::{Account, Amount};
use rsnano_ledger::RepWeights;
use rsnano_node::config::NodeConfig;
use rsnano_node::online_reps::{OnlineReps, ONLINE_WEIGHT_QUORUM};
use rsnano_node::stats::StatType::Message;
use rsnano_store_traits::Transaction;
use std::net::Shutdown::{Read, Write};
use std::ops::Deref;
use std::os::raw;
use std::os::raw::c_int;
use std::sync::Arc;
use std::time::Duration;
use std::{clone, ptr};

pub struct OnlineRepsHandle(pub OnlineReps);

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_create(
    ledger_handle: *mut LedgerHandle,
    node_config_dto: NodeConfigDto,
) -> *mut OnlineRepsHandle {
    Box::into_raw(Box::new(OnlineRepsHandle(OnlineReps::new(
        (*ledger_handle).clone(),
        Arc::new(NodeConfig::try_from(&node_config_dto).unwrap()),
    ))))
}

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
pub unsafe extern "C" fn rsn_online_reps_sample(handle: *mut OnlineRepsHandle) {
    (*handle).0.sample();
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_calculate_trend(
    tx_handle: *mut TransactionHandle,
    handle: *mut OnlineRepsHandle,
    result: *mut u8,
) {
    let amount = (*handle).0.calculate_trend((*tx_handle).as_txn());
    copy_amount_bytes(amount, result);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_calculate_online(
    handle: *mut OnlineRepsHandle,
    result: *mut u8,
) {
    let amount = (*handle).0.calculate_online();
    copy_amount_bytes(amount, result);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_trended(handle: *mut OnlineRepsHandle, result: *mut u8) {
    let amount = (*handle).0.trended();
    copy_amount_bytes(amount, result);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_online(handle: *mut OnlineRepsHandle, result: *mut u8) {
    let amount = (*handle).0.online();
    copy_amount_bytes(amount, result);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_set_online(
    handle: *mut OnlineRepsHandle,
    online: *const u8,
) {
    let amount = Amount::from_ptr(online);
    let mut mutex = (*handle).0.online_m.lock().unwrap();
    *mutex = amount;
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_online_weight_quorum() -> u8 {
    ONLINE_WEIGHT_QUORUM
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_delta(handle: *mut OnlineRepsHandle, result: *mut u8) {
    let amount = (*handle).0.delta();
    copy_amount_bytes(amount, result);
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

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_item_count(handle: *const OnlineRepsHandle) -> usize {
    (*handle).0.count()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_online_reps_item_size() -> usize {
    OnlineReps::item_size()
}
