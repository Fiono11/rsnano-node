#[macro_use]
extern crate strum_macros;

pub mod attempt_container;
pub mod bandwidth_limiter;
mod channel;
mod channel_stats;
mod dead_channel_cleanup;
mod network;
mod network_stats;
mod peer_connector;
pub mod peer_exclusion;
mod tcp_channel_adapter;
mod tcp_listener;
mod tcp_network_adapter;
pub mod token_bucket;
pub mod utils;
pub mod write_queue;

pub use channel::*;
pub use dead_channel_cleanup::*;
pub use network::*;
pub use peer_connector::*;
pub use tcp_channel_adapter::*;
pub use tcp_listener::*;
pub use tcp_network_adapter::*;

use std::{
    fmt::{Debug, Display},
    net::{Ipv6Addr, SocketAddrV6},
    sync::Arc,
};

use async_trait::async_trait;
use num_derive::FromPrimitive;

use rsnano_utils::stats::DetailType;

#[macro_use]
extern crate anyhow;

#[derive(PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Hash)]
pub struct ChannelId(usize);

impl ChannelId {
    pub const LOOPBACK: Self = Self(0);
    pub const MIN: Self = Self(usize::MIN);
    pub const MAX: Self = Self(usize::MAX);

    pub fn as_usize(&self) -> usize {
        self.0
    }
}

impl Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl Debug for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl From<usize> for ChannelId {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

#[derive(PartialEq, Eq, Clone, Copy, FromPrimitive, Debug)]
pub enum ChannelDirection {
    /// Socket was created by accepting an incoming connection
    Inbound,
    /// Socket was created by initiating an outgoing connection
    Outbound,
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, EnumCount, EnumIter, IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum TrafficType {
    Generic,
    /// Ascending bootstrap (asc_pull_ack, asc_pull_req) traffic
    BootstrapServer,
    BootstrapRequests,
    BlockBroadcast,
    BlockBroadcastInitial,
    BlockBroadcastRpc,
    ConfirmationRequests,
    Keepalive,
    Vote,
    VoteRebroadcast,
    VoteReply,
    RepCrawler,
    Telemetry,
    #[cfg(feature = "ledger_snapshots")]
    LedgerSnapshots,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, FromPrimitive)]
pub enum ChannelMode {
    /// Handshake is in progress, no other messages are allowed
    Handshake,
    /// Handshake was successfull => connection is established
    Established,
}

#[async_trait]
pub trait AsyncBufferReader {
    async fn read(&self, buffer: &mut [u8], count: usize) -> anyhow::Result<()>;
}

pub trait DataReceiverFactory {
    fn create_receiver_for(&self, channel: Arc<Channel>) -> Box<dyn DataReceiver + Send>;
}

pub enum ReceiveResult {
    Continue,
    Abort,
    Pause,
}

pub trait DataReceiver {
    fn receive(&mut self, data: &[u8]) -> ReceiveResult;
    /// after receive returns Pause this has to be called until it returns true
    fn try_unpause(&self) -> ReceiveResult;
}

#[derive(Default)]
pub struct NullDataReceiverFactory;

impl NullDataReceiverFactory {
    pub fn new() -> Self {
        Self
    }
}

impl DataReceiverFactory for NullDataReceiverFactory {
    fn create_receiver_for(&self, _channel: Arc<Channel>) -> Box<dyn DataReceiver + Send> {
        Box::new(NullDataReceiver::new())
    }
}

#[derive(Default)]
pub struct NullDataReceiver;

impl NullDataReceiver {
    pub fn new() -> Self {
        Self
    }
}

impl DataReceiver for NullDataReceiver {
    fn receive(&mut self, _: &[u8]) -> ReceiveResult {
        ReceiveResult::Continue
    }

    fn try_unpause(&self) -> ReceiveResult {
        ReceiveResult::Continue
    }
}

impl From<ChannelDirection> for rsnano_utils::stats::Direction {
    fn from(value: ChannelDirection) -> Self {
        match value {
            ChannelDirection::Inbound => rsnano_utils::stats::Direction::In,
            ChannelDirection::Outbound => rsnano_utils::stats::Direction::Out,
        }
    }
}

impl From<TrafficType> for DetailType {
    fn from(value: TrafficType) -> Self {
        match value {
            TrafficType::Generic => DetailType::Generic,
            TrafficType::BootstrapServer => DetailType::BootstrapServer,
            TrafficType::BootstrapRequests => DetailType::BootstrapRequests,
            TrafficType::BlockBroadcast => DetailType::BlockBroadcast,
            TrafficType::BlockBroadcastInitial => DetailType::BlockBroadcastInitial,
            TrafficType::BlockBroadcastRpc => DetailType::BlockBroadcastRpc,
            TrafficType::ConfirmationRequests => DetailType::ConfirmationRequests,
            TrafficType::Keepalive => DetailType::Keepalive,
            TrafficType::Vote => DetailType::Vote,
            TrafficType::VoteRebroadcast => DetailType::VoteRebroadcast,
            TrafficType::RepCrawler => DetailType::RepCrawler,
            TrafficType::VoteReply => DetailType::VoteReply,
            TrafficType::Telemetry => DetailType::Telemetry,
            #[cfg(feature = "ledger_snapshots")]
            TrafficType::LedgerSnapshots => DetailType::Preproposal,
        }
    }
}

pub const NULL_ENDPOINT: SocketAddrV6 = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0);

pub const TEST_ENDPOINT_1: SocketAddrV6 =
    SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 0x10, 0, 0, 1), 1111, 0, 0);

pub const TEST_ENDPOINT_2: SocketAddrV6 =
    SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 0x10, 0, 0, 2), 2222, 0, 0);

pub const TEST_ENDPOINT_3: SocketAddrV6 =
    SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 0x10, 0, 0, 3), 3333, 0, 0);
