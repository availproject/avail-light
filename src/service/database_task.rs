//! The service uses one or more tasks dedicated to reading/writing the database. Any database
//! access is dispatched to this task.
//!
//! Any piece of code that requires access to the database is encouraged to maintain a cache in
//! order to avoid accessing it too often.

use crate::database;

use alloc::sync::Arc;
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use hashbrown::HashMap;

/// Message that can be sent to the database task by the other parts of the code.
pub enum ToDatabase {
    /// Loads the hash of a block, given its number.
    BlockHashGet {
        /// Which block to load the header of.
        block_number: u64,
        /// Channel to send back the result. Sends back `None` if the block isn't in the database.
        send_back: oneshot::Sender<Option<[u8; 32]>>,
    },
    /// Loads the body of a block.
    BlockBodyGet {
        /// Which block to load the header of.
        block: BlockNumberOrHash,
        /// Channel to send back the result. Sends back `None` if the block isn't in the database.
        send_back: oneshot::Sender<Option<Vec<Vec<u8>>>>,
    },
    /// Load a value from the storage of a block.
    StorageGet {
        /// Hash of the block whose state is to be queried.
        block_hash: [u8; 32],
        /// Which key to read.
        key: Vec<u8>,
        /// Channel to send back the result.
        send_back: oneshot::Sender<Option<Vec<u8>>>,
    },
    /// Returns the list of keys in the storage.
    StorageKeys {
        /// Hash of the block whose state is to be queried.
        block_hash: [u8; 32],
        /// Prefix of the keys. Only keys that start with this prefix will be
        /// returned.
        prefix: Vec<u8>,
        /// Channel to send back the result.
        /// Sends back `None` iff the block is unknown.
        send_back: oneshot::Sender<Option<Vec<Vec<u8>>>>,
    },
    /// Store a new block in the database.
    StoreBlock {
        /// Parent of the block to add to the database.
        parent_block_hash: [u8; 32],
        /// Hash of the block to add to the database.
        new_block_hash: [u8; 32],
        /// Changes to the storage top trie that this block performs.
        storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>>,
        // TODO: other fields
    },
}

/// Either a block number or a block hash.
#[derive(Debug)]
pub enum BlockNumberOrHash {
    Number(u32),
    Hash([u8; 32]),
}

/// Configuration for that task.
pub struct Config {
    /// Database to load data from.
    pub database: Arc<database::Database>,
    /// Receiver for messages that the database task will process.
    pub to_database: mpsc::Receiver<ToDatabase>,
}

/// Runs the task itself.
pub async fn run_database_task(mut config: Config) {
    while let Some(event) = config.to_database.next().await {
        // TODO: how to handle corrupted database?
        let _ = handle_single_event(event, &config.database);
    }
}

fn handle_single_event(
    event: ToDatabase,
    database: &database::Database,
) -> Result<(), database::AccessError> {
    match event {
        ToDatabase::BlockHashGet {
            block_number,
            send_back,
        } => {
            if send_back.is_canceled() {
                return Ok(());
            }

            let hash = database.block_hash_by_number(block_number).unwrap();
            let _ = send_back.send(hash);
        }

        ToDatabase::BlockBodyGet { block, send_back } => {
            if send_back.is_canceled() {
                return Ok(());
            }

            unimplemented!()
        }

        ToDatabase::StorageGet {
            block_hash,
            key,
            send_back,
        } => {
            if send_back.is_canceled() {
                return Ok(());
            }

            let value = database.storage_top_trie_get(block_hash, &key).unwrap();
            let _ = send_back.send(value.map(|v| v.to_vec()));
        }

        ToDatabase::StorageKeys {
            block_hash,
            prefix,
            send_back,
        } => {
            if send_back.is_canceled() {
                return Ok(());
            }

            // TODO: make the database query more precise
            // TODO: block isn't checked
            let mut all_keys = database.storage_top_trie_keys(block_hash).unwrap();
            all_keys.retain(|k| k.starts_with(&prefix));
            let _ = send_back.send(Some(all_keys.into_iter().map(|k| k.to_vec()).collect()));
        }

        ToDatabase::StoreBlock { .. } => unimplemented!(),
    }

    Ok(())
}
