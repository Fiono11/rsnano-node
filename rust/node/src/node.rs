use crate::{
    config::{NodeConfig, NodeFlags},
    stats::Stats,
    utils::{
        AsyncRuntime, LongRunningTransactionLogger, ThreadPool, ThreadPoolImpl, TxnTrackingConfig,
    },
    work::DistributedWorkFactory,
    NetworkParams,
};
use rsnano_core::{work::WorkPoolImpl, KeyPair};
use rsnano_store_lmdb::{
    EnvOptions, EnvironmentWrapper, LmdbConfig, LmdbStore, NullTransactionTracker,
    TransactionTracker,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tracing::info;

pub struct Node {
    pub async_rt: Arc<AsyncRuntime>,
    application_path: PathBuf,
    pub node_id: KeyPair,
    pub config: NodeConfig,
    network_params: NetworkParams,
    pub stats: Arc<Stats>,
    pub workers: Arc<dyn ThreadPool>,
    pub bootstrap_workers: Arc<dyn ThreadPool>,
    flags: NodeFlags,
    work: Arc<WorkPoolImpl>,
    pub distributed_work: Arc<DistributedWorkFactory>,
    pub store: Arc<LmdbStore>,
}

impl Node {
    pub fn new(
        async_rt: Arc<AsyncRuntime>,
        application_path: impl Into<PathBuf>,
        config: NodeConfig,
        network_params: NetworkParams,
        flags: NodeFlags,
        work: Arc<WorkPoolImpl>,
    ) -> Self {
        let application_path = application_path.into();
        let node_id = load_or_create_node_id(&application_path);
        Self {
            node_id,
            stats: Arc::new(Stats::new(config.stat_config.clone())),
            workers: Arc::new(ThreadPoolImpl::create(
                config.background_threads as usize,
                "Worker".to_string(),
            )),
            bootstrap_workers: Arc::new(ThreadPoolImpl::create(
                config.bootstrap_serving_threads as usize,
                "Bootstrap work".to_string(),
            )),
            distributed_work: Arc::new(DistributedWorkFactory::new(
                Arc::clone(&work),
                Arc::clone(&async_rt),
            )),
            store: make_store(
                &application_path,
                true,
                &config.diagnostics_config.txn_tracking,
                Duration::from_millis(config.block_processor_batch_max_time_ms as u64),
                config.lmdb_config.clone(),
                config.backup_before_upgrade,
            )
            .expect("Could not create LMDB store"),
            application_path,
            network_params,
            config,
            flags,
            work,
            async_rt,
        }
    }
}

fn make_store(
    path: &Path,
    add_db_postfix: bool,
    txn_tracking_config: &TxnTrackingConfig,
    block_processor_batch_max_time: Duration,
    lmdb_config: LmdbConfig,
    backup_before_upgrade: bool,
) -> anyhow::Result<Arc<LmdbStore>> {
    let mut path = PathBuf::from(path);
    if add_db_postfix {
        path.push("data.ldb");
    }

    let txn_tracker: Arc<dyn TransactionTracker> = if txn_tracking_config.enable {
        Arc::new(LongRunningTransactionLogger::new(
            txn_tracking_config.clone(),
            block_processor_batch_max_time,
        ))
    } else {
        Arc::new(NullTransactionTracker::new())
    };

    let options = EnvOptions {
        config: lmdb_config,
        use_no_mem_init: true,
    };

    let store = LmdbStore::<EnvironmentWrapper>::open(&path)
        .options(&options)
        .backup_before_upgrade(backup_before_upgrade)
        .txn_tracker(txn_tracker)
        .build()?;
    Ok(Arc::new(store))
}

fn load_or_create_node_id(path: &Path) -> KeyPair {
    let mut private_key_path = PathBuf::from(path);
    private_key_path.push("node_id_private.key");
    if private_key_path.exists() {
        info!("Reading node id from: '{:?}'", private_key_path);
        let content =
            std::fs::read_to_string(&private_key_path).expect("Could not read node id file");
        KeyPair::from_priv_key_hex(&content).expect("Could not read node id")
    } else {
        std::fs::create_dir_all(path).expect("Could not create app dir");
        info!("Generating a new node id, saving to: '{:?}'", path);
        let keypair = KeyPair::new();
        std::fs::write(
            private_key_path,
            keypair.private_key().encode_hex().as_bytes(),
        )
        .expect("Could not write node id file");
        keypair
    }
}