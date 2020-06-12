use super::{database_task, executor_task, keystore_task, network_task, sync_task, Service};
use crate::{chain_spec::ChainSpec, database, keystore, network, storage};

use alloc::sync::Arc;
use core::{future::Future, pin::Pin, sync::atomic};
use futures::{channel::mpsc, executor::ThreadPool, prelude::*};
use hashbrown::HashMap;

/// Prototype for a service.
pub struct ServiceBuilder {
    /// State of the storage at the genesis block.
    genesis_storage_trie: HashMap<Vec<u8>, Vec<u8>>,

    /// Database where the chain data is stored. If `None`, data is kept in memory.
    database: Option<database::Database>,

    /// How to spawn background tasks. If you pass `None`, then a threads pool will be used by
    /// default.
    tasks_executor: Option<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync>>,

    /// Prototype for the network.
    network: network::builder::NetworkBuilder,
}

/// Creates a new prototype of the service.
pub fn builder() -> ServiceBuilder {
    ServiceBuilder {
        genesis_storage_trie: Default::default(),
        database: None,
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
        self.genesis_storage_trie = specs
            .genesis_storage()
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect();

        // TODO: chain specs should use stronger typing
        self.network.set_boot_nodes(
            specs
                .boot_nodes()
                .iter()
                .map(|bootnode_str| network::builder::parse_str_addr(bootnode_str).unwrap()),
        );

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
            genesis_storage_trie: self.genesis_storage_trie,
            database: self.database,
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
    pub async fn build(self) -> Service {
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
        let (to_network_tx, to_network_rx) = mpsc::channel(256);
        let (to_executor_tx, to_executor_rx) = mpsc::channel(256);
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
        tasks_executor(
            // TODO: the database task should be in its own thread because it's potentially blocking
            database_task::run_database_task(database_task::Config {
                database,
                to_database: to_database_rx,
            })
            .boxed(),
        );

        tasks_executor(
            sync_task::run_sync_task(sync_task::Config {
                to_executor: to_executor_tx,
                to_network: to_network_tx,
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
            executor_task::run_executor_task(executor_task::Config {
                tasks_executor: Box::new({
                    let tasks_executor = tasks_executor.clone();
                    Box::new(move |task| tasks_executor(task))
                }),
                to_executor: to_executor_rx,
                genesis_storage_trie: self.genesis_storage_trie,
            })
            .boxed(),
        );

        Service {
            events_in,
            num_network_connections: 0,
            num_connections_store,
            best_block_number: 0,      // TODO: wrong
            best_block_hash: [0; 32],  // TODO: wrong
            finalized_block_number: 0, // TODO: wrong
            finalized_block_hash: [0; 32],
            _threads_pool: threads_pool,
        }
    }
}
