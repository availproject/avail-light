use super::{block_import_task, database_task, network_task, sync_task, Service};
use crate::{babe, chain_spec::ChainSpec, database, network};

use alloc::sync::Arc;
use core::{future::Future, pin::Pin, sync::atomic};
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};

/// Prototype for a service.
pub struct Config<'a> {
    /// Database where the chain data is stored. If `None`, data is kept in memory.
    pub database: Option<database::sled::Database>,

    /// How to spawn background tasks. If you pass `None`, then a threads pool will be used by
    /// default.
    pub tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync>,

    /// Information related to the chain.
    pub chain_info: ChainInfoConfig<'a>,

    /// Optional external implementation of a libp2p transport. Used in WASM contexts where we
    /// need some binding between the networking provided by the operating system or environment
    /// and libp2p.
    ///
    /// This parameter exists whatever the target platform is, but it is expected to be set to
    /// `Some` only when compiling for WASM.
    pub wasm_external_transport: Option<network::wasm_ext::ExtTransport>,
}

/// Information related to the chain.
pub struct ChainInfoConfig<'a> {
    /// BABE configuration of the chain, as retreived from the genesis block.
    pub babe_genesis_config: babe::BabeGenesisConfiguration,

    /// List of peer ids and their addresses that we know are part of the peer-to-peer network.
    pub network_known_addresses: Vec<(network::PeerId, network::Multiaddr)>,

    /// Small string identifying the name of the chain, in order to detect incompatible nodes
    /// earlier.
    pub chain_spec_protocol_id: &'a str,

    /// Hash of the genesis block of the local node.
    pub local_genesis_hash: [u8; 32],
}

impl<'a> From<&'a ChainSpec> for ChainInfoConfig<'a> {
    fn from(specs: &'a ChainSpec) -> ChainInfoConfig {
        let babe_genesis_config = {
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
        };

        ChainInfoConfig {
            babe_genesis_config,
            network_known_addresses: specs
                .boot_nodes()
                .iter()
                .map(|bootnode_str| network::parse_str_addr(bootnode_str).unwrap())
                .collect(),

            chain_spec_protocol_id: specs.protocol_id(),
            local_genesis_hash: crate::calculate_genesis_block_hash(specs.genesis_storage()).into(),
        }
    }
}

impl<'a> Config<'a> {
    /// Builds the actual service, starting everything.
    pub async fn build(self) -> Service {
        // TODO: check that chain specs match database?

        // Turn the `Box<Executor>` into an `Arc`.
        let tasks_executor: Arc<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync> =
            Arc::from(self.tasks_executor);

        // This is when we actually create all the channels between the various tasks.
        // TODO: eventually this should be tweaked so that we are able to measure the congestion
        let (to_service_out, events_in) = mpsc::channel(16);
        let (mut to_network_tx, to_network_rx) = mpsc::channel(256);
        let (to_block_import_tx, to_block_import_rx) = mpsc::channel(16);
        let (to_database_tx, to_database_rx) = mpsc::channel(64);

        let num_connections_store = Arc::new(atomic::AtomicU64::new(0));

        // Now actually spawn all the tasks.
        // The order of the tasks spawning doesn't matter.
        tasks_executor(
            network_task::run_networking_task(network_task::Config {
                to_network: to_network_rx,
                to_service_out: to_service_out.clone(),
                network_config: network::Config {
                    known_addresses: self.chain_info.network_known_addresses,
                    chain_spec_protocol_id: self
                        .chain_info
                        .chain_spec_protocol_id
                        .as_bytes()
                        .to_owned(),
                    tasks_executor: Box::new({
                        let tasks_executor = tasks_executor.clone();
                        Box::new(move |task| tasks_executor(task))
                    }),
                    local_genesis_hash: self.chain_info.local_genesis_hash,
                    wasm_external_transport: self.wasm_external_transport,
                },
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

        tasks_executor(
            block_import_task::run_block_import_task(block_import_task::Config {
                database: database.clone(),
                // TODO: this unwraps if the service builder isn't properly configured; service builder should just be a Config struct instead
                babe_genesis_config: self.chain_info.babe_genesis_config,
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
            finalized_block_number: 0, // TODO: wrong
            finalized_block_hash: self.chain_info.local_genesis_hash, // TODO: wrong
        }
    }
}
