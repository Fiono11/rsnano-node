use std::marker::PhantomData;

use crate::{
    iterator::{BinaryDbIterator, DbIterator},
    Environment, EnvironmentWrapper, LmdbEnv, LmdbIteratorImpl, LmdbWriteTransaction,
};
use lmdb::{Cursor, Database, DatabaseFlags, WriteFlags};
use rsnano_core::{BlockHash, NoValue, RawKey, WalletId};
pub type WalletsIterator = BinaryDbIterator<[u8; 64], NoValue, LmdbIteratorImpl>;

pub struct LmdbWallets<T: Environment = EnvironmentWrapper> {
    pub handle: Option<Database>,
    pub send_action_ids_handle: Option<Database>,
    phantom: PhantomData<T>,
}

impl<T: Environment + 'static> LmdbWallets<T> {
    pub fn new() -> Self {
        Self {
            handle: None,
            send_action_ids_handle: None,
            phantom: PhantomData::default(),
        }
    }

    pub fn initialize(
        &mut self,
        txn: &mut LmdbWriteTransaction,
        env: &LmdbEnv,
    ) -> anyhow::Result<()> {
        self.handle = Some(unsafe { txn.rw_txn_mut().create_db(None, DatabaseFlags::empty())? });
        self.split_if_needed(txn, env)?;
        self.send_action_ids_handle = Some(unsafe {
            txn.rw_txn_mut()
                .create_db(Some("send_action_ids"), DatabaseFlags::empty())?
        });
        Ok(())
    }

    pub fn get_store_it(&self, txn: &dyn crate::Transaction, hash: &str) -> WalletsIterator {
        let hash_bytes: [u8; 64] = hash.as_bytes().try_into().unwrap();
        WalletsIterator::new(LmdbIteratorImpl::new::<T>(
            txn,
            self.handle.unwrap(),
            Some(&hash_bytes),
            true,
        ))
    }

    pub fn move_table(
        &self,
        name: &str,
        txn_source: &mut LmdbWriteTransaction,
        txn_destination: &mut LmdbWriteTransaction,
    ) -> anyhow::Result<()> {
        let rw_txn_source = txn_source.rw_txn_mut();
        let rw_txn_dest = txn_destination.rw_txn_mut();
        let handle_source = unsafe { rw_txn_source.create_db(Some(name), DatabaseFlags::empty()) }?;
        let handle_destination =
            unsafe { rw_txn_dest.create_db(Some(name), DatabaseFlags::empty()) }?;

        {
            let mut cursor = lmdb::Transaction::open_ro_cursor(rw_txn_source, handle_source)?;
            for x in cursor.iter_start() {
                let (k, v) = x?;
                rw_txn_dest.put(handle_destination, &k, &v, WriteFlags::empty())?;
            }
        }

        unsafe { rw_txn_source.drop_db(handle_source) }?;
        Ok(())
    }

    pub fn split_if_needed(
        &self,
        txn_destination: &mut LmdbWriteTransaction,
        env: &LmdbEnv,
    ) -> anyhow::Result<()> {
        let beginning = RawKey::from(0).encode_hex();
        let end = RawKey::from_bytes([1; 32]).encode_hex();

        // First do a read pass to check if there are any wallets that need extracting (to save holding a write lock and potentially being blocked)
        let wallets_need_splitting = {
            let transaction_source = env.tx_begin_read()?;
            let i = self.get_store_it(&transaction_source, &beginning);
            let n = self.get_store_it(&transaction_source, &end);
            i.current().map(|(k, _)| *k) != n.current().map(|(k, _)| *k)
        };

        if wallets_need_splitting {
            let mut txn_source = env.tx_begin_write().unwrap();
            let mut i = self.get_store_it(&txn_source, &beginning);
            while let Some((k, _)) = i.current() {
                let text = std::str::from_utf8(k)?;
                let _id = WalletId::decode_hex(text)?;
                self.move_table(text, &mut txn_source, txn_destination)?;
                i.next();
            }
        }
        Ok(())
    }

    pub fn get_wallet_ids(&self, txn: &dyn crate::Transaction) -> Vec<WalletId> {
        let mut wallet_ids = Vec::new();
        let beginning = RawKey::from(0).encode_hex();
        let mut i = self.get_store_it(txn, &beginning);
        while let Some((k, _)) = i.current() {
            let text = std::str::from_utf8(k).unwrap();
            wallet_ids.push(WalletId::decode_hex(&text).unwrap());
            i.next();
        }
        wallet_ids
    }

    pub fn get_block_hash(
        &self,
        txn: &dyn crate::Transaction,
        id: &str,
    ) -> anyhow::Result<Option<BlockHash>> {
        match txn.get(self.send_action_ids_handle.unwrap(), &id.as_bytes()) {
            Ok(bytes) => Ok(Some(
                BlockHash::from_slice(bytes).ok_or_else(|| anyhow!("invalid block hash"))?,
            )),
            Err(lmdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_block_hash(
        &self,
        txn: &mut LmdbWriteTransaction,
        id: &str,
        hash: &BlockHash,
    ) -> anyhow::Result<()> {
        txn.rw_txn_mut().put(
            self.send_action_ids_handle.unwrap(),
            &id.as_bytes(),
            hash.as_bytes(),
            WriteFlags::empty(),
        )?;
        Ok(())
    }

    pub fn clear_send_ids(&self, txn: &mut LmdbWriteTransaction) {
        txn.rw_txn_mut()
            .clear_db(self.send_action_ids_handle.unwrap())
            .unwrap();
    }
}