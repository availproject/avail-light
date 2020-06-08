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

/// Message that can be sent to the database task by the other parts of the code.
pub enum ToDatabase {
    /// Loads the SCALE-encoded header of a block.
    BlockScaleEncodedHeaderGet {
        /// Which block to load the header of.
        block_number: u32,
        /// Channel to send back the result. Sends back `None` if the block isn't in the database.
        send_back: oneshot::Sender<Option<Vec<u8>>>,
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
        ToDatabase::BlockScaleEncodedHeaderGet {
            block_number,
            send_back,
        } => {
            if send_back.is_canceled() {
                return Ok(());
            }

            let value = match database.block_by_number(block_number)? {
                Some(b) => Some(b.scale_encoded_header()?),
                None => None,
            };

            let _ = send_back.send(value);
        }

        ToDatabase::BlockBodyGet { block, send_back } => {
            if send_back.is_canceled() {
                return Ok(());
            }

            let block = match block {
                BlockNumberOrHash::Hash(hash) => database.block_by_hash(&hash),
                BlockNumberOrHash::Number(number) => database.block_by_number(number),
            };

            let value = match block? {
                Some(b) => Some(b.body()?),
                None => None,
            };

            let _ = send_back.send(value);
        }

        ToDatabase::StorageGet {
            block_hash,
            key,
            send_back,
        } => {
            if send_back.is_canceled() {
                return Ok(());
            }

            unimplemented!()
        }
    }

    Ok(())
}
