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

use super::super::blocks_tree;

use alloc::collections::VecDeque;
use core::{
    cmp,
    convert::TryFrom as _,
    iter, mem,
    num::{NonZeroU32, NonZeroU64},
};

/// Configuration for the [`OptimisticHeadersSync`].
#[derive(Debug)]
pub struct Config {
    /// Configuration for the tree of blocks.
    pub chain_config: blocks_tree::Config,

    /// Pre-allocated capacity for the number of block sources.
    pub sources_capacity: usize,

    /// Maximum number of blocks returned by a response.
    ///
    /// > **Note**: If blocks are requested from the network, this should match the network
    /// >           protocol enforced limit.
    pub blocks_request_granularity: NonZeroU32,

    /// Number of blocks to download ahead of the finalized block.
    ///
    /// Whenever the latest finalized block is updated, the state machine will start block
    /// requests for the block `finalized_block_height + download_ahead_blocks` and all its
    /// ancestors. Considering that requesting blocks has some latency, downloading blocks ahead
    /// of time ensures that verification isn't blocked waiting for a request to be finished.
    ///
    /// The ideal value here depends on the speed of blocks verification speed and latency of
    /// block requests.
    pub download_ahead_blocks: u32,
}

/// Optimistic headers-only syncing.
pub struct OptimisticHeadersSync<TSrc> {
    chain: blocks_tree::NonFinalizedTree<()>,

    /// List of sources of blocks.
    sources: slab::Slab<Source<TSrc>>,

    cancelling_requests: bool,

    /// Queue of block requests, either to be started, in progress, or completed.
    verification_queue: VecDeque<VerificationQueueEntry>,

    /// Value passed by [`Config::blocks_request_granularity`].
    blocks_request_granularity: NonZeroU32,

    /// Value passed by [`Config::download_ahead_blocks`].
    download_ahead_blocks: u32,

    /// Identifier to assign to the next request.
    next_request_id: RequestId,
}

struct VerificationQueueEntry {
    block_height: NonZeroU64,
    ty: VerificationQueueEntryTy,
}

struct Source<TSrc> {
    user_data: TSrc,
    banned: bool, // TODO: ban shouldn't be held forever
}

enum VerificationQueueEntryTy {
    Missing,
    Requested {
        id: RequestId,
        // Index of this source within [`OptimisticHeadersSync::sources`].
        source: usize,
    },
    Queued(Vec<RequestSuccessBlock>),
}

impl<TSrc> OptimisticHeadersSync<TSrc>
where
    TSrc: Eq, // TODO: no
{
    /// Builds a new [`OptimisticHeadersSync`].
    pub fn new(config: Config) -> Self {
        OptimisticHeadersSync {
            chain: blocks_tree::NonFinalizedTree::new(config.chain_config),
            sources: slab::Slab::with_capacity(config.sources_capacity),
            cancelling_requests: false,
            verification_queue: VecDeque::with_capacity(
                usize::try_from(
                    config.download_ahead_blocks / config.blocks_request_granularity.get(),
                )
                .unwrap()
                .saturating_add(1),
            ),
            blocks_request_granularity: config.blocks_request_granularity,
            download_ahead_blocks: config.download_ahead_blocks,
            next_request_id: RequestId(0),
        }
    }

    /// Inform the [`OptimisticHeadersSync`] of a new potential source of blocks.
    pub fn add_source(&mut self, source: TSrc) -> Result<(), AddSourceError> {
        // TODO: consider not checking this; needs some API rethinking
        if self.sources.iter().any(|(_, s)| s.user_data == source) {
            return Err(AddSourceError::Duplicate);
        }

        self.sources.insert(Source {
            user_data: source,
            banned: false,
        });

        Ok(())
    }

    /// Inform the [`OptimisticHeadersSync`] that a source of blocks is no longer available.
    ///
    /// This automatically cancels all the [`RequestId`]s that have been emitted for this source.
    /// This list of [`RequestId`]s is returned as part of this function.
    pub fn remove_source(
        &mut self,
        source: &TSrc,
    ) -> Result<impl Iterator<Item = RequestId>, RemoveSourceError> {
        if let Some(index) = self
            .sources
            .iter()
            .find(|(_, s)| &s.user_data == source)
            .map(|(i, _)| i)
        {
            self.sources.remove(index);
            Ok(iter::empty()) // TODO:
        } else {
            Err(RemoveSourceError::UnknownSource)
        }
    }

    /// Returns an iterator that extracts all requests that need to be started and requests that
    /// need to be cancelled.
    pub fn requests_actions<'a>(&'a mut self) -> impl Iterator<Item = RequestAction<'a, TSrc>> {
        // Decompose `self` into its components so shared borrows of the `sources` field can be
        // returned as part of the `RequestAction`, while the other fields continue to be mutably
        // borrowed by the iterator itself.
        let sources = &self.sources;
        let verification_queue = &mut self.verification_queue;
        let blocks_request_granularity = self.blocks_request_granularity;
        let download_ahead_blocks = self.download_ahead_blocks;
        let next_request_id = &mut self.next_request_id;
        let cancelling_requests = &mut self.cancelling_requests;
        let chain = &mut self.chain;

        iter::from_fn(move || {
            if *cancelling_requests {
                while let Some(queue_elem) = verification_queue.pop_back() {
                    match queue_elem.ty {
                        VerificationQueueEntryTy::Requested { id, source } => {
                            return Some(RequestAction::Cancel {
                                request_id: id,
                                source: &sources[source].user_data,
                            });
                        }
                        _ => {}
                    }
                }

                *cancelling_requests = false;
            }

            let finalized_block = chain.finalized_block_header().number;
            while verification_queue.back().map_or(true, |rq| {
                rq.block_height.get() + u64::from(blocks_request_granularity.get())
                    < finalized_block + u64::from(download_ahead_blocks)
            }) {
                let block_height = verification_queue
                    .back()
                    .map(|rq| rq.block_height.get() + u64::from(blocks_request_granularity.get()))
                    .unwrap_or(finalized_block + 1);
                verification_queue.push_back(VerificationQueueEntry {
                    block_height: NonZeroU64::new(block_height).unwrap(),
                    ty: VerificationQueueEntryTy::Missing,
                });
            }

            for missing_pos in verification_queue
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e.ty, VerificationQueueEntryTy::Missing))
                .map(|(n, _)| n)
            {
                let source = sources.iter().filter(|(_, src)| !src.banned).next()?.0; // TODO: some sort of round-robin source selection

                let block_height = verification_queue[missing_pos].block_height;

                let num_blocks = if let Some(next) = verification_queue.get(missing_pos + 1) {
                    NonZeroU32::new(
                        u32::try_from(cmp::min(
                            u64::from(blocks_request_granularity.get()),
                            next.block_height
                                .get()
                                .checked_sub(block_height.get())
                                .unwrap(),
                        ))
                        .unwrap(),
                    )
                    .unwrap()
                } else {
                    blocks_request_granularity
                };

                let request_id = *next_request_id;
                next_request_id.0 += 1;

                verification_queue[missing_pos].ty = VerificationQueueEntryTy::Requested {
                    id: request_id,
                    source,
                };

                return Some(RequestAction::Start {
                    request_id,
                    source: &sources[source].user_data,
                    block_height,
                    num_blocks,
                });
            }

            None
        })
    }

    /// Update the [`OptimisticHeadersSync`] with the outcome of a request.
    ///
    /// # Panic
    ///
    /// Panics if the [`RequestId`] is invalid.
    ///
    pub fn finish_request<'a>(
        &'a mut self,
        request_id: RequestId,
        outcome: Result<impl Iterator<Item = RequestSuccessBlock>, RequestFail>,
    ) -> FinishRequestOutcome<'a, TSrc> {
        let (verification_queue_entry, source_id) = self
            .verification_queue
            .iter()
            .enumerate()
            .filter_map(|(pos, entry)| match entry.ty {
                VerificationQueueEntryTy::Requested { id, source } if id == request_id => {
                    Some((pos, source))
                }
                _ => None,
            })
            .next()
            .unwrap();

        let blocks = match outcome {
            Ok(blocks) => blocks.collect(),
            Err(_) => {
                return FinishRequestOutcome::SourcePunished(&mut self.sources[source_id].user_data)
            }
        };

        // TODO: handle if blocks.len() < expected_number_of_blocks

        self.verification_queue[verification_queue_entry].ty =
            VerificationQueueEntryTy::Queued(blocks);

        FinishRequestOutcome::Queued
    }

    /// Process a single block in the queue of verification.
    // TODO: return value
    pub fn process_one(&mut self) -> Option<ChainStateUpdate> {
        if self.cancelling_requests {
            return None;
        }

        let blocks = match &mut self.verification_queue.get_mut(0)?.ty {
            VerificationQueueEntryTy::Queued(blocks) => mem::replace(blocks, Default::default()),
            _ => return None,
        };

        let mut expected_block_height = self.verification_queue[0].block_height.get();

        self.verification_queue.pop_front();

        for block in blocks {
            match self.chain.verify_header(block.scale_encoded_header.into()) {
                Ok(blocks_tree::HeaderVerifySuccess::Insert {
                    block_height,
                    is_new_best,
                    insert,
                }) => {
                    if !is_new_best || block_height != expected_block_height {
                        panic!() // TODO: punish source and reset queue
                    }

                    insert.insert(())
                } // TODO:
                Ok(blocks_tree::HeaderVerifySuccess::Duplicate) => panic!(), // TODO: punish source and reset queue
                Err(_) => {}                                                 // TODO: reset queue
            }

            if let Some(justification) = block.scale_encoded_justification {
                match self.chain.verify_justification(justification.as_ref()) {
                    Ok(apply) => apply.apply(),
                    Err(_) => {} // TODO: reset queue
                }
            }

            expected_block_height += 1;
        }

        // If we reach here, no block in the queue was processed.
        // TODO: what if response was empty?
        debug_assert!(!self.verification_queue.is_empty());
        Some(ChainStateUpdate {
            finalized_block_hash: self.chain.finalized_block_hash(),
            finalized_block_number: self.chain.finalized_block_header().number,
        })
    }
}

/// Identifier for an ongoing request in the [`OptimisticHeadersSync`].
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct RequestId(u64);

/// Request that should be emitted towards a certain source.
#[derive(Debug)]
pub enum RequestAction<'a, TSrc> {
    /// A request must be emitted for the given source.
    Start {
        /// Identifier for the request. Must later be passed back to
        /// [`OptimisticHeadersSync::finish_request`].
        request_id: RequestId,
        /// Source where to request blocks from.
        source: &'a TSrc,
        /// Height of the block to request.
        block_height: NonZeroU64,
        /// Number of blocks to request. Always smaller than the value passed through
        /// [`Config::blocks_request_granularity`].
        num_blocks: NonZeroU32,
    },

    /// The given [`RequestId`] is no longer valid.
    ///
    /// > **Note**: The request can either be cancelled, or the request can be let through but
    /// >           marked in a way that [`OptimisticHeadersSync::finish_request`] isn't called.
    Cancel {
        /// Identifier for the request. No longer valid.
        request_id: RequestId,
        /// Source where the request comes from.
        source: &'a TSrc,
    },
}

pub enum FinishRequestOutcome<'a, TSrc> {
    Queued,
    SourcePunished(&'a mut TSrc),
}

pub struct RequestSuccessBlock {
    pub scale_encoded_header: Vec<u8>,
    pub scale_encoded_justification: Option<Vec<u8>>,
}

/// Reason why a request has failed.
pub enum RequestFail {
    /// Requested blocks aren't available from this source.
    BlocksUnavailable,
}

/// Possible error when calling [`OptimisticHeadersSync::add_source`].
#[derive(Debug, derive_more::Display)]
pub enum AddSourceError {
    /// Source was already known.
    Duplicate,
}

/// Possible error when calling [`OptimisticHeadersSync::remove_source`].
#[derive(Debug, derive_more::Display)]
pub enum RemoveSourceError {
    /// Source wasn't in the list.
    UnknownSource,
}

#[derive(Debug)]
pub struct ChainStateUpdate {
    pub finalized_block_hash: [u8; 32],
    pub finalized_block_number: u64,
}
