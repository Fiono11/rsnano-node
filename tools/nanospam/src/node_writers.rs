use std::{
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::mpsc,
    task::JoinSet,
    time::timeout,
};
use tracing::info;

/// A write that takes this long is logged, and again each time it takes this much longer
const BLOCKED_AFTER: Duration = Duration::from_millis(500);

/// Writes published blocks to the nodes, each node on its own task with its
/// own queue. A connection that stops taking data delays only its own node: if
/// the publisher waited for every write, one blocked socket would stop the
/// offered load for all nodes, and the nodes would look idle.
pub(crate) struct NodeWriters<W> {
    nodes: Vec<NodeQueue<W>>,
    tasks: JoinSet<()>,
}

struct NodeQueue<W> {
    sender: mpsc::UnboundedSender<NodeWrite<W>>,
    waiting: Arc<AtomicUsize>,
}

enum NodeWrite<W> {
    Message(Arc<Vec<u8>>),
    Reconnected(Vec<W>),
}

impl<W> NodeWriters<W>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    /// `streams` holds the connections of each node, `None` for a node that is down
    pub(crate) fn start(streams: Vec<Option<Vec<W>>>) -> Self {
        let mut tasks = JoinSet::new();
        let mut nodes = Vec::with_capacity(streams.len());
        for (node_index, writers) in streams.into_iter().enumerate() {
            let (sender, receiver) = mpsc::unbounded_channel();
            let waiting = Arc::new(AtomicUsize::new(0));
            tasks.spawn(write_to_node(
                node_index,
                writers,
                receiver,
                waiting.clone(),
            ));
            nodes.push(NodeQueue { sender, waiting });
        }
        Self { nodes, tasks }
    }

    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Queues a message for one node and returns at once
    pub(crate) fn send(&self, node_index: usize, message: Arc<Vec<u8>>) {
        let node = &self.nodes[node_index];
        node.waiting.fetch_add(1, Ordering::Relaxed);
        if node.sender.send(NodeWrite::Message(message)).is_err() {
            node.waiting.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// A restarted node is published to again on its new connections
    pub(crate) fn reconnected(&self, node_index: usize, writers: Vec<W>) {
        let _ = self.nodes[node_index]
            .sender
            .send(NodeWrite::Reconnected(writers));
    }

    /// Lets the queues drain. A connection still blocked after `limit` is
    /// left behind, the end of a run does not wait for it.
    pub(crate) async fn finish(mut self, limit: Duration) {
        self.nodes.clear();
        let drained = timeout(limit, async {
            while self.tasks.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            info!("RAI_PUBLISH_NOT_DRAINED after {} ms", limit.as_millis());
            self.tasks.abort_all();
        }
    }
}

async fn write_to_node<W>(
    node_index: usize,
    mut writers: Option<Vec<W>>,
    mut receiver: mpsc::UnboundedReceiver<NodeWrite<W>>,
    waiting: Arc<AtomicUsize>,
) where
    W: AsyncWrite + Unpin,
{
    let mut next_connection = 0;
    while let Some(write) = receiver.recv().await {
        let message = match write {
            NodeWrite::Reconnected(new_writers) => {
                writers = Some(new_writers);
                continue;
            }
            NodeWrite::Message(message) => message,
        };
        waiting.fetch_sub(1, Ordering::Relaxed);
        let Some(connections) = &mut writers else {
            continue;
        };
        let connection = next_connection % connections.len();
        next_connection = next_connection.wrapping_add(1);
        let written = write_logging_blocks(
            &mut connections[connection],
            &message,
            node_index,
            connection,
            &waiting,
        )
        .await;
        if written.is_err() {
            info!("RAI_PUBLISH_DISCONNECTED pr={node_index}");
            writers = None;
        }
    }
}

async fn write_logging_blocks<W>(
    writer: &mut W,
    message: &[u8],
    node_index: usize,
    connection: usize,
    waiting: &AtomicUsize,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let started = Instant::now();
    let mut write = pin!(writer.write_all(message));
    let mut blocked = false;
    let result = loop {
        match timeout(BLOCKED_AFTER, &mut write).await {
            Ok(result) => break result,
            Err(_) => {
                blocked = true;
                info!(
                    "RAI_PUBLISH_BLOCKED pr={node_index} connection={connection} ms={} waiting={}",
                    started.elapsed().as_millis(),
                    waiting.load(Ordering::Relaxed)
                );
            }
        }
    };
    if blocked {
        info!(
            "RAI_PUBLISH_UNBLOCKED pr={node_index} connection={connection} ms={} waiting={}",
            started.elapsed().as_millis(),
            waiting.load(Ordering::Relaxed)
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, DuplexStream, ReadHalf, WriteHalf, duplex, split};

    /// The fault this type exists for: the peer of node 0 reads nothing
    /// while node 1 keeps receiving every message
    #[tokio::test]
    async fn a_blocked_node_does_not_hold_up_the_others() {
        let (blocked_peer, blocked) = connection();
        let (healthy_peer, healthy) = connection();
        let writers = NodeWriters::start(vec![Some(vec![blocked]), Some(vec![healthy])]);

        for i in 0..MESSAGES {
            let message = message(i);
            writers.send(0, message.clone());
            writers.send(1, message);
        }

        let received = timeout(Duration::from_secs(5), read_messages(healthy_peer))
            .await
            .expect("the healthy node must get its messages");
        assert_eq!(received, expected());
        drop(blocked_peer);
    }

    /// What was queued for a slow node arrives, in order, once it reads again
    #[tokio::test]
    async fn a_slow_node_gets_everything_in_order() {
        let (peer, writer) = connection();
        let writers = NodeWriters::start(vec![Some(vec![writer])]);

        for i in 0..MESSAGES {
            writers.send(0, message(i));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(read_messages(peer).await, expected());
    }

    #[tokio::test]
    async fn a_node_with_a_closed_connection_is_left_out() {
        let (peer, writer) = connection();
        let (healthy_peer, healthy) = connection();
        let writers = NodeWriters::start(vec![Some(vec![writer]), Some(vec![healthy])]);
        drop(peer);

        for i in 0..MESSAGES {
            let message = message(i);
            writers.send(0, message.clone());
            writers.send(1, message);
        }

        assert_eq!(read_messages(healthy_peer).await, expected());
    }

    #[tokio::test]
    async fn a_restarted_node_is_written_to_again() {
        let (peer, writer) = connection();
        let writers: NodeWriters<WriteHalf<DuplexStream>> = NodeWriters::start(vec![None]);

        writers.send(0, message(0));
        writers.reconnected(0, vec![writer]);
        for i in 0..MESSAGES {
            writers.send(0, message(i));
        }

        assert_eq!(read_messages(peer).await, expected());
    }

    #[tokio::test]
    async fn finish_does_not_wait_for_a_blocked_connection() {
        let (peer, writer) = connection();
        let writers = NodeWriters::start(vec![Some(vec![writer])]);
        for i in 0..MESSAGES {
            writers.send(0, message(i));
        }

        let finished = timeout(
            Duration::from_secs(5),
            writers.finish(Duration::from_millis(50)),
        )
        .await;

        assert!(finished.is_ok());
        drop(peer);
    }

    /* Test helpers */

    const MESSAGES: u8 = 64;
    const MESSAGE_LEN: usize = 1024;

    fn message(i: u8) -> Arc<Vec<u8>> {
        Arc::new(vec![i; MESSAGE_LEN])
    }

    fn expected() -> Vec<u8> {
        (0..MESSAGES).flat_map(|i| vec![i; MESSAGE_LEN]).collect()
    }

    /// Smaller than all the messages together, so an unread peer blocks the writer
    fn connection() -> (ReadHalf<DuplexStream>, WriteHalf<DuplexStream>) {
        let (ours, theirs) = duplex(MESSAGE_LEN);
        let (reader, _unused_writer) = split(theirs);
        let (_unused_reader, writer) = split(ours);
        (reader, writer)
    }

    async fn read_messages(mut peer: ReadHalf<DuplexStream>) -> Vec<u8> {
        let mut received = vec![0; expected().len()];
        peer.read_exact(&mut received).await.unwrap();
        received
    }
}
