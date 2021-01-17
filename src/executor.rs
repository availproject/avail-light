// Smoldot
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
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

//! WebAssembly runtime code execution.
//!
//! WebAssembly (often abbreviated *Wasm*) plays a big role in Substrate/Polkadot. The storage of
//! each block in the chain has a special key named `:code` which contains the WebAssembly code
//! of what we call *the runtime*.
//!
//! The runtime is a program (in WebAssembly) that decides, amongst other things, whether
//! transactions are valid and how to apply them on the storage, and whether blocks themselves are
//! valid.
//!
//! This module contains everything necessary to execute runtime code. The highest-level
//! sub-module is [`runtime_host`].

use alloc::{string::String, vec::Vec};
use parity_scale_codec::DecodeAll as _;

mod allocator; // TODO: make public after refactoring
pub mod host;
pub mod runtime_host;
pub mod vm;

/// Default number of heap pages if the storage doesn't specify otherwise.
///
/// # Context
///
/// In order to initialize a [`host::HostVmPrototype`], one needs to pass a certain number of
/// heap pages that are available to the runtime.
///
/// This number is normally found in the storage, at the key `:heappages`. But if it is not
/// specified, then the value of this constant must be used.
pub const DEFAULT_HEAP_PAGES: u64 = 1024;

/// Runs the `Core_version` function using the given virtual machine prototype, and returns
/// the output.
///
/// All externalities are forbidden.
// TODO: proper error
pub fn core_version(
    vm_proto: host::HostVmPrototype,
) -> Result<(CoreVersion, host::HostVmPrototype), ()> {
    // TODO: is there maybe a better way to handle that?
    let mut vm: host::HostVm = vm_proto
        .run_no_param("Core_version")
        .map_err(|_| ())?
        .into();

    loop {
        match vm {
            host::HostVm::ReadyToRun(r) => vm = r.run(),
            host::HostVm::Finished(finished) => {
                let decoded = CoreVersion::decode_all(&finished.value()).map_err(|_| ())?;
                return Ok((decoded, finished.into_prototype()));
            }
            host::HostVm::Error { .. } => return Err(()),

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
