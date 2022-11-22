use crate::core::{Account, Amount, Epoch, PendingInfo};
use crate::ffi::core::UncheckedInfoHandle;
use crate::ffi::{copy_account_bytes, copy_amount_bytes};
use num_format::Locale::ha;
use num_traits::FromPrimitive;

pub struct PendingInfoHandle(PendingInfo);

#[repr(C)]
pub struct PendingInfoDto {
    pub source: [u8; 32],
    pub amount: [u8; 16],
    pub epoch: u8,
}

impl From<&PendingInfoDto> for PendingInfo {
    fn from(dto: &PendingInfoDto) -> Self {
        Self {
            source: Account::from_bytes(dto.source),
            amount: Amount::from_be_bytes(dto.amount),
            epoch: FromPrimitive::from_u8(dto.epoch).unwrap_or(Epoch::Invalid),
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn rsn_pending_info_db_size(handle: *const PendingInfoHandle) -> usize {
    (*handle).0.db_size()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_pending_info_create() -> *mut PendingInfoHandle {
    let info = PendingInfo::default();
    Box::into_raw(Box::new(PendingInfoHandle(info)))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_pending_info_create1(
    source: *const u8,
    amount: *const u8,
    epoch: u8,
) -> *mut PendingInfoHandle {
    let info = PendingInfo::new(
        Account::from_ptr(source),
        Amount::from_ptr(amount),
        FromPrimitive::from_u8(epoch).unwrap(),
    );
    Box::into_raw(Box::new(PendingInfoHandle(info)))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_pending_info_get_source(
    handle: *const PendingInfoHandle,
    result: *mut u8,
) {
    copy_account_bytes((*handle).0.source, result);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_pending_info_get_amount(
    handle: *const PendingInfoHandle,
    result: *mut u8,
) {
    copy_amount_bytes((*handle).0.amount, result);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_pending_info_get_epoch(handle: *const PendingInfoHandle) -> u8 {
    (*handle).0.epoch as u8
}

#[no_mangle]
pub unsafe extern "C" fn rsn_pending_info_set_epoch(handle: *mut PendingInfoHandle, epoch: u8) {
    (*handle).0.epoch = FromPrimitive::from_u8(epoch).unwrap();
}

#[no_mangle]
pub unsafe extern "C" fn rsn_pending_info_destroy(handle: *mut PendingInfoHandle) {
    drop(Box::from_raw(handle))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_pending_info_clone(
    handle: *const PendingInfoHandle,
) -> *mut PendingInfoHandle {
    Box::into_raw(Box::new(PendingInfoHandle((*handle).0.clone())))
}
