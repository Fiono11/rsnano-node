use std::sync::mpsc::Sender;

use anyhow::anyhow;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing;

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_websocket_client::{
    NanoWebSocketClient, NanoWebSocketClientFactory, SubscribeArgs, TopicSub,
};
use rsnano_websocket_messages::{MessageEnvelope, Topic};

use crate::setup::websocket_port;

pub(crate) struct ConfirmationReceiver {
    ws_client: NanoWebSocketClient,
}

impl ConfirmationReceiver {
    pub async fn connect() -> anyhow::Result<Self> {
        let ws_url = format!("ws://[::1]:{}", websocket_port(0));
        tracing::info!("Connecting to websocket at: {ws_url}");
        let mut ws_client = NanoWebSocketClientFactory::default()
            .connect(&ws_url)
            .await?;
        tracing::info!("Websocket connection established");

        tracing::info!("Subscribing to confirmation topic");
        ws_client
            .subscribe(SubscribeArgs {
                topic: TopicSub::Confirmation(Default::default()),
                ack: true,
                id: None,
            })
            .await?;
        tracing::info!("Subscription request sent, waiting for ack");

        // wait for ack
        let ack_result = ws_client
            .next()
            .await
            .ok_or_else(|| anyhow!("no ws response received"))??;
        tracing::info!("Received subscription ack: {:?}", ack_result);

        Ok(Self { ws_client })
    }

    pub async fn run(
        &mut self,
        cancel_token: CancellationToken,
        tx_ws_msg: Sender<(MessageEnvelope, Timestamp)>,
        clock: &SteadyClock,
    ) {
        loop {
            let res = select! {
                res = self.ws_client.next() =>  res,
                _ = cancel_token.cancelled() =>{ break;}
            };

            match res {
                Some(Ok(msg)) => {
                    tracing::debug!("Received websocket message, topic: {:?}", msg.topic);
                    if msg.topic == Some(Topic::Confirmation) {
                        tracing::info!("Received confirmation message from websocket");
                    }
                    if let Err(e) = tx_ws_msg.send((msg, clock.now())) {
                        tracing::error!("Failed to send websocket message to channel: {e}");
                    }
                }
                Some(Err(e)) => {
                    tracing::error!("Error receiving websocket message: {e}");
                }
                None => {
                    tracing::warn!("Websocket client returned None (stream ended)");
                    break;
                }
            }
        }
    }
}
