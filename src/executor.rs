// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

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
//! At any given point, you can examine the [`WasmVm`] in order to know in which state the
//! execution currently is.
//! In case of a [`WasmVm::ReadyToRun`] (which initially is the case when you create the
//! [`WasmVm`]), you can execute the Wasm code by calling [`ReadyToRun::run`].
//! No background thread of any kind is used, and calling [`ReadyToRun::run`] directly performs
//! the execution of the Wasm code. If you need parallelism, you are encouraged to spawn a
//! background thread yourself and call this function from there.
//! [`ReadyToRun::run`] tries to make the execution progress as much as possible, and returns
//! the new state of the virtual machine once that is done.
//!
//! If the runtime has finished, or has crashed, or wants to perform an operation with side
//! effects, then the [`WasmVm`] determines what to do next. For example, for
//! [`WasmVm::ExternalStorageGet`], you must load a value from the storage and pass it back by
//! calling [`ExternalStorageGet::resume`].
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
//! let mut vm: substrate_lite::executor::WasmVm = {
//!     let prototype = substrate_lite::executor::WasmVmPrototype::new(
//!         &wasm_binary,
//!         1024,
//!         substrate_lite::executor::vm::ExecHint::Oneshot
//!     ).unwrap();
//!     prototype.run_no_param("Core_version").unwrap().into()
//! };
//!
//! // We need to answer the calls that the runtime might perform.
//! loop {
//!     match vm {
//!         // Calling `runner.run()` is what actually executes WebAssembly code and updates
//!         // the state.
//!         substrate_lite::executor::WasmVm::ReadyToRun(runner) => vm = runner.run(),
//!
//!         substrate_lite::executor::WasmVm::Finished(finished) => {
//!             // `finished.value()` here is an opaque blob of bytes returned by the runtime.
//!             // In the case of a call to `"Core_version"`, we know that it must be empty.
//!             assert!(finished.value().is_empty());
//!             println!("Success!");
//!             break;
//!         },
//!
//!         // Errors can happen if the WebAssembly code panics or does something wrong.
//!         // In a real-life situation, the host should obviously not panic in these situations.
//!         substrate_lite::executor::WasmVm::Error { .. } => {
//!             panic!("Error while executing code")
//!         },
//!
//!         // All the other variants correspond to function calls that the runtime might perform.
//!         // `ExternalStorageGet` is shown here as an example.
//!         substrate_lite::executor::WasmVm::ExternalStorageGet(req) => {
//!             println!("Runtime wants to read the storage at {:?}", req.key());
//!             // Injects the value into the virtual machine and updates the state.
//!             vm = req.resume(None); // Just a stub
//!         }
//!         _ => unimplemented!()
//!     }
//! }
//! ```
// TODO: use an actual Wasm blob extracted from somewhere as an example ^

use alloc::{string::String, vec::Vec};
use parity_scale_codec::DecodeAll as _;

mod allocator; // TODO: make public after refactoring
pub mod host;
pub mod runtime_host;
pub mod vm;

pub use host::{
    Error, ExternalStorageAppend, ExternalStorageGet, Finished, HostVm as WasmVm,
    HostVmPrototype as WasmVmPrototype, NewErr, ReadyToRun,
};
// TODO: reexports ^ ? shouldn't we just make the module public?

/// Runs the `Core_version` function using the given virtual machine prototype, and returns
/// the output.
///
/// All externalities are forbidden.
// TODO: proper error
pub fn core_version(vm_proto: WasmVmPrototype) -> Result<(CoreVersion, WasmVmPrototype), ()> {
    // TODO: is there maybe a better way to handle that?
    let mut vm: WasmVm = vm_proto
        .run_no_param("Core_version")
        .map_err(|_| ())?
        .into();

    loop {
        match vm {
            WasmVm::ReadyToRun(r) => vm = r.run(),
            WasmVm::Finished(finished) => {
                let decoded = CoreVersion::decode_all(&finished.value()).map_err(|_| ())?;
                return Ok((decoded, finished.into_prototype()));
            }
            WasmVm::Error { .. } => return Err(()),

            // Since there are potential ambiguities we don't allow any storage access
            // or anything similar. The last thing we want is to have an infinite
            // recursion of runtime calls.
            _ => return Err(()),
        }
    }
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
