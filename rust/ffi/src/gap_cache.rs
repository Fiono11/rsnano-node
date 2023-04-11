use rsnano_core::{BlockHash, Account};
use core::ffi::c_void;
use std::sync::Arc;
use crate::{VoidPointerCallback, utils::ContextWrapper, NodeConfigDto, online_reps::OnlineRepsHandle, ledger::datastore::{LedgerHandle, lmdb::LmdbStoreHandle}, NodeFlagsHandle, voting::VoteHandle};
use rsnano_node::{GapCache, config::NodeConfig};

pub struct GapCacheHandle(GapCache);

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_create(
    node_config_dto: NodeConfigDto,
    online_reps_handle: *mut OnlineRepsHandle,
    ledger_handle: *mut LedgerHandle,
    node_flags_handle: *mut NodeFlagsHandle,
    start_bootstrap_callback: StartBootstrapCallback,
    start_bootstrap_callback_context: *mut c_void,
    drop_start_bootstrap_callback: VoidPointerCallback,
) -> *mut GapCacheHandle {
    let node_config = Arc::new(NodeConfig::try_from(&node_config_dto).unwrap());
    let ledger = (*ledger_handle).clone();
    let online_reps = Arc::clone(&*online_reps_handle);
    let node_flags = Arc::new((*node_flags_handle).0.lock().unwrap().to_owned());

    let start_bootstrap_callback = wrap_start_bootstrap_callback(
        start_bootstrap_callback,
        start_bootstrap_callback_context,
        drop_start_bootstrap_callback,
    );
    
    let gap_cache = GapCache::new(node_config, online_reps, ledger, node_flags, start_bootstrap_callback);
    //let gap_cache = GapCache;
    Box::into_raw(Box::new(GapCacheHandle(gap_cache)))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_destroy(handle: *mut GapCacheHandle) {
    drop(Box::from_raw(handle))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_add(
    handle: *mut GapCacheHandle,
    hash_a: *const u8,
    time_point_a: i64,
) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(hash_a, 32));
    let hash = BlockHash::from_bytes(bytes);
    (*handle).0.add(&hash, time_point_a);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_erase(
    handle: *mut GapCacheHandle,
    hash_a: *const u8,
) {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(hash_a, 32));
    let hash = BlockHash::from_bytes(bytes);
    (*handle).0.erase(&hash);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_vote(
    handle: *mut GapCacheHandle,
    vote_handle: *mut VoteHandle,
) {
    (*handle).0.vote(&(*vote_handle).0.read().unwrap().to_owned());
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_bootstrap_check(
    handle: *mut GapCacheHandle,
    size: usize,
    voters: *const u8,
    hash: *const u8,
) -> bool {
    let byte_slice = std::slice::from_raw_parts(voters, size);

    let chunk_size = size / 32;
    let chunks = byte_slice.chunks(chunk_size);

    let mut voters: Vec<Account> = Vec::new();
    for chunk in chunks {
        let mut chunk_array = [0u8; 32];
        // Check if the chunk size is exactly 32, if not then return error or fill the array with default value
        if chunk.len() != 32 {
            // fill the remaining with 0s
            for i in 0..32 {
                chunk_array[i] = chunk.get(i).copied().unwrap_or(0u8);
            }
        } else {
            chunk_array.copy_from_slice(chunk);
        }
        let account = Account::from_bytes(chunk_array);
        voters.push(account);
    }

    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(hash, 32));
    (*handle).0.bootstrap_check(&voters.into_iter().collect(), &BlockHash::from_bytes(bytes));
    true
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_bootstrap_threshold(
    handle: *mut GapCacheHandle,
    mut result: *mut u8,
) {
    result = (*handle).0.bootstrap_threshold().to_be_bytes().as_mut_ptr();
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_size(
    handle: *mut GapCacheHandle,
) -> usize {
    (*handle).0.size()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_block_exists(
    handle: *mut GapCacheHandle,
    hash_a: *const u8,
) -> bool {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(hash_a, 32));
    let hash = BlockHash::from_bytes(bytes);
    (*handle).0.block_exists(&hash)
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_earliest(
    handle: *mut GapCacheHandle,
) -> i64 {
    (*handle).0.earliest()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_gap_cache_block_arrival(
    handle: *mut GapCacheHandle,
    hash_a: *const u8,
) -> i64 {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(std::slice::from_raw_parts(hash_a, 32));
    let hash = BlockHash::from_bytes(bytes);
    (*handle).0.block_arrival(&hash)
}

pub type StartBootstrapCallback = unsafe extern "C" fn(*mut c_void, *const u8);

unsafe fn wrap_start_bootstrap_callback(
    callback: StartBootstrapCallback,
    context: *mut c_void,
    drop_context: VoidPointerCallback
) -> Box<dyn Fn(BlockHash)> {
    let context_wrapper = ContextWrapper::new(context, drop_context);
    Box::new(move |block_hash: BlockHash| {
        callback(
            context_wrapper.get_context(),
            block_hash.as_bytes().as_ptr(),
        );
    })
}


