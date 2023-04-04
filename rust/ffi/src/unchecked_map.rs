use std::{sync::Arc, ffi::c_void, ops::Deref};

use rsnano_core::{BlockHash, UncheckedKey, UncheckedInfo, HashOrAccount};
use rsnano_node::unchecked_map::{UncheckedMapThread, EntriesContainer, UncheckedMap};

use crate::{StatHandle, ledger::datastore::{lmdb::{LmdbStoreHandle, UncheckedKeyDto}, TransactionHandle}, VoidPointerCallback, utils::ContextWrapper, core::{UncheckedInfoHandle, BlockHandle}};

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
        .stats((*stats_handle).deref().to_owned())
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

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_exists(handle: *mut UncheckedMapHandle, key: UncheckedKeyDto) -> bool {
    (*handle).0.exists(&UncheckedKey::from(&key))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_trigger(handle: *mut UncheckedMapHandle, ptr: *const u8) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(ptr, 32));
    let dependency = HashOrAccount::from_bytes(bytes);
    (*handle).0.trigger(&dependency)
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_stop(handle: *mut UncheckedMapHandle) {
    (*handle).0.stop();
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_flush(handle: *mut UncheckedMapHandle) {
    (*handle).0.flush()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_entries_count(handle: *mut UncheckedMapHandle) -> usize {
    (*handle).0.entries_count()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_entries_size(handle: *mut UncheckedMapHandle) -> usize {
    (*handle).0.entries_size()
}


#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_buffer_count(handle: *mut UncheckedMapHandle) -> usize {
    (*handle).0.buffer_count()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_buffer_size(handle: *mut UncheckedMapHandle) -> usize {
    (*handle).0.buffer_size()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_put(handle: *mut UncheckedMapHandle, ptr: *const u8, info: *mut UncheckedInfoHandle) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(ptr, 32));
    let dependency = HashOrAccount::from_bytes(bytes);
    (*handle).0.put(dependency, (*info).0.clone());
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_del(handle: *mut UncheckedMapHandle, key: UncheckedKeyDto) {
   (*handle).0.del( &UncheckedKey::from(&key));
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_clear(handle: *mut UncheckedMapHandle) {
    (*handle).0.clear();
}

pub type ActionCallback =
unsafe extern "C" fn(*mut c_void, *mut UncheckedKeyDto, *mut UncheckedInfoHandle);

pub type PredicateCallback =
unsafe extern "C" fn(*mut c_void) -> bool;

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

unsafe fn wrap_predicate_callback(
    callback: PredicateCallback,
    context: *mut c_void,
    drop_context: VoidPointerCallback,
) -> Box<dyn Fn() -> bool> {
    let context_wrapper = ContextWrapper::new(context, drop_context);
    Box::new(move || callback(context_wrapper.get_context()))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_for_each1(handle: *mut UncheckedMapHandle,
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

    (*handle).0.for_each1(notify_observers_callback, notify_observers_callback2);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_for_each2(handle: *mut UncheckedMapHandle,
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
    (*handle).0.for_each2(&HashOrAccount::from_bytes(bytes), notify_observers_callback, notify_observers_callback2);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_get(handle: *mut UncheckedMapHandle, transaction: &mut TransactionHandle, ptr: *const u8,
                                               target: *mut InfoVecDto) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(ptr, 32));
    let hash = HashOrAccount::from_bytes(bytes);
    let infos = (*handle).0.get(&hash);
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