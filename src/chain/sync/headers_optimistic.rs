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
// TODO: the quality of this module's code is sub-par compared to what we want

use crate::verify::babe;
use super::super::{blocks_tree, chain_information};
use super::optimistic;

use core::num::NonZeroU32;

pub use optimistic::{
    FinishRequestOutcome, RequestAction, RequestFail, RequestId, RequestSuccessBlock, SourceId,
    Start,
};

/// Configuration for the [`OptimisticHeadersSync`].
#[derive(Debug)]
pub struct Config {
    /// Information about the latest finalized block and its ancestors.
    pub chain_information: chain_information::ChainInformation,

    /// Configuration for BABE, retreived from the genesis block.
    pub babe_genesis_config: babe::BabeGenesisConfiguration,

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
    // TODO: to reduce memory usage, keep the finalized block of this chain close to its best block, and maintain a `ChainInformation` in parallel of the actual finalized block
    chain: blocks_tree::NonFinalizedTree<()>,

    sync: optimistic::OptimisticSync<TRq, TSrc>,
}

impl<TRq, TSrc> OptimisticHeadersSync<TRq, TSrc> {
    /// Builds a new [`OptimisticHeadersSync`].
    pub fn new(config: Config) -> Self {
        let chain = blocks_tree::NonFinalizedTree::new(blocks_tree::Config {
            chain_information: config.chain_information,
            babe_genesis_config: config.babe_genesis_config,
            blocks_capacity: 1024, // TODO: right now an arbitrary value; decrease to blocks_request_granularity when doing the change where we always consider the best block as finalized
        });

        let best_block_number = chain.best_block_header().number;

        OptimisticHeadersSync {
            chain,
            sync: optimistic::OptimisticSync::new(optimistic::Config {
                best_block_number,
                sources_capacity: config.sources_capacity,
                blocks_request_granularity: config.blocks_request_granularity,
                download_ahead_blocks: config.download_ahead_blocks,
                source_selection_randomness_seed: config.source_selection_randomness_seed,
            }),
        }
    }

    /// Builds a [`chain_information::ChainInformationRef`] struct corresponding to the current
    /// latest finalized block. Can later be used to reconstruct a chain.
    pub fn as_chain_information(&self) -> chain_information::ChainInformationRef {
        self.chain.as_chain_information()
    }

    /// Inform the [`OptimisticHeadersSync`] of a new potential source of blocks.
    pub fn add_source(&mut self, source: TSrc) -> SourceId {
        self.sync.add_source(source)
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
        self.sync.remove_source(source)
    }

    /// Returns an iterator that extracts all requests that need to be started and requests that
    /// need to be cancelled.
    pub fn next_request_action(&mut self) -> Option<RequestAction<TRq, TSrc>> {
        self.sync.next_request_action()
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
        self.sync.finish_request(request_id, outcome)
    }

    /// Process a single block in the queue of verification.
    // TODO: better return value
    pub fn process_one(&mut self) -> Option<ChainStateUpdate> {
        let mut to_process = self.sync.process_one()?;

        self.chain.reserve(to_process.blocks.len());

        // Verify each block one by one.
        //
        // In case something unexpected happens, such as an invalid block, there is unfortunately
        // no easy way to know which node is misbehaving. Blocks and justifications are valid
        // only in the context of a specific chain, and it is possible that the presumably invalid
        // block is invalid only because of an earlier block.
        //
        // Consequently, if something unexpected happens, the strategy employed is to clear any
        // non-finalized block, cancel all requests in progress, and restart from the finalized
        // block.
        for block in to_process.blocks {
            match self.chain.verify_header(block.scale_encoded_header.into()) {
                Ok(blocks_tree::HeaderVerifySuccess::Insert {
                    block_height,
                    is_new_best,
                    insert,
                }) => {
                    if !is_new_best || block_height != to_process.expected_block_height {
                        // Something unexpected happened. See above.
                        // TODO: report with an event that this has happened
                        self.chain.clear();
                        to_process
                            .report
                            .reset_to_finalized(self.chain.finalized_block_header().number);
                        panic!() // TODO: report with an event that this has happened
                    }

                    insert.insert(());
                }
                Ok(blocks_tree::HeaderVerifySuccess::Duplicate) => {
                    // Something unexpected happened. See above.
                    // TODO: report with an event that this has happened
                    self.chain.clear();
                    to_process
                        .report
                        .reset_to_finalized(self.chain.finalized_block_header().number);
                    panic!() // TODO: report with an event that this has happened
                }
                Err(err) => {
                    // Something unexpected happened. See above.
                    // TODO: report with an event that this has happened
                    self.chain.clear();
                    to_process
                        .report
                        .reset_to_finalized(self.chain.finalized_block_header().number);
                    panic!() // TODO: report with an event that this has happened
                }
            }

            if let Some(justification) = block.scale_encoded_justification {
                match self.chain.verify_justification(justification.as_ref()) {
                    Ok(apply) => apply.apply(),
                    Err(err) => {
                        // Something unexpected happened. See above.
                        // TODO: report with an event that this has happened
                        self.chain.clear();
                        to_process
                            .report
                            .reset_to_finalized(self.chain.finalized_block_header().number);
                        panic!() // TODO: report with an event that this has happened
                    }
                }
            }

            to_process.expected_block_height += 1;
        }

        to_process
            .report
            .update_block_height(self.chain.best_block_header().number);

        // TODO: consider finer granularity in report
        Some(ChainStateUpdate {
            finalized_block_hash: self.chain.finalized_block_hash(),
            finalized_block_number: self.chain.finalized_block_header().number,
            best_block_hash: self.chain.best_block_hash(),
            best_block_number: self.chain.best_block_header().number,
        })
    }
}

#[derive(Debug)]
pub struct ChainStateUpdate {
    pub best_block_hash: [u8; 32],
    pub best_block_number: u64,
    pub finalized_block_hash: [u8; 32],
    pub finalized_block_number: u64,
}
