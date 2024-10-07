use crate::{
    block_processing::{BacklogPopulationHandle, BlockProcessorHandle, UncheckedMapHandle},
    bootstrap::{BootstrapInitiatorHandle, TcpListenerHandle},
    cementation::ConfirmingSetHandle,
    consensus::{
        ActiveTransactionsHandle, ElectionEndedCallback, ElectionSchedulerHandle,
        ElectionStatusHandle, FfiAccountBalanceCallback, LocalVoteHistoryHandle,
        ManualSchedulerHandle, RepTiersHandle, VoteHandle, VoteProcessorVoteProcessedCallback,
        VoteWithWeightInfoVecHandle,
    },
    core::BlockVecHandle,
    fill_node_config_dto,
    ledger::datastore::{lmdb::LmdbStoreHandle, LedgerHandle},
    representatives::{RepCrawlerHandle, RepresentativeRegisterHandle},
    telemetry::TelemetryHandle,
    to_rust_string,
    transport::{EndpointDto, NetworkFilterHandle, TcpChannelsHandle},
    utils::{AsyncRuntimeHandle, ContainerInfoComponentHandle, ContextWrapper, ThreadPoolHandle},
    wallets::LmdbWalletsHandle,
    websocket::WebsocketListenerHandle,
    work::{DistributedWorkFactoryHandle, WorkPoolHandle},
    NetworkParamsDto, NodeConfigDto, NodeFlagsHandle, StatHandle, U256ArrayDto,
    VoidPointerCallback,
};
use rsnano_core::{
    utils::NULL_ENDPOINT, Amount, BlockEnum, BlockHash, PublicKey, Root, Vote, VoteCode, VoteSource,
};
use rsnano_network::ChannelId;
use rsnano_node::{
    consensus::{AccountBalanceChangedCallback, ElectionEndCallback},
    node::{Node, NodeExt},
};
use std::{
    collections::VecDeque,
    ffi::{c_char, c_void},
    net::SocketAddrV6,
    sync::Arc,
    time::Duration,
};

pub struct NodeHandle(Arc<Node>);

#[no_mangle]
pub unsafe extern "C" fn rsn_node_create(
    path: *const c_char,
    async_rt: &AsyncRuntimeHandle,
    config: &NodeConfigDto,
    params: &NetworkParamsDto,
    flags: &NodeFlagsHandle,
    work: &WorkPoolHandle,
    observers_context: *mut c_void,
    delete_observers_context: VoidPointerCallback,
    election_ended: ElectionEndedCallback,
    balance_changed: FfiAccountBalanceCallback,
    vote_processed: VoteProcessorVoteProcessedCallback,
) -> *mut NodeHandle {
    let path = to_rust_string(path);

    let ctx_wrapper = Arc::new(ContextWrapper::new(
        observers_context,
        delete_observers_context,
    ));

    let ctx = Arc::clone(&ctx_wrapper);
    let election_ended_wrapper: ElectionEndCallback = Box::new(
        move |status, votes, account, amount, is_state_send, is_state_epoch| {
            let status_handle = ElectionStatusHandle::new(status.clone());
            let votes_handle = VoteWithWeightInfoVecHandle::new(votes.clone());
            election_ended(
                ctx.get_context(),
                status_handle,
                votes_handle,
                account.as_bytes().as_ptr(),
                amount.to_be_bytes().as_ptr(),
                is_state_send,
                is_state_epoch,
            );
        },
    );

    let ctx = Arc::clone(&ctx_wrapper);
    let account_balance_changed_wrapper: AccountBalanceChangedCallback =
        Box::new(move |account, is_pending| {
            balance_changed(ctx.get_context(), account.as_bytes().as_ptr(), is_pending);
        });

    let ctx = Arc::clone(&ctx_wrapper);
    let vote_processed = Box::new(
        move |vote: &Arc<Vote>, _channel_id: ChannelId, source: VoteSource, code: VoteCode| {
            let vote_handle = VoteHandle::new(Arc::clone(vote));
            vote_processed(ctx.get_context(), vote_handle, source as u8, code as u8);
        },
    );

    Box::into_raw(Box::new(NodeHandle(Arc::new(Node::new(
        async_rt.tokio.handle().clone(),
        path,
        config.try_into().unwrap(),
        params.try_into().unwrap(),
        flags.lock().unwrap().clone(),
        Arc::clone(work),
        election_ended_wrapper,
        account_balance_changed_wrapper,
        vote_processed,
    )))))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_destroy(handle: *mut NodeHandle) {
    drop(Box::from_raw(handle))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_node_id(handle: &NodeHandle, result: *mut u8) {
    handle.0.node_id.private_key().copy_bytes(result);
}

#[no_mangle]
pub extern "C" fn rsn_node_config(handle: &NodeHandle, result: &mut NodeConfigDto) {
    fill_node_config_dto(result, &handle.0.config);
}

#[no_mangle]
pub extern "C" fn rsn_node_stats(handle: &NodeHandle) -> *mut StatHandle {
    StatHandle::new(&Arc::clone(&handle.0.stats))
}

#[no_mangle]
pub extern "C" fn rsn_node_workers(handle: &NodeHandle) -> *mut ThreadPoolHandle {
    Box::into_raw(Box::new(ThreadPoolHandle(Arc::clone(&handle.0.workers))))
}

#[no_mangle]
pub extern "C" fn rsn_node_distributed_work(
    handle: &NodeHandle,
) -> *mut DistributedWorkFactoryHandle {
    Box::into_raw(Box::new(DistributedWorkFactoryHandle(Arc::clone(
        &handle.0.distributed_work,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_store(handle: &NodeHandle) -> *mut LmdbStoreHandle {
    Box::into_raw(Box::new(LmdbStoreHandle(Arc::clone(&handle.0.store))))
}

#[no_mangle]
pub extern "C" fn rsn_node_unchecked(handle: &NodeHandle) -> *mut UncheckedMapHandle {
    Box::into_raw(Box::new(UncheckedMapHandle(Arc::clone(
        &handle.0.unchecked,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_ledger(handle: &NodeHandle) -> *mut LedgerHandle {
    Box::into_raw(Box::new(LedgerHandle(Arc::clone(&handle.0.ledger))))
}

#[no_mangle]
pub extern "C" fn rsn_node_tcp_channels(handle: &NodeHandle) -> *mut TcpChannelsHandle {
    Box::into_raw(Box::new(TcpChannelsHandle(Arc::clone(&handle.0.network))))
}

#[no_mangle]
pub extern "C" fn rsn_node_network_filter(handle: &NodeHandle) -> *mut NetworkFilterHandle {
    Box::into_raw(Box::new(NetworkFilterHandle(Arc::clone(
        &handle.0.publish_filter,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_telemetry(handle: &NodeHandle) -> *mut TelemetryHandle {
    Box::into_raw(Box::new(TelemetryHandle(Arc::clone(&handle.0.telemetry))))
}

#[no_mangle]
pub extern "C" fn rsn_node_representative_register(
    handle: &NodeHandle,
) -> *mut RepresentativeRegisterHandle {
    Box::into_raw(Box::new(RepresentativeRegisterHandle(Arc::clone(
        &handle.0.online_reps,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_rep_tiers(handle: &NodeHandle) -> *mut RepTiersHandle {
    Box::into_raw(Box::new(RepTiersHandle(Arc::clone(&handle.0.rep_tiers))))
}

#[no_mangle]
pub extern "C" fn rsn_node_history(handle: &NodeHandle) -> *mut LocalVoteHistoryHandle {
    Box::into_raw(Box::new(LocalVoteHistoryHandle(Arc::clone(
        &handle.0.history,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_confirming_set(handle: &NodeHandle) -> *mut ConfirmingSetHandle {
    Box::into_raw(Box::new(ConfirmingSetHandle(Arc::clone(
        &handle.0.confirming_set,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_block_processor(handle: &NodeHandle) -> *mut BlockProcessorHandle {
    Box::into_raw(Box::new(BlockProcessorHandle(Arc::clone(
        &handle.0.block_processor,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_wallets(handle: &NodeHandle) -> *mut LmdbWalletsHandle {
    Box::into_raw(Box::new(LmdbWalletsHandle(Arc::clone(&handle.0.wallets))))
}

#[no_mangle]
pub extern "C" fn rsn_node_active(handle: &NodeHandle) -> *mut ActiveTransactionsHandle {
    Box::into_raw(Box::new(ActiveTransactionsHandle(Arc::clone(
        &handle.0.active,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_websocket(handle: &NodeHandle) -> *mut WebsocketListenerHandle {
    match &handle.0.websocket {
        Some(ws) => Box::into_raw(Box::new(WebsocketListenerHandle(Arc::clone(ws)))),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn rsn_node_bootstrap_initiator(
    handle: &NodeHandle,
) -> *mut BootstrapInitiatorHandle {
    Box::into_raw(Box::new(BootstrapInitiatorHandle(Arc::clone(
        &handle.0.bootstrap_initiator,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_rep_crawler(handle: &NodeHandle) -> *mut RepCrawlerHandle {
    Box::into_raw(Box::new(RepCrawlerHandle(Arc::clone(
        &handle.0.rep_crawler,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_tcp_listener(handle: &NodeHandle) -> *mut TcpListenerHandle {
    Box::into_raw(Box::new(TcpListenerHandle(Arc::clone(
        &handle.0.tcp_listener,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_manual(handle: &NodeHandle) -> *mut ManualSchedulerHandle {
    Box::into_raw(Box::new(ManualSchedulerHandle(Arc::clone(
        &handle.0.manual_scheduler,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_priority(handle: &NodeHandle) -> *mut ElectionSchedulerHandle {
    Box::into_raw(Box::new(ElectionSchedulerHandle(Arc::clone(
        &handle.0.priority_scheduler,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_backlog_population(handle: &NodeHandle) -> *mut BacklogPopulationHandle {
    Box::into_raw(Box::new(BacklogPopulationHandle(Arc::clone(
        &handle.0.backlog_population,
    ))))
}

#[no_mangle]
pub extern "C" fn rsn_node_start(handle: &NodeHandle) {
    handle.0.start();
}

#[no_mangle]
pub extern "C" fn rsn_node_stop(handle: &NodeHandle) {
    handle.0.stop();
}

#[no_mangle]
pub extern "C" fn rsn_node_is_stopped(handle: &NodeHandle) -> bool {
    handle.0.is_stopped()
}

#[no_mangle]
pub extern "C" fn rsn_node_ledger_pruning(
    handle: &NodeHandle,
    batch_size: u64,
    bootstrap_weight_reached: bool,
) {
    handle
        .0
        .ledger_pruning(batch_size, bootstrap_weight_reached);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_connect(handle: &NodeHandle, endpoint: &EndpointDto) {
    handle.0.peer_connector.connect_to(endpoint.into());
}

#[no_mangle]
pub extern "C" fn rsn_node_bootstrap_wallet(handle: &NodeHandle) {
    handle.0.bootstrap_wallet();
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_vote(
    handle: &NodeHandle,
    vote: &VoteHandle,
    hash: *const u8,
) -> u8 {
    let result = handle.0.vote_router.vote(vote, VoteSource::Live);
    result
        .get(&BlockHash::from_ptr(hash))
        .cloned()
        .unwrap_or(VoteCode::Invalid) as u8
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_election_active(handle: &NodeHandle, hash: *const u8) -> bool {
    handle.0.vote_router.active(&BlockHash::from_ptr(hash))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_enqueue_vote_request(
    handle: &NodeHandle,
    root: *const u8,
    hash: *const u8,
) {
    handle
        .0
        .vote_generators
        .generate_non_final_vote(&Root::from_ptr(root), &BlockHash::from_ptr(hash));
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_get_rep_weight(
    handle: &NodeHandle,
    account: *const u8,
    weight: *mut u8,
) {
    let result = handle
        .0
        .ledger
        .rep_weights
        .weight(&PublicKey::from_ptr(account));
    result.copy_bytes(weight);
}

#[no_mangle]
pub extern "C" fn rsn_node_get_rep_weights(handle: &NodeHandle) -> *mut RepWeightsVecHandle {
    let mut weights = handle.0.ledger.rep_weights.read().clone();
    Box::into_raw(Box::new(RepWeightsVecHandle(weights.drain().collect())))
}

#[repr(C)]
pub struct RepWeightsVecHandle(Vec<(PublicKey, Amount)>);

#[no_mangle]
pub unsafe extern "C" fn rsn_rep_weights_vec_destroy(handle: *mut RepWeightsVecHandle) {
    drop(Box::from_raw(handle));
}

#[no_mangle]
pub unsafe extern "C" fn rsn_rep_weights_vec_len(handle: &RepWeightsVecHandle) -> usize {
    handle.0.len()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_rep_weights_vec_get(
    handle: &RepWeightsVecHandle,
    index: usize,
    account: *mut u8,
    weight: *mut u8,
) {
    let (acc, wei) = &handle.0[index];
    acc.copy_bytes(account);
    wei.copy_bytes(weight);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_collect_container_info(
    handle: &NodeHandle,
    name: *const c_char,
) -> *mut ContainerInfoComponentHandle {
    let container_info = handle.0.collect_container_info(to_rust_string(name));
    Box::into_raw(Box::new(ContainerInfoComponentHandle(container_info)))
}

#[no_mangle]
pub extern "C" fn rsn_node_confirmation_quorum(
    handle: &NodeHandle,
    result: &mut ConfirmationQuorumDto,
) {
    let reps = handle.0.online_reps.lock().unwrap();
    result.quorum_delta = reps.quorum_delta().to_be_bytes();
    result.online_weight_quorum_percent = reps.quorum_percent();
    result.online_weight_minimum = reps.online_weight_minimum().to_be_bytes();
    result.online_weight = reps.online_weight().to_be_bytes();
    result.trended_weight = reps.trended_weight().to_be_bytes();
    result.peers_weight = reps.peered_weight().to_be_bytes();
    result.minimum_principal_weight = reps.minimum_principal_weight().to_be_bytes();
}

#[repr(C)]
pub struct ConfirmationQuorumDto {
    pub quorum_delta: [u8; 16],
    pub online_weight_quorum_percent: u8,
    pub online_weight_minimum: [u8; 16],
    pub online_weight: [u8; 16],
    pub trended_weight: [u8; 16],
    pub peers_weight: [u8; 16],
    pub minimum_principal_weight: [u8; 16],
}

pub struct RepDetailsHandle(Vec<(PublicKey, SocketAddrV6, Amount)>);

#[no_mangle]
pub extern "C" fn rsn_node_representative_details(handle: &NodeHandle) -> *mut RepDetailsHandle {
    let mut result = Vec::new();
    for rep in handle.0.online_reps.lock().unwrap().peered_reps() {
        let endpoint = handle
            .0
            .network_info
            .read()
            .unwrap()
            .get(rep.channel_id)
            .map(|c| c.peer_addr())
            .unwrap_or(NULL_ENDPOINT);

        result.push((rep.account, endpoint, handle.0.ledger.weight(&rep.account)))
    }
    Box::into_raw(Box::new(RepDetailsHandle(result)))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_rep_details_destroy(handle: *mut RepDetailsHandle) {
    drop(Box::from_raw(handle))
}

#[no_mangle]
pub extern "C" fn rsn_rep_details_len(handle: &RepDetailsHandle) -> usize {
    handle.0.len()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_rep_details_get(
    handle: &RepDetailsHandle,
    index: usize,
    account: *mut u8,
    endpoint: &mut EndpointDto,
    amount: *mut u8,
) {
    let (acc, ep, weight) = &handle.0[index];
    acc.copy_bytes(account);
    *endpoint = ep.into();
    weight.copy_bytes(amount);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_list_online_reps(handle: &NodeHandle, result: *mut U256ArrayDto) {
    let reps = handle.0.online_reps.lock().unwrap();
    let data = reps.online_reps().map(|a| *a.as_bytes()).collect();
    (*result).initialize(data);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_set_online_weight(handle: &NodeHandle, online: *const u8) {
    let amount = Amount::from_ptr(online);
    handle.0.online_reps.lock().unwrap().set_online(amount);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_flood_block_many(
    handle: &NodeHandle,
    blocks: &BlockVecHandle,
    delay_ms: u64,
    callback: VoidPointerCallback,
    context: *mut c_void,
    drop_context: VoidPointerCallback,
) {
    let ctx_wrapper = ContextWrapper::new(context, drop_context);
    let blocks: VecDeque<BlockEnum> = blocks.0.iter().map(|b| b.as_ref().clone()).collect();
    handle.0.flood_block_many(
        blocks,
        Box::new(move || callback(ctx_wrapper.get_context())),
        Duration::from_millis(delay_ms),
    );
}

#[no_mangle]
pub extern "C" fn rsn_node_is_connected_to(handle: &NodeHandle, peer: &EndpointDto) -> bool {
    handle
        .0
        .network_info
        .read()
        .unwrap()
        .find_realtime_channel_by_remote_addr(&peer.into())
        .is_some()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_node_find_endpoint_for_node_id(
    handle: &NodeHandle,
    node_id: *const u8,
    result: &mut EndpointDto,
) -> bool {
    let network = handle.0.network_info.read().unwrap();
    match network.find_node_id(&PublicKey::from_ptr(node_id)) {
        Some(channel) => {
            *result = channel.peer_addr().into();
            true
        }
        None => false,
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct PeerInfoDto {
    pub has_node_id: bool,
    pub node_id: [u8; 32],
    pub protocol_version: u8,
    pub remote_endpoint: EndpointDto,
    pub peering_endpoint: EndpointDto,
}

pub struct PeerInfoListHandle(Vec<PeerInfoDto>);

#[no_mangle]
pub unsafe extern "C" fn rsn_node_get_peers(handle: &NodeHandle) -> *mut PeerInfoListHandle {
    let mut peers: Vec<_> = handle
        .0
        .network_info
        .read()
        .unwrap()
        .random_realtime_channels(usize::MAX, 0)
        .iter()
        .map(|channel| PeerInfoDto {
            has_node_id: channel.node_id().is_some(),
            node_id: *channel.node_id().unwrap_or_default().as_bytes(),
            protocol_version: channel.protocol_version(),
            remote_endpoint: channel.peer_addr().into(),
            peering_endpoint: channel.peering_addr_or_peer_addr().into(),
        })
        .collect();
    peers.sort_by(|a, b| {
        (a.remote_endpoint.bytes, a.remote_endpoint.port)
            .cmp(&(b.remote_endpoint.bytes, b.remote_endpoint.port))
    });
    Box::into_raw(Box::new(PeerInfoListHandle(peers)))
}

#[no_mangle]
pub unsafe extern "C" fn rsn_peer_info_list_destroy(handle: *mut PeerInfoListHandle) {
    drop(Box::from_raw(handle));
}

#[no_mangle]
pub unsafe extern "C" fn rsn_peer_info_list_len(handle: &PeerInfoListHandle) -> usize {
    handle.0.len()
}

#[no_mangle]
pub unsafe extern "C" fn rsn_peer_info_list_get(
    handle: &PeerInfoListHandle,
    index: usize,
    result: &mut PeerInfoDto,
) {
    *result = handle.0[index].clone();
}
