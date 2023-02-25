use std::{sync::Arc, ffi::c_void, ops::Deref};

use rsnano_core::{BlockHash, UncheckedKey, UncheckedInfo, HashOrAccount};
use rsnano_node::unchecked_map::{UncheckedMapThread, EntriesContainer, UncheckedMap};

use crate::{StatHandle, ledger::datastore::{lmdb::{LmdbStoreHandle, UncheckedKeyDto}, TransactionHandle}, VoidPointerCallback, utils::ContextWrapper, core::UncheckedInfoHandle};

pub struct UncheckedMapHandle(UncheckedMap);

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_create(
    store_handle: *mut LmdbStoreHandle,
    stats_handle: *mut StatHandle,
    disable_delete: bool,
) -> *mut UncheckedMapHandle {
    let unchecked_map = UncheckedMap::builder()
        .store((*store_handle).deref().to_owned())
        .disable_delete(disable_delete)
        .spawn()
        .unwrap();
    Box::into_raw(Box::new(UncheckedMapHandle(
        unchecked_map,
    )))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_destroy(handle: *mut UncheckedMapHandle) {
    drop(Box::from_raw(handle))
}

pub type ActionCallback =
unsafe extern "C" fn(*mut c_void, *mut UncheckedKeyDto, *mut UncheckedInfoHandle);

pub type PredicateCallback =
unsafe extern "C" fn(*mut c_void) -> bool;

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_for_each1(handle: *mut UncheckedMapHandle,  transaction: &mut TransactionHandle,
                                                     action_callback: ActionCallback,
                                                     action_callback_context: *mut c_void,
                                                     drop_action_callback: VoidPointerCallback,
                                                     predicate_callback: PredicateCallback,
                                                     predicate_callback_context: *mut c_void,
                                                     drop_predicate_callback: VoidPointerCallback) {
    let notify_observers_callback = wrap_action_callback(
        action_callback,
        action_callback_context,
        drop_action_callback,
    );

    let notify_observers_callback2 = wrap_predicate_callback(
        predicate_callback,
        predicate_callback_context,
        drop_predicate_callback,
    );

    (*handle).0.thread.for_each1(transaction.as_txn(), notify_observers_callback, notify_observers_callback2);
}

unsafe fn wrap_action_callback(
    callback: ActionCallback,
    context: *mut c_void,
    drop_context: VoidPointerCallback,
) -> Box<dyn FnMut(&UncheckedKey, &UncheckedInfo)> {
    let context_wrapper = ContextWrapper::new(context, drop_context);
    Box::new(move |k, i| {
        callback(
            context_wrapper.get_context(),
            Box::into_raw(Box::new(UncheckedKeyDto::from(k))),
            Box::into_raw(Box::new(UncheckedInfoHandle(i.clone()))),
        );
    })
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_for_each2(handle: *mut UncheckedMapHandle,  transaction: &mut TransactionHandle,
                                                     dependency: *const u8,
                                                     action_callback: ActionCallback,
                                                     action_callback_context: *mut c_void,
                                                     drop_action_callback: VoidPointerCallback,
                                                     predicate_callback: PredicateCallback,
                                                     predicate_callback_context: *mut c_void,
                                                     drop_predicate_callback: VoidPointerCallback,) {
    let notify_observers_callback = wrap_action_callback(
        action_callback,
        action_callback_context,
        drop_action_callback,
    );

    let notify_observers_callback2 = wrap_predicate_callback(
        predicate_callback,
        predicate_callback_context,
        drop_predicate_callback,
    );

    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(dependency, 32));
    (*handle).0.thread.for_each2(transaction.as_txn(), HashOrAccount::from_bytes(bytes), notify_observers_callback, notify_observers_callback2);
}

unsafe fn wrap_predicate_callback(
    callback: PredicateCallback,
    context: *mut c_void,
    drop_context: VoidPointerCallback,
) -> Box<dyn Fn() -> bool> {
    let context_wrapper = ContextWrapper::new(context, drop_context);
    Box::new(move || callback(context_wrapper.get_context()))
}
