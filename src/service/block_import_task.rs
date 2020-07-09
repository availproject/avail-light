//! Service task that tries to import blocks from the network into the database.
//!
//! The role of the block import task is to verify and append blocks to the head of the chain
//! stored in the database passed through [`Config::database`].
//!
//! The block import task receives blocks from other parts of the code (most likely the network)
//! through [`ToBlockImport::Import`] messages, verifies if they are correct by executing them, and
//! if so appends them to the head of the chain. Only blocks whose parent is the current head of
//! the chain are considered, and the others discarded.

use crate::{block, block_import, database, executor, trie::calculate_root};

use alloc::{collections::BTreeMap, sync::Arc};
use core::pin::Pin;
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use parity_scale_codec::Encode as _;
use parking_lot::Mutex;

/// Message that can be sent to the block import task by the other parts of the code.
pub enum ToBlockImport {
    /// Ask the block import task what the best block number is.
    BestBlockNumber {
        /// Channel where to send back the answer.
        send_back: oneshot::Sender<u64>,
    },
    /// Verify the correctness of a block and apply it on the storage.
    Import {
        /// Block to try execute.
        to_execute: block::Block,
        /// Channel where to send back the outcome of the execution.
        send_back: oneshot::Sender<Result<ImportSuccess, ImportError>>,
    },
}

pub struct ImportSuccess {
    /// The block that was passed as parameter.
    // TODO: do we really need to pass it back?
    pub block: block::Block,
    /// List of keys that have appeared, disappeared, or whose value has been modified during the
    /// execution of the block.
    pub modified_keys: Vec<Vec<u8>>,
}

/// Error that can happen when importing a block.
#[derive(Debug, derive_more::Display)]
pub enum ImportError {
    /// The parent of the block isn't the current best block.
    #[display(fmt = "The parent of the block isn't the current best block.")]
    ParentIsntBest {
        /// Hash of the current best block.
        current_best_hash: [u8; 32],
    },
    /// The block verification has failed. The block is invalid and should be thrown away.
    VerificationFailed(block_import::Error),
}

/// Configuration for that task.
pub struct Config {
    /// Database where to import blocks to.
    pub database: Arc<database::Database>,
    /// How to spawn other background tasks.
    pub tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
    /// Receiver for messages that the executor task will process.
    pub to_block_import: mpsc::Receiver<ToBlockImport>,
}

/// Runs the task itself.
pub async fn run_block_import_task(mut config: Config) {
    // The `WasmBlob` object corresponding to the head of the chain. Set to `None` if the runtime
    // code is modified.
    // Used to avoid recompiling it every single time.
    let mut wasm_blob_cache: Option<executor::WasmVmPrototype> = None;

    // Cache used to calculate the storage trie root.
    // This cache has to be kept up to date with the actual state of the storage.
    // We pass this value whenever we verify a block. The verification process returns an updated
    // version of this cache, suitable to be passed to verifying a direct child.
    let mut top_trie_root_calculation_cache = Some(calculate_root::CalculationCache::empty());

    // Cache of the storage at the head of the chain.
    let mut local_storage_cache = {
        let mut cache = BTreeMap::<Vec<u8>, Vec<u8>>::new();
        let best_block = config.database.best_block_hash().unwrap();
        let storage_keys = config.database.storage_top_trie_keys(best_block).unwrap();
        for key in storage_keys {
            let value = config
                .database
                .storage_top_trie_get(best_block, &key)
                .unwrap()
                .unwrap();
            cache.insert(key.to_vec(), value.to_vec());
        }
        cache
    };

    // Main loop of the task. Processes received messages.
    while let Some(event) = config.to_block_import.next().await {
        match event {
            ToBlockImport::BestBlockNumber { send_back } => {
                if send_back.is_canceled() {
                    continue;
                }

                let num = config.database.best_block_number().unwrap();
                let _ = send_back.send(num);
            }

            ToBlockImport::Import {
                to_execute,
                send_back,
            } => {
                // We only accept blocks whose parent is the current best block.
                let current_best_hash = config.database.best_block_hash().unwrap();
                if current_best_hash != <[u8; 32]>::from(to_execute.header.parent_hash) {
                    let _ = send_back.send(Err(ImportError::ParentIsntBest { current_best_hash }));
                    continue;
                }

                // In order to avoid parsing/compiling the runtime code every single time, we
                // maintain a cache of the `WasmBlob` of the head of the chain.
                let runtime_wasm_blob = if let Some(vm) = wasm_blob_cache.take() {
                    vm
                } else {
                    let code = local_storage_cache.get(&b":code"[..]).unwrap();
                    executor::WasmVmPrototype::new(&code).unwrap()
                };

                // Now perform the actual block verification.
                // Note that this does **not** modify `local_storage_cache`.
                let import_result = {
                    // TODO: this mutex is stupid, the `crate::block_import` module should be reworked
                    // to be coroutine-like
                    let local_storage_cache = Arc::new(Mutex::new(&mut local_storage_cache));

                    block_import::verify_block(block_import::Config {
                        runtime: runtime_wasm_blob,
                        block_header: &to_execute.header,
                        block_body: &to_execute.extrinsics,
                        parent_storage_get: {
                            let local_storage_cache = local_storage_cache.clone();
                            move |key: Vec<u8>| {
                                let ret: Option<Vec<u8>> =
                                    local_storage_cache.lock().get(&key).cloned();
                                async move { ret }
                            }
                        },
                        parent_storage_keys_prefix: {
                            let local_storage_cache = local_storage_cache.clone();
                            move |prefix: Vec<u8>| {
                                let ret = local_storage_cache
                                    .lock()
                                    .range(prefix.clone()..)
                                    .take_while(|(k, _)| k.starts_with(&prefix))
                                    .map(|(k, _)| k.to_vec())
                                    .collect();
                                async move { ret }
                            }
                        },
                        parent_storage_next_key: {
                            let local_storage_cache = local_storage_cache.clone();
                            move |key: Vec<u8>| {
                                struct CustomBound(Vec<u8>);
                                impl core::ops::RangeBounds<Vec<u8>> for CustomBound {
                                    fn start_bound(&self) -> core::ops::Bound<&Vec<u8>> {
                                        core::ops::Bound::Excluded(&self.0)
                                    }
                                    fn end_bound(&self) -> core::ops::Bound<&Vec<u8>> {
                                        core::ops::Bound::Unbounded
                                    }
                                }
                                let ret = local_storage_cache
                                    .lock()
                                    .range(CustomBound(key))
                                    .next()
                                    .map(|(k, _)| k.to_vec());
                                async move { ret }
                            }
                        },
                        top_trie_root_calculation_cache: top_trie_root_calculation_cache.take(),
                    })
                    .await
                };

                // If the block verification failed, we can just discard everything as nothing
                // has been committed yet.
                let import_result = match import_result {
                    Ok(r) => r,
                    Err(err) => {
                        assert!(top_trie_root_calculation_cache.is_none());
                        let _ = send_back.send(Err(ImportError::VerificationFailed(err)));
                        continue;
                    }
                };

                // The block is correct. Now import it into the database.
                // TODO: it seems that importing the block takes less than 1ms, which
                // makes it ok in an asynchronous context, but eventually make sure that
                // this remains cheap even with big blocks
                let db_import_result = config.database.insert_new_best(
                    current_best_hash,
                    &to_execute.header.encode(),
                    to_execute.extrinsics.iter().map(|e| e.0.to_vec()),
                    import_result
                        .storage_top_trie_changes
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone())),
                );

                match db_import_result {
                    Ok(()) => {}
                    Err(database::InsertNewBestError::ObsoleteCurrentHead) => {
                        // We have already checked above whether the parent of the block to import
                        // was indeed the best block in the database. However the import can still
                        // fail if something else has modified the database's best block while we
                        // were busy verifying the block.
                        let _ =
                            send_back.send(Err(ImportError::ParentIsntBest { current_best_hash }));
                        continue;
                    }
                    Err(database::InsertNewBestError::Access(err)) => {
                        panic!("Database internal error: {}", err);
                    }
                }

                // Block has been successfully imported! 🎉
                let _ = send_back.send(Ok(ImportSuccess {
                    block: to_execute,
                    modified_keys: import_result
                        .storage_top_trie_changes
                        .keys()
                        .cloned()
                        .collect(),
                }));

                // We now have to update the local values for the next iteration.
                // Put back the same runtime `wasm_blob_cache` unless changes have been made
                // to `:code`.
                top_trie_root_calculation_cache =
                    Some(import_result.top_trie_root_calculation_cache);
                if !import_result
                    .storage_top_trie_changes
                    .contains_key(&b":code"[..])
                {
                    wasm_blob_cache = Some(import_result.parent_runtime);
                }
                for (key, value) in import_result.storage_top_trie_changes {
                    if let Some(value) = value {
                        local_storage_cache.insert(key, value);
                    } else {
                        local_storage_cache.remove(&key);
                    }
                }
            }
        }
    }
}
