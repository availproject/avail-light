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

// TODO: docs

use crate::{
    author::{aura, runtime},
    executor::host,
    header,
    trie::calculate_root,
};

use alloc::vec::Vec;
use core::{convert::TryFrom as _, num::NonZeroU64, time::Duration};

/// Configuration for a block generation.
pub struct Config<'a, TLocAuth> {
    /// Consensus-specific configuration.
    pub consensus: ConfigConsensus<'a, TLocAuth>,
}

/// Extension to [`Config`].
pub enum ConfigConsensus<'a, TLocAuth> {
    /// Chain is using the Aura consensus algorithm.
    Aura {
        /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
        /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
        now_from_unix_epoch: Duration,

        /// Duration, in milliseconds, of an Aura slot.
        slot_duration: NonZeroU64,

        /// List of the Aura authorities allowed to produce a block. This is either the same as
        /// the ones of the current best block, or a new list if the current best block contains
        /// an authorities list change digest item.
        current_authorities: header::AuraAuthoritiesIter<'a>,

        /// Iterator to the list of sr25519 public keys available locally.
        ///
        /// Must implement `Iterator<Item = &[u8; 32]>`.
        local_authorities: TLocAuth,
    },
    // TODO: Babe isn't supported yet
}

/// Current state of the block building process.
#[must_use]
pub enum Builder {
    /// None of the authorities available locally are allowed to produce a block.
    Idle,

    /// Block production is idle, waiting for a slot.
    WaitSlot(WaitSlot),

    /// Block production is ready to start.
    Ready(AuthoringStart),

    /// Currently authoring a block.
    Authoring(BuilderAuthoring),
}

impl Builder {
    /// Initializes a new builder.
    ///
    /// Returns `None` if none of the local authorities are allowed to produce blocks.
    ///
    /// Keep in mind that the builder should be reconstructed every time the best block changes.
    pub fn new<'a>(config: Config<'a, impl Iterator<Item = &'a [u8; 32]>>) -> Self {
        let (slot, ready): (WaitSlotConsensus, bool) = match config.consensus {
            ConfigConsensus::Aura {
                current_authorities,
                local_authorities,
                now_from_unix_epoch,
                slot_duration,
            } => {
                let consensus = match aura::next_slot_claim(aura::Config {
                    current_authorities,
                    local_authorities,
                    now_from_unix_epoch,
                    slot_duration,
                }) {
                    Some(c) => c,
                    None => return Builder::Idle,
                };

                debug_assert!(now_from_unix_epoch < consensus.slot_end_from_unix_epoch);
                let ready = now_from_unix_epoch >= consensus.slot_start_from_unix_epoch;

                (WaitSlotConsensus::Aura(consensus), ready)
            }
        };

        if ready {
            Builder::Ready(AuthoringStart { consensus: slot })
        } else {
            Builder::WaitSlot(WaitSlot { consensus: slot })
        }
    }
}

/// Current state of the block building process.
#[must_use]
pub enum BuilderAuthoring {
    /// Error happened during the generation.
    Error(Error),

    /// Block building is ready to accept extrinsics.
    ///
    /// If [`ApplyExtrinsic::add_extrinsic`] is used, then a
    /// [`BuilderAuthoring::ApplyExtrinsicResult`] stage will be emitted later.
    ///
    /// > **Note**: These extrinsics are generally coming from a transactions pool, but this is
    /// >           out of scope of this module.
    ApplyExtrinsic(ApplyExtrinsic),

    /// Result of the previous call to [`ApplyExtrinsic::add_extrinsic`].
    ///
    /// An [`ApplyExtrinsic`] object is provided in order to continue the operation.
    ApplyExtrinsicResult {
        /// Result of the previous call to [`ApplyExtrinsic::add_extrinsic`].
        result: Result<Result<(), runtime::DispatchError>, runtime::TransactionValidityError>,
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

    /// Block has been produced by the runtime and must now be sealed.
    Seal(Seal),
}

/// Block production is idle, waiting for a slot.
#[must_use]
#[derive(Debug)]
pub struct WaitSlot {
    consensus: WaitSlotConsensus,
}

#[derive(Debug)]
enum WaitSlotConsensus {
    Aura(aura::SlotClaim),
}

impl WaitSlot {
    /// Returns when block production can begin, as a UNIX timestamp (i.e. number of seconds since
    /// the UNIX epoch, ignoring leap seconds).
    pub fn when(&self) -> Duration {
        // TODO: we can actually start building the block before our slot in some situations?
        match self.consensus {
            WaitSlotConsensus::Aura(claim) => claim.slot_start_from_unix_epoch,
        }
    }

    /// Start the block production.
    ///
    /// Shouldn't be called before the timestamp returned by [`WaitSlot::when`]. Blocks that are
    /// authored and sent to other nodes before the proper timestamp will be considered as
    /// invalid.
    pub fn start(self) -> AuthoringStart {
        AuthoringStart {
            consensus: self.consensus,
        }
    }
}

/// Ready to start producing blocks.
pub struct AuthoringStart {
    consensus: WaitSlotConsensus,
}

impl AuthoringStart {
    /// Start producing the block.
    pub fn start(self, config: AuthoringStartConfig) -> BuilderAuthoring {
        let inner_block_build = runtime::build_block(runtime::Config {
            parent_hash: config.parent_hash,
            parent_number: config.parent_number,
            parent_runtime: config.parent_runtime,
            top_trie_root_calculation_cache: config.top_trie_root_calculation_cache,
            consensus_digest_log_item: match self.consensus {
                WaitSlotConsensus::Aura(slot) => {
                    runtime::ConfigPreRuntime::Aura(header::AuraPreDigest {
                        slot_number: slot.slot_number,
                    })
                }
            },
        });

        let inherent_data = runtime::InherentData {
            timestamp: u64::try_from(config.now_from_unix_epoch.as_millis())
                .unwrap_or(u64::max_value()),
            consensus: match self.consensus {
                WaitSlotConsensus::Aura(slot) => runtime::InherentDataConsensus::Aura {
                    slot_number: slot.slot_number,
                },
            },
        };

        (Shared {
            inherent_data: Some(inherent_data),
            slot_claim: self.consensus,
        })
        .with_runtime_inner(inner_block_build)
    }
}

/// Configuration to pass when the actual block authoring is started.
pub struct AuthoringStartConfig<'a> {
    /// Hash of the parent of the block to generate.
    ///
    /// Used to populate the header of the new block.
    pub parent_hash: &'a [u8; 32],

    /// Height of the parent of the block to generate.
    ///
    /// Used to populate the header of the new block.
    pub parent_number: u64,

    /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
    /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
    pub now_from_unix_epoch: Duration,

    /// Runtime used to check the new block. Must be built using the Wasm code found at the
    /// `:code` key of the parent block storage.
    pub parent_runtime: host::HostVmPrototype,

    /// Optional cache corresponding to the storage trie root hash calculation coming from the
    /// parent block verification.
    pub top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,
}

/// More transactions can be added.
#[must_use]
pub struct ApplyExtrinsic {
    inner: runtime::ApplyExtrinsic,
    shared: Shared,
}

impl ApplyExtrinsic {
    /// Adds a SCALE-encoded extrinsic and resumes execution.
    ///
    /// See the module-level documentation for more information.
    pub fn add_extrinsic(self, extrinsic: Vec<u8>) -> BuilderAuthoring {
        self.shared
            .with_runtime_inner(self.inner.add_extrinsic(extrinsic))
    }

    /// Indicate that no more extrinsics will be added, and resume execution.
    pub fn finish(self) -> BuilderAuthoring {
        self.shared.with_runtime_inner(self.inner.finish())
    }
}

/// Loading a storage value from the parent storage is required in order to continue.
#[must_use]
pub struct StorageGet(runtime::StorageGet, Shared);

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
    pub fn inject_value(
        self,
        value: Option<impl Iterator<Item = impl AsRef<[u8]>>>,
    ) -> BuilderAuthoring {
        self.1.with_runtime_inner(self.0.inject_value(value))
    }
}

/// Fetching the list of keys with a given prefix from the parent storage is required in order to
/// continue.
#[must_use]
pub struct PrefixKeys(runtime::PrefixKeys, Shared);

impl PrefixKeys {
    /// Returns the prefix whose keys to load.
    pub fn prefix(&'_ self) -> impl AsRef<[u8]> + '_ {
        self.0.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(self, keys: impl Iterator<Item = impl AsRef<[u8]>>) -> BuilderAuthoring {
        self.1.with_runtime_inner(self.0.inject_keys(keys))
    }
}

/// Fetching the key that follows a given one in the parent storage is required in order to
/// continue.
#[must_use]
pub struct NextKey(runtime::NextKey, Shared);

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
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> BuilderAuthoring {
        self.1.with_runtime_inner(self.0.inject_key(key))
    }
}

/// Block has been produced and must now be sealed.
#[must_use]
pub struct Seal {
    shared: Shared,
    block: runtime::Success,
}

impl Seal {
    /// Returns the SCALE-encoded header that must be signed.
    pub fn scale_encoded_header(&self) -> &[u8] {
        &self.block.scale_encoded_header
    }

    /// Returns the index within the list of authorities of the authority that must sign the
    /// block.
    ///
    /// See [`ConfigConsensus::Aura::local_authorities`].
    pub fn authority_index(&self) -> usize {
        match self.shared.slot_claim {
            WaitSlotConsensus::Aura(slot) => slot.local_authorities_index,
        }
    }

    /// Injects the sr25519 signature of the SCALE-encoded header from the given authority.
    ///
    /// The method then returns the finished block.
    pub fn inject_sr25519_signature(mut self, signature: [u8; 64]) -> runtime::Success {
        // TODO: optimize?
        let mut header: header::Header = header::decode(&self.block.scale_encoded_header)
            .unwrap()
            .into();

        // `push_aura_seal` error if there is already an Aura seal, indicating that the runtime
        // code is misbehaving. This condition is already verified when the `Seal` is created.
        header.digest.push_aura_seal(signature).unwrap();

        self.block.scale_encoded_header = header.scale_encoding().fold(Vec::new(), |mut a, b| {
            a.extend_from_slice(b.as_ref());
            a
        });

        self.block
    }
}

/// Error that can happen during the block production.
#[derive(Debug, derive_more::Display, derive_more::From)]
pub enum Error {
    /// Error while producing the block in the runtime.
    #[display(fmt = "{}", _0)]
    Runtime(runtime::Error),
    /// Runtime has generated an invalid block header.
    #[from(ignore)]
    InvalidHeaderGenerated,
}

/// Extra information maintained in all variants of the [`Builder`].
#[derive(Debug)]
struct Shared {
    /// Inherent data waiting to be injected. Will be extracted from its `Option` when the inner
    /// block builder requests it.
    inherent_data: Option<runtime::InherentData>,

    /// Slot that has been claimed.
    slot_claim: WaitSlotConsensus,
}

impl Shared {
    fn with_runtime_inner(mut self, mut inner: runtime::BlockBuild) -> BuilderAuthoring {
        loop {
            match inner {
                runtime::BlockBuild::Finished(Ok(block)) => {
                    // After the runtime has produced a block, the last step is to seal it.

                    // Verify the correctness of the header. If not, the runtime is misbehaving.
                    let decoded_header = match header::decode(&block.scale_encoded_header) {
                        Ok(h) => h,
                        Err(_) => break BuilderAuthoring::Error(Error::InvalidHeaderGenerated),
                    };

                    // The `Seal` object created below assumes that there is no existing seal.
                    if decoded_header.digest.aura_seal().is_some()
                        || decoded_header.digest.babe_seal().is_some()
                    {
                        break BuilderAuthoring::Error(Error::InvalidHeaderGenerated);
                    }

                    break BuilderAuthoring::Seal(Seal {
                        shared: self,
                        block,
                    });
                }
                runtime::BlockBuild::Finished(Err(error)) => {
                    break BuilderAuthoring::Error(Error::Runtime(error))
                }
                runtime::BlockBuild::InherentExtrinsics(a) => {
                    // Injecting the inherent is guaranteed to be done only once per block.
                    inner = a.inject_inherents(self.inherent_data.take().unwrap());
                }
                runtime::BlockBuild::ApplyExtrinsic(a) => {
                    inner = a.finish();
                }
                runtime::BlockBuild::ApplyExtrinsicResult { result, resume } => {
                    break BuilderAuthoring::ApplyExtrinsicResult {
                        result,
                        resume: ApplyExtrinsic {
                            inner: resume,
                            shared: self,
                        },
                    }
                }
                runtime::BlockBuild::StorageGet(inner) => {
                    break BuilderAuthoring::StorageGet(StorageGet(inner, self))
                }
                runtime::BlockBuild::PrefixKeys(inner) => {
                    break BuilderAuthoring::PrefixKeys(PrefixKeys(inner, self))
                }
                runtime::BlockBuild::NextKey(inner) => {
                    break BuilderAuthoring::NextKey(NextKey(inner, self))
                }
            }
        }
    }
}
