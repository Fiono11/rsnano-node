use super::{create_message_handle2, message_handle_clone, MessageHandle};
use crate::{
    core::{copy_block_array_dto, BlockArrayDto, BlockHandle},
    NetworkConstantsDto,
};
use rsnano_core::{Account, BlockHash, Frontier};
use rsnano_messages::{
    AccountInfoAckPayload, AscPullAck, AscPullAckType, BlocksAckPayload, Message,
};
use std::{borrow::Borrow, ops::Deref, sync::Arc};

#[no_mangle]
pub unsafe extern "C" fn rsn_message_asc_pull_ack_create2(
    constants: *mut NetworkConstantsDto,
    id: u64,
    payload: *const AccountInfoAckPayloadDto,
) -> *mut MessageHandle {
    let payload = (*payload).borrow().into();
    create_message_handle2(constants, move || {
        Message::AscPullAck(AscPullAck {
            id,
            pull_type: AscPullAckType::AccountInfo(payload),
        })
    })
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_asc_pull_ack_create3(
    constants: *mut NetworkConstantsDto,
    id: u64,
    blocks: *const *const BlockHandle,
    count: usize,
) -> *mut MessageHandle {
    let blocks = if blocks.is_null() {
        &[]
    } else {
        std::slice::from_raw_parts(blocks, count)
    };
    let blocks = blocks
        .iter()
        .map(|&b| (*b).deref().deref().clone())
        .collect();

    create_message_handle2(constants, move || {
        Message::AscPullAck(AscPullAck {
            id,
            pull_type: AscPullAckType::Blocks(BlocksAckPayload::new(blocks)),
        })
    })
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_asc_pull_ack_create4(
    constants: *mut NetworkConstantsDto,
    id: u64,
    frontiers: &FrontierVecHandle,
) -> *mut MessageHandle {
    create_message_handle2(constants, move || {
        Message::AscPullAck(AscPullAck {
            id,
            pull_type: AscPullAckType::Frontiers(frontiers.0.clone()),
        })
    })
}

#[no_mangle]
pub extern "C" fn rsn_message_asc_pull_ack_clone(handle: &MessageHandle) -> *mut MessageHandle {
    message_handle_clone(handle)
}

fn get_payload_mut(handle: &mut MessageHandle) -> &mut AscPullAck {
    let Message::AscPullAck(payload) = &mut handle.message else {
        panic!("not an asc_pull_ack")
    };
    payload
}

fn get_payload(handle: &MessageHandle) -> &AscPullAck {
    let Message::AscPullAck(payload) = &handle.message else {
        panic!("not an asc_pull_ack")
    };
    payload
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_asc_pull_ack_set_id(handle: &mut MessageHandle, id: u64) {
    get_payload_mut(handle).id = id;
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_asc_pull_ack_get_id(handle: &MessageHandle) -> u64 {
    get_payload(handle).id
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_asc_pull_ack_pull_type(handle: &MessageHandle) -> u8 {
    get_payload(handle).payload_type() as u8
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_asc_pull_ack_payload_blocks(
    handle: &MessageHandle,
    blocks: &mut BlockArrayDto,
) {
    match &get_payload(handle).pull_type {
        AscPullAckType::Blocks(blks) => {
            let list: Vec<_> = blks.blocks().iter().map(|b| Arc::new(b.clone())).collect();
            copy_block_array_dto(list, blocks)
        }
        _ => panic!("not a blocks payload"),
    }
}

#[repr(C)]
pub struct AccountInfoAckPayloadDto {
    pub account: [u8; 32],
    pub account_open: [u8; 32],
    pub account_head: [u8; 32],
    pub account_block_count: u64,
    pub account_conf_frontier: [u8; 32],
    pub account_conf_height: u64,
}

impl From<&AccountInfoAckPayload> for AccountInfoAckPayloadDto {
    fn from(payload: &AccountInfoAckPayload) -> Self {
        Self {
            account: *payload.account.as_bytes(),
            account_open: *payload.account_open.as_bytes(),
            account_head: *payload.account_head.as_bytes(),
            account_block_count: payload.account_block_count,
            account_conf_frontier: *payload.account_conf_frontier.as_bytes(),
            account_conf_height: payload.account_conf_height,
        }
    }
}

impl From<&AccountInfoAckPayloadDto> for AccountInfoAckPayload {
    fn from(dto: &AccountInfoAckPayloadDto) -> Self {
        Self {
            account: Account::from_bytes(dto.account),
            account_open: BlockHash::from_bytes(dto.account_open),
            account_head: BlockHash::from_bytes(dto.account_head),
            account_block_count: dto.account_block_count,
            account_conf_frontier: BlockHash::from_bytes(dto.account_conf_frontier),
            account_conf_height: dto.account_conf_height,
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_asc_pull_ack_payload_account_info(
    handle: &MessageHandle,
    result: *mut AccountInfoAckPayloadDto,
) {
    match &get_payload(handle).pull_type {
        AscPullAckType::AccountInfo(account_info) => (*result) = account_info.into(),
        _ => panic!("not an account_info payload"),
    }
}

#[no_mangle]
pub unsafe extern "C" fn rsn_message_asc_pull_ack_payload_frontiers(
    handle: &MessageHandle,
) -> *mut FrontierVecHandle {
    match &get_payload(handle).pull_type {
        AscPullAckType::Frontiers(frontiers) => {
            Box::into_raw(Box::new(FrontierVecHandle(frontiers.clone())))
        }
        _ => panic!("not a frontier payload"),
    }
}

pub struct FrontierVecHandle(Vec<Frontier>);

#[no_mangle]
pub extern "C" fn rsn_frontier_vec_create() -> *mut FrontierVecHandle {
    Box::into_raw(Box::new(FrontierVecHandle(Vec::new())))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_frontier_vec_push(
    handle: &mut FrontierVecHandle,
    account: *const u8,
    hash: *const u8,
) {
    handle.0.push(Frontier::new(
        Account::from_ptr(account),
        BlockHash::from_ptr(hash),
    ))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_frontier_vec_destroy(handle: *mut FrontierVecHandle) {
    drop(Box::from_raw(handle))
}

#[no_mangle]
pub extern "C" fn rsn_frontier_vec_len(handle: &FrontierVecHandle) -> usize {
    handle.0.len()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_frontier_vec_get(
    handle: &FrontierVecHandle,
    index: usize,
    account: *mut u8,
    hash: *mut u8,
) {
    let frontier = handle.0.get(index).unwrap();
    frontier.account.copy_bytes(account);
    frontier.hash.copy_bytes(hash);
}
