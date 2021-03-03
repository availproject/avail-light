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
//! The runtime normally straight-up outputs some hardcoded structures.
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
//! # About storage accesses
//!
//! Generating the metadata might require access to the storage.
//! When that is the case, there is a chance that the content of the metadata depends on the
//! storage values that have been retrieved. Consequently, if these storage values change from one
//! block to the next, the metadata might change as well.
//!
//! In a perfect world, one would watch for changes in the storage keys that the metadata
//! generation accesses. At the time of the writing of this comment, however, runtimes only access
//! the storage during the metadata generation as hacks to circumvent limitations in the Rust
//! programming language used to write the runtime. It isn't known of any runtime that uses a
//! storage value during the generation of the metadata and that also potentially modifies these
//! same storage values.
//!
//! For this reason, it is considered as totally acceptable to not implement watching for changes
//! in these storage values, and consider that the metadata can only change after a modification
//! of the runtime itself.
//!

use crate::executor::{host, read_only_runtime_host};

use alloc::{borrow::ToOwned as _, vec::Vec};

/// Retrieves the SCALE-encoded metadata from the given virtual machine prototype.
///
/// Returns back the same virtual machine prototype as was passed as parameter.
pub fn query_metadata(virtual_machine: host::HostVmPrototype) -> Query {
    let vm = read_only_runtime_host::run(read_only_runtime_host::Config {
        virtual_machine,
        function_to_call: "Metadata_metadata",
        // The epoch functions don't take any parameters.
        parameter: core::iter::empty::<&[u8]>(),
    });

    match vm {
        Ok(vm) => Query::from_inner(vm),
        Err((err, proto)) => Query::Finished(Err(Error::VmStart(err, proto))),
    }
}

/// Current state of the operation.
#[must_use]
pub enum Query {
    /// Fetching the metadata is over.
    Finished(Result<(Vec<u8>, host::HostVmPrototype), Error>),
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet),
}

impl Query {
    fn from_inner(inner: read_only_runtime_host::RuntimeHostVm) -> Self {
        match inner {
            read_only_runtime_host::RuntimeHostVm::Finished(Ok(success)) => {
                let value =
                    match remove_metadata_length_prefix(success.virtual_machine.value().as_ref()) {
                        Ok(value) => value.to_owned(),
                        Err(err) => return Query::Finished(Err(Error::BadLengthPrefix(err))),
                    };

                Query::Finished(Ok((value, success.virtual_machine.into_prototype())))
            }
            read_only_runtime_host::RuntimeHostVm::Finished(Err(err)) => {
                Query::Finished(Err(Error::WasmRun(err)))
            }
            read_only_runtime_host::RuntimeHostVm::StorageGet(inner) => {
                Query::StorageGet(StorageGet(inner))
            }
            read_only_runtime_host::RuntimeHostVm::NextKey(_) => {
                Query::Finished(Err(Error::HostFunctionNotAllowed))
            }
            read_only_runtime_host::RuntimeHostVm::StorageRoot(_) => {
                Query::Finished(Err(Error::HostFunctionNotAllowed))
            }
        }
    }
}

/// Error when retrieving the metadata.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error when initializing the virtual machine.
    VmInitialization(host::NewErr),
    /// Error when starting the virtual machine.
    #[display(fmt = "{}", _0)]
    VmStart(host::StartErr, host::HostVmPrototype),
    /// Error while running the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    WasmRun(read_only_runtime_host::Error),
    /// Virtual machine tried to call a host function that isn't valid in this context.
    HostFunctionNotAllowed,
    /// Length prefix doesn't match actual length of the metadata.
    BadLengthPrefix(RemoveMetadataLengthPrefixError),
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet(read_only_runtime_host::StorageGet);

impl StorageGet {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    pub fn key(&'_ self) -> impl Iterator<Item = impl AsRef<[u8]> + '_> + '_ {
        self.0.key()
    }

    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    ///
    /// This method is a shortcut for calling `key` and concatenating the returned slices.
    pub fn key_as_vec(&self) -> Vec<u8> {
        self.0.key_as_vec()
    }

    /// Injects the corresponding storage value.
    pub fn inject_value(self, value: Option<impl Iterator<Item = impl AsRef<[u8]>>>) -> Query {
        Query::from_inner(self.0.inject_value(value))
    }
}

/// Removes the length prefix at the beginning of `metadata`. Returns an error if there is no
/// valid length prefix.
///
/// See the module-level documentation for more information.
pub fn remove_metadata_length_prefix(
    metadata: &[u8],
) -> Result<&[u8], RemoveMetadataLengthPrefixError> {
    let (after_prefix, length) = crate::util::nom_scale_compact_usize(metadata)
        .map_err(|_: nom::Err<nom::error::Error<&[u8]>>| RemoveMetadataLengthPrefixError)?;

    // Verify that the length prefix indeed matches the metadata's length.
    if length != after_prefix.len() {
        return Err(RemoveMetadataLengthPrefixError);
    }

    Ok(after_prefix)
}

/// Potential error when calling [`remove_metadata_length_prefix`].
#[derive(Debug, derive_more::Display)]
#[display(fmt = "No valid prefix in front of metadata")]
pub struct RemoveMetadataLengthPrefixError;
