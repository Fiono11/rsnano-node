use std::sync::{Arc, mpsc};

use rsnano_network::ChannelId;
use rsnano_types::Block;
use rsnano_wallet::Wallets;

use crate::block_processing::{BlockContext, BlockProcessorQueue};
use rsnano_ledger::BlockSource;

pub(crate) struct WalletBlockProcessor {
    inbound: mpsc::Receiver<Block>,
    wallets: Arc<Wallets>,
    block_processor: Arc<BlockProcessorQueue>,
}

impl WalletBlockProcessor {
    pub(crate) fn new(
        inbound: mpsc::Receiver<Block>,
        wallets: Arc<Wallets>,
        block_processor: Arc<BlockProcessorQueue>,
    ) -> Self {
        Self {
            inbound,
            wallets,
            block_processor,
        }
    }

    pub(crate) fn run(self) {
        while let Ok(block) = self.inbound.recv() {
            let wallets = self.wallets.clone();
            let hash = block.hash();

            let context = BlockContext::new_with_callback(
                block,
                BlockSource::Local,
                ChannelId::LOOPBACK,
                Box::new(move |hash, _, saved_block| {
                    wallets.block_processed(hash, saved_block.cloned());
                }),
            );

            let inserted = self.block_processor.push(context);
            if !inserted {
                self.wallets.block_processed(&hash, None);
                // TODO stats for drops?
            }
        }
    }
}
