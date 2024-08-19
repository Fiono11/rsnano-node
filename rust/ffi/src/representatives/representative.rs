use rsnano_node::representatives::PeeredRep;

pub struct RepresentativeHandle(pub PeeredRep);

#[no_mangle]
pub unsafe extern "C" fn rsn_representative_destroy(handle: *mut RepresentativeHandle) {
    drop(Box::from_raw(handle));
}

#[no_mangle]
pub unsafe extern "C" fn rsn_representative_clone(
    handle: *mut RepresentativeHandle,
) -> *mut RepresentativeHandle {
    Box::into_raw(Box::new(RepresentativeHandle((*handle).0.clone())))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_representative_account(
    handle: &RepresentativeHandle,
    account: *mut u8,
) {
    handle.0.account.copy_bytes(account);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_representative_channel_id(handle: &RepresentativeHandle) -> usize {
    handle.0.channel_id.as_usize()
}
