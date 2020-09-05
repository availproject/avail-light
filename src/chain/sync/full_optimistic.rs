//! Optimistic header and body syncing.
//!
//! This state machine builds, from a set of sources, a fully verified chain of blocks headers
//! and bodies.
//!
//! In addition to managing the sources, using [`OptimisticFullSync`] also requires holding the
//! storage of the latest finalized block.

// TODO: document better

use super::super::{blocks_tree, chain_information};
use super::optimistic;
use crate::{executor, verify::babe};

use alloc::{collections::BTreeMap, vec};
use core::num::NonZeroU32;

pub use optimistic::{
    FinishRequestOutcome, RequestAction, RequestFail, RequestId, SourceId, Start,
};

/// Configuration for the [`OptimisticFullSync`].
#[derive(Debug)]
pub struct Config {
    /// Information about the latest finalized block and its ancestors.
    pub chain_information: chain_information::ChainInformation,

    /// Configuration for BABE, retreived from the genesis block.
    pub babe_genesis_config: babe::BabeGenesisConfiguration,

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
    // TODO: doc
    chain: blocks_tree::NonFinalizedTree<Block>,

    /// Compiled runtime code of the finalized block.
    /// This field is a cache. As such, it will stay at `None` until this value has been needed
    /// for the first time.
    finalized_runtime_code_cache: Option<executor::WasmVmPrototype>,

    /// Underlying helper. Manages sources and requests.
    /// Always `Some`, except during some temporary extractions.
    sync: Option<optimistic::OptimisticSync<TRq, TSrc, RequestSuccessBlock>>,
}

struct Block {
    /// Changes in the storage compared to the parent block.
    storage_parent_diff: BTreeMap<Vec<u8>, Vec<u8>>,

    /// Compiled runtime code of this block.
    /// This field is a cache. As such, it will stay at `None` until this value has been needed
    /// for the first time.
    /// Can only ever contain `Some` if the runtime code has been modified.
    runtime_code_cache: Option<executor::WasmVmPrototype>,
}

impl<TRq, TSrc> OptimisticFullSync<TRq, TSrc> {
    /// Builds a new [`OptimisticFullSync`].
    pub fn new(config: Config) -> Self {
        let chain = blocks_tree::NonFinalizedTree::new(blocks_tree::Config {
            chain_information: config.chain_information.clone(),
            babe_genesis_config: config.babe_genesis_config,
            blocks_capacity: config.blocks_capacity,
        });

        let best_block_number = chain.best_block_header().number;

        OptimisticFullSync {
            chain,
            finalized_runtime_code_cache: None,
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
    // TODO: better return value
    pub fn process_one(mut self) -> ProcessOne<TRq, TSrc> {
        // TODO: don't unwrap
        let to_process = self
            .sync
            .take()
            .unwrap()
            .process_one()
            .unwrap_or_else(|_| panic!());

        self.chain.reserve(to_process.blocks.len());

        ProcessOne::from(
            Inner::Start(self.chain),
            ProcessOneShared {
                to_process,
                finalized_runtime_code_cache: self.finalized_runtime_code_cache,
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
    /// Processing is over.
    Finished {
        /// The state machine.
        /// The [`OptimisticFullSync::process_one`] method takes ownership of the
        /// [`OptimisticFullSync`]. This field yields it back.
        sync: OptimisticFullSync<TRq, TSrc>,
        // TODO: finalized_advance: ,
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

impl<TRq, TSrc> ProcessOne<TRq, TSrc> {
    fn from(mut inner: Inner, mut shared: ProcessOneShared<TRq, TSrc>) -> Self {
        loop {
            match inner {
                Inner::Start(chain) if !shared.to_process.blocks.as_slice().is_empty() => {
                    let next_block = shared.to_process.blocks.next().unwrap();
                    inner = Inner::Step1(chain.verify_body(
                        next_block.scale_encoded_header,
                        next_block.scale_encoded_extrinsics.into_iter(),
                    ));
                }
                Inner::Start(chain) => {
                    // TODO: verify justification

                    debug_assert!(shared.to_process.blocks.as_slice().is_empty());
                    let sync = shared
                        .to_process
                        .report
                        .update_block_height(chain.best_block_header().number);
                    break ProcessOne::Finished {
                        sync: OptimisticFullSync {
                            chain,
                            finalized_runtime_code_cache: shared.finalized_runtime_code_cache,
                            sync: Some(sync),
                        },
                    };
                }
                Inner::Step1(blocks_tree::BodyVerifyStep1::InvalidHeader(chain, error)) => {
                    let sync = shared
                        .to_process
                        .report
                        .reset_to_finalized(chain.finalized_block_header().number);
                    break ProcessOne::Finished {
                        sync: OptimisticFullSync {
                            chain,
                            finalized_runtime_code_cache: shared.finalized_runtime_code_cache,
                            sync: Some(sync),
                        },
                    };
                }
                Inner::Step1(blocks_tree::BodyVerifyStep1::Duplicate(chain)) => {
                    let sync = shared
                        .to_process
                        .report
                        .reset_to_finalized(chain.finalized_block_header().number);
                    break ProcessOne::Finished {
                        sync: OptimisticFullSync {
                            chain,
                            finalized_runtime_code_cache: shared.finalized_runtime_code_cache,
                            sync: Some(sync),
                        },
                    };
                }
                Inner::Step1(blocks_tree::BodyVerifyStep1::BadParent { chain, .. }) => {
                    let sync = shared
                        .to_process
                        .report
                        .reset_to_finalized(chain.finalized_block_header().number);
                    break ProcessOne::Finished {
                        sync: OptimisticFullSync {
                            chain,
                            finalized_runtime_code_cache: shared.finalized_runtime_code_cache,
                            sync: Some(sync),
                        },
                    };
                }
                Inner::Step1(blocks_tree::BodyVerifyStep1::ParentRuntimeRequired(rt_req)) => {
                    inner = Inner::Step2(rt_req.resume(todo!()));
                }
                Inner::Step2(blocks_tree::BodyVerifyStep2::Finished { parent_runtime }) => {
                    // TODO: put back runtime and all
                    inner = Inner::Start(todo!());
                }
                Inner::Step2(blocks_tree::BodyVerifyStep2::StorageGet(inner)) => {
                    // TODO: no; must look through hierarchy
                    break ProcessOne::FinalizedStorageGet(StorageGet { inner, shared });
                }
                Inner::Step2(blocks_tree::BodyVerifyStep2::StorageNextKey(inner)) => {
                    // TODO: no; must look through hierarchy
                    break ProcessOne::FinalizedStorageNextKey(StorageNextKey { inner, shared });
                }
                Inner::Step2(blocks_tree::BodyVerifyStep2::StoragePrefixKeys(inner)) => {
                    // TODO: no; must look through hierarchy
                    break ProcessOne::FinalizedStoragePrefixKeys(StoragePrefixKeys {
                        inner,
                        shared,
                    });
                }
            }
        }
    }
}

enum Inner {
    Step1(blocks_tree::BodyVerifyStep1<Block, vec::IntoIter<Vec<u8>>>),
    Step2(blocks_tree::BodyVerifyStep2<Block>),
    Start(blocks_tree::NonFinalizedTree<Block>),
}

struct ProcessOneShared<TRq, TSrc> {
    to_process: optimistic::ProcessOne<TRq, TSrc, RequestSuccessBlock>,
    finalized_runtime_code_cache: Option<executor::WasmVmPrototype>,
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet<TRq, TBl> {
    inner: blocks_tree::StorageGet<Block>,
    shared: ProcessOneShared<TRq, TBl>,
}

impl<TRq, TBl> StorageGet<TRq, TBl> {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    // TODO: shouldn't be mut
    pub fn key<'b>(&'b mut self) -> impl Iterator<Item = impl AsRef<[u8]> + 'b> + 'b {
        self.inner.key()
    }

    /// Injects the corresponding storage value.
    // TODO: change API, see unsealed::StorageGet
    pub fn inject_value(self, value: Option<&[u8]>) -> ProcessOne<TRq, TBl> {
        let inner = self.inner.inject_value(value);
        ProcessOne::from(Inner::Step2(inner), self.shared)
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
    // TODO: don't take &mut mut but &self
    pub fn prefix(&mut self) -> &[u8] {
        self.inner.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(self, keys: impl Iterator<Item = impl AsRef<[u8]>>) -> ProcessOne<TRq, TBl> {
        let inner = self.inner.inject_keys(keys);
        ProcessOne::from(Inner::Step2(inner), self.shared)
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct StorageNextKey<TRq, TBl> {
    inner: blocks_tree::StorageNextKey<Block>,
    shared: ProcessOneShared<TRq, TBl>,
}

impl<TRq, TBl> StorageNextKey<TRq, TBl> {
    /// Returns the key whose next key must be passed back.
    // TODO: don't take &mut mut but &self
    pub fn key(&mut self) -> &[u8] {
        self.inner.key()
    }

    /// Injects the key.
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> ProcessOne<TRq, TBl> {
        let inner = self.inner.inject_key(key);
        ProcessOne::from(Inner::Step2(inner), self.shared)
    }
}
