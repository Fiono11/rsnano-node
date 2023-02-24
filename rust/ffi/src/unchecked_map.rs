use std::sync::Arc;

use rsnano_node::unchecked_map::UncheckedMap;

use crate::{StatHandle, ledger::datastore::lmdb::LmdbStoreHandle};

pub struct UncheckedMapHandle(UncheckedMap);

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_create(
    store_handle: *mut LmdbStoreHandle,
    stats_handle: *mut StatHandle,
    disable_delete: bool,
) -> *mut UncheckedMapHandle {
    Box::into_raw(Box::new(UncheckedMapHandle(UncheckedMap::new(
        Arc::clone(&(*store_handle).0),
        Arc::clone(&(*stats_handle).0),
        disable_delete,
    ))))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_destroy(handle: *mut UncheckedMapHandle) {
    drop(Box::from_raw(handle))
}

/*pub type ActionCallback =
unsafe extern "C" fn(*mut c_void, *mut UncheckedKeyDto, *mut UncheckedInfoHandle);

pub type PredicateCallback =
unsafe extern "C" fn(*mut c_void) -> bool;

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_for_each1(handle: *mut UncheckedMapHandle,  transaction: &mut TransactionHandle,
                                                     action_callback: ActionCallback,
                                                     action_callback_context: *mut c_void,
                                                     drop_action_callback: VoidPointerCallback) {
    let notify_observers_callback = wrap_action_callback(
        action_callback,
        action_callback_context,
        drop_action_callback,
    );

    (*handle).0.notify.for_each1(transaction.as_txn(), notify_observers_callback);
}

unsafe fn wrap_action_callback(
    callback: ActionCallback,
    context: *mut c_void,
    drop_context: VoidPointerCallback,
) -> Box<dyn FnMut(&mut EntryContainer, &UncheckedKey, &UncheckedInfo)> {
    let context_wrapper = ContextWrapper::new(context, drop_context);
    Box::new(move |e, k, i| {
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

    (*handle).0.thread.for_each2(transaction.as_txn(), BlockHash::from_bytes(bytes), notify_observers_callback, notify_observers_callback2);
}

unsafe fn wrap_predicate_callback(
    callback: PredicateCallback,
    context: *mut c_void,
    drop_context: VoidPointerCallback,
) -> Box<dyn Fn() -> bool> {
    let context_wrapper = ContextWrapper::new(context, drop_context);
    Box::new(move || callback(context_wrapper.get_context()))
}*/
