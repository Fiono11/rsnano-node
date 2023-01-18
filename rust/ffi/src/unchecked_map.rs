use rsnano_core::{BlockHash, HashOrAccount, UncheckedInfo, UncheckedKey};
use rsnano_node::unchecked_map::UncheckedMap;
use crate::core::{BlockArrayDto, BlockArrayRawPtr, BlockHandle, UncheckedInfoHandle};
use crate::ledger::datastore::{LmdbStoreHandle, TransactionHandle, UncheckedKeyDto};

pub struct UncheckedMapHandle(UncheckedMap);

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_create(store_handle: *mut LmdbStoreHandle, disable_delete: bool) -> *mut UncheckedMapHandle {
    let unchecked_map = UncheckedMap::new(
        (*store_handle).clone(),
        disable_delete,
    );
    let unchecked_map_ptr = Box::into_raw(Box::new(UncheckedMapHandle(unchecked_map)));
    let unchecked_map = unsafe { &mut *(unchecked_map_ptr as *mut UncheckedMap) };
    //unchecked_map.run();
    unchecked_map_ptr
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_destroy(handle: *mut UncheckedMapHandle) {
    (*handle).0.stop();
    drop(Box::from_raw(handle))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_exists(handle: *mut UncheckedMapHandle, transaction: &mut TransactionHandle, key: UncheckedKeyDto) -> bool {
    (*handle).0.exists(transaction.as_write_txn(), &UncheckedKey::from(&key))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_trigger(handle: *mut UncheckedMapHandle, ptr: *const u8) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(ptr, 32));
    let dependency = HashOrAccount::from_bytes(bytes);
    (*handle).0.trigger(dependency);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_stop(handle: *mut UncheckedMapHandle) {
    (*handle).0.stop()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_flush(handle: *mut UncheckedMapHandle) {
    (*handle).0.flush()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_count(handle: *mut UncheckedMapHandle, transaction: &mut TransactionHandle) -> usize {
    (*handle).0.count(transaction.as_txn())
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_put(handle: *mut UncheckedMapHandle, ptr: *const u8, info: *mut UncheckedInfoHandle) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(ptr, 32));
    let dependency = HashOrAccount::from_bytes(bytes);
    (*handle).0.put(dependency, (*info).0.clone());
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_del(handle: *mut UncheckedMapHandle,  transaction: &mut TransactionHandle, key: UncheckedKeyDto) {
   (*handle).0.del(transaction.as_write_txn(), &UncheckedKey::from(&key));
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_clear(handle: *mut UncheckedMapHandle,  transaction: &mut TransactionHandle) {
    (*handle).0.clear(transaction.as_write_txn());
}

/*#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_for_each1(handle: *mut UncheckedMapHandle,  transaction: &mut TransactionHandle) {
    (*handle).0.clear(transaction.as_write_txn());
}

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_for_each2(handle: *mut UncheckedMapHandle,  transaction: &mut TransactionHandle) {
    (*handle).0.clear(transaction.as_write_txn());
}*/

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_get(handle: *mut UncheckedMapHandle, transaction: &mut TransactionHandle, ptr: *const u8,
                                               target: *mut InfoVecDto) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(ptr, 32));
    let hash = BlockHash::from_bytes(bytes);
    let infos = (*handle).0.get(transaction.as_txn(), hash);
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
