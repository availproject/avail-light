// Smoldot
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

//! Block generation system.
//!
//! This module provides the actual block generation code. The output is an unsealed header and
//! body.
//!
//! After a block has been generated, it must still be sealed (in other words, signed by its
//! author) by adding a corresponding entry to the log items in its header. This is out of scope
//! of this module.
//!
//! # Detail
//!
//! Building a block consists in four steps:
//!
//! - A runtime call to `Core_initialize_block`, passing a header prototype as input. This call
//!   performs some initial storage writes.
//! - A runtime call to `BlockBuilder_inherent_extrinsics`, passing as input a list of
//!   *intrinsics*. This pure call returns a list of extrinsics.
//! - Zero or more runtime calls to `BlockBuilder_apply_extrinsic`, passing as input an extrinsic.
//!   This must be done once per extrinsic returned by the previous step, plus once for each
//!   transaction to push in the block.
//! - A runtime call to `BlockBuilder_finalize_block`, which returns the newly-created unsealed
//! block header.
//!
//! The body of the newly-generated block consists in the extrinsics pushed using
//! `BlockBuilder_apply_extrinsic` (including the intrinsics).
//!

// TODO: expand docs
// TODO: explain what an inherent extrinsic is

mod tests;

use crate::{
    executor::{host, runtime_host},
    header,
    trie::calculate_root,
    util,
};

use alloc::{borrow::ToOwned as _, string::String, vec::Vec};
use core::{iter, mem};
use hashbrown::HashMap;

/// Configuration for a block generation.
pub struct Config<'a> {
    /// Hash of the parent of the block to generate.
    ///
    /// Used to populate the header of the new block.
    pub parent_hash: &'a [u8; 32],

    /// Height of the parent of the block to generate.
    ///
    /// Used to populate the header of the new block.
    pub parent_number: u64,

    /// Runtime used to check the new block. Must be built using the Wasm code found at the
    /// `:code` key of the parent block storage.
    pub parent_runtime: host::HostVmPrototype,

    /// Consensus-specific item to put in the digest of the header prototype.
    ///
    /// > **Note**: In the case of Aura and Babe, contains the slot being claimed.
    pub consensus_digest_log_item: ConfigPreRuntime<'a>,

    /// Optional cache corresponding to the storage trie root hash calculation coming from the
    /// parent block verification.
    pub top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,
}

/// Extra configuration depending on the consensus algorithm.
// TODO: consider not exposing `header` in the API
pub enum ConfigPreRuntime<'a> {
    /// Chain uses the Aura consensus algorithm.
    Aura(header::AuraPreDigest),
    /// Chain uses the Babe consensus algorithm.
    Babe(header::BabePreDigestRef<'a>),
}

/// Block successfully verified.
pub struct Success {
    /// SCALE-encoded header of the produced block.
    pub scale_encoded_header: Vec<u8>,
    /// Body of the produced block.
    pub body: Vec<Vec<u8>>,
    /// Runtime that was passed by [`Config`].
    pub parent_runtime: host::HostVmPrototype,
    /// List of changes to the storage top trie that the block performs.
    pub storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,
    /// List of changes to the offchain storage that this block performs.
    pub offchain_storage_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,
    /// Cache used for calculating the top trie root of the new block.
    pub top_trie_root_calculation_cache: calculate_root::CalculationCache,
    /// Concatenation of all the log messages printed by the runtime.
    pub logs: String,
}

/// Error that can happen during the block production.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error while executing the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    WasmVm(runtime_host::Error),
    /// Error while initializing the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    VmInit(host::StartErr, host::HostVmPrototype),
    /// Overflow when incrementing block height.
    BlockHeightOverflow,
    /// `Core_initialize_block` has returned a non-empty output.
    InitializeBlockNonEmptyOutput,
    /// Error while parsing output of `BlockBuilder_inherent_extrinsics`.
    BadInherentExtrinsicsOutput,
    /// Error while parsing output of `BlockBuilder_apply_extrinsic`.
    BadApplyExtrinsicOutput,
    /// Applying an inherent extrinsic has returned a [`DispatchError`].
    #[display(
        fmt = "Error while applying inherent extrinsic: {}\nExtrinsic: {:?}",
        error,
        extrinsic
    )]
    InherentExtrinsicDispatchError {
        /// Extrinsic that triggered the problem.
        extrinsic: Vec<u8>,
        /// Error returned by the runtime.
        error: DispatchError,
    },
    /// Applying an inherent extrinsic has returned a [`TransactionValidityError`].
    #[display(
        fmt = "Error while applying inherent extrinsic: {}\nExtrinsic: {:?}",
        error,
        extrinsic
    )]
    InherentExtrinsicTransactionValidityError {
        /// Extrinsic that triggered the problem.
        extrinsic: Vec<u8>,
        /// Error returned by the runtime.
        error: TransactionValidityError,
    },
}

/// Start a block building process.
pub fn build_block(config: Config) -> BlockBuild {
    let init_result = runtime_host::run(runtime_host::Config {
        virtual_machine: config.parent_runtime,
        function_to_call: "Core_initialize_block",
        parameter: {
            // The `Core_initialize_block` function expects a SCALE-encoded partially-initialized
            // header.
            header::HeaderRef {
                parent_hash: config.parent_hash,
                number: match config.parent_number.checked_add(1) {
                    Some(n) => n,
                    None => return BlockBuild::Finished(Err(Error::BlockHeightOverflow)),
                },
                extrinsics_root: &[0; 32],
                state_root: &[0; 32],
                digest: header::DigestRef::from_slice(&[match config.consensus_digest_log_item {
                    ConfigPreRuntime::Aura(item) => header::DigestItem::AuraPreDigest(item),
                    ConfigPreRuntime::Babe(item) => header::DigestItem::BabePreDigest(item.into()),
                }])
                .unwrap(),
            }
            .scale_encoding()
        },
        top_trie_root_calculation_cache: config.top_trie_root_calculation_cache,
        storage_top_trie_changes: Default::default(),
        offchain_storage_changes: Default::default(),
    });

    let vm = match init_result {
        Ok(vm) => vm,
        Err((err, proto)) => return BlockBuild::Finished(Err(Error::VmInit(err, proto))),
    };

    let shared = Shared {
        stage: Stage::InitializeBlock,
        block_body: Vec::new(), // TODO: with_capacity?
        logs: String::new(),
    };

    BlockBuild::from_inner(vm, shared)
}

/// Current state of the block building process.
#[must_use]
pub enum BlockBuild {
    /// Block generation is over.
    Finished(Result<Success, Error>),

    /// The inherent extrinsics are required in order to continue.
    ///
    /// [`BlockBuild::InherentExtrinsics`] is guaranteed to only be emitted once per block
    /// building process.
    ///
    /// The extrinsics returned by the call to `BlockBuilder_inherent_extrinsics` are
    /// automatically pushed to the runtime.
    InherentExtrinsics(InherentExtrinsics),

    /// Block building is ready to accept extrinsics.
    ///
    /// If [`ApplyExtrinsic::add_extrinsic`] is used, then a [`BlockBuild::ApplyExtrinsicResult`]
    /// stage will be emitted later.
    ///
    /// > **Note**: These extrinsics are generally coming from a transactions pool, but this is
    /// >           out of scope of this module.
    ApplyExtrinsic(ApplyExtrinsic),

    /// Result of the previous call to [`ApplyExtrinsic::add_extrinsic`].
    ///
    /// An [`ApplyExtrinsic`] object is provided in order to continue the operation.
    ApplyExtrinsicResult {
        /// Result of the previous call to [`ApplyExtrinsic::add_extrinsic`].
        result: Result<Result<(), DispatchError>, TransactionValidityError>,
        /// Object to use to continue trying to push other transactions or finish the block.
        resume: ApplyExtrinsic,
    },

    /// Loading a storage value from the parent storage is required in order to continue.
    StorageGet(StorageGet),

    /// Fetching the list of keys with a given prefix from the parent storage is required in order
    /// to continue.
    PrefixKeys(PrefixKeys),

    /// Fetching the key that follows a given one in the parent storage is required in order to
    /// continue.
    NextKey(NextKey),
}

impl BlockBuild {
    fn from_inner(inner: runtime_host::RuntimeHostVm, mut shared: Shared) -> Self {
        enum Inner {
            Runtime(runtime_host::RuntimeHostVm),
            Transition(runtime_host::Success),
        }

        let mut inner = Inner::Runtime(inner);

        loop {
            match (inner, &mut shared.stage) {
                (Inner::Runtime(runtime_host::RuntimeHostVm::Finished(Err(err))), _) => {
                    return BlockBuild::Finished(Err(Error::WasmVm(err)))
                }
                (Inner::Runtime(runtime_host::RuntimeHostVm::StorageGet(inner)), _) => {
                    return BlockBuild::StorageGet(StorageGet(inner, shared))
                }
                (Inner::Runtime(runtime_host::RuntimeHostVm::PrefixKeys(inner)), _) => {
                    return BlockBuild::PrefixKeys(PrefixKeys(inner, shared))
                }
                (Inner::Runtime(runtime_host::RuntimeHostVm::NextKey(inner)), _) => {
                    return BlockBuild::NextKey(NextKey(inner, shared))
                }

                (
                    Inner::Runtime(runtime_host::RuntimeHostVm::Finished(Ok(success))),
                    Stage::InitializeBlock,
                ) => {
                    if !success.virtual_machine.value().as_ref().is_empty() {
                        return BlockBuild::Finished(Err(Error::InitializeBlockNonEmptyOutput));
                    }

                    shared.logs.push_str(&success.logs);
                    shared.stage = Stage::InherentExtrinsics;

                    return BlockBuild::InherentExtrinsics(InherentExtrinsics {
                        shared,
                        parent_runtime: success.virtual_machine.into_prototype(),
                        storage_top_trie_changes: success.storage_top_trie_changes,
                        offchain_storage_changes: success.offchain_storage_changes,
                        top_trie_root_calculation_cache: success.top_trie_root_calculation_cache,
                    });
                }

                (
                    Inner::Runtime(runtime_host::RuntimeHostVm::Finished(Ok(success))),
                    Stage::InherentExtrinsics,
                ) => {
                    let extrinsics = match parse_inherent_extrinsics_output(
                        success.virtual_machine.value().as_ref(),
                    ) {
                        Ok(extrinsics) => extrinsics,
                        Err(err) => return BlockBuild::Finished(Err(err)),
                    };

                    shared.block_body.reserve(extrinsics.len());
                    shared.logs.push_str(&success.logs);
                    shared.stage = Stage::ApplyInherentExtrinsic { extrinsics };
                    inner = Inner::Transition(success);
                }

                (Inner::Transition(success), Stage::ApplyInherentExtrinsic { extrinsics })
                    if !extrinsics.is_empty() =>
                {
                    let extrinsic = &extrinsics[0];

                    let init_result = runtime_host::run(runtime_host::Config {
                        virtual_machine: success.virtual_machine.into_prototype(),
                        function_to_call: "BlockBuilder_apply_extrinsic",
                        parameter: {
                            // The `BlockBuilder_apply_extrinsic` function expects a SCALE-encoded
                            // `Vec<u8>`.
                            let len = util::encode_scale_compact_usize(extrinsic.len());
                            iter::once(len)
                                .map(either::Left)
                                .chain(iter::once(extrinsic).map(either::Right))
                        },
                        top_trie_root_calculation_cache: Some(
                            success.top_trie_root_calculation_cache,
                        ),
                        storage_top_trie_changes: success.storage_top_trie_changes,
                        offchain_storage_changes: success.offchain_storage_changes,
                    });

                    inner = Inner::Runtime(match init_result {
                        Ok(vm) => vm,
                        Err((err, proto)) => {
                            return BlockBuild::Finished(Err(Error::VmInit(err, proto)))
                        }
                    });
                }

                (Inner::Transition(success), Stage::ApplyInherentExtrinsic { .. }) => {
                    return BlockBuild::ApplyExtrinsic(ApplyExtrinsic {
                        shared,
                        parent_runtime: success.virtual_machine.into_prototype(),
                        storage_top_trie_changes: success.storage_top_trie_changes,
                        offchain_storage_changes: success.offchain_storage_changes,
                        top_trie_root_calculation_cache: success.top_trie_root_calculation_cache,
                    });
                }

                (
                    Inner::Runtime(runtime_host::RuntimeHostVm::Finished(Ok(success))),
                    Stage::ApplyInherentExtrinsic { .. },
                ) => {
                    let (extrinsic, new_stage) = match shared.stage {
                        Stage::ApplyInherentExtrinsic { mut extrinsics } => {
                            let extrinsic = extrinsics.remove(0);
                            (extrinsic, Stage::ApplyInherentExtrinsic { extrinsics })
                        }
                        _ => unreachable!(),
                    };

                    shared.stage = new_stage;

                    match parse_apply_extrinsic_output(&success.virtual_machine.value().as_ref()) {
                        Ok(Ok(Ok(()))) => {}
                        Ok(Ok(Err(error))) => {
                            return BlockBuild::Finished(Err(
                                Error::InherentExtrinsicDispatchError { extrinsic, error },
                            ))
                        }
                        Ok(Err(error)) => {
                            return BlockBuild::Finished(Err(
                                Error::InherentExtrinsicTransactionValidityError {
                                    extrinsic,
                                    error,
                                },
                            ))
                        }
                        Err(err) => return BlockBuild::Finished(Err(err)),
                    }

                    shared.block_body.push(extrinsic);

                    inner = Inner::Transition(success);
                }

                (
                    Inner::Runtime(runtime_host::RuntimeHostVm::Finished(Ok(success))),
                    Stage::ApplyExtrinsic(_),
                ) => {
                    let result = match parse_apply_extrinsic_output(
                        &success.virtual_machine.value().as_ref(),
                    ) {
                        Ok(r) => r,
                        Err(err) => return BlockBuild::Finished(Err(err)),
                    };

                    if result.is_ok() {
                        shared.block_body.push(match &mut shared.stage {
                            Stage::ApplyExtrinsic(ext) => mem::replace(ext, Vec::new()),
                            _ => unreachable!(),
                        });
                    }

                    // TODO: consider giving back extrinsic to user in case of failure

                    // TODO: IMPORTANT /!\ must throw away storage changes in case of error

                    return BlockBuild::ApplyExtrinsicResult {
                        result,
                        resume: ApplyExtrinsic {
                            shared,
                            parent_runtime: success.virtual_machine.into_prototype(),
                            storage_top_trie_changes: success.storage_top_trie_changes,
                            offchain_storage_changes: success.offchain_storage_changes,
                            top_trie_root_calculation_cache: success
                                .top_trie_root_calculation_cache,
                        },
                    };
                }

                (
                    Inner::Runtime(runtime_host::RuntimeHostVm::Finished(Ok(success))),
                    Stage::FinalizeBlock,
                ) => {
                    shared.logs.push_str(&success.logs);
                    let scale_encoded_header = success.virtual_machine.value().as_ref().to_owned();
                    return BlockBuild::Finished(Ok(Success {
                        scale_encoded_header,
                        body: shared.block_body,
                        parent_runtime: success.virtual_machine.into_prototype(),
                        storage_top_trie_changes: success.storage_top_trie_changes,
                        offchain_storage_changes: success.offchain_storage_changes,
                        top_trie_root_calculation_cache: success.top_trie_root_calculation_cache,
                        logs: shared.logs,
                    }));
                }

                (_, s) => unreachable!("{:?}", s),
            }
        }
    }
}

/// Extra information maintained in parallel of the [`runtime_host::RuntimeHostVm`].
#[derive(Debug)]
struct Shared {
    /// The block building process is separated into multiple stages.
    stage: Stage,
    /// Body of the block under construction. Items are added as construction progresses.
    block_body: Vec<Vec<u8>>,
    /// Concatenation of all logs produced by the multiple calls.
    logs: String,
}

/// The block building process is separated into multiple stages.
#[derive(Debug, Clone)]
enum Stage {
    InitializeBlock,
    InherentExtrinsics,
    ApplyInherentExtrinsic {
        /// List of inherent extrinsics being applied, including the one currently being applied.
        /// This list should thus never be empty.
        extrinsics: Vec<Vec<u8>>,
    },
    ApplyExtrinsic(Vec<u8>),
    FinalizeBlock,
}

/// The list of inherent extrinsics are needed in order to continue.
#[must_use]
pub struct InherentExtrinsics {
    shared: Shared,
    parent_runtime: host::HostVmPrototype,
    storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,
    offchain_storage_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,
    top_trie_root_calculation_cache: calculate_root::CalculationCache,
}

impl InherentExtrinsics {
    /// Injects the inherents extrinsics and resumes execution.
    ///
    /// See the module-level documentation for more information.
    pub fn inject_inherents(self, inherents: InherentData) -> BlockBuild {
        self.inject_raw_inherents_list(
            [
                (*b"timstap0", inherents.timestamp.to_le_bytes()),
                match inherents.consensus {
                    InherentDataConsensus::Aura { slot_number } => {
                        (*b"auraslot", slot_number.to_le_bytes())
                    }
                    InherentDataConsensus::Babe { slot_number } => {
                        (*b"babeslot", slot_number.to_le_bytes())
                    }
                },
            ]
            .iter()
            .cloned(),
        )
    }

    /// Injects a raw list of inherents and resumes execution.
    ///
    /// This method is a more weakly-typed equivalent to [`InherentExtrinsics::inject_inherents`].
    /// Only use this method if you know what you're doing.
    pub fn inject_raw_inherents_list(
        self,
        list: impl ExactSizeIterator<Item = ([u8; 8], impl AsRef<[u8]> + Clone)> + Clone,
    ) -> BlockBuild {
        debug_assert!(matches!(self.shared.stage, Stage::InherentExtrinsics));

        let init_result = runtime_host::run(runtime_host::Config {
            virtual_machine: self.parent_runtime,
            function_to_call: "BlockBuilder_inherent_extrinsics",
            parameter: {
                // The `BlockBuilder_inherent_extrinsics` function expects a SCALE-encoded list of
                // tuples containing an "inherent identifier" (`[u8; 8]`) and a value (`Vec<u8>`).
                let len = util::encode_scale_compact_usize(list.len());
                let encoded_list = list.flat_map(|(id, value)| {
                    let value_len = util::encode_scale_compact_usize(value.as_ref().len());
                    let value_and_len = iter::once(value_len)
                        .map(either::Left)
                        .chain(iter::once(value).map(either::Right));
                    iter::once(id)
                        .map(either::Left)
                        .chain(value_and_len.map(either::Right))
                });

                iter::once(len)
                    .map(either::Left)
                    .chain(encoded_list.map(either::Right))
            },
            top_trie_root_calculation_cache: Some(self.top_trie_root_calculation_cache),
            storage_top_trie_changes: self.storage_top_trie_changes,
            offchain_storage_changes: self.offchain_storage_changes,
        });

        let vm = match init_result {
            Ok(vm) => vm,
            Err((err, proto)) => return BlockBuild::Finished(Err(Error::VmInit(err, proto))),
        };

        BlockBuild::from_inner(vm, self.shared)
    }
}

/// Values of the inherents to pass to the runtime.
#[derive(Debug)]
pub struct InherentData {
    /// Number of milliseconds since the UNIX epoch when the block is generated, ignoring leap
    /// seconds.
    ///
    /// Its identifier passed to the runtime is: `timstap0`.
    pub timestamp: u64,

    /// Consensus-specific fields.
    pub consensus: InherentDataConsensus,
    // TODO: figure out uncles
    /*/// List of valid block headers that have the same height as the parent of the one being
    /// generated.
    ///
    /// Its identifier passed to the runtime is: `uncles00`.
    ///
    /// `TUnc` must be an iterator yielding SCALE-encoded headers.
    pub uncles: TUnc,*/

    // TODO: parachain-related inherents are missing
}

/// Extra consensus-specific items in [`InherentData`].
#[derive(Debug)]
pub enum InherentDataConsensus {
    /// Aura-specific items.
    Aura {
        /// Number of the Aura slot being claimed to generate this block.
        ///
        /// Its identifier passed to the runtime is: `auraslot`.
        ///
        /// > **Note**: This is redundant with the value passed through
        /// >           [`ConfigPreRuntime::Aura`]. This redundancy is considered as a wart in the
        /// >           runtime environment and is kept for backwards compatibility.
        slot_number: u64,
    },

    /// Babe-specific items.
    Babe {
        /// Number of the Babe slot being claimed to generate this block.
        ///
        /// Its identifier passed to the runtime is: `babeslot`.
        ///
        /// > **Note**: This is redundant with the value passed through
        /// >           [`ConfigPreRuntime::Babe`]. This redundancy is considered as a wart in the
        /// >           runtime environment and is kept for backwards compatibility.
        slot_number: u64,
    },
}

/// More transactions can be added.
#[must_use]
pub struct ApplyExtrinsic {
    shared: Shared,
    parent_runtime: host::HostVmPrototype,
    storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,
    offchain_storage_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,
    top_trie_root_calculation_cache: calculate_root::CalculationCache,
}

impl ApplyExtrinsic {
    /// Adds a SCALE-encoded extrinsic and resumes execution.
    ///
    /// See the module-level documentation for more information.
    pub fn add_extrinsic(mut self, extrinsic: Vec<u8>) -> BlockBuild {
        let init_result = runtime_host::run(runtime_host::Config {
            virtual_machine: self.parent_runtime,
            function_to_call: "BlockBuilder_apply_extrinsic",
            parameter: {
                // The `BlockBuilder_apply_extrinsic` function expects a SCALE-encoded `Vec<u8>`.
                let len = util::encode_scale_compact_usize(extrinsic.len());
                iter::once(len)
                    .map(either::Left)
                    .chain(iter::once(&extrinsic).map(either::Right))
            },
            top_trie_root_calculation_cache: Some(self.top_trie_root_calculation_cache),
            storage_top_trie_changes: self.storage_top_trie_changes,
            offchain_storage_changes: self.offchain_storage_changes,
        });

        self.shared.stage = Stage::ApplyExtrinsic(extrinsic);

        let vm = match init_result {
            Ok(vm) => vm,
            Err((err, proto)) => return BlockBuild::Finished(Err(Error::VmInit(err, proto))),
        };

        BlockBuild::from_inner(vm, self.shared)
    }

    /// Indicate that no more extrinsics will be added, and resume execution.
    pub fn finish(mut self) -> BlockBuild {
        self.shared.stage = Stage::FinalizeBlock;

        let init_result = runtime_host::run(runtime_host::Config {
            virtual_machine: self.parent_runtime,
            function_to_call: "BlockBuilder_finalize_block",
            parameter: iter::empty::<&[u8]>(),
            top_trie_root_calculation_cache: Some(self.top_trie_root_calculation_cache),
            storage_top_trie_changes: self.storage_top_trie_changes,
            offchain_storage_changes: self.offchain_storage_changes,
        });

        let vm = match init_result {
            Ok(vm) => vm,
            Err((err, proto)) => return BlockBuild::Finished(Err(Error::VmInit(err, proto))),
        };

        BlockBuild::from_inner(vm, self.shared)
    }
}

/// Loading a storage value from the parent storage is required in order to continue.
#[must_use]
pub struct StorageGet(runtime_host::StorageGet, Shared);

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
    pub fn inject_value(self, value: Option<impl Iterator<Item = impl AsRef<[u8]>>>) -> BlockBuild {
        BlockBuild::from_inner(self.0.inject_value(value), self.1)
    }
}

/// Fetching the list of keys with a given prefix from the parent storage is required in order to
/// continue.
#[must_use]
pub struct PrefixKeys(runtime_host::PrefixKeys, Shared);

impl PrefixKeys {
    /// Returns the prefix whose keys to load.
    pub fn prefix(&'_ self) -> impl AsRef<[u8]> + '_ {
        self.0.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(self, keys: impl Iterator<Item = impl AsRef<[u8]>>) -> BlockBuild {
        BlockBuild::from_inner(self.0.inject_keys(keys), self.1)
    }
}

/// Fetching the key that follows a given one in the parent storage is required in order to
/// continue.
#[must_use]
pub struct NextKey(runtime_host::NextKey, Shared);

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
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> BlockBuild {
        BlockBuild::from_inner(self.0.inject_key(key), self.1)
    }
}

/// Analyzes the output of a call to `BlockBuilder_inherent_extrinsics`, and returns the resulting
/// extrinsics.
fn parse_inherent_extrinsics_output(output: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    nom::combinator::all_consuming(nom::combinator::flat_map(
        crate::util::nom_scale_compact_usize,
        |num_elems| {
            nom::multi::many_m_n(num_elems, num_elems, |s| {
                nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |n| {
                    nom::combinator::map(nom::bytes::complete::take(n), |v: &[u8]| v.to_vec())
                })(s)
            })
        },
    ))(output)
    .map(|(_, parse_result)| parse_result)
    .map_err(|_: nom::Err<(&[u8], nom::error::ErrorKind)>| Error::BadInherentExtrinsicsOutput)
}

/// Analyzes the output of a call to `BlockBuilder_apply_extrinsic`.
fn parse_apply_extrinsic_output(
    output: &[u8],
) -> Result<Result<Result<(), DispatchError>, TransactionValidityError>, Error> {
    nom::combinator::all_consuming(apply_extrinsic_result)(output)
        .map(|(_, parse_result)| parse_result)
        .map_err(|_: nom::Err<nom::error::Error<&[u8]>>| Error::BadApplyExtrinsicOutput)
}

// TODO: some parsers below are common with the tx-pool ; figure out how/whether they should be merged

/// Errors that can occur while checking the validity of a transaction.
#[derive(Debug, derive_more::Display, Clone, PartialEq, Eq)]
pub enum TransactionValidityError {
    /// The transaction is invalid.
    Invalid(InvalidTransaction),
    /// Transaction validity can't be determined.
    Unknown(UnknownTransaction),
}

/// An invalid transaction validity.
#[derive(Debug, derive_more::Display, Clone, PartialEq, Eq)]
pub enum InvalidTransaction {
    /// The call of the transaction is not expected.
    Call,
    /// General error to do with the inability to pay some fees (e.g. account balance too low).
    Payment,
    /// General error to do with the transaction not yet being valid (e.g. nonce too high).
    Future,
    /// General error to do with the transaction being outdated (e.g. nonce too low).
    Stale,
    /// General error to do with the transaction's proofs (e.g. signature).
    ///
    /// # Possible causes
    ///
    /// When using a signed extension that provides additional data for signing, it is required
    /// that the signing and the verifying side use the same additional data. Additional
    /// data will only be used to generate the signature, but will not be part of the transaction
    /// itself. As the verifying side does not know which additional data was used while signing
    /// it will only be able to assume a bad signature and cannot express a more meaningful error.
    BadProof,
    /// The transaction birth block is ancient.
    AncientBirthBlock,
    /// The transaction would exhaust the resources of current block.
    ///
    /// The transaction might be valid, but there are not enough resources
    /// left in the current block.
    ExhaustsResources,
    /// Any other custom invalid validity that is not covered by this enum.
    Custom(u8),
    /// An extrinsic with a Mandatory dispatch resulted in Error. This is indicative of either a
    /// malicious validator or a buggy `provide_inherent`. In any case, it can result in dangerously
    /// overweight blocks and therefore if found, invalidates the block.
    BadMandatory,
    /// A transaction with a mandatory dispatch. This is invalid; only inherent extrinsics are
    /// allowed to have mandatory dispatches.
    MandatoryDispatch,
}

/// An unknown transaction validity.
#[derive(Debug, derive_more::Display, Clone, PartialEq, Eq)]
pub enum UnknownTransaction {
    /// Could not lookup some information that is required to validate the transaction.
    CannotLookup,
    /// No validator found for the given unsigned transaction.
    NoUnsignedValidator,
    /// Any other custom unknown validity that is not covered by this enum.
    Custom(u8),
}

/// Reason why a dispatch call failed.
#[derive(Debug, derive_more::Display, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// Failed to lookup some data.
    CannotLookup,
    /// A bad origin.
    BadOrigin,
    /// A custom error in a module.
    #[display(fmt = "Error in module #{}, error number #{}", index, error)]
    Module {
        /// Module index, matching the metadata module index.
        index: u8,
        /// Module specific error value.
        error: u8,
    },
}

fn apply_extrinsic_result(
    bytes: &[u8],
) -> nom::IResult<&[u8], Result<Result<(), DispatchError>, TransactionValidityError>> {
    nom::error::context(
        "apply extrinsic result",
        nom::branch::alt((
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[0]), dispatch_outcome),
                Ok,
            ),
            nom::combinator::map(
                nom::sequence::preceded(
                    nom::bytes::complete::tag(&[1]),
                    transaction_validity_error,
                ),
                Err,
            ),
        )),
    )(bytes)
}

fn dispatch_outcome(bytes: &[u8]) -> nom::IResult<&[u8], Result<(), DispatchError>> {
    nom::error::context(
        "dispatch outcome",
        nom::branch::alt((
            nom::combinator::map(nom::bytes::complete::tag(&[0]), |_| Ok(())),
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[1]), dispatch_error),
                Err,
            ),
        )),
    )(bytes)
}

fn dispatch_error(bytes: &[u8]) -> nom::IResult<&[u8], DispatchError> {
    nom::error::context(
        "dispatch error",
        nom::branch::alt((
            nom::combinator::map(nom::bytes::complete::tag(&[0]), |_| {
                DispatchError::CannotLookup
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[1]), |_| {
                DispatchError::BadOrigin
            }),
            nom::combinator::map(
                nom::sequence::preceded(
                    nom::bytes::complete::tag(&[2]),
                    nom::sequence::tuple((nom::number::complete::u8, nom::number::complete::u8)),
                ),
                |(index, error)| DispatchError::Module { index, error },
            ),
        )),
    )(bytes)
}

fn transaction_validity_error(bytes: &[u8]) -> nom::IResult<&[u8], TransactionValidityError> {
    nom::error::context(
        "transaction validity error",
        nom::branch::alt((
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[0]), invalid_transaction),
                TransactionValidityError::Invalid,
            ),
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[1]), unknown_transaction),
                TransactionValidityError::Unknown,
            ),
        )),
    )(bytes)
}

fn invalid_transaction(bytes: &[u8]) -> nom::IResult<&[u8], InvalidTransaction> {
    nom::error::context(
        "invalid transaction",
        nom::branch::alt((
            nom::combinator::map(nom::bytes::complete::tag(&[0]), |_| {
                InvalidTransaction::Call
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[1]), |_| {
                InvalidTransaction::Payment
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[2]), |_| {
                InvalidTransaction::Future
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[3]), |_| {
                InvalidTransaction::Stale
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[4]), |_| {
                InvalidTransaction::BadProof
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[5]), |_| {
                InvalidTransaction::AncientBirthBlock
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[6]), |_| {
                InvalidTransaction::ExhaustsResources
            }),
            nom::combinator::map(
                nom::sequence::preceded(
                    nom::bytes::complete::tag(&[7]),
                    nom::bytes::complete::take(1u32),
                ),
                |n: &[u8]| InvalidTransaction::Custom(n[0]),
            ),
            nom::combinator::map(nom::bytes::complete::tag(&[8]), |_| {
                InvalidTransaction::BadMandatory
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[9]), |_| {
                InvalidTransaction::MandatoryDispatch
            }),
        )),
    )(bytes)
}

fn unknown_transaction(bytes: &[u8]) -> nom::IResult<&[u8], UnknownTransaction> {
    nom::error::context(
        "unknown transaction",
        nom::branch::alt((
            nom::combinator::map(nom::bytes::complete::tag(&[0]), |_| {
                UnknownTransaction::CannotLookup
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[1]), |_| {
                UnknownTransaction::NoUnsignedValidator
            }),
            nom::combinator::map(
                nom::sequence::preceded(
                    nom::bytes::complete::tag(&[2]),
                    nom::bytes::complete::take(1u32),
                ),
                |n: &[u8]| UnknownTransaction::Custom(n[0]),
            ),
        )),
    )(bytes)
}
