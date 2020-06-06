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
                                resolve.finish_call(overlay.clone());
                            } else {
                                resolve.finish_call(
                                    parent.get(&storage_key).map(|v| v.as_ref().to_vec()),
                                );
                            }
                        }
                        executor::State::ExternalStorageSet {
                            storage_key,
                            new_storage_value,
                            resolve,
                        } => {
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
                            current_value.extend_from_slice(value);
                            overlay_storage_changes
                                .insert(storage_key.to_vec(), Some(current_value));
                            // TODO: implement properly?
                            resolve.finish_call(());
                        }
                        executor::State::ExternalStorageClearPrefix {
                            storage_key: _,
                            resolve,
                        } => {
                            // TODO: implement
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
                            resolve.finish_call(H256::from(hash));
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
