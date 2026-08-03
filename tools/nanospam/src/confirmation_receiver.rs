use std::sync::mpsc::Sender;

use anyhow::anyhow;
use tokio::select;
use tokio_util::sync::CancellationToken;

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_websocket_client::{
    NanoWebSocketClient, NanoWebSocketClientFactory, SubscribeArgs, TopicSub,
};
use rsnano_websocket_messages::MessageEnvelope;

use crate::setup::websocket_port;

pub(crate) struct ConfirmationReceiver {
    ws_client: NanoWebSocketClient,
}

impl ConfirmationReceiver {
    pub async fn connect() -> anyhow::Result<Self> {
        let mut ws_client = NanoWebSocketClientFactory::default()
            .connect(&format!("ws://[::1]:{}", websocket_port(0)))
            .await?;

        ws_client
            .subscribe(SubscribeArgs {
                topic: TopicSub::Confirmation(Default::default()),
                ack: true,
                id: None,
            })
            .await?;

        // wait for ack
        ws_client
            .next()
            .await
            .ok_or_else(|| anyhow!("no ws response received"))??;

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

            let Some(Ok(msg)) = res else {
                break;
            };
            if tx_ws_msg.send((msg, clock.now())).is_err() {
                break;
            }
        }
    }
}
