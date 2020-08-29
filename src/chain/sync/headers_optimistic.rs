//! Optimistic headers-only syncing.
//!
//! Optimistic syncing consists in assuming that all sources of blocks form the same chain. A
//! query for blocks is performed on a random peer, and the response is verified. If it turns out
//! that the source doesn't belong to the same chain, a different source is tried.
//!
//! While this syncing strategy is very simplistic, it is the most effective when the majority of
//! sources are well-behaviour, which is normally the case.
//!
//! The [`OptimisticHeadersSync`] makes it possible to sync the finalized blocks of a chain, but
//! not the non-finalized blocks.

use super::super::blocks_tree;

use alloc::collections::VecDeque;
use core::{cmp, convert::TryFrom as _, iter};

/// Configuration for the [`OptimisticHeadersSync`].
#[derive(Debug)]
pub struct Config {
    /// Configuration for the tree of blocks.
    pub chain_config: blocks_tree::Config,

    /// Pre-allocated capacity for the number of block sources.
    pub sources_capacity: usize,

    /// Maximum number of blocks returned by a response.
    pub blocks_request_granularity: u32,
}

/// Optimistic headers-only syncing.
pub struct OptimisticHeadersSync<TBlock, TSrc> {
    chain: blocks_tree::NonFinalizedTree<TBlock>,

    /// List of sources of blocks.
    sources: slab::Slab<TSrc>,

    /// Queue of block requests, either to be started, in progress, or completed.
    verification_queue: VecDeque<VerificationQueueEntry>,

    /// Value passed by [`Config::blocks_request_granularity`].
    blocks_request_granularity: u32,

    /// Identifier to assign to the next request.
    next_request_id: RequestId,
}

struct VerificationQueueEntry {
    block_height: u64,
    ty: VerificationQueueEntryTy,
}

enum VerificationQueueEntryTy {
    Missing,
    Requested {
        id: RequestId,
        // Index of this source within [`OptimisticHeadersSync::sources`].
        source: usize,
        num_blocks: u32,
    },
    Queued(VecDeque<RequestSuccessBlock<Vec<u8>>>),
}

impl<TBlock, TSrc> OptimisticHeadersSync<TBlock, TSrc> {
    /// Builds a new [`OptimisticHeadersSync`].
    pub fn new(config: Config) -> Self {
        OptimisticHeadersSync {
            chain: blocks_tree::NonFinalizedTree::new(config.chain_config),
            sources: slab::Slab::with_capacity(config.sources_capacity),
            verification_queue: VecDeque::new(), // TODO: make capacity configurable
            blocks_request_granularity: config.blocks_request_granularity,
            next_request_id: RequestId(0),
        }
    }
}

impl<TBlock, TSrc> OptimisticHeadersSync<TBlock, TSrc>
where
    TSrc: Eq,
{
    /// Inform the [`OptimisticHeadersSync`] of a new potential source of blocks.
    pub fn add_source(&mut self, source: TSrc) -> Result<(), AddSourceError> {
        // TODO: consider not checking this; needs some API rethinking
        if self.sources.iter().any(|(_, s)| s == source) {
            return Err(AddSourceError::Duplicate);
        }

        self.sources.insert(source);
        Ok(())
    }

    /// Inform the [`OptimisticHeadersSync`] that a source of blocks is no longer available.
    ///
    /// This automatically cancels all the [`RequestId`]s that have been emitted for this source.
    /// This list of [`RequestId`]s is returned as part of this function.
    pub fn remove_source(
        &mut self,
        source: &TSrc,
    ) -> Result<impl Iterator<Item = RequestId>, RemoveSourceError>
    {
        if let Some(pos) = self.sources.iter().position(|(_, s)| s == source) {
            self.sources.remove(pos);
            Ok(iter::empty()) // TODO:
        } else {
            Err(RemoveSourceError::UnknownSource)
        }
    }

    /// Returns an iterator that extracts all requests that need to be started and requests that
    /// need to be cancelled.
    pub fn requests_actions<'a>(&'a mut self) -> impl Iterator<Item = RequestAction<'a, TSrc>> {
        iter::from_fn(move || {
            if let Some(missing_pos) = self
                .verification_queue
                .iter()
                .position(|(_, s)| matches!(s, VerificationQueueEntry::Missing))
            {
                let block_height = self.verification_queue[missing_pos].0;

                let request_id = self.next_request_id;
                self.next_request_id.0 += 1;

                self.verification_queue[missing_pos].1 =
                    VerificationQueueEntry::Requested(request_id);

                let num_blocks = if let Some((next, _)) = self.verification_queue.get(missing_pos + 1) {
                    u32::try_from(cmp::min(u64::from(self.blocks_request_granularity), next.checked_sub(block_height).unwrap())).unwrap()
                } else {
                    self.blocks_request_granularity
                };

                return Some(RequestAction::Start {
                    request_id,
                    source: todo!(), // TODO:
                    block_height,
                    num_blocks,
                });
            }

            /*if self.verification_queue.back().map_or(false, |(n, _)| ) {

            }*/

            todo!()
        })
    }

    /// Update the [`OptimisticHeadersSync`] with the outcome of a request.
    ///
    /// The [`OptimisticHeadersSync`] verifies that the response is correct and returns a
    /// [`FinishRequestOutcome`] describing what has happened.
    ///
    /// # Panic
    ///
    /// Panics if the [`RequestId`] is invalid.
    ///
    pub fn finish_request<J: AsRef<[u8]> + Into<Vec<u8>>>(
        &mut self,
        id: RequestId,
        outcome: Result<impl Iterator<Item = RequestSuccessBlock<J>>, RequestFail>,
    ) -> FinishRequestOutcome {
        let (request, src_info) = {
            let mut rq_src = None;

            for (_, src_info) in &mut self.sources {
                let request = match src_info.pending_requests.iter().position(|rq| rq.id == id) {
                    Some(pos) => src_info.pending_requests.remove(pos),
                    None => continue,
                };

                rq_src = Some((request, src_info));
                break;
            }

            rq_src.unwrap()
        };

        let blocks = match outcome {
            Ok(blocks) => blocks,
            Err(_) => return FinishRequestOutcome::Queued, // TODO: no
        };

        debug_assert!(request.requested_block_number > self.chain.finalized_block_header().number);
        debug_assert!(self
            .verification_queue
            .iter()
            .all(|(n, _)| *n != request.requested_block_number));

        // TODO: check that blocks is not empty

        let insert_position = self
            .verification_queue
            .iter()
            .position(|(n, _)| *n >= request.requested_block_number)
            .unwrap_or(self.verification_queue.len());

        self.verification_queue.insert(
            insert_position,
            (
                request.requested_block_number,
                blocks
                    .map(|b| RequestSuccessBlock {
                        scale_encoded_header: b.scale_encoded_header.into(),
                        scale_encoded_justification: b.scale_encoded_justification.map(Into::into),
                    })
                    .collect(),
            ),
        );

        FinishRequestOutcome::Queued
    }

    pub fn queue_process<'a>(&'a mut self) -> impl Iterator<Item = ()> + 'a {
        iter::from_fn(move || {
            // TODO:
            self.process_one();
            None
        })
    }

    fn process_one(&mut self) {
        while let Some((_, first_in_queue)) = self.verification_queue.front_mut() {
            match first_in_queue {}

            if let Some(to_import) = first_in_queue.pop_front() {
                match self
                    .chain
                    .verify_header(to_import.scale_encoded_header.into())
                {
                    Ok(blocks_tree::HeaderVerifySuccess::Insert { insert, .. }) => {
                        insert.insert(todo!())
                    } // TODO:
                    Ok(blocks_tree::HeaderVerifySuccess::Duplicate) => unreachable!(),
                    Err(_) => {} // TODO:
                }

                if let Some(justification) = to_import.scale_encoded_justification {
                    match self.chain.verify_justification(justification.as_ref()) {
                        Ok(apply) => apply.apply(),
                    }
                }
            } else {
                let _ = self.verification_queue.pop_front().unwrap();
            }
        }

        // If we reach here, no block in the queue was processed.
        // TODO: what if response was empty?
        debug_assert!(!self.verification_queue.is_empty());
        FinishRequestOutcome::StoredForLater
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
        block_height: u64,
        /// Number of blocks to request. Always smaller than the value passed through
        /// [`Config::blocks_request_granularity`].
        num_blocks: u32,
    },

    /// The given [`RequestId`] is no longer valid. The request can either be cancelled, or the
    /// request can be let through while the response is ignored.
    Cancel(RequestId),
}

pub enum FinishRequestOutcome {
    Queued,
}

pub struct RequestSuccessBlock<J> {
    pub scale_encoded_header: Vec<u8>,
    pub scale_encoded_justification: Option<J>,
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
