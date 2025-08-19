use std::{
    collections::{HashMap, HashSet},
    fmt,
    fs::Permissions,
    mem::size_of,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use rand::{seq::IndexedRandom, Rng};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use rsnano_core::{
    utils::{ContainerInfo, ContainerInfoProvider},
    Account, Amount, Block, BlockDetails, BlockHash, Epoch, KeyDerivationFunction, Link, Networks,
    PendingKey, PrivateKey, PublicKey, RawKey, Root, SavedBlock, StateBlockArgs, WalletId,
    WorkNonce,
};
use rsnano_ledger::{AnySet, ConfirmedSet, Ledger, LedgerSet, DEV_GENESIS_PUB_KEY};
use rsnano_nullable_lmdb::{
    DatabaseFlags, LmdbDatabase, LmdbEnvironment, Transaction, WriteFlags, WriteTransaction,
};
use rsnano_store_lmdb::{KeyType, LmdbIterator, LmdbWalletStore};
use rsnano_work::WorkThresholds;

use super::{Wallet, WalletActionThread};
use crate::{
    block_processing::{BlockProcessorQueue, BlockSource},
    utils::{ThreadPool, ThreadPoolImpl},
    work::{WorkFactory, WorkRequest},
};

#[derive(FromPrimitive, Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub enum WalletsError {
    Generic,
    WalletNotFound,
    WalletLocked,
    AccountNotFound,
    InvalidPassword,
    BadPublicKey,
}

impl WalletsError {
    pub fn as_str(&self) -> &'static str {
        match self {
            WalletsError::Generic => "Unknown error",
            WalletsError::WalletNotFound => "Wallet not found",
            WalletsError::WalletLocked => "Wallet is locked",
            WalletsError::AccountNotFound => "Account not found",
            WalletsError::InvalidPassword => "Invalid password",
            WalletsError::BadPublicKey => "Bad public key",
        }
    }
}

impl fmt::Display for WalletsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::error::Error for WalletsError {}

pub enum PreparedSend {
    Cached(SavedBlock),
    New(Block, BlockDetails),
}

#[derive(Clone)]
pub struct WalletsConfig {
    pub preconfigured_representatives: Vec<PublicKey>,
    pub password_fanout: usize,
    pub receive_minimum: Amount,
    pub vote_minimum: Amount,
    pub voting_enabled: bool,
    /// How long to wait until the next cached work is created
    pub cached_work_generation_delay: Duration,
    pub kdf_work: u32,
}

impl WalletsConfig {
    pub fn default_for(network: Networks) -> Self {
        match network {
            Networks::Invalid => unreachable!(),
            Networks::NanoDevNetwork => Self::defaults_dev(),
            Networks::NanoBetaNetwork => Self::defaults_beta(),
            Networks::NanoLiveNetwork => Self::defaults_live(),
            Networks::NanoTestNetwork => Self::defaults_test(),
        }
    }

    pub fn defaults_live() -> Self {
        Self {
            preconfigured_representatives: default_preconfigured_representatives_for_live(),
            password_fanout: 1024,
            receive_minimum: Amount::micronano(1),
            vote_minimum: Amount::nano(1000),
            voting_enabled: false,
            cached_work_generation_delay: Duration::from_secs(10),
            kdf_work: 1024 * 64,
        }
    }

    pub fn defaults_dev() -> Self {
        Self {
            voting_enabled: true,
            preconfigured_representatives: vec![*DEV_GENESIS_PUB_KEY],
            cached_work_generation_delay: Duration::from_secs(1),
            kdf_work: 8,
            ..Self::defaults_live()
        }
    }

    pub fn defaults_beta() -> Self {
        Self {
            preconfigured_representatives: vec![Account::decode_account(
                "nano_1defau1t9off1ine9rep99999999999999999999999999999999wgmuzxxy",
            )
            .unwrap()
            .into()],
            ..Self::defaults_live()
        }
    }

    pub fn defaults_test() -> Self {
        Self {
            preconfigured_representatives: Vec::new(),
            ..Self::defaults_live()
        }
    }
}

impl Default for WalletsConfig {
    fn default() -> Self {
        Self::defaults_live()
    }
}

pub(crate) fn default_preconfigured_representatives_for_live() -> Vec<PublicKey> {
    const REP_KEYS: [&'static str; 8] = [
        "A30E0A32ED41C8607AA9212843392E853FCBCB4E7CB194E35C94F07F91DE59EF",
        "67556D31DDFC2A440BF6147501449B4CB9572278D034EE686A6BEE29851681DF",
        "5C2FBB148E006A8E8BA7A75DD86C9FE00C83F5FFDBFD76EAA09531071436B6AF",
        "AE7AC63990DAAAF2A69BF11C913B928844BF5012355456F2F164166464024B29",
        "BD6267D6ECD8038327D2BCC0850BDF8F56EC0414912207E81BCF90DFAC8A4AAA",
        "2399A083C600AA0572F5E36247D978FCFC840405F8D4B6D33161C0066A55F431",
        "2298FAB7C61058E77EA554CB93EDEEDA0692CBFCC540AB213B2836B29029E23A",
        "3FE80B4BC842E82C1C18ABFEEC47EA989E63953BC82AC411F304D13833D52A56",
    ];

    REP_KEYS
        .iter()
        .map(|s| PublicKey::decode_hex(s).unwrap())
        .collect()
}

pub struct Wallets {
    db: Option<LmdbDatabase>,
    send_action_ids_handle: Option<LmdbDatabase>,
    env: Arc<LmdbEnvironment>,
    wallets: Mutex<HashMap<WalletId, Arc<Wallet>>>,
    wallets_config: WalletsConfig,
    ledger: Arc<Ledger>,
    work_factory: Arc<WorkFactory>,
    work_thresholds: WorkThresholds,
    pub delayed_work: Mutex<HashMap<Account, Root>>,
    workers: Arc<dyn ThreadPool>,
    wallet_actions: WalletActionThread,
    block_processor_queue: Arc<BlockProcessorQueue>,
    kdf: KeyDerivationFunction,
    start_election: Mutex<Option<Box<dyn Fn(SavedBlock) + Send + Sync>>>,
}

impl Wallets {
    pub fn new(
        wallets_config: WalletsConfig,
        env: Arc<LmdbEnvironment>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
        work: WorkThresholds,
        work_factory: Arc<WorkFactory>,
    ) -> Self {
        let kdf = KeyDerivationFunction::new(wallets_config.kdf_work);

        Self {
            db: None,
            send_action_ids_handle: None,
            wallets: Mutex::new(HashMap::new()),
            env,
            wallets_config,
            ledger: Arc::clone(&ledger),
            work_factory,
            work_thresholds: work,
            delayed_work: Mutex::new(HashMap::new()),
            workers: Arc::new(ThreadPoolImpl::create(1, "wallet work")),
            wallet_actions: WalletActionThread::new(),
            block_processor_queue,
            kdf: kdf.clone(),
            start_election: Mutex::new(None),
        }
    }

    pub fn new_null() -> Self {
        let network = Networks::NanoLiveNetwork;
        let env = Arc::new(LmdbEnvironment::new_null());
        let ledger = Arc::new(Ledger::new_null());
        let wallets_config = WalletsConfig::default();
        let block_processor_queue = Arc::new(BlockProcessorQueue::default());
        let work = WorkThresholds::default_for(network);
        let work_factory = Arc::new(WorkFactory::disabled());
        Self::new(
            wallets_config,
            env,
            ledger,
            block_processor_queue,
            work,
            work_factory,
        )
    }

    pub fn start(&self) {
        self.wallet_actions.start();
    }

    pub fn stop(&self) {
        self.wallet_actions.stop();
        self.env.sync().expect("sync failed");
    }

    pub fn set_start_election_callback(&self, callback: Box<dyn Fn(SavedBlock) + Send + Sync>) {
        *self.start_election.lock().unwrap() = Some(callback);
    }

    fn random_representative(&self) -> PublicKey {
        self.wallets_config
            .preconfigured_representatives
            .choose(&mut rand::rng())
            .cloned()
            .unwrap_or(self.ledger.constants.genesis_account.into())
    }

    pub fn enter_initial_password(&self, wallet: &Arc<Wallet>) {
        let password = wallet.store.password();
        if password.is_zero() {
            let mut txn = self.env.begin_write();
            if wallet.store.valid_password(&txn) {
                // Newly created wallets have a zero key
                let _ = wallet.store.rekey(&mut txn, "");
            } else {
                let _ = self.enter_password_wallet(wallet, &txn, "");
            }
            txn.commit();
        }
    }

    fn enter_password_wallet(
        &self,
        wallet: &Arc<Wallet>,
        wallet_tx: &dyn Transaction,
        password: &str,
    ) -> Result<(), ()> {
        if !wallet.store.attempt_password(wallet_tx, password) {
            warn!("Invalid password, wallet locked");
            Err(())
        } else {
            info!("Wallet unlocked");
            Ok(())
        }
    }

    pub fn initialize(&mut self) -> anyhow::Result<()> {
        {
            let mut guard = self.wallets.lock().unwrap();
            self.db = Some(self.env.create_db(None, DatabaseFlags::empty())?);
            self.send_action_ids_handle = Some(
                self.env
                    .create_db(Some("send_action_ids"), DatabaseFlags::empty())?,
            );

            let wallet_ids = {
                let txn = self.env.begin_write();
                let ids = self.get_wallet_ids_with_tx(&txn);
                txn.commit();
                ids
            };

            for id in wallet_ids {
                assert!(!guard.contains_key(&id));
                let representative = self.random_representative();
                let text = PathBuf::from(id.encode_hex());
                let wallet = Wallet::new(
                    &self.env,
                    self.wallets_config.password_fanout as usize,
                    self.kdf.clone(),
                    representative,
                    &text,
                )?;

                guard.insert(id, Arc::new(wallet));
            }

            info!("Found {} wallet(s)", guard.len());
            for i in guard.keys() {
                info!("Wallet: {}", i);
            }

            for (_, wallet) in guard.iter() {
                self.enter_initial_password(wallet);
            }
        }

        Ok(())
    }

    fn iter_wallets<'tx>(&self, tx: &'tx dyn Transaction) -> impl Iterator<Item = WalletId> + 'tx {
        let cursor = tx
            .open_ro_cursor(self.db.unwrap())
            .expect("Could not read from wallets db");

        LmdbIterator::new(cursor, |k, _| {
            // wallet tables are identified by their wallet id hex string which is 64 bytes
            let key = if k.len() == 64 {
                WalletId::decode_hex(std::str::from_utf8(k).unwrap()).unwrap()
            } else {
                WalletId::zero()
            };
            (key, ())
        })
        .filter_map(|(k, _)| if k.is_zero() { None } else { Some(k) })
    }

    pub fn wallet_ids(&self) -> Vec<WalletId> {
        let txn = self.env.begin_read();
        let ids = self.get_wallet_ids_with_tx(&txn);
        txn.commit();
        ids
    }

    pub fn get_wallet_ids(&self) -> Vec<WalletId> {
        let txn = self.env.begin_read();
        let ids = self.iter_wallets(&txn).collect::<Vec<_>>();
        txn.commit();
        ids
    }

    pub fn get_wallet_ids_with_tx(&self, tx: &dyn Transaction) -> Vec<WalletId> {
        self.iter_wallets(tx).collect()
    }

    pub fn get_block_hash(
        &self,
        txn: &dyn Transaction,
        id: &str,
    ) -> anyhow::Result<Option<BlockHash>> {
        match txn.get(self.send_action_ids_handle.unwrap(), id.as_bytes()) {
            Ok(bytes) => Ok(Some(
                BlockHash::from_slice(bytes).ok_or_else(|| anyhow!("invalid block hash"))?,
            )),
            Err(rsnano_nullable_lmdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_block_hash(
        &self,
        txn: &mut WriteTransaction,
        id: &str,
        hash: &BlockHash,
    ) -> anyhow::Result<()> {
        txn.put(
            self.send_action_ids_handle.unwrap(),
            id.as_bytes(),
            hash.as_bytes(),
            WriteFlags::empty(),
        )?;
        Ok(())
    }

    pub fn clear_send_ids(&self) {
        let mut txn = self.env.begin_write();
        txn.clear_db(self.send_action_ids_handle.unwrap()).unwrap();
        txn.commit();
    }

    pub fn get_all_pub_keys(&self) -> Vec<PublicKey> {
        let mut wallet_keys = Vec::new();
        {
            let wallets_guard = self.wallets.lock().unwrap();
            let txn = self.env.begin_read();
            for (_, wallet) in wallets_guard.iter() {
                for (pub_key, _) in wallet.store.iter(&txn) {
                    wallet_keys.push(pub_key);
                }
            }
            txn.commit();
        }

        wallet_keys
    }

    pub fn get_all_private_keys(&self) -> Vec<PrivateKey> {
        let mut all_priv_keys: Vec<PrivateKey> = Vec::new();
        {
            let txn = self.env.begin_read();
            let lock = self.wallets.lock().unwrap();
            for (_, wallet) in lock.iter() {
                if wallet.store.valid_password(&txn) {
                    for (pub_key, _) in wallet.store.iter(&txn) {
                        if let Ok(prv_key) = wallet.store.fetch(&txn, &pub_key) {
                            all_priv_keys.push(prv_key.into());
                        }
                    }
                }
            }
            txn.commit();
        }
        all_priv_keys
    }

    fn work_cache_blocking(&self, wallet: &Wallet, pub_key: &PublicKey, root: &Root) {
        if self.work_factory.work_generation_enabled() {
            let work_request = WorkRequest::new(*root, self.work_thresholds.threshold_base());

            if let Some(work) = self.work_factory.generate_work(work_request) {
                let mut txn = self.env.begin_write();
                if wallet.live() && wallet.store.exists(&txn, pub_key) {
                    let latest = self.ledger.any().latest_root(&pub_key.into());
                    if latest == *root {
                        wallet.work_put(&mut txn, pub_key, work);
                    } else {
                        warn!("Cached work no longer valid, discarding");
                    }
                }
                txn.commit();
            } else {
                warn!(
                    "Could not precache work for root {} due to work generation failure",
                    root
                );
            }
        }
    }

    pub fn get_wallet(&self, wallet_id: &WalletId) -> Option<Arc<Wallet>> {
        self.wallets.lock().unwrap().get(wallet_id).cloned()
    }

    fn get_wallet_guard<'a>(
        guard: &'a HashMap<WalletId, Arc<Wallet>>,
        wallet_id: &WalletId,
    ) -> Result<&'a Arc<Wallet>, WalletsError> {
        guard.get(wallet_id).ok_or(WalletsError::WalletNotFound)
    }

    pub fn insert_watch(
        &self,
        wallet_id: &WalletId,
        accounts: &[Account],
    ) -> Result<(), WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        let mut txn = self.env.begin_write();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }

        for account in accounts {
            if wallet
                .store
                .insert_watch(&mut txn, &account.into())
                .is_err()
            {
                return Err(WalletsError::BadPublicKey);
            }
        }
        txn.commit();

        Ok(())
    }

    pub fn valid_password(&self, wallet_id: &WalletId) -> Result<bool, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        let txn = self.env.begin_read();
        let valid = wallet.store.valid_password(&txn);
        txn.commit();
        Ok(valid)
    }

    pub fn attempt_password(
        &self,
        wallet_id: &WalletId,
        password: impl AsRef<str>,
    ) -> Result<(), WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        let txn = self.env.begin_write();
        if wallet.store.attempt_password(&txn, password.as_ref()) {
            txn.commit();
            Ok(())
        } else {
            Err(WalletsError::InvalidPassword)
        }
    }

    pub fn lock(&self, wallet_id: &WalletId) -> Result<(), WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        wallet.store.lock();
        Ok(())
    }

    pub fn rekey(
        &self,
        wallet_id: &WalletId,
        password: impl AsRef<str>,
    ) -> Result<(), WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        let mut txn = self.env.begin_write();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }

        let result = wallet
            .store
            .rekey(&mut txn, password.as_ref())
            .map_err(|_| WalletsError::Generic);
        txn.commit();
        result
    }

    pub fn set_observer(&self, observer: Box<dyn Fn(bool) + Send>) {
        self.wallet_actions.set_observer(observer);
    }

    pub fn exists(&self, pub_key: &PublicKey) -> bool {
        let guard = self.wallets.lock().unwrap();
        let txn = self.env.begin_read();
        let exists = guard
            .values()
            .any(|wallet| wallet.store.exists(&txn, pub_key));
        txn.commit();
        exists
    }

    pub fn reload(&self) {
        let mut guard = self.wallets.lock().unwrap();
        let mut stored_items = HashSet::new();

        let wallet_ids = {
            let txn = self.env.begin_write();
            let ids = self.get_wallet_ids_with_tx(&txn);
            txn.commit();
            ids
        };

        for id in wallet_ids {
            // New wallet
            if !guard.contains_key(&id) {
                let text = PathBuf::from(id.encode_hex());
                let representative = self.random_representative();
                if let Ok(wallet) = Wallet::new(
                    &self.env,
                    self.wallets_config.password_fanout as usize,
                    self.kdf.clone(),
                    representative,
                    &text,
                ) {
                    guard.insert(id, Arc::new(wallet));
                }
            }
            // List of wallets on disk
            stored_items.insert(id);
        }
        // Delete non existing wallets from memory
        let mut deleted_items = Vec::new();
        for &id in guard.keys() {
            if !stored_items.contains(&id) {
                deleted_items.push(id);
            }
        }
        for i in &deleted_items {
            guard.remove(i);
        }
    }

    pub fn wallet_exists(&self, wallet_id: &WalletId) -> bool {
        self.wallets.lock().unwrap().contains_key(wallet_id)
    }

    pub fn destroy(&self, id: &WalletId) {
        let mut guard = self.wallets.lock().unwrap();
        let mut txn = self.env.begin_write();
        // action_mutex should be locked after transactions to prevent deadlocks in deterministic_insert () & insert_adhoc ()
        let _action_guard = self.wallet_actions.lock_safe();
        let wallet = guard.remove(id).unwrap();
        wallet.store.destroy(&mut txn);
        txn.commit();
    }

    pub fn remove_key(
        &self,
        wallet_id: &WalletId,
        pub_key: &PublicKey,
    ) -> Result<(), WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        let mut txn = self.env.begin_write();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }
        if wallet.store.find(&txn, pub_key).is_none() {
            return Err(WalletsError::AccountNotFound);
        }
        wallet.store.erase(&mut txn, pub_key);
        txn.commit();
        Ok(())
    }

    pub fn work_set(
        &self,
        wallet_id: &WalletId,
        pub_key: &PublicKey,
        work: WorkNonce,
    ) -> Result<(), WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        let mut txn = self.env.begin_write();
        if wallet.store.find(&txn, pub_key).is_none() {
            return Err(WalletsError::AccountNotFound);
        }
        wallet.store.work_put(&mut txn, pub_key, work);
        txn.commit();
        Ok(())
    }

    pub fn move_accounts(
        &self,
        source_id: &WalletId,
        target_id: &WalletId,
        accounts: &[PublicKey],
    ) -> Result<(), WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let source = Self::get_wallet_guard(&guard, source_id)?;
        let target = Self::get_wallet_guard(&guard, target_id)?;
        let txn = self.env.begin_read();
        let is_locked = !source.store.valid_password(&txn) || !target.store.valid_password(&txn);
        txn.commit();

        if is_locked {
            return Err(WalletsError::WalletLocked);
        }

        let mut txn = self.env.begin_write();
        let result = target
            .store
            .move_keys(&mut txn, &source.store, accounts)
            .map_err(|_| WalletsError::AccountNotFound);
        txn.commit();
        result
    }

    pub fn backup(&self, path: &Path) -> anyhow::Result<()> {
        let guard = self.wallets.lock().unwrap();
        let txn = self.env.begin_read();
        for (id, wallet) in guard.iter() {
            std::fs::create_dir_all(path)?;
            std::fs::set_permissions(path, Permissions::from_mode(0o700))?;
            let mut backup_path = PathBuf::from(path);
            backup_path.push(format!("{}.json", id));
            wallet.store.write_backup(&txn, &backup_path)?;
        }
        txn.commit();
        Ok(())
    }

    pub fn deterministic_index_get(&self, wallet_id: &WalletId) -> Result<u32, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        let txn = self.env.begin_read();
        let index = wallet.store.deterministic_index_get(&txn);
        txn.commit();
        Ok(index)
    }

    fn prepare_send(
        &self,
        tx: &dyn Transaction,
        wallet: &Arc<Wallet>,
        source: Account,
        destination: Account,
        amount: Amount,
        mut work: WorkNonce,
    ) -> anyhow::Result<PreparedSend> {
        let any = self.ledger.any();
        if !wallet.store.valid_password(tx) {
            bail!("invalid password");
        }
        let balance = any.account_balance(&source);

        if balance.is_zero() || balance < amount {
            bail!("insufficient balance");
        }

        let info = any.get_account(&source).unwrap();
        let prv_key_raw = wallet.store.fetch(tx, &source.into()).unwrap();
        if work.is_zero() {
            work = wallet
                .store
                .work_get(tx, &source.into())
                .unwrap_or_default();
        }
        let priv_key = PrivateKey::from(prv_key_raw);
        let state_block: Block = StateBlockArgs {
            key: &priv_key,
            previous: info.head,
            representative: info.representative,
            balance: balance - amount,
            link: destination.into(),
            work,
        }
        .into();
        let details = BlockDetails::new(info.epoch, true, false, false);
        Ok(PreparedSend::New(state_block, details))
    }

    fn prepare_send_with_id(
        &self,
        tx: &mut WriteTransaction,
        id: &str,
        wallet: &Arc<Wallet>,
        source: Account,
        destination: Account,
        amount: Amount,
        mut work: WorkNonce,
    ) -> anyhow::Result<PreparedSend> {
        let any = self.ledger.any();

        let block = match self.get_block_hash(tx, id)? {
            Some(hash) => Some(any.get_block(&hash).unwrap()),
            None => None,
        };

        if let Some(block) = block {
            Ok(PreparedSend::Cached(block))
        } else {
            if !wallet.store.valid_password(tx) {
                bail!("invalid password");
            }

            let balance = any.account_balance(&source);

            if balance.is_zero() || balance < amount {
                bail!("insufficient balance");
            }

            let info = any.get_account(&source).unwrap();
            let prv_key_raw = wallet.store.fetch(tx, &source.into()).unwrap();
            if work.is_zero() {
                work = wallet
                    .store
                    .work_get(tx, &source.into())
                    .unwrap_or_default();
            }
            let priv_key = PrivateKey::from(prv_key_raw);
            let state_block: Block = StateBlockArgs {
                key: &priv_key,
                previous: info.head,
                representative: info.representative,
                balance: balance - amount,
                link: destination.into(),
                work,
            }
            .into();
            let details = BlockDetails::new(info.epoch, true, false, false);
            self.set_block_hash(tx, id, &state_block.hash())?;
            Ok(PreparedSend::New(state_block, details))
        }
    }

    pub fn work_get(&self, wallet_id: &WalletId, pub_key: &PublicKey) -> WorkNonce {
        let guard = self.wallets.lock().unwrap();
        let Some(wallet) = guard.get(&wallet_id) else {
            return 1.into();
        };
        let txn = self.env.begin_read();
        let work = wallet.store.work_get(&txn, pub_key).unwrap_or(1.into());
        txn.commit();
        work
    }

    pub fn work_get2(
        &self,
        wallet_id: &WalletId,
        pub_key: &PublicKey,
    ) -> Result<WorkNonce, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let txn = self.env.begin_read();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        if wallet.store.find(&txn, pub_key).is_none() {
            return Err(WalletsError::AccountNotFound);
        }
        Ok(wallet.store.work_get(&txn, pub_key).unwrap_or(1.into()))
    }

    pub fn get_accounts(&self, max_results: usize) -> Vec<Account> {
        let mut accounts = Vec::new();
        let guard = self.wallets.lock().unwrap();
        let txn = self.env.begin_read();
        for wallet in guard.values() {
            for (pub_key, _) in wallet.store.iter(&txn) {
                if accounts.len() >= max_results {
                    break;
                }

                accounts.push(pub_key.into());
            }
        }
        txn.commit();
        accounts
    }

    pub fn get_accounts_of_wallet(
        &self,
        wallet_id: &WalletId,
    ) -> Result<Vec<Account>, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        let txn = self.env.begin_read();
        let mut accounts = Vec::new();
        for (account, _) in wallet.store.iter(&txn) {
            accounts.push(account.into());
        }
        txn.commit();
        Ok(accounts)
    }

    pub fn fetch(&self, wallet_id: &WalletId, pub_key: &PublicKey) -> Result<RawKey, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, wallet_id)?;
        let txn = self.env.begin_read();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }
        if wallet.store.find(&txn, pub_key).is_none() {
            return Err(WalletsError::AccountNotFound);
        }
        let result = wallet
            .store
            .fetch(&txn, pub_key)
            .map_err(|_| WalletsError::Generic);
        txn.commit();
        result
    }

    pub fn import(&self, wallet_id: WalletId, json: &str) -> anyhow::Result<()> {
        let _guard = self.wallets.lock().unwrap();
        let _wallet = Wallet::new_from_json(
            &self.env,
            self.wallets_config.password_fanout as usize,
            self.kdf.clone(),
            &PathBuf::from(wallet_id.to_string()),
            json,
        )?;
        Ok(())
    }

    pub fn import_replace(
        &self,
        wallet_id: WalletId,
        json: &str,
        password: &str,
    ) -> anyhow::Result<()> {
        let guard = self.wallets.lock().unwrap();
        let existing = guard
            .get(&wallet_id)
            .ok_or_else(|| anyhow!("wallet not found"))?;
        let id = WalletId::from_bytes(rand::rng().random());
        let temp = LmdbWalletStore::new_from_json(
            1,
            self.kdf.clone(),
            &self.env,
            &PathBuf::from(id.to_string()),
            json,
        )?;

        let mut txn = self.env.begin_write();
        let result = if temp.attempt_password(&txn, password) {
            existing.store.import(&mut txn, &temp)
        } else {
            Err(anyhow!("bad password"))
        };
        temp.destroy(&mut txn);
        txn.commit();
        result
    }

    pub fn get_seed(&self, wallet_id: WalletId) -> Result<RawKey, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, &wallet_id)?;
        let txn = self.env.begin_read();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }
        let seed = wallet.store.seed(&txn);
        txn.commit();
        Ok(seed)
    }

    pub fn key_type(&self, wallet_id: WalletId, pub_key: &PublicKey) -> KeyType {
        let guard = self.wallets.lock().unwrap();
        match guard.get(&wallet_id) {
            Some(wallet) => {
                let txn = self.env.begin_read();
                let key_type = wallet.store.get_key_type(&txn, pub_key);
                txn.commit();
                key_type
            }
            None => KeyType::Unknown,
        }
    }

    pub fn get_representative(&self, wallet_id: WalletId) -> Result<PublicKey, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, &wallet_id)?;
        let txn = self.env.begin_read();
        Ok(wallet.store.representative(&txn))
    }

    pub fn decrypt(&self, wallet_id: WalletId) -> Result<Vec<(PublicKey, RawKey)>, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, &wallet_id)?;
        let txn = self.env.begin_read();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }

        let mut result = Vec::new();
        for (account, _) in wallet.store.iter(&txn) {
            let key = wallet
                .store
                .fetch(&txn, &account)
                .map_err(|_| WalletsError::Generic)?;
            result.push((account, key));
        }
        txn.commit();

        Ok(result)
    }

    pub fn serialize(&self, wallet_id: WalletId) -> Result<String, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Self::get_wallet_guard(&guard, &wallet_id)?;
        let txn = self.env.begin_read();
        let json = wallet.store.serialize_json(&txn);
        txn.commit();
        Ok(json)
    }

    pub fn wallet_count(&self) -> usize {
        self.wallets.lock().unwrap().len()
    }
}

impl Drop for Wallets {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ContainerInfoProvider for Wallets {
    fn container_info(&self) -> ContainerInfo {
        [
            (
                "items",
                self.wallet_count(),
                size_of::<usize>() * size_of::<WalletId>(),
            ),
            ("actions", self.wallet_actions.len(), size_of::<usize>() * 2),
        ]
        .into()
    }
}

const GENERATE_PRIORITY: Amount = Amount::MAX;
const HIGH_PRIORITY: Amount = Amount::raw(u128::MAX - 1);

pub trait WalletsExt {
    fn deterministic_insert(
        &self,
        wallet: &Arc<Wallet>,
        tx: &mut WriteTransaction,
        generate_work: bool,
    ) -> PublicKey;

    fn deterministic_insert_at(
        &self,
        wallet_id: &WalletId,
        index: u32,
        generate_work: bool,
    ) -> Result<PublicKey, WalletsError>;

    fn deterministic_insert2(
        &self,
        wallet_id: &WalletId,
        generate_work: bool,
    ) -> Result<PublicKey, WalletsError>;

    fn insert_adhoc(&self, wallet: &Arc<Wallet>, key: &RawKey, generate_work: bool) -> PublicKey;

    fn insert_adhoc2(
        &self,
        wallet_id: &WalletId,
        key: &RawKey,
        generate_work: bool,
    ) -> Result<PublicKey, WalletsError>;

    fn work_ensure(&self, wallet: &Arc<Wallet>, account: Account, root: Root);

    fn action_complete(
        &self,
        wallet: Arc<Wallet>,
        block: Block,
        account: Account,
        generate_work: bool,
        details: &BlockDetails,
    ) -> anyhow::Result<SavedBlock>;

    fn change_seed(
        &self,
        wallet_id: WalletId,
        prv_key: &RawKey,
        count: u32,
    ) -> Result<(u32, Account), WalletsError>;

    fn send(
        &self,
        wallet_id: WalletId,
        source: Account,
        account: Account,
        amount: Amount,
        work: WorkNonce,
        generate_work: bool,
        id: Option<String>,
    ) -> BlockPromise;

    fn change(
        &self,
        wallet_id: &WalletId,
        source: Account,
        representative: PublicKey,
        work: WorkNonce,
        generate_work: bool,
    ) -> BlockPromise;

    fn receive(
        &self,
        wallet_id: WalletId,
        block: BlockHash,
        representative: PublicKey,
        amount: Amount,
        account: Account,
        work: WorkNonce,
        generate_work: bool,
    ) -> BlockPromise;

    fn search_receivable(&self, wallet_id: &WalletId) -> MultiBlockPromise;

    fn search_receivable_all(&self) -> MultiBlockPromise;

    fn enter_password(&self, wallet_id: WalletId, password: &str) -> Result<(), WalletsError>;
    fn create(&self, wallet_id: WalletId);

    fn set_representative(
        &self,
        wallet_id: WalletId,
        rep: PublicKey,
        update_existing_accounts: bool,
    ) -> MultiBlockPromise;

    fn ensure_wallet_is_unlocked(&self, wallet_id: WalletId, password: &str) -> bool;
}

impl WalletsExt for Arc<Wallets> {
    fn deterministic_insert(
        &self,
        wallet: &Arc<Wallet>,
        tx: &mut WriteTransaction,
        generate_work: bool,
    ) -> PublicKey {
        if !wallet.store.valid_password(tx) {
            return PublicKey::zero();
        }
        let key = wallet.store.deterministic_insert(tx);

        info!(account=%key.as_account().encode_account(), "Deterministically inserted new account");

        if generate_work {
            self.work_ensure(wallet, key.into(), key.into());
        }
        key
    }

    fn deterministic_insert_at(
        &self,
        wallet_id: &WalletId,
        index: u32,
        generate_work: bool,
    ) -> Result<PublicKey, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Wallets::get_wallet_guard(&guard, wallet_id)?;
        let mut txn = self.env.begin_write();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }
        let account = wallet.store.deterministic_insert_at(&mut txn, index);
        txn.commit();

        info!(account=%account.as_account().encode_account(), "Deterministically inserted new account");

        if generate_work {
            self.work_ensure(wallet, account.into(), account.into());
        }
        Ok(account)
    }

    fn deterministic_insert2(
        &self,
        wallet_id: &WalletId,
        generate_work: bool,
    ) -> Result<PublicKey, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Wallets::get_wallet_guard(&guard, wallet_id)?;
        let mut txn = self.env.begin_write();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }
        let key = self.deterministic_insert(wallet, &mut txn, generate_work);
        txn.commit();
        Ok(key)
    }

    fn insert_adhoc(&self, wallet: &Arc<Wallet>, key: &RawKey, generate_work: bool) -> PublicKey {
        let mut tx = self.env.begin_write();
        if !wallet.store.valid_password(&tx) {
            return PublicKey::zero();
        }
        let key = wallet.store.insert_adhoc(&mut tx, key);
        if generate_work {
            self.work_ensure(
                wallet,
                key.into(),
                self.ledger.any().latest_root(&key.into()),
            );
        }
        tx.commit();

        key
    }

    fn insert_adhoc2(
        &self,
        wallet_id: &WalletId,
        key: &RawKey,
        generate_work: bool,
    ) -> Result<PublicKey, WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Wallets::get_wallet_guard(&guard, wallet_id)?;
        let txn = self.env.begin_read();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }
        txn.commit();
        Ok(self.insert_adhoc(wallet, key, generate_work))
    }

    fn work_ensure(&self, wallet: &Arc<Wallet>, account: Account, root: Root) {
        let precache_delay = self.wallets_config.cached_work_generation_delay;
        self.delayed_work.lock().unwrap().insert(account, root);
        let self_clone = Arc::clone(self);
        let wallet = Arc::clone(wallet);
        self.workers.post_delayed(
            precache_delay,
            Box::new(move || {
                let mut guard = self_clone.delayed_work.lock().unwrap();
                if let Some(&existing) = guard.get(&account) {
                    if existing == root {
                        guard.remove(&account);
                        let self_clone_2 = Arc::clone(&self_clone);
                        self_clone.wallet_actions.queue_wallet_action(
                            GENERATE_PRIORITY,
                            wallet,
                            Box::new(move |w| {
                                self_clone_2.work_cache_blocking(&w, &account.into(), &root);
                            }),
                        );
                    }
                }
            }),
        );
    }

    fn action_complete(
        &self,
        wallet: Arc<Wallet>,
        mut block: Block,
        account: Account,
        generate_work: bool,
        details: &BlockDetails,
    ) -> anyhow::Result<SavedBlock> {
        // Unschedule any work caching for this account
        self.delayed_work.lock().unwrap().remove(&account);
        let hash = block.hash();
        let required_difficulty = self.work_thresholds.threshold(details);
        if self.work_thresholds.difficulty_block(&block) < required_difficulty {
            info!(
                "Cached or provided work for block {} account {} is invalid, regenerating...",
                block.hash(),
                account.encode_account()
            );

            let work_request = WorkRequest::new(block.root(), required_difficulty);

            let work = self
                .work_factory
                .generate_work(work_request)
                .ok_or_else(|| anyhow!("no work generated"))?;

            block.set_work(work);
        }
        let arc_block = Arc::new(block.clone());
        let saved_block = self
            .block_processor_queue
            .push_blocking(arc_block.clone(), BlockSource::Local)?
            .map_err(|s| anyhow!("block processor failed: {:?}", s))?;

        if generate_work {
            // Pregenerate work for next block based on the block just created
            self.work_ensure(&wallet, account, hash.into());
        }
        Ok(saved_block)
    }

    fn change_seed(
        &self,
        wallet_id: WalletId,
        prv_key: &RawKey,
        mut count: u32,
    ) -> Result<(u32, Account), WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Wallets::get_wallet_guard(&guard, &wallet_id)?;
        let mut txn = self.env.begin_write();
        if !wallet.store.valid_password(&txn) {
            return Err(WalletsError::WalletLocked);
        }

        info!("Changing wallet seed");
        wallet.store.set_seed(&mut txn, prv_key);
        let mut first_account = self.deterministic_insert(wallet, &mut txn, true);
        if count == 0 {
            count = wallet.deterministic_check(&txn, 0, &self.ledger);
            info!("Auto-detected {} accounts to generate", count);
        }
        for _ in 0..count {
            // Disable work generation to prevent weak CPU nodes stuck
            first_account = self.deterministic_insert(wallet, &mut txn, false);
        }
        info!("Completed changing wallet seed and generating accounts");

        let restored_count = wallet.store.deterministic_index_get(&txn);
        txn.commit();
        Ok((restored_count, first_account.into()))
    }

    fn change(
        &self,
        wallet_id: &WalletId,
        source: Account,
        representative: PublicKey,
        mut work: WorkNonce,
        generate_work: bool,
    ) -> BlockPromise {
        let guard = self.wallets.lock().unwrap();
        let wallet = match Wallets::get_wallet_guard(&guard, wallet_id) {
            Ok(w) => w,
            Err(e) => {
                return BlockPromise::new_failed(e);
            }
        };
        let epoch: Epoch;
        let block: Block;
        {
            let wallet_tx = self.env.begin_read();
            let any = self.ledger.any();
            if !wallet.store.valid_password(&wallet_tx) {
                warn!(
                    "Changing representative for account {} failed, wallet locked",
                    source.encode_account()
                );
                return BlockPromise::new_failed(WalletsError::WalletLocked);
            }

            let existing = wallet.store.find(&wallet_tx, &source.into());
            if existing.is_some() && any.account_head(&source).is_some() {
                info!(
                    "Changing representative for account {} to {}",
                    source.encode_account(),
                    representative.as_account().encode_account()
                );
                let info = any.get_account(&source).unwrap();
                let prv = wallet.store.fetch(&wallet_tx, &source.into()).unwrap();
                if work.is_zero() {
                    work = wallet
                        .store
                        .work_get(&wallet_tx, &source.into())
                        .unwrap_or_default();
                }
                let priv_key = PrivateKey::from(prv);
                block = StateBlockArgs {
                    key: &priv_key,
                    previous: info.head,
                    representative,
                    balance: info.balance,
                    link: Link::zero(),
                    work,
                }
                .into();
                epoch = info.epoch;
            } else {
                warn!("Changing representative for account {} failed, wallet locked or account not found",
                    source.encode_account());
                return BlockPromise::new_failed(WalletsError::AccountNotFound);
            }
        }

        let details = BlockDetails::new(epoch, false, false, false);
        let self_l = Arc::clone(self);
        let promise = BlockPromise::new();
        let promise2 = promise.clone();
        self.wallet_actions.queue_wallet_action(
            HIGH_PRIORITY,
            wallet.clone(),
            Box::new(move |wallet| {
                let result = self_l
                    .action_complete(wallet, block.clone(), source, generate_work, &details)
                    .map_err(|_| WalletsError::Generic);
                promise2.set_result(result);
            }),
        );

        promise
    }

    fn receive(
        &self,
        wallet_id: WalletId,
        send_hash: BlockHash,
        representative: PublicKey,
        amount: Amount,
        account: Account,
        mut work: WorkNonce,
        generate_work: bool,
    ) -> BlockPromise {
        let wallet = {
            let guard = self.wallets.lock().unwrap();
            match Wallets::get_wallet_guard(&guard, &wallet_id) {
                Ok(wallet) => wallet.clone(),
                Err(e) => return BlockPromise::new_failed(e),
            }
        };

        if amount < self.wallets_config.receive_minimum {
            warn!(
                "Not receiving block {} due to minimum receive threshold",
                send_hash
            );
            return BlockPromise::new_failed(WalletsError::Generic);
        }

        let mut block: Option<Block> = None;
        let mut epoch = Epoch::Epoch0;
        let any = self.ledger.any();
        let wallet_tx = self.env.begin_read();
        if any.block_exists_or_pruned(&send_hash) {
            if let Some(pending_info) = any.get_pending(&PendingKey::new(account, send_hash)) {
                if let Ok(prv) = wallet.store.fetch(&wallet_tx, &account.into()) {
                    info!(
                        "Receiving block {} from account {}, amount {}",
                        send_hash,
                        account.encode_account(),
                        pending_info.amount.number()
                    );
                    if work.is_zero() {
                        work = wallet
                            .store
                            .work_get(&wallet_tx, &account.into())
                            .unwrap_or_default();
                    }
                    let priv_key = PrivateKey::from(prv);
                    if let Some(info) = any.get_account(&account) {
                        block = Some(
                            StateBlockArgs {
                                key: &priv_key,
                                previous: info.head,
                                representative: info.representative,
                                balance: info.balance + pending_info.amount,
                                link: send_hash.into(),
                                work,
                            }
                            .into(),
                        );
                        epoch = std::cmp::max(info.epoch, pending_info.epoch);
                    } else {
                        block = Some(
                            StateBlockArgs {
                                key: &priv_key,
                                previous: BlockHash::zero(),
                                representative,
                                balance: pending_info.amount,
                                link: send_hash.into(),
                                work,
                            }
                            .into(),
                        );
                        epoch = pending_info.epoch;
                    }
                } else {
                    warn!(
                        "Unable to receive, wallet locked, block {} to account: {}",
                        send_hash,
                        account.encode_account()
                    );
                }
            } else {
                // Ledger doesn't have this marked as available to receive anymore
                warn!("Not receiving block {}, block already received", send_hash);
            }
        } else {
            // Ledger doesn't have this block anymore.
            warn!(
                "Not receiving block {}, block no longer exists or pruned",
                send_hash
            );
        }
        wallet_tx.commit();

        let Some(block) = block else {
            return BlockPromise::new_failed(WalletsError::Generic);
        };
        let details = BlockDetails::new(epoch, false, true, false);

        let block_promise = BlockPromise::new();
        let block_promise2 = block_promise.clone();
        let self_l = Arc::clone(self);
        self.wallet_actions.queue_wallet_action(
            amount,
            wallet,
            Box::new(move |wallet| {
                let result = self_l
                    .action_complete(wallet, block.clone(), account, generate_work, &details)
                    .map_err(|_| WalletsError::Generic);
                block_promise2.set_result(result);
            }),
        );
        block_promise
    }

    fn send(
        &self,
        wallet_id: WalletId,
        source: Account,
        destination: Account,
        amount: Amount,
        work: WorkNonce,
        generate_work: bool,
        id: Option<String>,
    ) -> BlockPromise {
        let guard = self.wallets.lock().unwrap();
        let wallet = match Wallets::get_wallet_guard(&guard, &wallet_id) {
            Ok(w) => w,
            Err(e) => return BlockPromise::new_failed(e),
        };
        let txn = self.env.begin_write();
        if !wallet.store.valid_password(&txn) {
            return BlockPromise::new_failed(WalletsError::WalletLocked);
        }
        if wallet.store.find(&txn, &source.into()).is_none() {
            return BlockPromise::new_failed(WalletsError::AccountNotFound);
        }
        txn.commit();

        let promise = BlockPromise::new();
        let promise2 = promise.clone();
        let self_l = Arc::clone(self);

        self.wallet_actions.queue_wallet_action(
            HIGH_PRIORITY,
            wallet.clone(),
            Box::new(move |wallet| {
                let result = match &id {
                    Some(id) => {
                        let mut txn = self_l.env.begin_write();
                        let result = self_l.prepare_send_with_id(
                            &mut txn,
                            &id,
                            &wallet,
                            source,
                            destination,
                            amount,
                            work,
                        );
                        txn.commit();
                        result
                    }
                    None => {
                        let txn = self_l.env.begin_read();
                        self_l.prepare_send(&txn, &wallet, source, destination, amount, work)
                    }
                };

                let block = match result {
                    Ok(PreparedSend::Cached(block)) => Ok(block),
                    Ok(PreparedSend::New(block, details)) => self_l
                        .action_complete(wallet, block, source, generate_work, &details)
                        .map_err(|_| WalletsError::Generic),
                    Err(_) => Err(WalletsError::Generic),
                };

                promise2.set_result(block);
            }),
        );

        promise
    }

    fn search_receivable(&self, wallet_id: &WalletId) -> MultiBlockPromise {
        let wallet = match self.get_wallet(wallet_id) {
            Some(w) => w,
            None => return MultiBlockPromise::new_failed(WalletsError::WalletNotFound),
        };

        let txn = self.env.begin_read();
        if !wallet.store.valid_password(&txn) {
            info!("Unable to search receivable blocks, wallet is locked. Blocks won't be auto-received until the wallet is unlocked");
            return MultiBlockPromise::new_failed(WalletsError::WalletLocked);
        }

        debug!("Beginning receivable block search");

        let mut block_promises = Vec::new();
        for (account, wallet_value) in wallet.store.iter(&txn) {
            let any = self.ledger.any();
            // Don't search pending for watch-only accounts
            if !wallet_value.key.is_zero() {
                for (key, info) in
                    any.account_receivable_upper_bound(account.into(), BlockHash::zero())
                {
                    let hash = key.send_block_hash;
                    let amount = info.amount;
                    if self.wallets_config.receive_minimum <= amount {
                        info!(
                            "Found a receivable block {} for account {}",
                            hash,
                            info.source.encode_account()
                        );
                        if any.confirmed().block_exists_or_pruned(&hash) {
                            let representative = wallet.store.representative(&txn);
                            // Receive confirmed block
                            let promise = self.receive(
                                *wallet_id,
                                hash,
                                representative,
                                amount,
                                account.into(),
                                0.into(),
                                true,
                            );
                            block_promises.push(promise);
                        }
                    }
                }
            }
        }

        txn.commit();

        debug!("Receivable block search phase completed");
        MultiBlockPromise::new(block_promises)
    }

    fn search_receivable_all(&self) -> MultiBlockPromise {
        let wallet_ids = self.wallet_ids();
        let mut result = MultiBlockPromise::empty();
        for id in wallet_ids {
            result.append(self.search_receivable(&id));
        }
        result
    }

    fn enter_password(&self, wallet_id: WalletId, password: &str) -> Result<(), WalletsError> {
        let guard = self.wallets.lock().unwrap();
        let wallet = Wallets::get_wallet_guard(&guard, &wallet_id)?;
        let tx = self.env.begin_write();
        let result = self
            .enter_password_wallet(wallet, &tx, password)
            .map_err(|_| WalletsError::InvalidPassword);
        if result.is_ok() {
            info!("Wallet unlocked");
        } else {
            warn!("Invalid password, wallet locked");
        }
        result
    }

    fn create(&self, wallet_id: WalletId) {
        let mut guard = self.wallets.lock().unwrap();
        debug_assert!(!guard.contains_key(&wallet_id));
        let wallet = {
            let Ok(wallet) = Wallet::new(
                &self.env,
                self.wallets_config.password_fanout as usize,
                self.kdf.clone(),
                self.random_representative(),
                &PathBuf::from(wallet_id.to_string()),
            ) else {
                return;
            };
            Arc::new(wallet)
        };
        guard.insert(wallet_id, Arc::clone(&wallet));
        self.enter_initial_password(&wallet);
    }

    fn set_representative(
        &self,
        wallet_id: WalletId,
        rep: PublicKey,
        update_existing_accounts: bool,
    ) -> MultiBlockPromise {
        let mut accounts = Vec::new();
        {
            let guard = self.wallets.lock().unwrap();
            let wallet = match Wallets::get_wallet_guard(&guard, &wallet_id) {
                Ok(w) => w,
                Err(err) => {
                    return MultiBlockPromise::new_failed(err);
                }
            };

            {
                let mut txn = self.env.begin_write();
                if update_existing_accounts && !wallet.store.valid_password(&txn) {
                    return MultiBlockPromise::new_failed(WalletsError::WalletLocked);
                }

                wallet.store.representative_set(&mut txn, &rep);
                txn.commit();
            }

            // Change representative for all wallet accounts
            if update_existing_accounts {
                let txn = self.env.begin_read();
                let any = self.ledger.any();
                for (account, _) in wallet.store.iter(&txn) {
                    if let Some(info) = any.get_account(&account.into()) {
                        if info.representative != rep {
                            accounts.push(account);
                        }
                    }
                }
                txn.commit();
            }
        }

        let mut block_promises = Vec::new();
        for account in accounts {
            block_promises.push(self.change(&wallet_id, account.into(), rep, 0.into(), false));
        }

        MultiBlockPromise::new(block_promises)
    }

    fn ensure_wallet_is_unlocked(&self, wallet_id: WalletId, password: &str) -> bool {
        let guard = self.wallets.lock().unwrap();
        let Some(existing) = guard.get(&wallet_id) else {
            return false;
        };
        let txn = self.env.begin_write();
        let mut valid = existing.store.valid_password(&txn);
        if !valid {
            valid = self.enter_password_wallet(existing, &txn, password).is_ok();
        }
        txn.commit();

        valid
    }
}

#[derive(Clone)]
pub struct BlockPromise {
    done_notification: Arc<Condvar>,
    state: Arc<Mutex<BlockPromiseState>>,
}

impl BlockPromise {
    pub fn new() -> Self {
        Self::with_state(BlockPromiseState::new())
    }

    pub fn new_failed(err: WalletsError) -> Self {
        Self::with_state(BlockPromiseState::new_failed(err))
    }

    fn with_state(state: BlockPromiseState) -> Self {
        Self {
            done_notification: Arc::new(Condvar::new()),
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn set_result(&self, result: Result<SavedBlock, WalletsError>) {
        {
            let mut state = self.state.lock().unwrap();
            state.result = result;
            state.done = true;
        }
        self.done_notification.notify_all();
    }

    pub fn wait(&self) -> Result<SavedBlock, WalletsError> {
        let result_guard = self.state.lock().unwrap();
        self.done_notification
            .wait_while(result_guard, |i| !i.done)
            .unwrap()
            .result
            .clone()
    }
}

struct BlockPromiseState {
    done: bool,
    result: Result<SavedBlock, WalletsError>,
}

impl BlockPromiseState {
    fn new() -> Self {
        Self {
            done: false,
            result: Err(WalletsError::Generic),
        }
    }

    fn new_failed(err: WalletsError) -> Self {
        Self {
            done: true,
            result: Err(err),
        }
    }
}

#[derive(Clone)]
pub struct MultiBlockPromise {
    children: Vec<BlockPromise>,
}

impl MultiBlockPromise {
    pub fn new(children: Vec<BlockPromise>) -> Self {
        Self { children }
    }

    pub fn new_failed(error: WalletsError) -> Self {
        Self {
            children: vec![BlockPromise::new_failed(error)],
        }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    pub fn append(&mut self, other: MultiBlockPromise) {
        self.children.extend(other.children)
    }

    pub fn wait(&self) -> Result<Vec<SavedBlock>, WalletsError> {
        self.children.iter().map(|c| c.wait()).collect()
    }
}
