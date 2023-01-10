use std::clone;
use rsnano_node::unchecked_map::UncheckedMap;
use crate::ledger::datastore::LmdbStoreHandle;

pub struct UncheckedMapHandle(UncheckedMap);

#[no_mangle]
pub unsafe extern "C" fn rsn_unchecked_map_create(store_handle: *mut LmdbStoreHandle, disable_delete: bool) -> *mut UncheckedMapHandle {
    let handle = Box::into_raw(Box::new(UncheckedMapHandle(UncheckedMap::new(
        (*store_handle).clone(),
        disable_delete,
    ))));
    (*handle).0.run();
    handle
}