use super::{block_import_task, database_task, keystore_task, network_task, sync_task, Service};
use crate::{babe, chain_spec::ChainSpec, database, keystore, network};

use alloc::sync::Arc;
use core::{future::Future, pin::Pin, sync::atomic};
use futures::{
    channel::{mpsc, oneshot},
    executor::ThreadPool,
    prelude::*,
};

/// Prototype for a service.
// TODO: replace with a config pattern
pub struct ServiceBuilder {
    /// Database where the chain data is stored. If `None`, data is kept in memory.
    database: Option<database::Database>,

    /// BABE configuration of the chain, as retreived from the genesis block.
    babe_genesis_config: Option<babe::BabeGenesisConfiguration>,

    /// How to spawn background tasks. If you pass `None`, then a threads pool will be used by
    /// default.
    tasks_executor: Option<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync>>,

    /// Prototype for the network.
    network: network::builder::NetworkBuilder,
}

/// Creates a new prototype of the service.
pub fn builder() -> ServiceBuilder {
    ServiceBuilder {
        database: None,
        babe_genesis_config: None,
        tasks_executor: None,
        network: network::builder(),
    }
}

impl<'a> From<&'a ChainSpec> for ServiceBuilder {
    fn from(specs: &'a ChainSpec) -> ServiceBuilder {
        let mut builder = builder();
        builder.load_chain_specs(specs);
        builder
    }
}

impl ServiceBuilder {
    /// Overwrites the current configuration with values from the given chain specs.
    pub fn load_chain_specs(&mut self, specs: &ChainSpec) {
        // TODO: chain specs should use stronger typing for bootnodes
        self.network.set_boot_nodes(
            specs
                .boot_nodes()
                .iter()
                .map(|bootnode_str| network::builder::parse_str_addr(bootnode_str).unwrap()),
        );

        self.babe_genesis_config = Some({
            let wasm_code = specs
                .genesis_storage()
                .find(|(k, _)| k == b":code")
                .unwrap()
                .1
                .to_owned();

            babe::BabeGenesisConfiguration::from_runtime_code(&wasm_code, |k| {
                specs
                    .genesis_storage()
                    .find(|(k2, _)| *k2 == k)
                    .map(|(_, v)| v.to_owned())
            })
            .unwrap()
        });

        self.network
            .set_chain_spec_protocol_id(specs.protocol_id().unwrap());

        self.network
            .set_genesis_hash(crate::calculate_genesis_block_hash(specs.genesis_storage()).into());
    }

    /// Sets how the service should spawn background tasks.
    pub fn with_tasks_executor(
        mut self,
        executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync>,
    ) -> Self {
        self.tasks_executor = Some(executor);
        self
    }

    /// Sets the name of the chain to use on the network to identify incompatible peers earlier.
    pub fn with_chain_spec_protocol_id(self, id: impl AsRef<[u8]>) -> Self {
        ServiceBuilder {
            database: self.database,
            babe_genesis_config: self.babe_genesis_config,
            tasks_executor: self.tasks_executor,
            network: self.network.with_chain_spec_protocol_id(id),
        }
    }

    /// Sets the database where to load and save the chain's information.
    pub fn with_database(mut self, database: database::Database) -> Self {
        self.database = Some(database);
        self
    }

    /// Builds the actual service, starting everything.
    pub async fn build(mut self) -> Service {
        // TODO: check that chain specs match database?

        // Start by building the function that will spawn tasks. Since the user is allowed to
        // not specify an executor, we also spawn a supporting threads pool if necessary.
        let (threads_pool, tasks_executor) = match self.tasks_executor {
            Some(tasks_executor) => {
                let tasks_executor = Arc::new(tasks_executor)
                    as Arc<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync>>;
                (None, tasks_executor)
            }
            None => {
                let threads_pool = ThreadPool::builder()
                    .name_prefix("thread-")
                    .create()
                    .unwrap(); // TODO: don't unwrap
                let tasks_executor = {
                    let threads_pool = threads_pool.clone();
                    let exec =
                        Box::new(move |task| threads_pool.spawn_obj_ok(From::from(task))) as Box<_>;
                    Arc::new(exec)
                };
                (Some(threads_pool), tasks_executor)
            }
        };

        // This is when we actually create all the channels between the various tasks.
        // TODO: eventually this should be tweaked so that we are able to measure the congestion
        let (to_service_out, events_in) = mpsc::channel(16);
        let (mut to_network_tx, to_network_rx) = mpsc::channel(256);
        let (to_block_import_tx, to_block_import_rx) = mpsc::channel(16);
        let (_to_keystore_tx, to_keystore_rx) = mpsc::channel(16);
        let (to_database_tx, to_database_rx) = mpsc::channel(64);

        let num_connections_store = Arc::new(atomic::AtomicU64::new(0));

        // Now actually spawn all the tasks.
        // The order of the tasks spawning doesn't matter.
        tasks_executor(
            network_task::run_networking_task(network_task::Config {
                to_network: to_network_rx,
                to_service_out: to_service_out.clone(),
                network_builder: self.network.with_executor({
                    let tasks_executor = tasks_executor.clone();
                    Box::new(move |task| (*tasks_executor)(task))
                }),
                num_connections_store: num_connections_store.clone(),
            })
            .boxed(),
        );

        // TODO: obviously don't panic
        let database = Arc::new(
            self.database
                .expect("in-memory db not implemented, please use ServiceBuilder::with_database"),
        );
        // TODO: is this task necessary?
        tasks_executor(
            // TODO: the database task should be in its own thread because it's potentially blocking
            database_task::run_database_task(database_task::Config {
                database: database.clone(),
                to_database: to_database_rx,
            })
            .boxed(),
        );

        tasks_executor(
            sync_task::run_sync_task(sync_task::Config {
                to_block_import: to_block_import_tx,
                to_network: to_network_tx.clone(),
                to_service_out,
            })
            .boxed(),
        );

        // TODO: unused at the moment, maybe out of scope of this project
        tasks_executor(
            keystore_task::run_keystore_task(keystore_task::Config {
                keystore: keystore::Keystore::empty(),
                to_keystore: to_keystore_rx,
            })
            .boxed(),
        );

        tasks_executor(
            block_import_task::run_block_import_task(block_import_task::Config {
                database: database.clone(),
                // TODO: this unwraps if the service builder isn't properly configured; service builder should just be a Config struct instead
                babe_genesis_config: self.babe_genesis_config.take().unwrap(),
                tasks_executor: Box::new({
                    let tasks_executor = tasks_executor.clone();
                    Box::new(move |task| tasks_executor(task))
                }),
                to_block_import: to_block_import_rx,
            })
            .boxed(),
        );

        let local_peer_id = {
            let (tx, rx) = oneshot::channel();
            to_network_tx
                .send(network_task::ToNetwork::LocalPeerId(tx))
                .await
                .unwrap();
            rx.await.unwrap()
        };

        Service {
            events_in,
            to_database: to_database_tx,
            num_network_connections: 0,
            num_connections_store,
            local_peer_id,
            best_block_number: database.best_block_number().unwrap(),
            best_block_hash: database.best_block_hash().unwrap(),
            finalized_block_number: 0,     // TODO: wrong
            finalized_block_hash: [0; 32], // TODO:
            _threads_pool: threads_pool,
        }
    }
}
