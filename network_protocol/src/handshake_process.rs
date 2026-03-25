use std::{net::SocketAddrV6, sync::Arc};

use tracing::{debug, warn};

use rsnano_messages::{Handshake, HandshakeQuery, HandshakeResponse};
use rsnano_types::{BlockHash, NodeId, PrivateKey};

use crate::SynCookies;
use thiserror::Error;

pub enum HandshakeStatus {
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
}

impl HandshakeProcess {
    pub fn new(
        genesis_hash: BlockHash,
        node_id_key: PrivateKey,
        syn_cookies: Arc<SynCookies>,
    ) -> Self {
        Self {
            genesis_hash,
            node_id_key,
            syn_cookies,
            handshake_received: false,
        }
    }

    pub fn initiate_handshake(&mut self, peer: SocketAddrV6) -> Result<Handshake, HandshakeError> {
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
        if message.query.is_none() && message.response.is_none() {
            // There must be a query or a response or both!
            return Err(HandshakeError::EmptyResponse);
        }

        if message.query.is_some() && self.handshake_received {
            // Second handshake message should be a response only
            return Err(HandshakeError::MultipleQueries);
        }

        self.handshake_received = true;

        let log_type = match (message.query.is_some(), message.response.is_some()) {
            (true, true) => "query + response",
            (true, false) => "query",
            (false, true) => "response",
            (false, false) => "none",
        };
        debug!("Handshake message received: {} ({})", log_type, peer);

        // Send response + our own query
        let our_response = message
            .query
            .as_ref()
            .map(|query| self.create_response(query, message.is_v2, peer));

        if let Some(their_response) = &message.response {
            match self.verify_response(their_response, peer) {
                Ok(()) => {
                    return Ok((Some(their_response.node_id), our_response));
                }
                Err(HandshakeError::OwnNodeId) => {
                    warn!(
                        "This node tried to connect to itself. Closing channel ({})",
                        peer
                    );
                    return Err(HandshakeError::OwnNodeId);
                }
                Err(e) => {
                    return Err(e);
                }
            }
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
