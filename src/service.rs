//! The "service" is where all the major components are plugged together:
//!
//! - The networking.
//! - The Wasm virtual machines.
//! - The storage and database.
//!
//! The service performs the following actions:
//!
//! - Tries to download all the active blocks (i.e. all blocks that descend from the latest
//! finalized block that have been announced) and put them in the database after having verified
//! their validity.
//! - Relays all block announces and transaction announces between the peers we're connected to.
//! - Announces our own locally-emitted transactions.
//! - Answers blocks requests made by remotes.
//!
//! At the moment, authoring blocks and running GrandPa isn't supported.

use crate::{executor, network, storage, telemetry};

use core::num::NonZeroU64;
use futures::{executor::ThreadPool, prelude::*};
use primitive_types::H256;

pub use builder::{builder, ServiceBuilder};

mod builder;

pub struct Service {
    /// Collection of all the Wasm VMs that are currently running.
    wasm_vms: executor::WasmVirtualMachines<()>,
    /// Database of the state of all the blocks.
    storage: storage::Storage,
    /// Management of the network. Contains all the active connections and their state.
    network: network::Network,
    /// Connections to zero or more telemetry servers.
    telemetry: telemetry::Telemetry,

    /// Optional threads pool that is used to dispatch tasks and that we keep alive.
    _threads_pool: Option<ThreadPool>,
}

pub enum Event {
    /// Head of the chain has been updated.
    NewChainHead(u64),

    /// The finalized block has been updated to a different one.
    NewFinalized {
        /// Number of the finalized block.
        number: u64,
        /// Hash of the finalized block.
        hash: H256,
    },
}

impl Service {
    /// Returns the next event that happens in the service.
    pub async fn next_event(&mut self) -> Event {
        // TODO: block0's hash is not 0
        let block0 = "0000000000000000000000000000000000000000000000000000000000000000"
            .parse()
            .unwrap();
        let wasm_runtime = executor::WasmBlob::from_bytes(
            self.storage
                .block(&block0)
                .storage()
                .unwrap()
                .code_key()
                .unwrap(),
        )
        .unwrap();
        self.wasm_vms
            .execute((), &wasm_runtime, executor::FunctionToCall::CoreVersion);

        // TODO: don't put that here
        self.network
            .start_block_request(network::BlocksRequestConfig {
                start: network::BlocksRequestConfigStart::Number(NonZeroU64::new(1).unwrap()),
                direction: network::BlocksRequestDirection::Ascending,
                desired_count: 1,
                fields: network::BlocksRequestFields {
                    header: true,
                    body: true,
                    justification: false,
                },
            })
            .await;

        loop {
            futures::select! {
                event = self.network.next_event().fuse() => {
                    match event {
                        network::Event::BlockAnnounce(header) => {
                            //self.network.start_block_request(header.number).await;
                            return Event::NewChainHead(header.number); // TODO: not necessarily the head
                        }
                        network::Event::BlocksRequestFinished { id, result } => {
                            println!("{:?}", result);
                        }
                    }
                }
            }
        }
    }
}
