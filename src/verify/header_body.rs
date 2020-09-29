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

use super::execute_block;
use crate::{executor, header, trie::calculate_root, verify::babe};

use core::{num::NonZeroU64, time::Duration};
use hashbrown::HashMap;

/// Configuration for a block verification.
pub struct Config<'a, TBody> {
    /// Runtime used to check the new block. Must be built using the `:code` of the parent
    /// block.
    pub parent_runtime: executor::WasmVmPrototype,

    /// Header of the parent of the block to verify.
    ///
    /// The hash of this header must be the one referenced in [`Config::block_header`].
    pub parent_block_header: header::HeaderRef<'a>,

    /// BABE configuration retrieved from the genesis block.
    ///
    /// See the documentation of [`babe::BabeGenesisConfiguration`] to know how to get this.
    pub babe_genesis_configuration: &'a babe::BabeGenesisConfiguration,

    /// Slot number of block #1. **Must** be provided, unless the block being verified is block
    /// #1 itself.
    ///
    /// Must be the value of [`Success::slot_number`] for block #1.
    pub block1_slot_number: Option<u64>,

    /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
    /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
    pub now_from_unix_epoch: Duration,

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

/// Block successfully verified.
pub struct Success {
    /// Runtime that was passed by [`Config`].
    pub parent_runtime: executor::WasmVmPrototype,

    /// If `Some`, the verified block contains an epoch transition describing the given epoch.
    /// This epoch transition must later be provided back as part of the [`Config`] when verifying
    /// the blocks that are part of that epoch.
    pub babe_epoch_transition_target: Option<NonZeroU64>,

    /// Slot number the block belongs to.
    pub slot_number: u64,

    /// Epoch number the block belongs to.
    pub epoch_number: u64,

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
    /// Error while verifying the unsealed block.
    Unsealed(execute_block::Error),
    /// Failed to verify the authenticity of the block with the BABE algorithm.
    BabeVerification(babe::VerifyError),
}

/// Verifies whether a block is valid.
pub fn verify<'a>(
    config: Config<'a, impl ExactSizeIterator<Item = impl AsRef<[u8]> + Clone> + Clone>,
) -> Verify {
    // Start the BABE verification process.
    let babe_verification = {
        let result = babe::start_verify_header(babe::VerifyConfig {
            header: config.block_header.clone(),
            parent_block_header: config.parent_block_header,
            genesis_configuration: config.babe_genesis_configuration,
            now_from_unix_epoch: config.now_from_unix_epoch,
            block1_slot_number: config.block1_slot_number,
        });

        match result {
            Ok(s) => s,
            Err(err) => return Verify::Finished(Err(Error::BabeVerification(err))),
        }
    };

    // BABE adds a seal at the end of the digest logs. This seal is guaranteed to be the last
    // item. We need to remove it before we can verify the unsealed header.
    let mut unsealed_header = config.block_header.clone();
    let _seal_log = unsealed_header.digest.pop_babe_seal();
    debug_assert!(_seal_log.is_some());

    let import_process = execute_block::execute_block(execute_block::Config {
        parent_runtime: config.parent_runtime,
        block_header: unsealed_header,
        block_body: config.block_body,
        top_trie_root_calculation_cache: config.top_trie_root_calculation_cache,
    });

    VerifyInner::Babe {
        babe_verification,
        import_process,
    }
    .run()
}

/// Current state of the verification.
#[must_use]
pub enum Verify {
    /// Verification is over.
    Finished(Result<Success, Error>),
    /// Fetching an epoch information is required in order to continue.
    BabeEpochInformation(BabeEpochInformation),
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet),
    /// Fetching the list of keys with a given prefix is required in order to continue.
    StoragePrefixKeys(StoragePrefixKeys),
    /// Fetching the key that follows a given one is required in order to continue.
    StorageNextKey(StorageNextKey),
}

enum VerifyInner {
    /// Verifying BABE.
    Babe {
        babe_verification: babe::SuccessOrPending,
        import_process: execute_block::Verify,
    },
    /// Error in BABE verification.
    BabeError(babe::VerifyError),
    /// Verifying the unsealed block.
    Unsealed {
        inner: execute_block::Verify,
        babe_success: babe::VerifySuccess,
    },
}

impl VerifyInner {
    fn run(mut self) -> Verify {
        loop {
            break match self {
                VerifyInner::Babe {
                    babe_verification,
                    import_process,
                } => match babe_verification {
                    babe::SuccessOrPending::Success(babe_success) => {
                        self = VerifyInner::Unsealed {
                            inner: import_process,
                            babe_success,
                        };
                        continue;
                    }
                    babe::SuccessOrPending::Pending(pending) => {
                        Verify::BabeEpochInformation(BabeEpochInformation {
                            inner: pending,
                            import_process,
                        })
                    }
                },
                VerifyInner::BabeError(err) => Verify::Finished(Err(Error::BabeVerification(err))),
                VerifyInner::Unsealed {
                    inner,
                    babe_success,
                } => match inner {
                    execute_block::Verify::Finished(Err(err)) => {
                        Verify::Finished(Err(Error::Unsealed(err)))
                    }
                    execute_block::Verify::Finished(Ok(success)) => Verify::Finished(Ok(Success {
                        parent_runtime: success.parent_runtime,
                        babe_epoch_transition_target: babe_success.epoch_transition_target,
                        slot_number: babe_success.slot_number,
                        epoch_number: babe_success.epoch_number,
                        storage_top_trie_changes: success.storage_top_trie_changes,
                        offchain_storage_changes: success.offchain_storage_changes,
                        top_trie_root_calculation_cache: success.top_trie_root_calculation_cache,
                        logs: success.logs,
                    })),
                    execute_block::Verify::StorageGet(inner) => Verify::StorageGet(StorageGet {
                        inner,
                        babe_success,
                    }),
                    execute_block::Verify::PrefixKeys(inner) => {
                        Verify::StoragePrefixKeys(StoragePrefixKeys {
                            inner,
                            babe_success,
                        })
                    }
                    execute_block::Verify::NextKey(inner) => {
                        Verify::StorageNextKey(StorageNextKey {
                            inner,
                            babe_success,
                        })
                    }
                },
            };
        }
    }
}

/// Fetching an epoch information is required in order to continue.
#[must_use]
pub struct BabeEpochInformation {
    inner: babe::PendingVerify,
    import_process: execute_block::Verify,
}

impl BabeEpochInformation {
    /// Returns the epoch number whose information must be passed to
    /// [`BabeEpochInformation::inject_epoch`].
    pub fn epoch_number(&self) -> u64 {
        self.inner.epoch_number()
    }

    /// Returns true if the epoch of the verified block is the same as its parent's.
    pub fn same_epoch_as_parent(&self) -> bool {
        self.inner.same_epoch_as_parent()
    }

    /// Finishes the verification. Must provide the information about the epoch whose number is
    /// obtained with [`BabeEpochInformation::epoch_number`].
    pub fn inject_epoch(
        self,
        epoch_info: (header::BabeNextEpochRef, header::BabeNextConfig),
    ) -> Verify {
        match self.inner.finish(epoch_info) {
            Ok(babe_success) => VerifyInner::Unsealed {
                inner: self.import_process,
                babe_success,
            }
            .run(),
            Err(err) => VerifyInner::BabeError(err).run(),
        }
    }
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet {
    inner: execute_block::StorageGet,
    babe_success: babe::VerifySuccess,
}

impl StorageGet {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    pub fn key<'b>(&'b self) -> impl Iterator<Item = impl AsRef<[u8]> + 'b> + 'b {
        self.inner.key()
    }

    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    ///
    /// This method is a shortcut for calling `key` and concatenating the returned slices.
    pub fn key_as_vec(&self) -> Vec<u8> {
        self.inner.key_as_vec()
    }

    /// Injects the corresponding storage value.
    pub fn inject_value(self, value: Option<&[u8]>) -> Verify {
        VerifyInner::Unsealed {
            inner: self.inner.inject_value(value),
            babe_success: self.babe_success,
        }
        .run()
    }
}

/// Fetching the list of keys with a given prefix is required in order to continue.
#[must_use]
pub struct StoragePrefixKeys {
    inner: execute_block::PrefixKeys,
    babe_success: babe::VerifySuccess,
}

impl StoragePrefixKeys {
    /// Returns the prefix whose keys to load.
    pub fn prefix(&self) -> &[u8] {
        self.inner.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(self, keys: impl Iterator<Item = impl AsRef<[u8]>>) -> Verify {
        VerifyInner::Unsealed {
            inner: self.inner.inject_keys(keys),
            babe_success: self.babe_success,
        }
        .run()
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct StorageNextKey {
    inner: execute_block::NextKey,
    babe_success: babe::VerifySuccess,
}

impl StorageNextKey {
    /// Returns the key whose next key must be passed back.
    pub fn key(&self) -> &[u8] {
        self.inner.key()
    }

    /// Injects the key.
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> Verify {
        VerifyInner::Unsealed {
            inner: self.inner.inject_key(key),
            babe_success: self.babe_success,
        }
        .run()
    }
}
