use std::{
    net::SocketAddrV6,
    sync::{Arc, atomic::Ordering::Relaxed},
};

use rsnano_messages::{Handshake, HandshakeQuery, HandshakeResponse};
use rsnano_types::{BlockHash, NodeId, PrivateKey};

use crate::{HandshakeStats, SynCookies};
use thiserror::Error;

pub enum HandshakeStatus {
    /// Something went wrong. Abort the connection.
    Abort,
    AbortOwnNodeId,
    Handshake,
    /// Contains the node id of the remote node
    Completed(NodeId),
}

/// Responsible for performing a correct handshake when connecting to another node
pub struct HandshakeProcess {
    genesis_hash: BlockHash,
    node_id_key: PrivateKey,
    syn_cookies: Arc<SynCookies>,
    handshake_received: bool,
    stats: Arc<HandshakeStats>,
}

impl HandshakeProcess {
    pub fn new(
        genesis_hash: BlockHash,
        node_id_key: PrivateKey,
        syn_cookies: Arc<SynCookies>,
        stats: Arc<HandshakeStats>,
    ) -> Self {
        Self {
            genesis_hash,
            node_id_key,
            syn_cookies,
            handshake_received: false,
            stats,
        }
    }

    pub fn initiate_handshake(&mut self, peer: SocketAddrV6) -> Result<Handshake, HandshakeError> {
        self.stats.initiate.fetch_add(1, Relaxed);
        let query = self.prepare_query(peer);
        if query.is_none() {
            return Err(HandshakeError::CookieCreationFailed);
        }

        Ok(Handshake {
            query,
            response: None,
            is_v2: true,
        })
    }

    pub fn process_handshake(
        &mut self,
        message: &Handshake,
        peer: SocketAddrV6,
    ) -> Result<(Option<NodeId>, Option<Handshake>), HandshakeError> {
        self.stats.handshakes_received.fetch_add(1, Relaxed);

        if message.query.is_none() && message.response.is_none() {
            // There must be a query or a response or both!
            let e = HandshakeError::EmptyResponse;
            self.stats.errors[e as usize].fetch_add(1, Relaxed);
            self.stats.handshake_error.fetch_add(1, Relaxed);
            return Err(e);
        }

        if message.query.is_some() && self.handshake_received {
            // Second handshake message should be a response only
            let e = HandshakeError::MultipleQueries;
            self.stats.errors[e as usize].fetch_add(1, Relaxed);
            self.stats.handshake_error.fetch_add(1, Relaxed);
            return Err(e);
        }

        self.handshake_received = true;

        // Send response + our own query
        let our_response = message
            .query
            .as_ref()
            .map(|query| self.create_response(query, message.is_v2, peer));

        if let Some(their_response) = &message.response {
            match self.verify_response(their_response, peer) {
                Ok(()) => {
                    self.stats.response_ok.fetch_add(1, Relaxed);
                    return Ok((Some(their_response.node_id), our_response));
                }
                Err(e) => {
                    self.stats.errors[e as usize].fetch_add(1, Relaxed);
                    self.stats.handshake_error.fetch_add(1, Relaxed);
                    return Err(e);
                }
            }
        }

        if our_response.is_some() {
            self.stats.response_sent.fetch_add(1, Relaxed);
        }

        // Handshake is in progress
        Ok((None, our_response))
    }

    fn create_response(&self, query: &HandshakeQuery, v2: bool, peer: SocketAddrV6) -> Handshake {
        let response = self.prepare_response(query, v2);
        let own_query = self.prepare_query(peer);

        Handshake {
            is_v2: own_query.is_some() || response.v2.is_some(),
            query: own_query,
            response: Some(response),
        }
    }

    fn verify_response(
        &self,
        response: &HandshakeResponse,
        peer_addr: SocketAddrV6,
    ) -> Result<(), HandshakeError> {
        // Prevent connection with ourselves
        if response.node_id == self.node_id_key.public_key().into() {
            return Err(HandshakeError::OwnNodeId);
        }

        // Prevent mismatched genesis
        if let Some(v2) = &response.v2
            && v2.genesis != self.genesis_hash
        {
            return Err(HandshakeError::InvalidGenesis);
        }

        let Some(cookie) = self.syn_cookies.cookie(&peer_addr) else {
            return Err(HandshakeError::MissingCookie);
        };

        if response.validate(&cookie).is_err() {
            return Err(HandshakeError::InvalidSignature);
        }

        Ok(())
    }

    fn prepare_response(&self, query: &HandshakeQuery, v2: bool) -> HandshakeResponse {
        if v2 {
            HandshakeResponse::new_v2(&query.cookie, &self.node_id_key, self.genesis_hash)
        } else {
            HandshakeResponse::new_v1(&query.cookie, &self.node_id_key)
        }
    }

    fn prepare_query(&self, peer_addr: SocketAddrV6) -> Option<HandshakeQuery> {
        self.syn_cookies
            .assign(&peer_addr)
            .map(|cookie| HandshakeQuery { cookie })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HandshakeStats, SynCookies};
    use rsnano_types::PrivateKey;
    use std::sync::atomic::Ordering::Relaxed;

    /*
     * initiate_handshake
     */

    #[test]
    fn initiate_creates_query() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        let msg = process.initiate_handshake(bob_addr()).unwrap();
        assert!(msg.query.is_some());
        assert!(msg.response.is_none());
    }

    #[test]
    fn initiate_is_v2() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        let msg = process.initiate_handshake(bob_addr()).unwrap();
        assert!(msg.is_v2);
    }

    #[test]
    fn initiate_fails_for_same_peer_twice() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        process.initiate_handshake(bob_addr()).unwrap();
        let result = process.initiate_handshake(bob_addr());
        assert!(matches!(result, Err(HandshakeError::CookieCreationFailed)));
    }

    #[test]
    fn initiate_increments_stat() {
        let (mut process, stats) = make_process(PrivateKey::from(1));
        process.initiate_handshake(bob_addr()).unwrap();
        assert_eq!(stats.initiate.load(Relaxed), 1);
    }

    /*
     * process_handshake: invalid messages
     */

    #[test]
    fn empty_message_returns_error() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        let result = process.process_handshake(&empty_handshake(), alice_addr());
        assert!(matches!(result, Err(HandshakeError::EmptyResponse)));
    }

    #[test]
    fn second_query_returns_error() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        process
            .process_handshake(&query_handshake(), alice_addr())
            .unwrap();
        let result = process.process_handshake(&query_handshake(), alice_addr());
        assert!(matches!(result, Err(HandshakeError::MultipleQueries)));
    }

    #[test]
    fn error_increments_handshake_error_stat() {
        let (mut process, stats) = make_process(PrivateKey::from(1));
        process
            .process_handshake(&empty_handshake(), alice_addr())
            .unwrap_err();
        assert_eq!(stats.handshake_error.load(Relaxed), 1);
    }

    #[test]
    fn error_increments_per_error_stat() {
        let (mut process, stats) = make_process(PrivateKey::from(1));
        process
            .process_handshake(&empty_handshake(), alice_addr())
            .unwrap_err();
        assert_eq!(
            stats.errors[HandshakeError::EmptyResponse as usize].load(Relaxed),
            1
        );
    }

    /*
     * process_handshake: responding to a query
     */

    #[test]
    fn query_received_creates_response() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        let (_, msg) = process
            .process_handshake(&query_handshake(), alice_addr())
            .unwrap();
        assert!(msg.unwrap().response.is_some());
    }

    #[test]
    fn query_received_creates_own_query() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        let (_, msg) = process
            .process_handshake(&query_handshake(), alice_addr())
            .unwrap();
        assert!(msg.unwrap().query.is_some());
    }

    #[test]
    fn query_received_node_id_is_none() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        let (node_id, _) = process
            .process_handshake(&query_handshake(), alice_addr())
            .unwrap();
        assert!(node_id.is_none());
    }

    #[test]
    fn query_increments_handshakes_received() {
        let (mut process, stats) = make_process(PrivateKey::from(1));
        process
            .process_handshake(&query_handshake(), alice_addr())
            .unwrap();
        assert_eq!(stats.handshakes_received.load(Relaxed), 1);
    }

    #[test]
    fn query_increments_response_sent() {
        let (mut process, stats) = make_process(PrivateKey::from(1));
        process
            .process_handshake(&query_handshake(), alice_addr())
            .unwrap();
        assert_eq!(stats.response_sent.load(Relaxed), 1);
    }

    /*
     * process_handshake: completing the handshake
     */

    #[test]
    fn successful_response_returns_remote_node_id() {
        let bob_key = PrivateKey::from(2);
        let (mut alice, _) = make_process(PrivateKey::from(1));
        let (mut bob, _) = make_process(bob_key.clone());

        let msg1 = alice.initiate_handshake(bob_addr()).unwrap();
        let (_, msg2) = bob.process_handshake(&msg1, alice_addr()).unwrap();
        let (node_id, _) = alice.process_handshake(&msg2.unwrap(), bob_addr()).unwrap();

        assert_eq!(node_id, Some(bob_key.public_key().into()));
    }

    #[test]
    fn successful_response_increments_response_ok() {
        let (mut alice, alice_stats) = make_process(PrivateKey::from(1));
        let (mut bob, _) = make_process(PrivateKey::from(2));

        let msg1 = alice.initiate_handshake(bob_addr()).unwrap();
        let (_, msg2) = bob.process_handshake(&msg1, alice_addr()).unwrap();
        alice.process_handshake(&msg2.unwrap(), bob_addr()).unwrap();

        assert_eq!(alice_stats.response_ok.load(Relaxed), 1);
    }

    /*
     * process_handshake: response validation errors
     */

    #[test]
    fn own_node_id_returns_error() {
        let my_key = PrivateKey::from(1);
        let (mut process, _) = make_process(my_key.clone());
        let response = HandshakeResponse::new_v2(&[0; 32], &my_key, BlockHash::default());
        let result = process.process_handshake(&response_handshake(response), bob_addr());
        assert!(matches!(result, Err(HandshakeError::OwnNodeId)));
    }

    #[test]
    fn invalid_genesis_returns_error() {
        let genesis = BlockHash::from(1);
        let stats = Arc::new(HandshakeStats::default());
        let mut process = HandshakeProcess::new(
            genesis,
            PrivateKey::from(1),
            Arc::new(SynCookies::new(10)),
            stats,
        );
        let response =
            HandshakeResponse::new_v2(&[0; 32], &PrivateKey::from(2), BlockHash::from(99));
        let result = process.process_handshake(&response_handshake(response), bob_addr());
        assert!(matches!(result, Err(HandshakeError::InvalidGenesis)));
    }

    #[test]
    fn missing_cookie_returns_error() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        let response =
            HandshakeResponse::new_v2(&[0; 32], &PrivateKey::from(2), BlockHash::default());
        let result = process.process_handshake(&response_handshake(response), bob_addr());
        assert!(matches!(result, Err(HandshakeError::MissingCookie)));
    }

    #[test]
    fn invalid_signature_returns_error() {
        let (mut process, _) = make_process(PrivateKey::from(1));
        process.initiate_handshake(bob_addr()).unwrap();
        // Sign with a different cookie than the one stored in syn_cookies
        let wrong_cookie = [0u8; 32];
        let response =
            HandshakeResponse::new_v2(&wrong_cookie, &PrivateKey::from(2), BlockHash::default());
        let result = process.process_handshake(&response_handshake(response), bob_addr());
        assert!(matches!(result, Err(HandshakeError::InvalidSignature)));
    }

    /*
     * Test helpers
     */

    fn make_process(key: PrivateKey) -> (HandshakeProcess, Arc<HandshakeStats>) {
        let stats = Arc::new(HandshakeStats::default());
        let process = HandshakeProcess::new(
            BlockHash::default(),
            key,
            Arc::new(SynCookies::new(10)),
            stats.clone(),
        );
        (process, stats)
    }

    fn alice_addr() -> SocketAddrV6 {
        "[::1]:1000".parse().unwrap()
    }

    fn bob_addr() -> SocketAddrV6 {
        "[::2]:2000".parse().unwrap()
    }

    fn empty_handshake() -> Handshake {
        Handshake {
            query: None,
            response: None,
            is_v2: true,
        }
    }

    fn query_handshake() -> Handshake {
        Handshake {
            query: Some(HandshakeQuery { cookie: [42; 32] }),
            response: None,
            is_v2: true,
        }
    }

    fn response_handshake(response: HandshakeResponse) -> Handshake {
        Handshake {
            query: None,
            response: Some(response),
            is_v2: true,
        }
    }
}

#[derive(Debug, Clone, Copy, EnumCount, EnumIter, Error)]
pub enum HandshakeError {
    #[error("cookie creation failed")]
    CookieCreationFailed,
    #[error("the node tried to connect to itself")]
    OwnNodeId,
    #[error("invalid genesis hash")]
    InvalidGenesis,
    #[error("missing cookie")]
    MissingCookie,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("empty response")]
    EmptyResponse,
    #[error("multiple queries")]
    MultipleQueries,
}
