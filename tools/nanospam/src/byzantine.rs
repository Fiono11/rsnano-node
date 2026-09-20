use std::{
    collections::VecDeque,
    net::{Ipv6Addr, SocketAddrV6},
    sync::{Arc, Mutex},
    time::Duration,
};

use rand::{RngExt, seq::IndexedRandom};
use tokio::{io::AsyncWriteExt, select, time::sleep};
use tokio_util::sync::CancellationToken;
use tracing::info;

use rsnano_messages::{ConfirmAck, Message, MessageSerializer};
use rsnano_nullable_tcp::TcpStreamFactory;
use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey, ProtocolInfo, RawKey, Vote, VoteKind};

use crate::{
    handshake::perform_handshake,
    setup::{peering_port, pr_key},
};

/// How many recently published hashes a Byzantine representative votes about
const RECENT_HASHES: usize = 512;
/// Votes per burst and the pause between bursts
const VOTES_PER_BURST: usize = 8;
const BURST_INTERVAL: Duration = Duration::from_millis(50);

/// RAI: the hashes nanospam published lately. A Byzantine representative votes
/// about real blocks most of the time - a vote for a hash nobody holds is
/// discarded on arrival and would cost the honest replicas nothing.
#[derive(Clone, Default)]
pub(crate) struct RecentBlocks(Arc<Mutex<VecDeque<BlockHash>>>);

impl RecentBlocks {
    pub fn push(&self, hash: BlockHash) {
        let mut queue = self.0.lock().unwrap();
        if queue.len() == RECENT_HASHES {
            queue.pop_front();
        }
        queue.push_back(hash);
    }

    fn sample(&self, count: usize) -> Vec<BlockHash> {
        let queue = self.0.lock().unwrap();
        let hashes: Vec<BlockHash> = queue.iter().copied().collect();
        drop(queue);
        let mut rng = rand::rng();
        (0..count)
            .filter_map(|_| hashes.choose(&mut rng).copied())
            .collect()
    }
}

/// RAI: the f Byzantine representatives. They run no node; nanospam holds
/// their keys and votes with them at random, so they hold their share of the
/// weight and follow no rule of the protocol: any kind, any epoch, any block,
/// contradicting themselves and each other as often as they please.
pub(crate) async fn run_byzantine(
    keys: Vec<PrivateKey>,
    honest_prs: usize,
    protocol: ProtocolInfo,
    genesis_hash: BlockHash,
    recent: RecentBlocks,
    cancel_token: CancellationToken,
    tcp_stream_factory: &TcpStreamFactory,
) {
    if keys.is_empty() {
        return;
    }
    let mut writers = Vec::new();
    for node_index in 0..honest_prs {
        let peer_addr = SocketAddrV6::new(Ipv6Addr::LOCALHOST, peering_port(node_index), 0, 0);
        let Ok(mut stream) = tcp_stream_factory.connect(peer_addr).await else {
            info!("Byzantine: could not connect to PR{node_index}");
            continue;
        };
        // A node id of its own, so the honest nodes keep these channels apart
        let node_id_key: PrivateKey = RawKey::from(900 + node_index as u64).into();
        if perform_handshake(protocol, genesis_hash, node_id_key, &mut stream)
            .await
            .is_err()
        {
            info!("Byzantine: handshake with PR{node_index} failed");
            continue;
        }
        let (_, write) = tokio::io::split(stream);
        writers.push(write);
    }
    if writers.is_empty() {
        info!("Byzantine: no node to vote at");
        return;
    }
    info!(
        "{} Byzantine representative(s) voting at random into {} node(s)",
        keys.len(),
        writers.len()
    );

    let mut serializer = MessageSerializer::new(protocol);
    loop {
        select! {
            _ = cancel_token.cancelled() => break,
            _ = sleep(BURST_INTERVAL) => {}
        }
        for _ in 0..VOTES_PER_BURST {
            // The random number generator is not Send: everything random
            // happens here, before the first await
            let buffer = {
                let mut rng = rand::rng();
                let Some(key) = keys.choose(&mut rng).cloned() else {
                    break;
                };
                // Mostly real blocks, sometimes a hash nobody has ever seen
                let mut hashes = recent.sample(rng.random_range(1..=3));
                if hashes.is_empty() || rng.random_bool(0.1) {
                    hashes.push(BlockHash::from_bytes(rng.random()));
                }
                let kind = [
                    VoteKind::First,
                    VoteKind::Notar,
                    VoteKind::Timeout,
                    VoteKind::Abstain,
                    VoteKind::Final,
                ][rng.random_range(0..5)];
                // Any epoch it feels like, including ones nobody has reached
                let epoch = ConsensusEpoch::new(rng.random_range(0..4));
                let vote = Vote::new_in_epoch(&key, kind, epoch, hashes);
                let message = Message::ConfirmAck(ConfirmAck::new_with_own_vote(vote));
                serializer.serialize(&message).to_vec()
            };
            for writer in writers.iter_mut() {
                // A node that went away is no reason to stop
                let _ = writer.write_all(&buffer).await;
            }
        }
    }
}

/// The keys of the Byzantine representatives, which nanospam votes with
pub(crate) fn byzantine_keys(prs: usize, byzantine: usize) -> Vec<PrivateKey> {
    (prs - byzantine..prs).map(pr_key).collect()
}
