// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Wasm runtime code execution.
//!
//! WebAssembly (often abbreviated *Wasm*) plays a big role in Substrate/Polkadot. The storage of
//! each block in the chain has a special key named `:code` which contains the WebAssembly code
//! of what we call *the runtime*.
//!
//! The runtime is a program (in WebAssembly) that decides, amongst other things, whether
//! transactions are valid and how to apply them on the storage.
//!
//! This module contains everything necessary to execute the runtime.
//!
//! # Usage
//!
//! The first step is to create a [`WasmVmPrototype`] object from the WebAssembly code. Creating
//! this object performs some initial steps, such as parsing and compiling the WebAssembly code.
//! You are encouraged to maintain a cache of [`WasmVmPrototype`] objects (one instance per
//! WebAssembly byte code) in order to avoid performing these operations too often.
//!
//! To start calling the runtime, create a [`WasmVm`] by calling [`WasmVmPrototype::run`].
//!
//! While the Wasm runtime code has side-effects (such as storing values in the storage), the
//! [`WasmVm`] itself is a pure state machine with no side effects.
//!
//! At any given point, you can call [`WasmVm::state`] in order to know in which state the
//! execution currently is.
//! If [`State::ReadyToRun`] is returned (which initially is the case when you create the
//! [`WasmVm`]), then you can execute the Wasm code by calling [`ReadyToRun::run`].
//! No background thread of any kind is used, and calling [`ReadyToRun::run`] directly performs
//! the execution of the Wasm code. If you need parallelism, you are encouraged to spawn a
//! background thread yourself and call this function from there.
//!
//! If the runtime has finished, or has crashed, or wants to perform an operation with side
//! effects, then [`ReadyToRun::run`] will return and you must call [`WasmVm::state`] again to
//! determine what to do next. For example, if [`State::ExternalStorageGet`] is returned, then you
//! must load a value from the storage and pass it back by calling [`Resume::finish_call`].
//!
//! The Wasm execution is fully deterministic, and the outcome of the execution only depends on
//! the inputs. There is, for example, no implicit injection of randomness or of the current time.
//!
//! # Example
//!
//! ```no_run
//! let wasm_binary: &[u8] = unimplemented!();
//!
//! // Start executing a function on the runtime.
//! let vm = substrate_lite::executor::WasmVmPrototype::new(&wasm_binary, 1024).unwrap()
//!     .run_no_param("Core_version").unwrap();
//!
//! // We need to answer the calls that the runtime might perform.
//! loop {
//!     match vm.state() {
//!         // Calling `runner.run()` is what actually executes WebAssembly code and updates
//!         // the state.
//!         substrate_lite::executor::State::ReadyToRun(runner) => runner.run(),
//!
//!         substrate_lite::executor::State::Finished(value) => {
//!             // `value` here is an opaque blob of bytes returned by the runtime.
//!             // In the case of a call to `"Core_version"`, we know that it must be empty.
//!             assert!(value.is_empty());
//!             println!("Success!");
//!             break;
//!         },
//!
//!         // Errors can happen if the WebAssembly code panics or does something wrong.
//!         // In a real-life situation, the host should obviously not panic in these situations.
//!         substrate_lite::executor::State::NonConforming(_) |
//!         substrate_lite::executor::State::Trapped => panic!("Error while executing code"),
//!
//!         // All the other variants correspond to function calls that the runtime might perform.
//!         // `ExternalStorageGet` is shown here as an example.
//!         substrate_lite::executor::State::ExternalStorageGet { storage_key, resolve, .. } => {
//!             println!("Runtime wants to read the storage at {:?}", storage_key);
//!             // Injects the value into the virtual machine and updates the state.
//!             resolve.finish_call(None); // Just a stub
//!         }
//!         _ => unimplemented!()
//!     }
//! }
//! ```
// TODO: use an actual Wasm blob extracted from somewhere as an example ^

use parity_scale_codec::DecodeAll as _;

mod allocator;
mod externals;
mod vm;

pub use externals::{
    ExternalsVm as WasmVm, ExternalsVmPrototype as WasmVmPrototype, NewErr, NonConformingErr,
    ReadyToRun, Resume, State,
};

/// Runs the `Core_version` function using the given virtual machine prototype, and returns
/// the output.
///
/// All externalities are forbidden.
// TODO: proper error
pub fn core_version(vm_proto: WasmVmPrototype) -> Result<(CoreVersion, WasmVmPrototype), ()> {
    // TODO: is there maybe a better way to handle that?
    let mut vm = vm_proto.run_no_param("Core_version").map_err(|_| ())?;

    let core_version = loop {
        match vm.state() {
            State::ReadyToRun(r) => r.run(),
            State::Finished(data) => {
                let decoded = CoreVersion::decode_all(&data).map_err(|_| ())?;
                break decoded;
            }
            State::Trapped => return Err(()),

            // Since there are potential ambiguities we don't allow any storage access
            // or anything similar. The last thing we want is to have an infinite
            // recursion of runtime calls.
            _ => return Err(()),
        }
    };

    Ok((core_version, vm.into_prototype()))
}

/// Structure that the `CoreVersion` function returns.
// TODO: don't expose Encode/Decode trait impls
#[derive(Debug, Clone, PartialEq, Eq, parity_scale_codec::Encode, parity_scale_codec::Decode)]
pub struct CoreVersion {
    pub spec_name: String,
    pub impl_name: String,
    pub authoring_version: u32,
    pub spec_version: u32,
    pub impl_version: u32,
    // TODO: stronger typing
    pub apis: Vec<([u8; 8], u32)>,
    pub transaction_version: u32,
}
