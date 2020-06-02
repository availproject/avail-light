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

// # Implementation notes
//
// In terms of implementation, the service works by spawning various tasks that send messages to
// each other.
//
// Most of the magic happens at initialization, as that is the moment when we spawn the tasks.

use crate::{executor, network, storage};

use core::num::NonZeroU64;
use futures::{channel::mpsc, executor::ThreadPool, prelude::*};
use primitive_types::H256;

pub use builder::{builder, ServiceBuilder};

mod builder;
mod executor_task;
mod import_queue_task;
mod keystore_task;
mod network_task;
mod sync_task;

pub struct Service {
    /// Channel used by the background tasks to report what happens.
    /// Remember that this channel is bounded, and tasks will back-pressure if the user doesn't
    /// process events. This is an intended behaviour.
    events_in: mpsc::Receiver<Event>,

    /// Optional threads pool that is used to dispatch tasks and that we keep alive.
    _threads_pool: Option<ThreadPool>,
}

/// Event that happened on the service.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
        // The events channel is never closed unless the background tasks have all closed as well,
        // in which case it is totally appropriate to panic.
        self.events_in.next().await.unwrap()
    }
    /*
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
    }*/
}
