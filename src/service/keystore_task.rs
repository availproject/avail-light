//! Service task that processes keystore signing and verification requests.

use crate::keystore;

use core::pin::Pin;
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use primitive_types::{H256, U256};

/// Message that can be sent to the keystore task by the other parts of the code.
pub enum ToKeystore {
    // TODO: sign, validate, ...
}

/// Configuration for that task.
pub struct Config {
    /// The collection of keys.
    pub keystore: keystore::Keystore,
    /// Receiver for messages that the keystore task will process.
    pub to_keystore: mpsc::Receiver<ToKeystore>,
}

/// Runs the task itself.
pub async fn run_keystore_task(mut config: Config) {
    while let Some(event) = config.to_keystore.next().await {}
}
