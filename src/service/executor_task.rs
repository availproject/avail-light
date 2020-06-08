//! Service task that processes Wasm executions requests.

use crate::{block, executor, storage};

use core::{cmp, pin::Pin};
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use hashbrown::HashMap;
use primitive_types::H256;

/// Message that can be sent to the executors task by the other parts of the code.
pub enum ToExecutor {
    /// Call the runtime to apply a block on the state.
    Execute {
        /// Block to try execute.
        to_execute: block::Block,
        /// Channel where to send back the outcome of the execution.
        // TODO: better return type
        send_back: oneshot::Sender<Result<(), ()>>,
    },
}

/// Configuration for that task.
pub struct Config {
    /// Access to all the data of the blockchain.
    pub storage: storage::Storage,
    /// How to spawn other background tasks.
    pub tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
    /// Receiver for messages that the executor task will process.
    pub to_executor: mpsc::Receiver<ToExecutor>,
}

/// Runs the task itself.
pub async fn run_executor_task(mut config: Config) {
    while let Some(event) = config.to_executor.next().await {
        match event {
            ToExecutor::Execute {
                to_execute,
                send_back: _,
            } => {
                let parent = config
                    .storage
                    .block(&to_execute.header.parent_hash)
                    .storage()
                    .unwrap();
                let code = parent.code_key().unwrap();
                let wasm_blob = executor::WasmBlob::from_bytes(code).unwrap(); // TODO: have a cache of that

                let mut vm = executor::WasmVm::new(
                    &wasm_blob,
                    executor::FunctionToCall::CoreExecuteBlock(&to_execute),
                )
                .unwrap();

                let mut overlay_storage_changes = HashMap::<Vec<u8>, Option<Vec<u8>>>::new();

                loop {
                    match vm.state() {
                        executor::State::ReadyToRun(r) => r.run(),
                        executor::State::Finished(executor::Success::CoreExecuteBlock(_result)) => {
                            panic!("success")
                        }
                        executor::State::Finished(_) => unreachable!(),
                        executor::State::Trapped => panic!("trapped"),
                        executor::State::ExternalStorageGet {
                            storage_key,
                            offset,
                            max_size,
                            resolve,
                        } => {
                            // TODO: this clones the storage value, meh
                            // TODO: no, doesn't respect constraints
                            if let Some(overlay) = overlay_storage_changes.get(storage_key) {
                                println!(
                                    "Get {}={:?}",
                                    hex::encode(storage_key),
                                    overlay.as_ref().map(|v| hex::encode(v))
                                );
                                resolve.finish_call(overlay.clone());
                            } else {
                                let result = parent.get(&storage_key).map(|v| v.as_ref().to_vec());
                                println!(
                                    "Get {}={:?}",
                                    hex::encode(storage_key),
                                    result.as_ref().map(|v| hex::encode(v))
                                );
                                resolve.finish_call(result);
                            }
                        }
                        executor::State::ExternalStorageSet {
                            storage_key,
                            new_storage_value,
                            resolve,
                        } => {
                            println!(
                                "Put {}={:?}",
                                hex::encode(storage_key),
                                new_storage_value.as_ref().map(|v| hex::encode(v))
                            );
                            overlay_storage_changes.insert(
                                storage_key.to_vec(),
                                new_storage_value.map(|v| v.to_vec()),
                            );
                            resolve.finish_call(());
                        }
                        executor::State::ExternalStorageAppend {
                            storage_key,
                            value,
                            resolve,
                        } => {
                            let mut current_value =
                                if let Some(overlay) = overlay_storage_changes.get(storage_key) {
                                    overlay.clone().unwrap_or(Vec::new())
                                } else {
                                    parent
                                        .get(&storage_key)
                                        .map(|v| v.as_ref().to_vec())
                                        .unwrap_or(Vec::new())
                                };
                            let curr_len = <parity_scale_codec::Compact::<u64> as parity_scale_codec::Decode>::decode(&mut &current_value[..]);
                            let new_value = if let Ok(mut curr_len) = curr_len {
                                let len_size = <parity_scale_codec::Compact::<u64> as parity_scale_codec::CompactLen::<u64>>::compact_len(&curr_len.0);
                                curr_len.0 += 1;
                                let mut new_value = parity_scale_codec::Encode::encode(&curr_len);
                                new_value.extend_from_slice(&current_value[len_size..]);
                                new_value.extend_from_slice(value);
                                new_value
                            } else {
                                let mut new_value = parity_scale_codec::Encode::encode(
                                    &parity_scale_codec::Compact(1u64),
                                );
                                new_value.extend_from_slice(value);
                                new_value
                            };
                            overlay_storage_changes.insert(storage_key.to_vec(), Some(new_value));
                            resolve.finish_call(());
                        }
                        executor::State::ExternalStorageClearPrefix {
                            storage_key,
                            resolve,
                        } => {
                            for key in parent.storage_keys() {
                                let key = key.as_ref();
                                if !key.starts_with(&storage_key) {
                                    continue;
                                }
                                overlay_storage_changes.insert(key.to_vec(), None);
                            }
                            for (key, value) in overlay_storage_changes.iter_mut() {
                                if !key.starts_with(&storage_key) {
                                    continue;
                                }
                                *value = None;
                            }
                            resolve.finish_call(());
                        }
                        executor::State::ExternalStorageRoot { resolve } => {
                            let mut trie = crate::trie::Trie::new();
                            for key in parent.storage_keys() {
                                let value =
                                    parent.get(key.as_ref()).as_ref().unwrap().as_ref().to_vec();
                                trie.insert(key.as_ref(), value);
                            }
                            for (key, value) in overlay_storage_changes.iter() {
                                if let Some(value) = value.as_ref() {
                                    trie.insert(key, value.clone())
                                } else {
                                    trie.remove(key);
                                }
                            }
                            let hash = trie.root_merkle_value();
                            println!("Root {}", hex::encode(hash));
                            resolve.finish_call(hash.to_vec());
                        }
                        executor::State::ExternalStorageChangesRoot {
                            parent_hash,
                            resolve,
                        } => {
                            // TODO: this is probably one of the most complicated things to
                            // implement, but slava told me that it's ok to just return None on
                            // flaming fir because the feature is disabled
                            resolve.finish_call(None);
                        }
                        executor::State::ExternalStorageNextKey {
                            storage_key,
                            resolve,
                        } => {
                            // TODO: not optimized regarding cloning
                            let in_storage =
                                parent.next_key(&storage_key).map(|v| v.as_ref().to_vec());
                            let in_overlay = overlay_storage_changes
                                .keys()
                                .filter(|k| &***k > storage_key)
                                .min();
                            let outcome = match (in_storage, in_overlay) {
                                (Some(a), Some(b)) => Some(cmp::min(a, b.clone())),
                                (Some(a), None) => Some(a),
                                (None, Some(b)) => Some(b.clone()),
                                (None, None) => None,
                            };
                            println!(
                                "next key {} => {:?}",
                                hex::encode(storage_key),
                                outcome.as_ref().map(|v| hex::encode(v))
                            );
                            resolve.finish_call(outcome);
                        }
                        s => unimplemented!("unimplemented externality: {:?}", s),
                    }
                }

                unimplemented!("executor")
            }
        }
    }
}
