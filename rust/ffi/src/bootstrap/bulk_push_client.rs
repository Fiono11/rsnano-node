use super::{bootstrap_attempt::BootstrapAttemptHandle, bootstrap_client::BootstrapClientHandle};
use crate::ledger::datastore::LedgerHandle;
use rsnano_node::bootstrap::{BootstrapStrategy, BulkPushClient};
use std::sync::Arc;

pub struct BulkPushClientHandle(Arc<BulkPushClient>);

#[no_mangle]
pub extern "C" fn rsn_bulk_push_client_create(
    connection: &BootstrapClientHandle,
    ledger: &LedgerHandle,
    attempt: &BootstrapAttemptHandle,
) -> *mut BulkPushClientHandle {
    let mut client = BulkPushClient::new(Arc::clone(connection), Arc::clone(ledger));
    let BootstrapStrategy::Legacy(legacy) = &***attempt else {
        panic!("not legacy")
    };
    client.set_attempt(legacy);
    Box::into_raw(Box::new(BulkPushClientHandle(Arc::new(client))))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_bulk_push_client_destroy(handle: *mut BulkPushClientHandle) {
    drop(Box::from_raw(handle))
}