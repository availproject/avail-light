//! Service task that processes Wasm executions requests.

use crate::{block, executor, storage};

use core::pin::Pin;
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};

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

                {
                    let mut state = vm.state();
                    loop {
                        match state {
                            executor::State::ReadyToRun(r) => state = r.run(),
                            executor::State::Finished(executor::Success::CoreExecuteBlock(
                                _result,
                            )) => panic!("success"),
                            executor::State::Finished(_) => unreachable!(),
                            executor::State::ExternalStorageGet {
                                storage_key,
                                resolve,
                            } => {
                                // TODO: this clones the storage value, meh
                                state = resolve
                                    .finish_call(
                                        parent.get(&storage_key).map(|v| v.as_ref().to_vec()),
                                    )
                                    .run();
                            }
                            executor::State::ExternalStorageSet {
                                storage_key: _,
                                new_storage_value: _,
                                resolve,
                            } => {
                                // TODO: implement
                                state = resolve.finish_call(()).run();
                            }
                            executor::State::ExternalStorageClear {
                                storage_key: _,
                                resolve,
                            } => {
                                // TODO: implement
                                state = resolve.finish_call(()).run();
                            }
                            _ => unimplemented!(),
                        }
                    }
                }

                unimplemented!("executor")
            }
        }
    }
}
