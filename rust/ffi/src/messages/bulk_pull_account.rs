use super::{create_message_handle2, MessageHandle};
use crate::{NetworkConstantsDto, StringDto};
use num_traits::FromPrimitive;
use rsnano_core::{Account, Amount};
use rsnano_messages::{BulkPullAccount, BulkPullAccountFlags, Message};
use std::ops::Deref;

unsafe fn get_payload_mut(message_handle: &mut MessageHandle) -> &mut BulkPullAccount {
    let Message::BulkPullAccount(payload) = &mut message_handle.message else {
        panic!("not a bulk_pull_account message")
    };
    payload
}

unsafe fn get_payload(message_handle: &MessageHandle) -> &BulkPullAccount {
    let Message::BulkPullAccount(payload) = &message_handle.message else {
        panic!("not a bulk_pull_account message")
    };
    payload
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_create3(
    constants: *mut NetworkConstantsDto,
    payload: &BulkPullAccountPayloadDto,
) -> *mut MessageHandle {
    let payload = BulkPullAccount {
        account: Account::from_bytes(payload.account),
        minimum_amount: Amount::from_be_bytes(payload.minimum_amount),
        flags: FromPrimitive::from_u8(payload.flags).unwrap(),
    };
    create_message_handle2(constants, || Message::BulkPullAccount(payload))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_clone(
    other: &MessageHandle,
) -> *mut MessageHandle {
    MessageHandle::new(other.deref().clone())
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_account(
    handle: &MessageHandle,
    account: *mut u8,
) {
    get_payload(handle).account.copy_bytes(account);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_set_account(
    handle: &mut MessageHandle,
    account: *const u8,
) {
    get_payload_mut(handle).account = Account::from_ptr(account);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_minimum_amount(
    handle: &MessageHandle,
    amount: *mut u8,
) {
    get_payload(handle).minimum_amount.copy_bytes(amount);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_set_minimum_amount(
    handle: &mut MessageHandle,
    amount: *const u8,
) {
    get_payload_mut(handle).minimum_amount = Amount::from_ptr(amount);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_flags(handle: &MessageHandle) -> u8 {
    get_payload(handle).flags as u8
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_set_flags(
    handle: &mut MessageHandle,
    flags: u8,
) {
    get_payload_mut(handle).flags = BulkPullAccountFlags::from_u8(flags).unwrap();
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_size() -> usize {
    BulkPullAccount::serialized_size()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_bulk_pull_account_to_string(
    handle: &MessageHandle,
    result: *mut StringDto,
) {
    (*result) = handle.message.to_string().into();
}

#[repr(C)]
pub struct BulkPullAccountPayloadDto {
    pub account: [u8; 32],
    pub minimum_amount: [u8; 16],
    pub flags: u8,
}
