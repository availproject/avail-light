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

//! Unsealed block execution.
//!
//! The [`execute_block`] verifies the validity of a block header and body by *executing* the
//! block. Executing the block consists in running the `Core_execute_block` function of the
//! runtime, passing as parameter the header and the body of the block. The runtime function is
//! then tasked with verifying the validity of its parameters and calling the host functions
//! that modify the state of the storage.
//!
//! The header passed to the runtime must not contain a seal.
//!
//! Executing the block does **not** verify the validity of the consensus-related aspects of the
//! block header. The runtime blindly assumes that the author of the block had indeed the rights
//! to craft the block.
//!
//! # Usage
//!
//! Calling [`execute_block`] returns a [`Verify`] enum containing the state of the verification.
//!
//! If the [`Verify`] is a [`Verify::Finished`], then the verification is over and the result can
//! be retrieved.
//! Otherwise, the verification process requires an information from the storage of the parent
//! block in order to continue.
//!

use crate::{
    executor::{host, runtime_host},
    header,
    trie::calculate_root,
    util,
};

use alloc::{string::String, vec::Vec};
use core::iter;
use hashbrown::HashMap;

/// Configuration for an unsealed block verification.
pub struct Config<'a, TBody> {
    /// Runtime used to check the new block. Must be built using the Wasm code found at the
    /// `:code` key of the parent block storage.
    pub parent_runtime: host::HostVmPrototype,

    /// Header of the block to verify, in SCALE encoding.
    ///
    /// The `parent_hash` field is the hash of the parent whose storage can be accessed through
    /// the other fields.
    ///
    /// Block headers typically contain a `Seal` item as their last digest log item. When calling
    /// the [`execute_block`] function, this header must **not** contain any `Seal` item.
    pub block_header: header::HeaderRef<'a>,

    /// Body of the block to verify.
    pub block_body: TBody,

    /// Optional cache corresponding to the storage trie root hash calculation.
    pub top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,
}

/// Block successfully verified.
pub struct Success {
    /// Runtime that was passed by [`Config`].
    pub parent_runtime: host::HostVmPrototype,
    /// List of changes to the storage top trie that the block performs.
    pub storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,
    /// List of changes to the offchain storage that this block performs.
    pub offchain_storage_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,
    /// Cache used for calculating the top trie root.
    pub top_trie_root_calculation_cache: calculate_root::CalculationCache,
    /// Concatenation of all the log messages printed by the runtime.
    pub logs: String,
}

/// Error that can happen during the verification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error while starting the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    WasmStart(host::StartErr),
    /// Error while running the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    WasmVm(runtime_host::ErrorDetail),
    /// Output of `Core_execute_block` wasn't empty.
    NonEmptyOutput,
}

/// Verifies whether a block is valid.
pub fn execute_block(
    config: Config<impl ExactSizeIterator<Item = impl AsRef<[u8]> + Clone> + Clone>,
) -> Verify {
    let vm = runtime_host::run(runtime_host::Config {
        virtual_machine: config.parent_runtime,
        function_to_call: "Core_execute_block",
        parameter: {
            // The `Code_execute_block` function expects a SCALE-encoded `(header, body)`
            // where `body` is a `Vec<Vec<u8>>`. We perform the encoding manually to avoid
            // performing redundant data copies.
            let encoded_body_len = util::encode_scale_compact_usize(config.block_body.len());
            let body = config.block_body.flat_map(|ext| {
                let encoded_ext_len = util::encode_scale_compact_usize(ext.as_ref().len());
                iter::once(either::Left(encoded_ext_len)).chain(iter::once(either::Right(ext)))
            });

            config
                .block_header
                .scale_encoding()
                .map(|b| either::Right(either::Left(b)))
                .chain(iter::once(either::Right(either::Right(encoded_body_len))))
                .chain(body.map(either::Left))
        },
        top_trie_root_calculation_cache: config.top_trie_root_calculation_cache,
        storage_top_trie_changes: Default::default(),
        offchain_storage_changes: Default::default(),
    });

    match vm {
        Ok(vm) => Verify::from_inner(vm),
        Err((error, prototype)) => Verify::Finished(Err((Error::WasmStart(error), prototype))),
    }
}

/// Current state of the verification.
#[must_use]
pub enum Verify {
    /// Verification is over.
    Finished(Result<Success, (Error, host::HostVmPrototype)>),
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet),
    /// Fetching the list of keys with a given prefix is required in order to continue.
    PrefixKeys(PrefixKeys),
    /// Fetching the key that follows a given one is required in order to continue.
    NextKey(NextKey),
}

impl Verify {
    fn from_inner(inner: runtime_host::RuntimeHostVm) -> Self {
        match inner {
            runtime_host::RuntimeHostVm::Finished(Ok(success)) => {
                if !success.virtual_machine.value().as_ref().is_empty() {
                    return Verify::Finished(Err((
                        Error::NonEmptyOutput,
                        success.virtual_machine.into_prototype(),
                    )));
                }

                Verify::Finished(Ok(Success {
                    parent_runtime: success.virtual_machine.into_prototype(),
                    storage_top_trie_changes: success.storage_top_trie_changes,
                    offchain_storage_changes: success.offchain_storage_changes,
                    top_trie_root_calculation_cache: success.top_trie_root_calculation_cache,
                    logs: success.logs,
                }))
            }
            runtime_host::RuntimeHostVm::Finished(Err(err)) => {
                Verify::Finished(Err((Error::WasmVm(err.detail), err.prototype)))
            }
            runtime_host::RuntimeHostVm::StorageGet(inner) => Verify::StorageGet(StorageGet(inner)),
            runtime_host::RuntimeHostVm::PrefixKeys(inner) => Verify::PrefixKeys(PrefixKeys(inner)),
            runtime_host::RuntimeHostVm::NextKey(inner) => Verify::NextKey(NextKey(inner)),
        }
    }
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet(runtime_host::StorageGet);

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
    pub fn inject_value(self, value: Option<impl Iterator<Item = impl AsRef<[u8]>>>) -> Verify {
        Verify::from_inner(self.0.inject_value(value))
    }
}

/// Fetching the list of keys with a given prefix is required in order to continue.
#[must_use]
pub struct PrefixKeys(runtime_host::PrefixKeys);

impl PrefixKeys {
    /// Returns the prefix whose keys to load.
    pub fn prefix(&'_ self) -> impl AsRef<[u8]> + '_ {
        self.0.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(self, keys: impl Iterator<Item = impl AsRef<[u8]>>) -> Verify {
        Verify::from_inner(self.0.inject_keys(keys))
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct NextKey(runtime_host::NextKey);

impl NextKey {
    /// Returns the key whose next key must be passed back.
    pub fn key(&'_ self) -> impl AsRef<[u8]> + '_ {
        self.0.key()
    }

    /// Injects the key.
    ///
    /// # Panic
    ///
    /// Panics if the key passed as parameter isn't strictly superior to the requested key.
    ///
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> Verify {
        Verify::from_inner(self.0.inject_key(key))
    }
}
