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

//! Optimistic header and body syncing.
//!
//! This state machine builds, from a set of sources, a fully verified chain of blocks headers
//! and bodies.
//!
//! In addition to managing the sources, using [`OptimisticFullSync`] also requires holding the
//! storage of the latest finalized block.

// TODO: document better
// TODO: this entire module needs clean up

use super::super::{blocks_tree, chain_information};
use super::optimistic;
use crate::{executor, header, trie::calculate_root};

use alloc::{collections::BTreeMap, vec, vec::Vec};
use core::{convert::TryFrom as _, iter, num::NonZeroU32};
use hashbrown::{HashMap, HashSet};

pub use optimistic::{
    FinishRequestOutcome, RequestAction, RequestFail, RequestId, SourceId, Start,
};

/// Configuration for the [`OptimisticFullSync`].
#[derive(Debug)]
pub struct Config {
    /// Information about the latest finalized block and its ancestors.
    pub chain_information_config: chain_information::ChainInformationConfig,

    /// Pre-allocated capacity for the number of block sources.
    pub sources_capacity: usize,

    /// Pre-allocated capacity for the number of blocks between the finalized block and the head
    /// of the chain.
    ///
    /// Should be set to the maximum number of block between two consecutive justifications.
    pub blocks_capacity: usize,

    /// Maximum number of blocks returned by a response.
    ///
    /// > **Note**: If blocks are requested from the network, this should match the network
    /// >           protocol enforced limit.
    pub blocks_request_granularity: NonZeroU32,

    /// Number of blocks to download ahead of the best block.
    ///
    /// Whenever the latest best block is updated, the state machine will start block
    /// requests for the block `best_block_height + download_ahead_blocks` and all its
    /// ancestors. Considering that requesting blocks has some latency, downloading blocks ahead
    /// of time ensures that verification isn't blocked waiting for a request to be finished.
    ///
    /// The ideal value here depends on the speed of blocks verification speed and latency of
    /// block requests.
    pub download_ahead_blocks: u32,

    /// Seed used by the PRNG (Pseudo-Random Number Generator) that selects which source to start
    /// requests with.
    ///
    /// You are encouraged to use something like `rand::random()` to fill this field, except in
    /// situations where determinism/reproducibility is desired.
    pub source_selection_randomness_seed: u64,
}

/// Optimistic headers-only syncing.
pub struct OptimisticFullSync<TRq, TSrc> {
    /// Data structure containing the blocks.
    ///
    /// The user data, [`Block`], isn't used internally but stores information later reported
    /// to the user.
    chain: blocks_tree::NonFinalizedTree<Block>,

    /// Changes in the storage of the best block compared to the finalized block.
    /// The `BTreeMap`'s keys are storage keys, and its values are new values or `None` if the
    /// value has been erased from the storage.
    best_to_finalized_storage_diff: BTreeMap<Vec<u8>, Option<Vec<u8>>>,

    /// Compiled runtime code of the best block block.
    /// This field is a cache. As such, it will stay at `None` until this value has been needed
    /// for the first time.
    runtime_code_cache: Option<executor::WasmVmPrototype>,

    /// Cache of calculation for the storage trie of the best block.
    /// Providing this value when verifying a block considerably speeds up the verification.
    top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,

    /// Underlying helper. Manages sources and requests.
    /// Always `Some`, except during some temporary extractions.
    sync: Option<optimistic::OptimisticSync<TRq, TSrc, RequestSuccessBlock>>,
}

// TODO: doc
pub struct Block {
    /// Header of the block.
    pub header: header::Header,

    /// List of SCALE-encoded extrinsics that form the block's body.
    pub body: Vec<Vec<u8>>,

    /// SCALE-encoded justification of this block, if any.
    pub justification: Option<Vec<u8>>,

    /// Changes to the storage made by this block compared to its parent.
    pub storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,

    /// List of changes to the offchain storage that this block performs.
    pub offchain_storage_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,
}

impl<TRq, TSrc> OptimisticFullSync<TRq, TSrc> {
    /// Builds a new [`OptimisticFullSync`].
    pub fn new(config: Config) -> Self {
        let chain = blocks_tree::NonFinalizedTree::new(blocks_tree::Config {
            chain_information_config: config.chain_information_config,
            blocks_capacity: config.blocks_capacity,
        });

        let best_block_number = chain.best_block_header().number;

        OptimisticFullSync {
            chain,
            best_to_finalized_storage_diff: BTreeMap::new(),
            runtime_code_cache: None,
            top_trie_root_calculation_cache: None,
            sync: Some(optimistic::OptimisticSync::new(optimistic::Config {
                best_block_number,
                sources_capacity: config.sources_capacity,
                blocks_request_granularity: config.blocks_request_granularity,
                download_ahead_blocks: config.download_ahead_blocks,
                source_selection_randomness_seed: config.source_selection_randomness_seed,
            })),
        }
    }

    /// Builds a [`chain_information::ChainInformationRef`] struct corresponding to the current
    /// latest finalized block. Can later be used to reconstruct a chain.
    pub fn as_chain_information(&self) -> chain_information::ChainInformationRef {
        self.chain.as_chain_information()
    }

    /// Returns the number of the best block.
    ///
    /// > **Note**: This value is provided only for informative purposes. Keep in mind that this
    /// >           best block might be reverted in the future.
    pub fn best_block_number(&self) -> u64 {
        self.chain.best_block_header().number
    }

    /// Returns the hash of the best block.
    ///
    /// > **Note**: This value is provided only for informative purposes. Keep in mind that this
    /// >           best block might be reverted in the future.
    pub fn best_block_hash(&self) -> [u8; 32] {
        self.chain.best_block_hash()
    }

    /// Inform the [`OptimisticFullSync`] of a new potential source of blocks.
    pub fn add_source(&mut self, source: TSrc) -> SourceId {
        self.sync.as_mut().unwrap().add_source(source)
    }

    /// Inform the [`OptimisticFullSync`] that a source of blocks is no longer available.
    ///
    /// This automatically cancels all the requests that have been emitted for this source.
    /// This list of requests is returned as part of this function.
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is invalid.
    ///
    pub fn remove_source<'a>(
        &'a mut self,
        source: SourceId,
    ) -> (TSrc, impl Iterator<Item = (RequestId, TRq)> + 'a) {
        self.sync.as_mut().unwrap().remove_source(source)
    }

    /// Returns an iterator that extracts all requests that need to be started and requests that
    /// need to be cancelled.
    pub fn next_request_action(&mut self) -> Option<RequestAction<TRq, TSrc, RequestSuccessBlock>> {
        self.sync.as_mut().unwrap().next_request_action()
    }

    /// Update the [`OptimisticFullSync`] with the outcome of a request.
    ///
    /// Returns the user data that was associated to that request.
    ///
    /// # Panic
    ///
    /// Panics if the [`RequestId`] is invalid.
    ///
    pub fn finish_request<'a>(
        &'a mut self,
        request_id: RequestId,
        outcome: Result<impl Iterator<Item = RequestSuccessBlock>, RequestFail>,
    ) -> (TRq, FinishRequestOutcome<'a, TSrc>) {
        self.sync
            .as_mut()
            .unwrap()
            .finish_request(request_id, outcome)
    }

    /// Process a chunk of blocks in the queue of verification.
    ///
    /// This method takes ownership of the [`OptimisticFullSync`] and starts a verification
    /// process. The [`OptimisticFullSync`] is yielded back at the end of this process.
    pub fn process_one(mut self) -> ProcessOne<TRq, TSrc> {
        let sync = self.sync.take().unwrap();

        let to_process = match sync.process_one() {
            Ok(tp) => tp,
            Err(sync) => {
                self.sync = Some(sync);
                return ProcessOne::Idle { sync: self };
            }
        };

        self.chain.reserve(to_process.blocks.len());

        ProcessOne::from(
            Inner::Start(self.chain),
            ProcessOneShared {
                pending_encoded_justification: None,
                to_process,
                best_to_finalized_storage_diff: self.best_to_finalized_storage_diff,
                runtime_code_cache: self.runtime_code_cache,
                top_trie_root_calculation_cache: self.top_trie_root_calculation_cache,
                finalized_blocks: Vec::new(),
            },
        )
    }
}

pub struct RequestSuccessBlock {
    pub scale_encoded_header: Vec<u8>,
    pub scale_encoded_justification: Option<Vec<u8>>,
    pub scale_encoded_extrinsics: Vec<Vec<u8>>,
}

/// State of the processing of blocks.
pub enum ProcessOne<TRq, TSrc> {
    /// No processing is necessary.
    ///
    /// Calling [`OptimisticFullSync::process_one`] again is unnecessary.
    Idle {
        /// The state machine.
        /// The [`OptimisticFullSync::process_one`] method takes ownership of the
        /// [`OptimisticFullSync`]. This field yields it back.
        sync: OptimisticFullSync<TRq, TSrc>,
    },
    /// Processing is over.
    ///
    /// There might be more blocks remaining. Call [`OptimisticFullSync::process_one`] again.
    Finished {
        /// The state machine.
        /// The [`OptimisticFullSync::process_one`] method takes ownership of the
        /// [`OptimisticFullSync`]. This field yields it back.
        sync: OptimisticFullSync<TRq, TSrc>,
        /// Blocks that have been finalized after the verification.
        /// Ordered by increasing block number.
        // TODO: consider returning them one at a time?
        finalized_blocks: Vec<Block>,
    },
    /// A step in the processing has been completed.
    ///
    /// This variant is returned periodically in order to report on the advancement of the
    /// syncing. No action is required except call [`InProgress::resume`].
    InProgress {
        /// Object that resumes the processing.
        resume: InProgress<TRq, TSrc>,
        current_best_number: u64,
        current_best_hash: [u8; 32],
    },
    /// Loading a storage value of the finalized block is required in order to continue.
    FinalizedStorageGet(StorageGet<TRq, TSrc>),
    /// Fetching the list of keys of the finalized block with a given prefix is required in order
    /// to continue.
    FinalizedStoragePrefixKeys(StoragePrefixKeys<TRq, TSrc>),
    /// Fetching the key of the finalized block storage that follows a given one is required in
    /// order to continue.
    FinalizedStorageNextKey(StorageNextKey<TRq, TSrc>),
}

enum Inner {
    Start(blocks_tree::NonFinalizedTree<Block>),
    Step1(blocks_tree::BodyVerifyStep1<Block, vec::IntoIter<Vec<u8>>>),
    Step2(blocks_tree::BodyVerifyStep2<Block>),
}

struct ProcessOneShared<TRq, TSrc> {
    pending_encoded_justification: Option<Vec<u8>>,
    to_process: optimistic::ProcessOne<TRq, TSrc, RequestSuccessBlock>,
    best_to_finalized_storage_diff: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    runtime_code_cache: Option<executor::WasmVmPrototype>,
    top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,
    // TODO: make sure we're not throwing this away in case of error
    finalized_blocks: Vec<Block>,
}

impl<TRq, TSrc> ProcessOne<TRq, TSrc> {
    fn from(mut inner: Inner, mut shared: ProcessOneShared<TRq, TSrc>) -> Self {
        // This loop drives the process of the verification.
        // `inner` is updated at each iteration until a state that cannot be resolved internally
        // is found.
        'verif_steps: loop {
            match inner {
                Inner::Start(chain) => {
                    // Start of the verification process.
                    // The next block needs to be picked.

                    debug_assert!(shared.pending_encoded_justification.is_none());

                    if !shared.to_process.blocks.as_slice().is_empty() {
                        let next_block = shared.to_process.blocks.next().unwrap();
                        if let Some(justification) = next_block.scale_encoded_justification {
                            shared.pending_encoded_justification = Some(justification);
                        }
                        inner = Inner::Step1(chain.verify_body(
                            next_block.scale_encoded_header,
                            next_block.scale_encoded_extrinsics.into_iter(),
                        ));
                    } else {
                        debug_assert!(shared.to_process.blocks.as_slice().is_empty());
                        let sync = shared
                            .to_process
                            .report
                            .update_block_height(chain.best_block_header().number);
                        break ProcessOne::Finished {
                            sync: OptimisticFullSync {
                                chain,
                                best_to_finalized_storage_diff: shared
                                    .best_to_finalized_storage_diff,
                                runtime_code_cache: shared.runtime_code_cache,
                                top_trie_root_calculation_cache: shared
                                    .top_trie_root_calculation_cache,
                                sync: Some(sync),
                            },
                            finalized_blocks: shared.finalized_blocks,
                        };
                    }
                }

                Inner::Step1(blocks_tree::BodyVerifyStep1::InvalidHeader(chain, error)) => {
                    // TODO: DRY
                    let sync = shared
                        .to_process
                        .report
                        .reset_to_finalized(chain.finalized_block_header().number);
                    break ProcessOne::Finished {
                        sync: OptimisticFullSync {
                            chain,
                            best_to_finalized_storage_diff: Default::default(),
                            runtime_code_cache: None,
                            top_trie_root_calculation_cache: None,
                            sync: Some(sync),
                        },
                        finalized_blocks: shared.finalized_blocks,
                    };
                }

                Inner::Step1(blocks_tree::BodyVerifyStep1::Duplicate(chain)) => {
                    // TODO: DRY
                    let sync = shared
                        .to_process
                        .report
                        .reset_to_finalized(chain.finalized_block_header().number);
                    break ProcessOne::Finished {
                        sync: OptimisticFullSync {
                            chain,
                            best_to_finalized_storage_diff: Default::default(),
                            runtime_code_cache: None,
                            top_trie_root_calculation_cache: None,
                            sync: Some(sync),
                        },
                        finalized_blocks: shared.finalized_blocks,
                    };
                }

                Inner::Step1(blocks_tree::BodyVerifyStep1::BadParent { chain, .. }) => {
                    // TODO: DRY
                    let sync = shared
                        .to_process
                        .report
                        .reset_to_finalized(chain.finalized_block_header().number);
                    break ProcessOne::Finished {
                        sync: OptimisticFullSync {
                            chain,
                            best_to_finalized_storage_diff: shared.best_to_finalized_storage_diff,
                            runtime_code_cache: shared.runtime_code_cache,
                            top_trie_root_calculation_cache: None,
                            sync: Some(sync),
                        },
                        finalized_blocks: shared.finalized_blocks,
                    };
                }

                Inner::Step1(blocks_tree::BodyVerifyStep1::ParentRuntimeRequired(req)) => {
                    // The verification process is asking for a Wasm virtual machine containing
                    // the parent block's runtime.
                    //
                    // Since virtual machines are expensive to create, a re-usable virtual machine
                    // is maintained for the best block.
                    //
                    // The code below extracts that re-usable virtual machine with the intention
                    // to store it back after the verification is over.
                    let parent_runtime = match shared.runtime_code_cache.take() {
                        Some(r) => r,
                        None => {
                            // TODO: simplify code below
                            match (
                                shared.best_to_finalized_storage_diff.get(&b":code"[..]),
                                shared
                                    .best_to_finalized_storage_diff
                                    .get(&b":heappages"[..]),
                            ) {
                                (Some(wasm_code), Some(heap_pages)) => {
                                    let wasm_code =
                                        wasm_code.as_ref().expect("no runtime code?!?!"); // TODO: what to do?
                                    let heap_pages = u64::from_le_bytes(
                                        <[u8; 8]>::try_from(&heap_pages.as_ref().unwrap()[..])
                                            .unwrap(), // TODO: don't unwrap
                                    );
                                    executor::WasmVmPrototype::new(&wasm_code, heap_pages)
                                        .expect("invalid runtime code?!?!") // TODO: what to do?
                                }
                                (Some(wasm_code), None) => {
                                    return ProcessOne::FinalizedStorageGet(StorageGet {
                                        inner: StorageGetTarget::HeapPages(
                                            req,
                                            wasm_code.as_ref().unwrap().clone(),
                                        ), // TODO: don't unwrap
                                        shared,
                                    });
                                }
                                (None, Some(heap_pages)) => {
                                    let heap_pages = u64::from_le_bytes(
                                        <[u8; 8]>::try_from(&heap_pages.as_ref().unwrap()[..])
                                            .unwrap(), // TODO: don't unwrap
                                    );
                                    return ProcessOne::FinalizedStorageGet(StorageGet {
                                        inner: StorageGetTarget::Runtime(req, heap_pages), // TODO: don't unwrap
                                        shared,
                                    });
                                }
                                (None, None) => {
                                    // No cache has been found anywhere in the hierarchy.
                                    // The user needs to be asked for the storage entry containing the
                                    // runtime code.
                                    return ProcessOne::FinalizedStorageGet(StorageGet {
                                        inner: StorageGetTarget::HeapPagesAndRuntime(req),
                                        shared,
                                    });
                                }
                            }
                        }
                    };

                    inner = Inner::Step2(req.resume(
                        parent_runtime,
                        shared.top_trie_root_calculation_cache.take(),
                    ));
                }

                Inner::Step2(blocks_tree::BodyVerifyStep2::Finished {
                    storage_top_trie_changes,
                    offchain_storage_changes,
                    top_trie_root_calculation_cache,
                    parent_runtime,
                    result: Ok(success),
                }) => {
                    // Successfully verified block!
                    // Inserting it into the chain and updated all the caches.
                    if !storage_top_trie_changes.contains_key(&b":code"[..])
                        && !storage_top_trie_changes.contains_key(&b":heappages"[..])
                    {
                        shared.runtime_code_cache = Some(parent_runtime);
                    }
                    shared.top_trie_root_calculation_cache = Some(top_trie_root_calculation_cache);
                    for (key, value) in &storage_top_trie_changes {
                        shared
                            .best_to_finalized_storage_diff
                            .insert(key.clone(), value.clone());
                    }

                    let mut chain = {
                        let header = success.header().into();
                        success.insert(Block {
                            header,
                            body: Vec::new(), // TODO: // FIXME: wrong! dummy!
                            // Set to `Some` below if the justification check success.
                            justification: None,
                            storage_top_trie_changes,
                            offchain_storage_changes,
                        })
                    };

                    // `pending_encoded_verification` contains the justification (if any)
                    // corresponding to the block that has just been verified. Verifying the
                    // justification as well.
                    if let Some(justification) = shared.pending_encoded_justification.take() {
                        let mut apply = match chain.verify_justification(&justification) {
                            Ok(a) => a,
                            Err(_) => todo!(), // TODO:
                        };

                        assert!(apply.is_current_best_block()); // TODO: can legitimately fail in case of malicious node

                        // As part of the finalization, put the justification in the chain that's
                        // going to be reported to the user.
                        apply.block_user_data().justification = Some(justification);

                        // Applying the finalization and iterating over the now-finalized block.
                        // Since `apply()` returns the blocks in decreasing block number, we have
                        // to revert the list in order to get them in increasing block number
                        // instead.
                        // While this intermediary buffering is an overhead, the increased code
                        // complexity to avoid it is probably not worth the speed gain.
                        for block in apply.apply().collect::<Vec<_>>().into_iter().rev() {
                            shared.finalized_blocks.push(block);
                        }

                        // Since the best block is now the finalized block, reset the storage
                        // diff.
                        debug_assert!(chain.is_empty());
                        shared.best_to_finalized_storage_diff.clear();

                        // Since the verification process requires querying the finalized block
                        // storage from the user, we need to report changes to the finalized
                        // blocks to the user before we can continue.
                        let sync = shared
                            .to_process
                            .report
                            .update_block_height(chain.best_block_header().number);
                        break ProcessOne::Finished {
                            sync: OptimisticFullSync {
                                chain,
                                best_to_finalized_storage_diff: shared
                                    .best_to_finalized_storage_diff,
                                runtime_code_cache: shared.runtime_code_cache,
                                top_trie_root_calculation_cache: shared
                                    .top_trie_root_calculation_cache,
                                sync: Some(sync),
                            },
                            finalized_blocks: shared.finalized_blocks,
                        };
                    }

                    // Before looping again, report the progress to the user.
                    let current_best_hash = chain.best_block_hash();
                    let current_best_number = chain.best_block_header().number;
                    break ProcessOne::InProgress {
                        resume: InProgress {
                            inner: Inner::Start(chain),
                            shared,
                        },
                        current_best_hash,
                        current_best_number,
                    };
                }

                Inner::Step2(blocks_tree::BodyVerifyStep2::Finished {
                    result: Err(err), ..
                }) => todo!("verif failure"),

                Inner::Step2(blocks_tree::BodyVerifyStep2::StorageGet(mut req)) => {
                    // The underlying verification process is asking for a storage entry in the
                    // parent block.
                    //
                    // The [`OptimisticFullSync`] stores the difference between the best block's
                    // storage and the finalized block's storage.
                    // As such, the requested value is either found in one of this diff, in which
                    // case it can be returned immediately to continue the verification, or in
                    // the finalized block, in which case the user needs to be queried.
                    if let Some(value) =
                        shared.best_to_finalized_storage_diff.get(&req.key_as_vec())
                    {
                        inner = Inner::Step2(req.inject_value(value.as_ref().map(|v| &v[..])));
                        continue 'verif_steps;
                    }

                    // The value hasn't been found in any of the diffs, meaning that the storage
                    // value of the parent is the same as the one of the finalized block. The
                    // user needs to be queried.
                    break ProcessOne::FinalizedStorageGet(StorageGet {
                        inner: StorageGetTarget::Storage(req),
                        shared,
                    });
                }

                Inner::Step2(blocks_tree::BodyVerifyStep2::StorageNextKey(req)) => {
                    // The underlying verification process is asking for the key that follows
                    // the requested one.
                    break ProcessOne::FinalizedStorageNextKey(StorageNextKey {
                        inner: req,
                        shared,
                        key_overwrite: None,
                    });
                }

                Inner::Step2(blocks_tree::BodyVerifyStep2::StoragePrefixKeys(req)) => {
                    // The underlying verification process is asking for all the keys that start
                    // with a certain prefix.
                    // The first step is to ask the user for that information when it comes to
                    // the finalized block.
                    break ProcessOne::FinalizedStoragePrefixKeys(StoragePrefixKeys {
                        inner: req,
                        shared,
                    });
                }
            }
        }
    }
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet<TRq, TBl> {
    inner: StorageGetTarget,
    shared: ProcessOneShared<TRq, TBl>,
}

enum StorageGetTarget {
    Storage(blocks_tree::StorageGet<Block>),
    HeapPagesAndRuntime(blocks_tree::BodyVerifyRuntimeRequired<Block, vec::IntoIter<Vec<u8>>>),
    Runtime(
        blocks_tree::BodyVerifyRuntimeRequired<Block, vec::IntoIter<Vec<u8>>>,
        u64,
    ),
    HeapPages(
        blocks_tree::BodyVerifyRuntimeRequired<Block, vec::IntoIter<Vec<u8>>>,
        Vec<u8>,
    ),
}

impl<TRq, TBl> StorageGet<TRq, TBl> {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    pub fn key<'b>(&'b self) -> impl Iterator<Item = impl AsRef<[u8]> + 'b> + 'b {
        match &self.inner {
            StorageGetTarget::Storage(inner) => {
                either::Either::Left(inner.key().map(either::Either::Left))
            }
            StorageGetTarget::HeapPagesAndRuntime(_) | StorageGetTarget::HeapPages(_, _) => {
                either::Either::Right(iter::once(either::Either::Right(&b":heappages"[..])))
            }
            StorageGetTarget::Runtime(_, _) => {
                either::Either::Right(iter::once(either::Either::Right(&b":code"[..])))
            }
        }
    }

    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    ///
    /// This method is a shortcut for calling `key` and concatenating the returned slices.
    pub fn key_as_vec(&self) -> Vec<u8> {
        match &self.inner {
            StorageGetTarget::Storage(inner) => inner.key_as_vec(),
            StorageGetTarget::HeapPagesAndRuntime(_) | StorageGetTarget::HeapPages(_, _) => {
                b":heappages".to_vec()
            }
            StorageGetTarget::Runtime(_, _) => b":code".to_vec(),
        }
    }

    /// Injects the corresponding storage value.
    // TODO: change API, see execute_block::StorageGet
    pub fn inject_value(mut self, value: Option<&[u8]>) -> ProcessOne<TRq, TBl> {
        // TODO: simplify code inside here
        match self.inner {
            StorageGetTarget::Storage(inner) => {
                let inner = inner.inject_value(value);
                ProcessOne::from(Inner::Step2(inner), self.shared)
            }
            StorageGetTarget::HeapPagesAndRuntime(inner) => {
                let heap_pages = if let Some(value) = value {
                    u64::from_le_bytes(
                        <[u8; 8]>::try_from(&value[..]).unwrap(), // TODO: don't unwrap
                    )
                } else {
                    1024 // TODO: default heap pages
                };
                ProcessOne::FinalizedStorageGet(StorageGet {
                    inner: StorageGetTarget::Runtime(inner, heap_pages),
                    shared: self.shared,
                })
            }
            StorageGetTarget::Runtime(inner, heap_pages) => {
                let wasm_code = value.expect("no runtime code in storage?"); // TODO: ?!?!
                let wasm_vm = executor::WasmVmPrototype::new(wasm_code, heap_pages)
                    .expect("invalid runtime code?!?!"); // TODO: ?!?!
                let inner =
                    inner.resume(wasm_vm, self.shared.top_trie_root_calculation_cache.take());
                ProcessOne::from(Inner::Step2(inner), self.shared)
            }
            StorageGetTarget::HeapPages(inner, wasm_code) => {
                let heap_pages = if let Some(value) = value {
                    u64::from_le_bytes(
                        <[u8; 8]>::try_from(&value[..]).unwrap(), // TODO: don't unwrap
                    )
                } else {
                    1024 // TODO: default heap pages
                };
                let wasm_vm = executor::WasmVmPrototype::new(&wasm_code, heap_pages)
                    .expect("invalid runtime code?!?!"); // TODO: ?!?!
                let inner =
                    inner.resume(wasm_vm, self.shared.top_trie_root_calculation_cache.take());
                ProcessOne::from(Inner::Step2(inner), self.shared)
            }
        }
    }
}

/// Fetching the list of keys with a given prefix is required in order to continue.
#[must_use]
pub struct StoragePrefixKeys<TRq, TBl> {
    inner: blocks_tree::StoragePrefixKeys<Block>,
    shared: ProcessOneShared<TRq, TBl>,
}

impl<TRq, TBl> StoragePrefixKeys<TRq, TBl> {
    /// Returns the prefix whose keys to load.
    pub fn prefix(&self) -> &[u8] {
        self.inner.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(self, keys: impl Iterator<Item = impl AsRef<[u8]>>) -> ProcessOne<TRq, TBl> {
        let mut keys = keys
            .map(|k| k.as_ref().to_owned())
            .collect::<HashSet<_, fnv::FnvBuildHasher>>();

        let prefix = self.inner.prefix();
        for (k, v) in self
            .shared
            .best_to_finalized_storage_diff
            .range(prefix.to_owned()..)
            .take_while(|(k, _)| k.starts_with(prefix))
        {
            if v.is_some() {
                keys.insert(k.clone());
            } else {
                keys.remove(k);
            }
        }

        let inner = self.inner.inject_keys(keys.iter());
        ProcessOne::from(Inner::Step2(inner), self.shared)
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct StorageNextKey<TRq, TBl> {
    inner: blocks_tree::StorageNextKey<Block>,
    shared: ProcessOneShared<TRq, TBl>,

    /// If `Some`, ask for the key inside of this field rather than the one of `inner`. Used in
    /// corner-case situations where the key provided by the user has been erased from storage.
    key_overwrite: Option<Vec<u8>>,
}

impl<TRq, TBl> StorageNextKey<TRq, TBl> {
    /// Returns the key whose next key must be passed back.
    pub fn key(&self) -> &[u8] {
        if let Some(key_overwrite) = &self.key_overwrite {
            key_overwrite
        } else {
            self.inner.key()
        }
    }

    /// Injects the key.
    ///
    /// # Panic
    ///
    /// Panics if the key passed as parameter isn't strictly superior to the requested key.
    ///
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> ProcessOne<TRq, TBl> {
        let key = key.as_ref().map(|k| k.as_ref());

        // The key provided by the user as parameter is the next key in the storage of the
        // finalized block.
        // `best_to_finalized_storage_diff` needs to be taken into account in order to provide
        // the next key in the best block instead.

        let requested_key = if let Some(key_overwrite) = &self.key_overwrite {
            key_overwrite
        } else {
            self.inner.key()
        };

        if let Some(key) = key {
            assert!(key > requested_key);
        }

        let in_diff = self
            .shared
            .best_to_finalized_storage_diff
            .range(requested_key.to_vec()..) // TODO: don't use to_vec()
            .map(|(k, v)| (k, v.is_some()))
            .filter(|(k, _)| &***k > requested_key)
            .next();

        let outcome = match (key, in_diff) {
            (Some(a), Some((b, true))) if a <= &b[..] => Some(a),
            (Some(a), Some((b, false))) if a < &b[..] => Some(a),
            (Some(a), Some((b, false))) => {
                debug_assert!(a >= &b[..]);
                debug_assert_ne!(&b[..], requested_key);

                // The next key according to the finalized block storage has been erased since
                // then. It is necessary to ask the user again, this time for the key after the
                // one that has been erased.
                // This `clone()` is necessary, as `b` borrows from
                // `self.shared.best_to_finalized_storage_diff`.
                let key_overwrite = Some(b.clone());
                return ProcessOne::FinalizedStorageNextKey(StorageNextKey {
                    inner: self.inner,
                    shared: self.shared,
                    key_overwrite,
                });
            }
            (Some(a), Some((b, true))) => {
                debug_assert!(a >= &b[..]);
                Some(&b[..])
            }

            (Some(a), None) => Some(a),
            (None, Some((b, _))) => Some(&b[..]),
            (None, None) => None,
        };

        let inner = self.inner.inject_key(outcome);
        ProcessOne::from(Inner::Step2(inner), self.shared)
    }
}

/// Resume the processing after a [`ProcessOne::InProgress`].
#[must_use]
pub struct InProgress<TRq, TBl> {
    inner: Inner,
    shared: ProcessOneShared<TRq, TBl>,
}

impl<TRq, TBl> InProgress<TRq, TBl> {
    /// Resume the processing.
    pub fn resume(self) -> ProcessOne<TRq, TBl> {
        ProcessOne::from(self.inner, self.shared)
    }
}
