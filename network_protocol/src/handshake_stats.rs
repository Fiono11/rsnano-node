use crate::HandshakeError;
use rsnano_utils::stats::{Direction, StatsCollection, StatsSource};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use strum::{EnumCount, IntoEnumIterator};

#[derive(Default)]
pub struct HandshakeStats {
    pub initiate: AtomicUsize,
    pub handshakes_received: AtomicUsize,
    pub response_sent: AtomicUsize,
    pub handshake_error: AtomicUsize,
    pub response_ok: AtomicUsize,
    pub errors: [AtomicUsize; HandshakeError::COUNT],
}

impl StatsSource for HandshakeStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert_dir(
            "tcp_server",
            "handshake_initiate",
            Direction::Out,
            self.initiate.load(Relaxed),
        );
        result.insert(
            "tcp_server",
            "handshake_error",
            self.handshake_error.load(Relaxed),
        );
        result.insert_dir(
            "tcp_server",
            "handshake",
            Direction::In,
            self.handshakes_received.load(Relaxed),
        );

        result.insert("handshake", "ok", self.response_ok.load(Relaxed));

        for e in HandshakeError::iter() {
            let detail = match e {
                HandshakeError::OwnNodeId => "invalid_node_id",
                HandshakeError::InvalidGenesis => "invalid_genesis",
                HandshakeError::MissingCookie => "missing_cookie",
                HandshakeError::InvalidSignature => "invalid_signature",
                HandshakeError::EmptyResponse => "empty_response",
                HandshakeError::MultipleQueries => "multiple_queries",
                HandshakeError::CookieCreationFailed => "cookie_creation_failed",
            };
            result.insert("handshake", detail, self.handshake_error.load(Relaxed));
        }
    }
}
