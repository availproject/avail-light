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

//! Retrieving the metadata from the runtime.
//!
//! The metadata can be obtained by calling the `Metadata_metadata` entry point of the runtime.
//! The runtime normally straight-up outputs some hardcoded structures, and no access to the
//! storage (or any other host function) is necessary.
//!
//! # About the length prefix
//!
//! The Wasm runtime returns the metadata prefixed with a SCALE-compact-encoded length. The
//! functions in this module remove this length prefix before returning the value.
//!
//! While it would be more flexible to return the raw result and let the user remove the prefix
//! if desired, the presence of this prefix is clearly the result of a mistake during the
//! development process that now has to be maintained in order to preserve backwards
//! compatibility.
//!
//! A lot of documentation concerning the metadata is available on the Internet, and several
//! projects are capable of parsing the metadata, and what is referred to as "the metadata"
//! in this documentation and projects systematically describes the metadata *without* any length
//! prefix. It would be confusing and error-prone if the value returned here obeyed a slightly
//! different definition.
//!

use crate::executor::{host, vm};

use alloc::vec::Vec;

/// Retrieves the SCALE-encoded metadata from the runtime code of a block.
///
/// > **Note**: This function is a convenient shortcut for
/// >           [`metadata_from_virtual_machine_prototype`]. In performance-critical situations,
/// >           where the overhead of the Wasm compilation is undesirable, you are encouraged to
/// >           call [`metadata_from_virtual_machine_prototype`] instead.
// TODO: document heap_pages
pub fn metadata_from_runtime_code(wasm_code: &[u8], heap_pages: u64) -> Result<Vec<u8>, Error> {
    let vm = host::HostVmPrototype::new(&wasm_code, heap_pages, vm::ExecHint::Oneshot)
        .map_err(Error::VmInitialization)?;
    let (out, _vm) = metadata_from_virtual_machine_prototype(vm)?;
    Ok(out)
}

/// Retrieves the SCALE-encoded metadata from the given virtual machine prototype.
///
/// Returns back the same virtual machine prototype as was passed as parameter.
pub fn metadata_from_virtual_machine_prototype(
    vm: host::HostVmPrototype,
) -> Result<(Vec<u8>, host::HostVmPrototype), Error> {
    let mut vm: host::HostVm = vm
        .run_no_param("Metadata_metadata")
        .map_err(Error::VmStart)?
        .into();

    loop {
        match vm {
            host::HostVm::ReadyToRun(r) => vm = r.run(),
            host::HostVm::Finished(finished) => {
                let value = remove_length_prefix(finished.value())?.to_owned();
                return Ok((value, finished.into_prototype()));
            }
            host::HostVm::Error { .. } => return Err(Error::Trapped),
            host::HostVm::LogEmit(rq) => vm = rq.resume(),

            // Querying the metadata shouldn't require any extrinsic such as accessing the
            // storage.
            _ => return Err(Error::HostFunctionNotAllowed),
        }
    }
}

/// Error when retrieving the metadata.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error when initializing the virtual machine.
    VmInitialization(host::NewErr),
    /// Error when starting the virtual machine.
    VmStart(host::StartErr),
    /// Crash while running the virtual machine.
    Trapped,
    /// Virtual machine tried to call a host function that isn't valid in this context.
    HostFunctionNotAllowed,
    /// Length prefix doesn't match actual length of the metadata.
    BadLengthPrefix,
}

/// Removes the length prefix at the beginning of `metadata`. Returns an error if there is no
/// valid length prefix.
fn remove_length_prefix(metadata: &[u8]) -> Result<&[u8], Error> {
    let (after_prefix, length) = crate::util::nom_scale_compact_usize(metadata)
        .map_err(|_: nom::Err<nom::error::Error<&[u8]>>| Error::BadLengthPrefix)?;

    // Verify that the length prefix indeed matches the metadata's length.
    if length != after_prefix.len() {
        return Err(Error::BadLengthPrefix);
    }

    Ok(after_prefix)
}
