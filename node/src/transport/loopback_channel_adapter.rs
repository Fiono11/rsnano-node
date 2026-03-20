use std::{sync::Arc, time::Duration};
use tokio::{select, time::sleep};

use rsnano_messages::MessageDeserializer;
use rsnano_network::Channel;
use rsnano_network_protocol::InboundMessageQueue;
use rsnano_types::{NetworkType, NodeId, ProtocolInfo};

pub(crate) async fn run_loopback_channel_adapter(
    loopback: Arc<Channel>,
    node_id: NodeId,
    network: NetworkType,
    inbound: Arc<InboundMessageQueue>,
) {
    loopback.set_node_id(node_id);
    let protocol = ProtocolInfo::default_for(network);
    let mut deserializer = MessageDeserializer::new(protocol);
    loop {
        let res = select! {
            _ = loopback.cancelled() =>{
                return;
            },
          res = loopback.pop() => res
        };

        if let Some(entry) = res {
            deserializer.push(&entry.buffer);
            if let Some(Ok(m)) = deserializer.try_deserialize() {
                while !inbound.put(m.message.clone(), loopback.clone()) && loopback.is_cancelled() {
                    sleep(Duration::from_millis(1)).await;
                }
            }
        }
    }
}
