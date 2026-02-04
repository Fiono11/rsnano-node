use crate::command_handler::RpcCommandHandler;
use anyhow::bail;
use rsnano_rpc_messages::{SignArgs, SignResponse};
use rsnano_types::{Block, PrivateKey};

impl RpcCommandHandler {
    pub(crate) fn sign(&self, args: SignArgs) -> anyhow::Result<SignResponse> {
        let mut hash = args.hash.unwrap_or_default();
        let block = args.block.map(Block::from);
        if let Some(b) = &block {
            hash = b.hash();
        }
        // Hash or block are not initialized
        if hash.is_zero() {
            bail!("Block is invalid")
        }
        // Hash is initialized without config permission
        // TODO Check sign hash pemrmission!

        let prv: PrivateKey = if let Some(key) = args.key {
            // Retrieving private key from request
            key.into()
        } else if let Some(wallet_id) = args.wallet
            && let Some(account) = args.account
        {
            // Retrieving private key from wallet
            self.node.wallets.fetch(&wallet_id, &account.into())?.into()
        } else {
            PrivateKey::zero()
        };

        // Signing
        if prv.is_zero() {
            bail!("Private key or local wallet and account required");
        }

        let signature = prv.sign(hash.as_bytes());
        let json_block = if let Some(mut block) = block {
            block.set_signature(signature.clone());
            Some(block.json_representation())
        } else {
            None
        };

        Ok(SignResponse {
            signature,
            block: json_block,
        })
    }
}
