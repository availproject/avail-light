//! Service task that tries to import blocks from the network into the database.

use crate::{block, database, executor, trie::calculate_root};

use alloc::{collections::BTreeMap, sync::Arc};
use core::pin::Pin;
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use hashbrown::HashMap;
use parity_scale_codec::Encode as _;
use parking_lot::Mutex;

/// Message that can be sent to the block import task by the other parts of the code.
pub enum ToBlockImport {
    /// Ask the block import task what the best block number is.
    BestBlockNumber {
        /// Channel where to send back the answer.
        send_back: oneshot::Sender<u64>,
    },
    /// Call the runtime to apply a block on the state.
    Import {
        /// Block to try execute.
        to_execute: block::Block,
        /// Channel where to send back the outcome of the execution.
        // TODO: better return type
        send_back: oneshot::Sender<Result<ImportSuccess, ()>>,
    },
}

pub struct ImportSuccess {
    /// The block that was passed as parameter.
    pub block: block::Block,
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
    // Tuple of the runtime code of the chain head and its corresponding `WasmBlob`.
    // Used to avoid recompiling it every single time.
    let mut wasm_blob_cache: Option<(Vec<u8>, executor::WasmBlob)> = None;

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
            cache.insert(key.as_ref().to_vec(), value.as_ref().to_vec());
        }
        cache
    };

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
                //  TODO:
                if config.database.best_block_hash().unwrap()
                    != <[u8; 32]>::from(to_execute.header.parent_hash)
                {
                    unimplemented!("import a block whose parent isn't the best block isn't supported: {:?} vs {:?}", config.database.best_block_hash().unwrap(), to_execute.header.parent_hash);
                }

                // In order to avoid parsing/compiling the runtime code every single time, we
                // maintain a cache of the `WasmBlob` of the head of the chain.
                let runtime_wasm_blob = {
                    let code = local_storage_cache.get(&b":code"[..]).unwrap();
                    if wasm_blob_cache
                        .as_ref()
                        .map(|(c, _)| *c != *code)
                        .unwrap_or(true)
                    {
                        let wasm_blob = executor::WasmBlob::from_bytes(&code).unwrap();
                        wasm_blob_cache = Some((code.to_vec(), wasm_blob));
                    }
                    &wasm_blob_cache.as_ref().unwrap().1
                };

                // TODO: this mutex is stupid, the `crate::block_import` module should be reworked
                // to be coroutine-like
                let import_result = {
                    let local_storage_cache = Arc::new(Mutex::new(&mut local_storage_cache));

                    crate::block_import::verify_block(crate::block_import::Config {
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

                match import_result {
                    Ok(success) => {
                        top_trie_root_calculation_cache =
                            Some(success.top_trie_root_calculation_cache);

                        // Invalidate the `wasm_blob_cache` if some changes have been made
                        // to `:code`.
                        if success.storage_top_trie_changes.contains_key(&b":code"[..]) {
                            wasm_blob_cache = None;
                        }

                        // TODO: it seems that importing the block takes less than 1ms, which
                        // makes it ok in an asynchronous context, but eventually make sure that
                        // this remains cheap even with big blocks
                        // TODO: handle the `ObsoleteBestBlock` database error
                        config
                            .database
                            .insert_new_best(
                                to_execute.header.parent_hash.into(),
                                &to_execute.header.encode(),
                                to_execute.extrinsics.iter().map(|e| e.0.to_vec()),
                                success
                                    .storage_top_trie_changes
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone())),
                            )
                            .unwrap();

                        for (key, value) in success.storage_top_trie_changes {
                            if let Some(value) = value {
                                local_storage_cache.insert(key, value);
                            } else {
                                local_storage_cache.remove(&key);
                            }
                        }

                        let _ = send_back.send(Ok(ImportSuccess { block: to_execute }));
                    }
                    Err(_) => panic!(), // TODO:
                }
            }
        }
    }
}
