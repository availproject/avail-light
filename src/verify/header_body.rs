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

use super::execute_block;
use crate::{
    chain::chain_information,
    executor::{self, host, vm},
    header,
    trie::calculate_root,
    verify::{aura, babe},
};

use alloc::{string::String, vec::Vec};
use core::{num::NonZeroU64, time::Duration};
use hashbrown::HashMap;

/// Configuration for a block verification.
pub struct Config<'a, TBody> {
    /// Runtime used to check the new block. Must be built using the `:code` of the parent
    /// block.
    pub parent_runtime: host::HostVmPrototype,

    /// Header of the parent of the block to verify.
    ///
    /// The hash of this header must be the one referenced in [`Config::block_header`].
    pub parent_block_header: header::HeaderRef<'a>,

    /// Configuration items related to the consensus engine.
    pub consensus: ConfigConsensus<'a>,

    /// Header of the block to verify.
    ///
    /// The `parent_hash` field is the hash of the parent whose storage can be accessed through
    /// the other fields.
    pub block_header: header::HeaderRef<'a>,

    /// Body of the block to verify.
    pub block_body: TBody,

    /// Optional cache corresponding to the storage trie root hash calculation of the parent
    /// block.
    pub top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,
}

/// Extra items of [`Config`] that are dependant on the consensus engine of the chain.
pub enum ConfigConsensus<'a> {
    /// Any node on the chain is allowed to produce blocks.
    ///
    /// No seal must be present in the header.  // TODO: is this true?
    ///
    /// > **Note**: Be warned that this variant makes it possible for a huge number of blocks to
    /// >           be produced. If this variant is used, the user is encouraged to limit, through
    /// >           other means, the number of blocks being accepted.
    AllAuthorized,

    /// Chain is using the Aura consensus engine.
    Aura {
        /// Aura authorities that must validate the block.
        ///
        /// This list is either equal to the parent's list, or, if the parent changes the list of
        /// authorities, equal to that new modified list.
        // TODO: consider not using a Vec
        current_authorities: Vec<header::AuraAuthorityRef<'a>>,

        /// Duration of a slot in milliseconds.
        /// Can be found by calling the `AuraApi_slot_duration` runtime function.
        slot_duration: NonZeroU64,

        /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
        /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
        now_from_unix_epoch: Duration,
    },

    /// Chain is using the Babe consensus engine.
    Babe {
        /// Number of slots per epoch in the Babe configuration.
        slots_per_epoch: NonZeroU64,

        /// Epoch the parent block belongs to. Must be `None` if and only if the parent block's
        /// number is 0, as block #0 doesn't belong to any epoch.
        parent_block_epoch: Option<chain_information::BabeEpochInformationRef<'a>>,

        /// Epoch that follows the epoch the parent block belongs to.
        parent_block_next_epoch: chain_information::BabeEpochInformationRef<'a>,

        /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
        /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
        now_from_unix_epoch: Duration,
    },
}

/// Block successfully verified.
pub struct Success {
    /// Runtime that was passed by [`Config`].
    pub parent_runtime: host::HostVmPrototype,

    /// Contains `Some` if and only if [`Success::storage_top_trie_changes`] contains a change in
    /// the `:code` or `:heappages` keys, indicating that the runtime has been modified. Contains
    /// the new runtime.
    pub new_runtime: Option<host::HostVmPrototype>,

    /// Extra items in [`Success`] relevant to the consensus engine.
    pub consensus: SuccessConsensus,

    /// List of changes to the storage top trie that the block performs.
    pub storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,

    /// List of changes to the offchain storage that this block performs.
    pub offchain_storage_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,

    /// Cache used for calculating the top trie root.
    pub top_trie_root_calculation_cache: calculate_root::CalculationCache,

    /// Concatenation of all the log messages printed by the runtime.
    pub logs: String,
}

/// Extra items in [`Success`] relevant to the consensus engine.
pub enum SuccessConsensus {
    /// [`ConfigConsensus::AllAuthorized`] was passed to [`Config`].
    AllAuthorized,

    /// Chain is using the Aura consensus engine.
    Aura {
        /// True if the list of authorities is modified by this block.
        authorities_change: bool,
    },

    /// Chain is using the Babe consensus engine.
    Babe {
        /// Slot number the block belongs to.
        ///
        /// > **Note**: This is a simple reminder. The value can also be found in the header of the
        /// >           block.
        slot_number: u64,

        /// If `Some`, the verified block contains an epoch transition describing the new
        /// "next epoch". When verifying blocks that are children of this one, the value in this
        /// field must be provided as [`ConfigConsensus::Babe::parent_block_next_epoch`], and the
        /// value previously in [`ConfigConsensus::Babe::parent_block_next_epoch`] must instead be
        /// passed as [`ConfigConsensus::Babe::parent_block_epoch`].
        epoch_transition_target: Option<chain_information::BabeEpochInformation>,
    },
}

/// Error that can happen during the verification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error while verifying the unsealed block.
    Unsealed(execute_block::Error),
    /// Block header contains items relevant to multiple consensus engines at the same time.
    MultipleConsensusEngines,
    /// Failed to verify the authenticity of the block with the AURA algorithm.
    #[display(fmt = "{}", _0)]
    AuraVerification(aura::VerifyError),
    /// Failed to verify the authenticity of the block with the BABE algorithm.
    #[display(fmt = "{}", _0)]
    BabeVerification(babe::VerifyError),
    /// Error while compiling new runtime.
    NewRuntimeCompilationError(host::NewErr),
    /// Block being verified has erased the `:code` key from the storage.
    CodeKeyErased,
    /// Block has modified the `:heappages` key in a way that fails to parse.
    HeapPagesParseError(executor::InvalidHeapPagesError),
    /// Block has modified the `:heappages` key without modifying the `:code` key. This isn't
    /// supported by smoldot.
    // TODO: this is something that we should support but don't because it's annoying to implement and is clearly not worth the effort
    HeapPagesOnlyModification,
}

/// Verifies whether a block is valid.
pub fn verify(
    config: Config<impl ExactSizeIterator<Item = impl AsRef<[u8]> + Clone> + Clone>,
) -> Verify {
    // Start the consensus engine verification process.
    let consensus_success = match config.consensus {
        ConfigConsensus::AllAuthorized => SuccessConsensus::AllAuthorized,
        ConfigConsensus::Aura {
            current_authorities,
            slot_duration,
            now_from_unix_epoch,
        } => {
            if config.block_header.digest.has_any_babe() {
                return Verify::Finished(Err((
                    Error::MultipleConsensusEngines,
                    config.parent_runtime,
                )));
            }

            let result = aura::verify_header(aura::VerifyConfig {
                header: config.block_header.clone(),
                parent_block_header: config.parent_block_header,
                now_from_unix_epoch,
                current_authorities: current_authorities.into_iter(),
                slot_duration,
            });

            match result {
                Ok(s) => SuccessConsensus::Aura {
                    authorities_change: s.authorities_change,
                },
                Err(err) => {
                    return Verify::Finished(Err((
                        Error::AuraVerification(err),
                        config.parent_runtime,
                    )))
                }
            }
        }
        ConfigConsensus::Babe {
            parent_block_epoch,
            parent_block_next_epoch,
            slots_per_epoch,
            now_from_unix_epoch,
        } => {
            if config.block_header.digest.has_any_aura() {
                return Verify::Finished(Err((
                    Error::MultipleConsensusEngines,
                    config.parent_runtime,
                )));
            }

            let result = babe::verify_header(babe::VerifyConfig {
                header: config.block_header.clone(),
                parent_block_header: config.parent_block_header,
                parent_block_next_epoch,
                parent_block_epoch,
                slots_per_epoch,
                now_from_unix_epoch,
            });

            match result {
                Ok(s) => SuccessConsensus::Babe {
                    epoch_transition_target: s.epoch_transition_target,
                    slot_number: s.slot_number,
                },
                Err(err) => {
                    return Verify::Finished(Err((
                        Error::BabeVerification(err),
                        config.parent_runtime,
                    )))
                }
            }
        }
    };

    // Consensus engines adds a seal at the end of the digest logs. This seal is guaranteed to be
    // the last item. We need to remove it before we can verify the unsealed header.
    let import_process = {
        let mut unsealed_header = config.block_header.clone();
        let _seal_log = unsealed_header.digest.pop_seal();

        execute_block::execute_block(execute_block::Config {
            parent_runtime: config.parent_runtime,
            block_header: unsealed_header,
            block_body: config.block_body,
            top_trie_root_calculation_cache: config.top_trie_root_calculation_cache,
        })
    };

    VerifyInner {
        inner: import_process,
        consensus_success,
    }
    .run()
}

/// Current state of the verification.
#[must_use]
pub enum Verify {
    /// Verification is over.
    ///
    /// In case of error, also contains the value that was passed through
    /// [`Config::parent_runtime`].
    Finished(Result<Success, (Error, host::HostVmPrototype)>),
    /// A new runtime must be compiled.
    ///
    /// This variant doesn't require any specific input from the user, but is provided in order to
    /// make it possible to benchmark the time it takes to compile runtimes.
    RuntimeCompilation(RuntimeCompilation),
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet),
    /// Fetching the list of keys with a given prefix is required in order to continue.
    StoragePrefixKeys(StoragePrefixKeys),
    /// Fetching the key that follows a given one is required in order to continue.
    StorageNextKey(StorageNextKey),
}

struct VerifyInner {
    inner: execute_block::Verify,
    consensus_success: SuccessConsensus,
}

impl VerifyInner {
    fn run(self) -> Verify {
        match self.inner {
            execute_block::Verify::Finished(Err((err, prototype))) => {
                Verify::Finished(Err((Error::Unsealed(err), prototype)))
            }
            execute_block::Verify::Finished(Ok(success)) => {
                match (
                    success.storage_top_trie_changes.get(&b":code"[..]),
                    success.storage_top_trie_changes.get(&b":heappages"[..]),
                ) {
                    (None, None) => {}
                    (Some(None), _) => {
                        return Verify::Finished(Err((
                            Error::CodeKeyErased,
                            success.parent_runtime,
                        )))
                    }
                    (None, Some(_)) => {
                        return Verify::Finished(Err((
                            Error::HeapPagesOnlyModification,
                            success.parent_runtime,
                        )))
                    }
                    (Some(Some(_code)), heap_pages) => {
                        let heap_pages = match heap_pages {
                            Some(heap_pages) => {
                                match executor::storage_heap_pages_to_value(heap_pages.as_deref()) {
                                    Ok(hp) => hp,
                                    Err(err) => {
                                        return Verify::Finished(Err((
                                            Error::HeapPagesParseError(err),
                                            success.parent_runtime,
                                        )))
                                    }
                                }
                            }
                            None => success.parent_runtime.heap_pages(),
                        };

                        return Verify::RuntimeCompilation(RuntimeCompilation {
                            consensus_success: self.consensus_success,
                            heap_pages,
                            success,
                        });
                    }
                }

                Verify::Finished(Ok(Success {
                    parent_runtime: success.parent_runtime,
                    new_runtime: None,
                    consensus: self.consensus_success,
                    storage_top_trie_changes: success.storage_top_trie_changes,
                    offchain_storage_changes: success.offchain_storage_changes,
                    top_trie_root_calculation_cache: success.top_trie_root_calculation_cache,
                    logs: success.logs,
                }))
            }
            execute_block::Verify::StorageGet(inner) => Verify::StorageGet(StorageGet {
                inner,
                consensus_success: self.consensus_success,
            }),
            execute_block::Verify::PrefixKeys(inner) => {
                Verify::StoragePrefixKeys(StoragePrefixKeys {
                    inner,
                    consensus_success: self.consensus_success,
                })
            }
            execute_block::Verify::NextKey(inner) => Verify::StorageNextKey(StorageNextKey {
                inner,
                consensus_success: self.consensus_success,
            }),
        }
    }
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet {
    inner: execute_block::StorageGet,
    consensus_success: SuccessConsensus,
}

impl StorageGet {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    pub fn key(&'_ self) -> impl Iterator<Item = impl AsRef<[u8]> + '_> + '_ {
        self.inner.key()
    }

    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    ///
    /// This method is a shortcut for calling `key` and concatenating the returned slices.
    pub fn key_as_vec(&self) -> Vec<u8> {
        self.inner.key_as_vec()
    }

    /// Injects the corresponding storage value.
    pub fn inject_value(self, value: Option<impl Iterator<Item = impl AsRef<[u8]>>>) -> Verify {
        VerifyInner {
            inner: self.inner.inject_value(value),
            consensus_success: self.consensus_success,
        }
        .run()
    }
}

/// Fetching the list of keys with a given prefix is required in order to continue.
#[must_use]
pub struct StoragePrefixKeys {
    inner: execute_block::PrefixKeys,
    consensus_success: SuccessConsensus,
}

impl StoragePrefixKeys {
    /// Returns the prefix whose keys to load.
    pub fn prefix(&'_ self) -> impl AsRef<[u8]> + '_ {
        self.inner.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(self, keys: impl Iterator<Item = impl AsRef<[u8]>>) -> Verify {
        VerifyInner {
            inner: self.inner.inject_keys(keys),
            consensus_success: self.consensus_success,
        }
        .run()
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct StorageNextKey {
    inner: execute_block::NextKey,
    consensus_success: SuccessConsensus,
}

impl StorageNextKey {
    /// Returns the key whose next key must be passed back.
    pub fn key(&'_ self) -> impl AsRef<[u8]> + '_ {
        self.inner.key()
    }

    /// Injects the key.
    ///
    /// # Panic
    ///
    /// Panics if the key passed as parameter isn't strictly superior to the requested key.
    ///
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> Verify {
        VerifyInner {
            inner: self.inner.inject_key(key),
            consensus_success: self.consensus_success,
        }
        .run()
    }
}

/// A new runtime must be compiled.
///
/// This variant doesn't require any specific input from the user, but is provided in order to
/// make it possible to benchmark the time it takes to compile runtimes.
#[must_use]
pub struct RuntimeCompilation {
    success: execute_block::Success,
    heap_pages: vm::HeapPages,
    consensus_success: SuccessConsensus,
}

impl RuntimeCompilation {
    /// Performs the runtime compilation.
    pub fn build(self) -> Verify {
        // A `RuntimeCompilation` object is built only if `:code` has been modified and to a
        // specific value.
        let code = self
            .success
            .storage_top_trie_changes
            .get(&b":code"[..])
            .unwrap()
            .as_ref()
            .unwrap();

        let new_runtime = match host::HostVmPrototype::new(
            code,
            self.heap_pages,
            vm::ExecHint::CompileAheadOfTime,
        ) {
            Ok(vm) => vm,
            Err(err) => {
                return Verify::Finished(Err((
                    Error::NewRuntimeCompilationError(err),
                    self.success.parent_runtime,
                )))
            }
        };

        Verify::Finished(Ok(Success {
            parent_runtime: self.success.parent_runtime,
            new_runtime: Some(new_runtime),
            consensus: self.consensus_success,
            storage_top_trie_changes: self.success.storage_top_trie_changes,
            offchain_storage_changes: self.success.offchain_storage_changes,
            top_trie_root_calculation_cache: self.success.top_trie_root_calculation_cache,
            logs: self.success.logs,
        }))
    }
}
