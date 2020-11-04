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

//! Optimistic headers-only syncing.
//!
//! Optimistic syncing consists in assuming that all sources of blocks form the same chain. A
//! query for blocks is performed on a random source, and the response is verified. If it turns
//! out that the source doesn't belong to the same chain (or is malicious), a different source
//! is tried.
//!
//! While this syncing strategy is very simplistic, it is the most effective when the majority of
//! sources are well-behaved, which is normally the case.
//!
//! The [`OptimisticHeadersSync`] makes it possible to sync the finalized blocks of a chain, but
//! not the non-finalized blocks.

// TODO: document usage

use super::super::{blocks_tree, chain_information};
use super::optimistic;

use alloc::vec::Vec;
use core::{convert::TryFrom as _, num::NonZeroU32, time::Duration};

pub use optimistic::{
    FinishRequestOutcome, RequestAction, RequestFail, RequestId, SourceId, Start,
};

/// Configuration for the [`OptimisticHeadersSync`].
#[derive(Debug)]
pub struct Config {
    /// Information about the latest finalized block and its ancestors.
    pub chain_information: chain_information::ChainInformation,

    /// Pre-allocated capacity for the number of block sources.
    pub sources_capacity: usize,

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
pub struct OptimisticHeadersSync<TRq, TSrc> {
    /// Configuration for the actual finalized block of the chain.
    /// Used if the `chain` field needs to be recreated.
    finalized_chain_information: blocks_tree::Config,

    /// Chain containing the state necessary to verify blocks.
    ///
    /// Important: the finalized block in this chain is not the actual finalized blocks. In order
    /// to reduce memory consumption, every block that isn't the best block is discarded. This is
    /// done by considering the best block as finalized even though it's not actually.
    chain: blocks_tree::NonFinalizedTree<()>,

    /// Underlying helper. Manages sources and requests.
    /// Always `Some`, except during some temporary extractions.
    sync: Option<optimistic::OptimisticSync<TRq, TSrc, RequestSuccessBlock>>,
}

impl<TRq, TSrc> OptimisticHeadersSync<TRq, TSrc> {
    /// Builds a new [`OptimisticHeadersSync`].
    pub fn new(config: Config) -> Self {
        let blocks_tree_config = blocks_tree::Config {
            chain_information: config.chain_information,
            blocks_capacity: usize::try_from(config.blocks_request_granularity.get())
                .unwrap_or(usize::max_value()),
        };

        let chain = blocks_tree::NonFinalizedTree::new(blocks_tree_config.clone());
        let best_block_number = chain.best_block_header().number;

        OptimisticHeadersSync {
            finalized_chain_information: blocks_tree_config,
            chain,
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
        (&self.finalized_chain_information.chain_information).into()
    }

    /// Inform the [`OptimisticHeadersSync`] of a new potential source of blocks.
    pub fn add_source(&mut self, source: TSrc) -> SourceId {
        self.sync.as_mut().unwrap().add_source(source)
    }

    /// Inform the [`OptimisticHeadersSync`] that a source of blocks is no longer available.
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

    /// Update the [`OptimisticHeadersSync`] with the outcome of a request.
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

    /// Process a batch of blocks in the queue of verification.
    ///
    /// This method processes a batch of blocks passed earlier to
    /// [`OptimisticHeadersSync::finish_request`]. Only one batch is process rather than the
    /// entire queue, in order to not block the CPU for too long in unpredictable ways.
    ///
    /// It is encouraged to call this method multiple times in a row until
    /// [`ProcessOneOutcome::Idle`] is returned, interleaving any necessary high-priority
    /// operations (e.g. processing network sockets) in-between two calls.
    ///
    /// Must be passed the current UNIX time in order to verify that the blocks don't pretend to
    /// come from the future.
    pub fn process_one(&mut self, now_from_unix_epoch: Duration) -> ProcessOneOutcome {
        let mut to_process = match self.sync.take().unwrap().process_one() {
            Ok(tp) => tp,
            Err(sync) => {
                self.sync = Some(sync);
                return ProcessOneOutcome::Idle;
            }
        };

        self.chain.reserve(to_process.blocks.len());

        // Verify each block one by one.
        // The loop stops whenever something unexpected (such as a verification error) happens.
        let mut finalized_update = false;
        let mut has_error = None;
        for block in to_process.blocks {
            match self
                .chain
                .verify_header(block.scale_encoded_header.into(), now_from_unix_epoch)
            {
                Ok(blocks_tree::HeaderVerifySuccess::Insert {
                    block_height,
                    is_new_best,
                    insert,
                }) => {
                    if !is_new_best {
                        debug_assert!(has_error.is_none());
                        has_error = Some(ResetCause::NonCanonical);
                        break;
                    }
                    if block_height != to_process.expected_block_height {
                        debug_assert!(has_error.is_none());
                        has_error = Some(ResetCause::UnexpectedBlockNumber {
                            expected: to_process.expected_block_height,
                            actual: block_height,
                        });
                        break;
                    }

                    insert.insert(());
                }
                Ok(blocks_tree::HeaderVerifySuccess::Duplicate) => {
                    debug_assert!(has_error.is_none());
                    has_error = Some(ResetCause::NonCanonical);
                    break;
                }
                Err(err) => {
                    debug_assert!(has_error.is_none());
                    has_error = Some(ResetCause::HeaderError(err));
                    break;
                }
            }

            if let Some(justification) = block.scale_encoded_justification {
                match self.chain.verify_justification(justification.as_ref()) {
                    Ok(apply) => {
                        apply.apply();
                        finalized_update = true;
                        self.finalized_chain_information.chain_information =
                            self.chain.as_chain_information().into();
                    }
                    Err(err) => {
                        debug_assert!(has_error.is_none());
                        has_error = Some(ResetCause::JustificationError(err));
                        break;
                    }
                }
            }

            to_process.expected_block_height += 1;
        }

        // In case something unexpected happens, such as an invalid block, there is unfortunately
        // no easy way to know which node is misbehaving. Blocks and justifications are valid
        // only in the context of a specific chain, and it is possible that the presumably invalid
        // block is invalid only because of an earlier block.
        //
        // Example. Let's say a source A sends back blocks 1 to 128 and a source B sends back
        // blocks 129 to 256. If source A sends back a block 128 which happens to be from a
        // non-canonical fork, then block 129 will fail to import. It is however not B's fault,
        // but A's fault.
        //
        // Consequently, if something unexpected happens, the strategy employed is to clear any
        // non-finalized block, cancel all requests in progress, and restart from the finalized
        // block. No attempt is made to punish nodes.
        if let Some(has_error) = has_error {
            // As documented, the `chain` field does not contain the *actual* finalized block.
            // Instead, a new chain is recreated in order to reset to the actual finalized block.
            self.chain =
                blocks_tree::NonFinalizedTree::new(self.finalized_chain_information.clone());
            let sync = to_process
                .report
                .reset_to_finalized(self.chain.finalized_block_header().number);
            self.sync = Some(sync);
            return ProcessOneOutcome::Reset {
                reason: has_error,
                new_best_block_number: self.chain.best_block_header().number,
                new_best_block_hash: self.chain.best_block_hash(),
            };
        }

        let sync = to_process
            .report
            .update_block_height(self.chain.best_block_header().number);
        self.sync = Some(sync);

        // As documented, the finalized block tracked by the `chain` field is not the actual
        // finalized block. The optimistic sync state machine tracks the actual finalized block
        // separately, and the finalized block of `chain` is always set to the best block.
        let best_block_hash = self.chain.best_block_hash();
        // `set_finalized_block` will error if best block == finalized block, which will
        // frequently and legitimately happen. As such, we ignore errors.
        let _ = self.chain.set_finalized_block(&best_block_hash);

        // Success! 🎉
        ProcessOneOutcome::Updated {
            best_block_hash,
            best_block_number: self.chain.best_block_header().number,
            finalized_block: if finalized_update {
                let number = self
                    .finalized_chain_information
                    .chain_information
                    .finalized_block_header
                    .number;
                let hash = self
                    .finalized_chain_information
                    .chain_information
                    .finalized_block_header
                    .hash();
                Some((number, hash))
            } else {
                None
            },
        }
    }
}

/// Single block in the outcome of a request. A list of these must be passed to
/// [`OptimisticHeadersSync::finish_request`].
#[derive(Debug)]
pub struct RequestSuccessBlock {
    /// SCALE-encoded block header.
    pub scale_encoded_header: Vec<u8>,
    /// SCALE-encoded justification of this block, or `None` if none is available.
    pub scale_encoded_justification: Option<Vec<u8>>,
}

/// Outcome of calling [`OptimisticHeadersSync::process_one`].
#[derive(Debug)]
pub enum ProcessOneOutcome {
    /// There was nothing to do.
    Idle,

    /// An issue happened when verifying a block or justification, resulting in resetting the
    /// chain to the latest finalized block.
    ///
    /// > **Note**: The latest finalized block might be a block imported during the same
    /// >           operation.
    Reset {
        /// Problem that happened and caused the reset.
        reason: ResetCause,
        /// Number of the new best block. Identical to the number of the finalized block.
        new_best_block_number: u64,
        /// Hash of the new best block. Identical to the hash of the finalized block.
        new_best_block_hash: [u8; 32],
    },

    /// One or more blocks have been successfully imported.
    Updated {
        /// Number of the new best block.
        best_block_number: u64,
        /// Hash of the new best block.
        best_block_hash: [u8; 32],
        /// Number and hash of the finalized block. `None` if the finalized block hasn't changed.
        finalized_block: Option<(u64, [u8; 32])>,
    },
}

/// Problem that happened and caused the reset.
#[derive(Debug, derive_more::Display)]
pub enum ResetCause {
    /// Error while verifying a justification.
    JustificationError(blocks_tree::JustificationVerifyError),
    /// Error while verifying a header.
    HeaderError(blocks_tree::HeaderVerifyError),
    /// Received block isn't a child of the current best block.
    NonCanonical,
    /// Received block number doesn't match expected number.
    #[display(fmt = "Received block height doesn't match expected number")]
    UnexpectedBlockNumber {
        /// Number of the block that was expected to be verified next.
        expected: u64,
        /// Number of the block that was verified.
        actual: u64,
    },
}
