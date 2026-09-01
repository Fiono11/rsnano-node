use std::sync::mpsc::Sender;

use anyhow::anyhow;
use tokio::select;
use tokio_util::sync::CancellationToken;

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_websocket_client::{
    ConfirmationSubArgs, NanoWebSocketClient, NanoWebSocketClientFactory, SubscribeArgs, TopicSub,
};
use rsnano_websocket_messages::MessageEnvelope;

use crate::setup::websocket_port;

pub(crate) struct ConfirmationReceiver {
    ws_client: NanoWebSocketClient,
}

impl ConfirmationReceiver {
    pub async fn connect(node_index: usize) -> anyhow::Result<Self> {
        let mut ws_client = NanoWebSocketClientFactory::default()
            .connect(&format!("ws://[::1]:{}", websocket_port(node_index)))
            .await?;

        ws_client
            .subscribe(SubscribeArgs {
                topic: TopicSub::Confirmation(ConfirmationSubArgs {
                    include_election_info: true,
                    ..Default::default()
                }),
                ack: true,
                id: None,
            })
            .await?;

        // wait for ack
        ws_client
            .next()
            .await
            .ok_or_else(|| anyhow!("no ws response received"))??;

        ws_client
            .subscribe(SubscribeArgs {
                topic: TopicSub::ElectionTerminated,
                ack: true,
                id: None,
            })
            .await?;
        ws_client
            .next()
            .await
            .ok_or_else(|| anyhow!("no termination subscription response received"))??;

        #[cfg(feature = "rai_protocol")]
        for topic in [TopicSub::EpochCut, TopicSub::EpochComplete] {
            ws_client
                .subscribe(SubscribeArgs {
                    topic,
                    ack: true,
                    id: None,
                })
                .await?;
            ws_client
                .next()
                .await
                .ok_or_else(|| anyhow!("no epoch subscription response received"))??;
        }

        Ok(Self { ws_client })
    }

    pub async fn run(
        &mut self,
        node_index: usize,
        cancel_token: CancellationToken,
        tx_ws_msg: Sender<(usize, MessageEnvelope, Timestamp)>,
        clock: &SteadyClock,
    ) {
        loop {
            let res = select! {
                res = self.ws_client.next() =>  res,
                _ = cancel_token.cancelled() =>{ break;}
            };

            let msg = res.unwrap().unwrap();
            tx_ws_msg.send((node_index, msg, clock.now())).unwrap();
        }
    }
}
