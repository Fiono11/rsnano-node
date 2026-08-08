use std::{collections::HashMap, sync::Mutex, time::Duration};

use anyhow::anyhow;
use tokio::{select, sync::mpsc::Sender, time::sleep};
use tokio_util::sync::CancellationToken;
use tracing::info;

use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::SendArgs;
use rsnano_types::{
    Account, Amount, Block, BlockHash, JsonBlock, Link, PrivateKey, PublicKey, RawKey,
    StateBlockArgs, WalletId,
};

use crate::domain::{Forks, spam_logic::SpamLogic};
use crate::setup::pr_key;
use crate::wallets_factory::wait_until_confirmed_on_all;

const PRIO_ACCOUNTS: usize = 20;
const INITIAL_ACCOUNT_BALANCE: Amount = Amount::millinano(1500); // bucket 16

/// Periodically publishes a high priority block and tracks confirmation time
pub(crate) struct HighPrioCheck<'a> {
    rpc_client: &'a NanoRpcClient,
    logic: &'a Mutex<SpamLogic>,
    /// prio account => key + frontier hash + height
    accounts: HashMap<Account, (PrivateKey, BlockHash, u64)>,
}

impl<'a> HighPrioCheck<'a> {
    pub(crate) fn new(rpc_client: &'a NanoRpcClient, logic: &'a Mutex<SpamLogic>) -> Self {
        Self {
            rpc_client,
            logic,
            accounts: prio_account_keys()
                .map(|k| (k.account(), (k, BlockHash::ZERO, 0)))
                .collect(),
        }
    }

    pub(crate) async fn create_prio_accounts(
        &mut self,
        wallet_id: WalletId,
        rpc_clients: &[NanoRpcClient],
    ) -> anyhow::Result<()> {
        info!("Creating high priority accounts...");
        let account = self
            .rpc_client
            .account_list(wallet_id)
            .await?
            .accounts
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("Wallet is empty"))?;

        let keys: Vec<_> = self
            .accounts
            .values()
            .map(|(key, _, _)| key.clone())
            .collect();

        for key in keys {
            let send_block = self
                .rpc_client
                .send(SendArgs {
                    wallet: wallet_id,
                    source: account,
                    destination: key.account(),
                    amount: INITIAL_ACCOUNT_BALANCE,
                    work: Some(0.into()),
                    id: None,
                })
                .await?;
            wait_until_confirmed_on_all(rpc_clients, send_block.block).await?;

            let receive_block: Block = StateBlockArgs {
                key: &key,
                previous: BlockHash::ZERO,
                // These funds come from the genesis account, which represents
                // PR0. Retaining that delegation keeps the balanced committee
                // weights unchanged while creating the priority accounts.
                representative: pr_key(0).public_key(),
                balance: INITIAL_ACCOUNT_BALANCE,
                link: send_block.block.into(),
                work: 0.into(),
            }
            .into();

            let receive_hash = receive_block.hash();
            self.rpc_client
                .process(JsonBlock::from(receive_block))
                .await?;
            wait_until_confirmed_on_all(rpc_clients, receive_hash).await?;

            self.accounts
                .insert(key.account(), (key.clone(), receive_hash, 1));
        }

        Ok(())
    }

    pub(crate) async fn sync_accounts(&mut self) -> anyhow::Result<()> {
        let keys: Vec<_> = self
            .accounts
            .values()
            .map(|(key, _, _)| key.clone())
            .collect();

        for key in keys {
            let result = self.rpc_client.account_info(key.account()).await?;
            self.accounts.insert(
                key.account(),
                (key, result.frontier, result.block_count.inner()),
            );
        }

        Ok(())
    }

    pub(crate) async fn run(&mut self, cancel_token: CancellationToken, tx_forks: Sender<Forks>) {
        loop {
            select! {
                _ = cancel_token.cancelled() => { break;},
                _ =sleep(Duration::from_secs(10)) => {}
            }

            let (key, frontier, height) = self
                .accounts
                .values()
                .min_by(|(_, _, x), (_, _, y)| x.cmp(y))
                .unwrap();

            let block: Block = StateBlockArgs {
                key,
                previous: *frontier,
                representative: PublicKey::from(*height),
                balance: INITIAL_ACCOUNT_BALANCE,
                link: Link::ZERO,
                work: 0.into(),
            }
            .into();

            let account = key.account();
            let hash = block.hash();

            {
                let mut logic = self.logic.lock().unwrap();
                logic.delayed.insert(block.clone());
                logic.high_prio_tracker.enqueued(hash);
            }

            tx_forks.send(Forks::new(block)).await.unwrap();
            let (_, frontier, height) = self.accounts.get_mut(&account).unwrap();
            *frontier = hash;
            *height += 1;
        }
    }
}

fn prio_account_keys() -> impl Iterator<Item = PrivateKey> {
    (0..PRIO_ACCOUNTS).map(account_key)
}

fn account_key(index: usize) -> PrivateKey {
    RawKey::from((1000 + index) as u64).into()
}
