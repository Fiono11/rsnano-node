use std::ffi::c_void;
use std::ops::Deref;
use rsnano_core::{BlockHash, HashOrAccount, UncheckedInfo, UncheckedKey};
use rsnano_node::unchecked_map::{EntryContainer, StateUncheckedMap};
use crate::confirmation_height::ContextWrapper;
use crate::core::{BlockHandle, UncheckedInfoHandle};
use crate::ledger::datastore::{LmdbStoreHandle, TransactionHandle, UncheckedKeyDto};
use crate::VoidPointerCallback;

pub struct UncheckedMapHandle(StateUncheckedMap);

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_create(store_handle: *mut LmdbStoreHandle, disable_delete: bool) -> *mut UncheckedMapHandle {
    //let store = Arc::clone(&(*store_handle).clone());
    let verification = StateUncheckedMap::builder()
        .store((*store_handle).deref().to_owned())
        .disable_delete(disable_delete)
        .spawn()
        .unwrap();
    Box::into_raw(Box::new(UncheckedMapHandle(
        verification,
    )))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_destroy(handle: *mut UncheckedMapHandle) {
    (*handle).0.stop();
    drop(Box::from_raw(handle))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_exists(handle: *mut UncheckedMapHandle, transaction: &mut TransactionHandle, key: UncheckedKeyDto) -> bool {
    (*handle).0.thread.exists(transaction.as_txn(), &UncheckedKey::from(&key))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_trigger(handle: *mut UncheckedMapHandle, ptr: *const u8) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(ptr, 32));
    let dependency = HashOrAccount::from_bytes(bytes);
    (*handle).0.thread.trigger(dependency)
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_stop(handle: *mut UncheckedMapHandle) {
    let verification = unsafe { &mut (*handle).0 };
    verification.stop().unwrap();
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_flush(handle: *mut UncheckedMapHandle) {
    (*handle).0.thread.flush()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_count(handle: *mut UncheckedMapHandle, transaction: &mut TransactionHandle) -> usize {
    (*handle).0.thread.count(transaction.as_txn())
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_put(handle: *mut UncheckedMapHandle, ptr: *const u8, info: *mut UncheckedInfoHandle) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(ptr, 32));
    let dependency = HashOrAccount::from_bytes(bytes);
    (*handle).0.thread.put(dependency, (*info).0.clone());
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_del(handle: *mut UncheckedMapHandle,  transaction: &mut TransactionHandle, key: UncheckedKeyDto) {
   (*handle).0.thread.del(transaction.as_write_txn(), &UncheckedKey::from(&key));
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_clear(handle: *mut UncheckedMapHandle,  transaction: &mut TransactionHandle) {
    (*handle).0.thread.clear(transaction.as_write_txn());
}

pub type ActionCallback =
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

    (*handle).0.thread.for_each1(transaction.as_txn(), notify_observers_callback);
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
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_get(handle: *mut UncheckedMapHandle, transaction: &mut TransactionHandle, ptr: *const u8,
                                               target: *mut InfoVecDto) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(ptr, 32));
    let hash = BlockHash::from_bytes(bytes);
    let infos = (*handle).0.thread.get(transaction.as_txn(), hash);
    let mut items: Vec<InfoItemDto> = Vec::new();
    for info in infos {
        let info_item_dto = InfoItemDto {
            block: Box::into_raw(Box::new(BlockHandle::new(info.block.unwrap()))),
            modified: info.modified,
        };
        items.push(info_item_dto);
    }
    let raw_data = Box::new(InfoVecRawPtr(items));
    (*target).items = raw_data.0.as_ptr();
    (*target).count = raw_data.0.len();
    (*target).raw_data = Box::into_raw(raw_data);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_destroy_dto(vector: *mut InfoVecDto) {
    drop(Box::from_raw((*vector).raw_data))
}

#[repr(C)]
pub struct InfoItemDto {
    block: *mut BlockHandle,
    modified: u64,
}

#[repr(C)]
pub struct InfoVecDto {
    pub items: *const InfoItemDto,
    pub count: usize,
    pub raw_data: *mut InfoVecRawPtr,
}

pub struct InfoVecRawPtr(Vec<InfoItemDto>);
