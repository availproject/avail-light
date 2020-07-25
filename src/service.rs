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

use crate::network;

use alloc::sync::Arc;
use core::sync::atomic;
use futures::{
    channel::{mpsc, oneshot},
    executor::ThreadPool,
    prelude::*,
};
use network::PeerId;

pub use config::{ChainInfoConfig, Config};

mod block_import_task;
mod config;
mod database_task;
mod keystore_task;
mod network_task;
mod sync_task;

pub struct Service {
    /// Channel used by the background tasks to report what happens.
    /// Remember that this channel is bounded, and tasks will back-pressure if the user doesn't
    /// process events. This is an intended behaviour.
    events_in: mpsc::Receiver<Event>,

    /// Sender for messages towards the database task.
    to_database: mpsc::Sender<database_task::ToDatabase>,

    /// Number of transport-level (e.g. TCP/IP) network connections. Only updated by receiving
    /// events.
    num_network_connections: u64,

    /// `Arc` whose content is updated by the network task. Used to update
    /// [`Service::num_network_connections`].
    num_connections_store: Arc<atomic::AtomicU64>,

    /// [`PeerId`] of the local node. Never changes.
    local_peer_id: PeerId,

    /// Number of the best known block. Only updated by receiving events.
    best_block_number: u64,

    /// Hash of the best known block. Only updated by receiving events.
    best_block_hash: [u8; 32],

    /// Number of the latest finalized block. Only updated by receiving events.
    finalized_block_number: u64,

    /// Hash of the latest finalized block. Only updated by receiving events.
    finalized_block_hash: [u8; 32],
}

/// Event that happened on the service.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Event {
    /// Database state has been updated for the given block.
    NewChainHead {
        number: u64,
        hash: [u8; 32],
        head_update: ChainHeadUpdate,
        /// List of keys whose values has been added, removed, or has changed in the storage as a
        /// result of the chain head update.
        modified_keys: Vec<Vec<u8>>,
    },

    /// The finalized block has been updated to a different one.
    NewFinalized {
        /// Number of the finalized block.
        number: u64,
        /// Hash of the finalized block.
        hash: [u8; 32],
    },

    /// Received a block announce from the network.
    BlockAnnounceReceived {
        /// Block number.
        number: u64,
        /// Block hash.
        hash: [u8; 32],
    },

    /// Networking has detected a new external address.
    NewNetworkExternalAddress {
        /// The address in question. Contains a `/p2p/` suffix.
        address: network::Multiaddr,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChainHeadUpdate {
    NoUpdate,
    FastForward,
    Reorg,
}

impl Service {
    /// Returns the next event that happens in the service.
    pub async fn next_event(&mut self) -> Event {
        // The events channel is never closed unless the background tasks have all closed as well,
        // in which case it is totally appropriate to panic.
        let event = self.events_in.next().await.unwrap();

        self.num_network_connections = self.num_connections_store.load(atomic::Ordering::Relaxed);

        // Update the local state.
        match &event {
            Event::NewChainHead { number, hash, .. } => {
                self.best_block_number = *number;
                self.best_block_hash = (*hash).into();
            }
            Event::NewFinalized { number, hash } => {
                self.finalized_block_number = *number;
                self.finalized_block_hash = (*hash).into();
            }
            _ => {}
        }

        event
    }

    /// Returns the number of transport-level (e.g. TCP/IP) connections of the network. Only
    /// updated when calling [`Service::next_event`].
    pub fn num_network_connections(&self) -> u64 {
        self.num_network_connections
    }

    /// Returns the [`PeerId`] of the local node.
    pub fn local_peer_id(&self) -> &PeerId {
        &self.local_peer_id
    }

    /// Returns the number of the best known block. Only updated when calling
    /// [`Service::next_event`].
    pub fn best_block_number(&self) -> u64 {
        self.best_block_number
    }

    /// Returns the hash of the best known block. Only updated when calling
    /// [`Service::next_event`].
    pub fn best_block_hash(&self) -> [u8; 32] {
        self.best_block_hash
    }

    /// Returns the number of the latest finalized block. Only updated when calling
    /// [`Service::next_event`].
    pub fn finalized_block_number(&self) -> u64 {
        self.finalized_block_number
    }

    /// Returns the hash of the latest finalized block. Only updated when calling
    /// [`Service::next_event`].
    pub fn finalized_block_hash(&self) -> [u8; 32] {
        self.finalized_block_hash
    }

    // TODO: crap API
    pub async fn best_effort_block_hash(&self, num: u64) -> Option<[u8; 32]> {
        let (tx, rx) = oneshot::channel();

        // TODO: don't clone the channel, it reserves an extra slot
        self.to_database
            .clone()
            .send(database_task::ToDatabase::BlockHashGet {
                block_number: num,
                send_back: tx,
            })
            .await
            .unwrap();

        rx.await.unwrap()
    }

    // TODO: crap API
    pub async fn storage_keys(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        let (tx, rx) = oneshot::channel();

        // TODO: don't clone the channel, it reserves an extra slot
        self.to_database
            .clone()
            .send(database_task::ToDatabase::StorageKeys {
                block_hash: self.best_block_hash,
                prefix: prefix.to_owned(),
                send_back: tx,
            })
            .await
            .unwrap();

        rx.await.unwrap().unwrap()
    }

    // TODO: crap API
    pub async fn storage_get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let (tx, rx) = oneshot::channel();

        // TODO: don't clone the channel, it reserves an extra slot
        self.to_database
            .clone()
            .send(database_task::ToDatabase::StorageGet {
                block_hash: self.best_block_hash,
                key: key.to_owned(),
                send_back: tx,
            })
            .await
            .unwrap();

        rx.await.unwrap()
    }
}
