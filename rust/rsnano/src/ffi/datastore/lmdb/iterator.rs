use std::{ffi::c_void, ptr};

use crate::{
    datastore::{
        lmdb::{LmdbIteratorImpl, LmdbRawIterator, LmdbReadTransaction, MdbVal},
        DbIterator, DbIterator2,
    },
    ffi::VoidPointerCallback,
    utils::{Deserialize, Serialize},
};

use super::{TransactionHandle, TransactionType};

pub struct LmdbIteratorHandle(LmdbRawIterator);

impl LmdbIteratorHandle {
    //todo delete
    pub fn new(it: LmdbRawIterator) -> *mut Self {
        Box::into_raw(Box::new(LmdbIteratorHandle(it)))
    }

    pub fn new2<K, V>(it: DbIterator2<K, V, LmdbIteratorImpl>) -> *mut Self
    where
        K: Serialize + Deserialize<Target = K>,
        V: Deserialize<Target = V>,
    {
        Box::into_raw(Box::new(LmdbIteratorHandle(
            it.take_impl().take_raw_iterator(),
        )))
    }
}

#[no_mangle]
pub unsafe extern "C" fn rsn_lmdb_iterator_destroy(handle: *mut LmdbIteratorHandle) {
    drop(Box::from_raw(handle))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_lmdb_iterator_current(
    handle: *mut LmdbIteratorHandle,
    key: *mut MdbVal,
    value: *mut MdbVal,
) {
    *key = (*handle).0.key.clone();
    *value = (*handle).0.value.clone();
}

#[no_mangle]
pub unsafe extern "C" fn rsn_lmdb_iterator_next(handle: *mut LmdbIteratorHandle) {
    (*handle).0.next();
}

//todo delete
pub fn to_lmdb_iterator_handle<K, V>(
    iterator: &mut dyn DbIterator<K, V>,
) -> *mut LmdbIteratorHandle {
    match iterator.take_lmdb_raw_iterator() {
        Some(it) => LmdbIteratorHandle::new(it),
        None => ptr::null_mut(),
    }
}

pub fn to_lmdb_iterator_handle2<K, V>(
    iterator: DbIterator2<K, V, LmdbIteratorImpl>,
) -> *mut LmdbIteratorHandle
where
    K: Serialize + Deserialize<Target = K>,
    V: Deserialize<Target = V>,
{
    LmdbIteratorHandle::new(iterator.take_impl().take_raw_iterator())
}

pub type ForEachParCallback = extern "C" fn(
    *mut c_void,
    *mut TransactionHandle,
    *mut LmdbIteratorHandle,
    *mut LmdbIteratorHandle,
);

pub struct ForEachParWrapper {
    pub action: ForEachParCallback,
    pub context: *mut c_void,
    pub delete_context: VoidPointerCallback,
}

impl ForEachParWrapper {
    //todo delete
    pub fn execute<K, V>(
        &self,
        txn: &LmdbReadTransaction,
        begin: &mut dyn DbIterator<K, V>,
        end: &mut dyn DbIterator<K, V>,
    ) {
        let lmdb_txn = unsafe {
            std::mem::transmute::<&LmdbReadTransaction, &'static LmdbReadTransaction>(txn)
        };
        let txn_handle = TransactionHandle::new(TransactionType::ReadRef(lmdb_txn));
        let begin_handle = to_lmdb_iterator_handle(begin);
        let end_handle = to_lmdb_iterator_handle(end);
        (self.action)(self.context, txn_handle, begin_handle, end_handle);
    }

    pub fn execute2<K, V>(
        &self,
        txn: &LmdbReadTransaction,
        begin: DbIterator2<K, V, LmdbIteratorImpl>,
        end: DbIterator2<K, V, LmdbIteratorImpl>,
    ) where
        K: Serialize + Deserialize<Target = K>,
        V: Deserialize<Target = V>,
    {
        let lmdb_txn = unsafe {
            std::mem::transmute::<&LmdbReadTransaction, &'static LmdbReadTransaction>(txn)
        };
        let txn_handle = TransactionHandle::new(TransactionType::ReadRef(lmdb_txn));
        let begin_handle = to_lmdb_iterator_handle2(begin);
        let end_handle = to_lmdb_iterator_handle2(end);
        (self.action)(self.context, txn_handle, begin_handle, end_handle);
    }
}

unsafe impl Send for ForEachParWrapper {}
unsafe impl Sync for ForEachParWrapper {}

impl Drop for ForEachParWrapper {
    fn drop(&mut self) {
        unsafe { (self.delete_context)(self.context) }
    }
}
