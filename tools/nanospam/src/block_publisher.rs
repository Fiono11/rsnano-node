use tokio::{
    io::{AsyncWriteExt, WriteHalf},
    task::JoinSet,
};

use rsnano_messages::{Message, MessageSerializer, Publish};
use rsnano_nullable_tcp::{TcpStream, TcpStreamFactory};
use rsnano_types::{Block, NetworkType, ProtocolInfo};

use crate::{
    app::{connect_node, drain_reader},
    setup::NodeHosts,
};

/// Publishes setup blocks straight to every node, as the spam does. A block
/// given to one node's RPC reaches the others at the rate of its local
/// broadcaster (32 blocks/s) and through elections and relays, which leave a
/// node missing blocks when a setup block arrives every few milliseconds.
pub(crate) struct BlockPublisher {
    writers: Vec<WriteHalf<TcpStream>>,
    readers: JoinSet<()>,
    serializer: MessageSerializer,
}

impl BlockPublisher {
    pub(crate) async fn connect(
        tcp_stream_factory: &TcpStreamFactory,
        hosts: &NodeHosts,
        node_count: usize,
    ) -> anyhow::Result<Self> {
        let mut writers = Vec::with_capacity(node_count);
        let mut readers = JoinSet::new();
        for node_index in 0..node_count {
            let (node_readers, mut node_writers) =
                connect_node(tcp_stream_factory, hosts, node_index).await?;
            for reader in node_readers {
                readers.spawn(drain_reader(reader));
            }
            // One connection per node keeps its blocks in order
            writers.push(node_writers.swap_remove(0));
        }
        let protocol = ProtocolInfo::default_for(NetworkType::NanoTestNetwork);
        Ok(Self {
            writers,
            readers,
            serializer: MessageSerializer::new(protocol),
        })
    }

    pub(crate) async fn publish(&mut self, block: Block) -> std::io::Result<()> {
        let message = Message::Publish(Publish::new_from_originator(block));
        let buffer = self.serializer.serialize(&message);
        for writer in &mut self.writers {
            writer.write_all(buffer).await?;
        }
        Ok(())
    }

    /// The spam opens its own connections, with the same node ids
    pub(crate) async fn close(mut self) {
        self.readers.shutdown().await;
        for writer in &mut self.writers {
            let _ = writer.shutdown().await;
        }
    }
}
