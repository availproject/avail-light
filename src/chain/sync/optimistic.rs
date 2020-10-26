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

//! Code shared in common between headers-only and headers-and-body optimistic syncing.

// TODO: document usage
// TODO: the quality of this module's code is sub-par compared to what we want

use alloc::{collections::VecDeque, vec};
use core::{
    cmp,
    convert::TryFrom as _,
    fmt, iter,
    marker::PhantomData,
    mem,
    num::{NonZeroU32, NonZeroU64},
};
use rand::{seq::IteratorRandom as _, SeedableRng as _};

/// Configuration for the [`OptimisticSync`].
#[derive(Debug)]
pub struct Config {
    /// Best block in the chain.
    pub best_block_number: u64,

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
pub struct OptimisticSync<TRq, TSrc, TBl> {
    best_block_number: u64,

    /// List of sources of blocks.
    sources: slab::Slab<Source<TSrc>>,

    cancelling_requests: bool,

    /// Queue of block requests, either to be started, in progress, or completed.
    verification_queue: VecDeque<VerificationQueueEntry<TRq, TBl>>,

    /// Value passed by [`Config::blocks_request_granularity`].
    blocks_request_granularity: NonZeroU32,

    /// Value passed by [`Config::download_ahead_blocks`].
    download_ahead_blocks: u32,

    /// Identifier to assign to the next request.
    next_request_id: RequestId,

    /// PRNG used to select the source to start a query with.
    source_selection_rng: rand_chacha::ChaCha8Rng,
}

struct VerificationQueueEntry<TRq, TBl> {
    block_height: NonZeroU64,
    ty: VerificationQueueEntryTy<TRq, TBl>,
}

struct Source<TSrc> {
    user_data: TSrc,
    banned: bool, // TODO: ban shouldn't be held forever
}

enum VerificationQueueEntryTy<TRq, TBl> {
    Missing,
    Requested {
        id: RequestId,
        /// User-chosen data for this request.
        user_data: TRq,
        // Index of this source within [`OptimisticSync::sources`].
        source: usize,
    },
    Queued(Vec<TBl>),
}

impl<TRq, TSrc, TBl> OptimisticSync<TRq, TSrc, TBl> {
    /// Builds a new [`OptimisticSync`].
    pub fn new(config: Config) -> Self {
        OptimisticSync {
            best_block_number: config.best_block_number,
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
            source_selection_rng: rand_chacha::ChaCha8Rng::seed_from_u64(
                config.source_selection_randomness_seed,
            ),
        }
    }

    /// Inform the [`OptimisticSync`] of a new potential source of blocks.
    pub fn add_source(&mut self, source: TSrc) -> SourceId {
        SourceId(self.sources.insert(Source {
            user_data: source,
            banned: false,
        }))
    }

    /// Inform the [`OptimisticSync`] that a source of blocks is no longer available.
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
        let src_user_data = self.sources.remove(source.0).user_data;
        let drain = RequestsDrain {
            iter: self.verification_queue.iter_mut().fuse(),
            source_index: source.0,
        };
        (src_user_data, drain)
    }

    /// Returns an iterator that extracts all requests that need to be started and requests that
    /// need to be cancelled.
    pub fn next_request_action(&mut self) -> Option<RequestAction<TRq, TSrc, TBl>> {
        if self.cancelling_requests {
            while let Some(queue_elem) = self.verification_queue.pop_back() {
                match queue_elem.ty {
                    VerificationQueueEntryTy::Requested {
                        id,
                        source,
                        user_data,
                    } => {
                        return Some(RequestAction::Cancel {
                            request_id: id,
                            user_data,
                            source_id: SourceId(source),
                            source: &mut self.sources[source].user_data,
                        });
                    }
                    _ => {}
                }
            }

            self.cancelling_requests = false;
        }

        while self.verification_queue.back().map_or(true, |rq| {
            rq.block_height.get() + u64::from(self.blocks_request_granularity.get())
                < self
                    .best_block_number
                    .checked_add(u64::from(self.download_ahead_blocks))
                    .unwrap()
        }) {
            let block_height = self
                .verification_queue
                .back()
                .map(|rq| rq.block_height.get() + u64::from(self.blocks_request_granularity.get()))
                .unwrap_or(self.best_block_number + 1);
            self.verification_queue.push_back(VerificationQueueEntry {
                block_height: NonZeroU64::new(block_height).unwrap(),
                ty: VerificationQueueEntryTy::Missing,
            });
        }

        if let Some((missing_pos, _)) = self
            .verification_queue
            .iter()
            .enumerate()
            .find(|(_, e)| matches!(e.ty, VerificationQueueEntryTy::Missing))
        {
            let source = self
                .sources
                .iter()
                .filter(|(_, src)| !src.banned)
                .choose(&mut self.source_selection_rng)?
                .0;

            let block_height = self.verification_queue[missing_pos].block_height;

            let num_blocks = if let Some(next) = self.verification_queue.get(missing_pos + 1) {
                NonZeroU32::new(
                    u32::try_from(cmp::min(
                        u64::from(self.blocks_request_granularity.get()),
                        next.block_height
                            .get()
                            .checked_sub(block_height.get())
                            .unwrap(),
                    ))
                    .unwrap(),
                )
                .unwrap()
            } else {
                self.blocks_request_granularity
            };

            return Some(RequestAction::Start {
                source_id: SourceId(source),
                source: &mut self.sources[source].user_data,
                block_height,
                num_blocks,
                start: Start {
                    verification_queue: &mut self.verification_queue,
                    missing_pos,
                    next_request_id: &mut self.next_request_id,
                    source,
                    marker: PhantomData,
                },
            });
        }

        None
    }

    /// Update the [`OptimisticSync`] with the outcome of a request.
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
        outcome: Result<impl Iterator<Item = TBl>, RequestFail>,
    ) -> (TRq, FinishRequestOutcome<'a, TSrc>) {
        let (verification_queue_entry, source_id) = self
            .verification_queue
            .iter()
            .enumerate()
            .filter_map(|(pos, entry)| match entry.ty {
                VerificationQueueEntryTy::Requested { id, source, .. } if id == request_id => {
                    Some((pos, source))
                }
                _ => None,
            })
            .next()
            .expect("invalid RequestId");

        let blocks = match outcome {
            Ok(blocks) => blocks.collect(),
            Err(_) => {
                let user_data = match mem::replace(
                    &mut self.verification_queue[verification_queue_entry].ty,
                    VerificationQueueEntryTy::Missing,
                ) {
                    VerificationQueueEntryTy::Requested { user_data, .. } => user_data,
                    _ => unreachable!(),
                };

                return (
                    user_data,
                    FinishRequestOutcome::SourcePunished(&mut self.sources[source_id].user_data),
                );
            }
        };

        // TODO: handle if blocks.len() < expected_number_of_blocks

        let user_data = match mem::replace(
            &mut self.verification_queue[verification_queue_entry].ty,
            VerificationQueueEntryTy::Queued(blocks),
        ) {
            VerificationQueueEntryTy::Requested { user_data, .. } => user_data,
            _ => unreachable!(),
        };

        (user_data, FinishRequestOutcome::Queued)
    }

    /// Process a single block in the queue of verification.
    ///
    /// Returns an error if the queue is empty.
    pub fn process_one(mut self) -> Result<ProcessOne<TRq, TSrc, TBl>, Self> {
        if self.cancelling_requests {
            return Err(self);
        }

        // Extract the chunk of blocks to process next.
        let blocks = match &mut self.verification_queue.get_mut(0).map(|b| &mut b.ty) {
            Some(VerificationQueueEntryTy::Queued(blocks)) => {
                mem::replace(blocks, Default::default())
            }
            _ => return Err(self),
        };

        let expected_block_height = self.verification_queue[0].block_height.get();

        self.verification_queue.pop_front();

        Ok(ProcessOne {
            expected_block_height,
            blocks: blocks.into_iter(),
            report: ProcessOneReport { parent: self },
        })
    }
}

/// Identifier for an ongoing request in the [`OptimisticSync`].
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct RequestId(u64);

/// Identifier for a source in the [`OptimisticSync`].
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct SourceId(usize);

/// Request that should be emitted towards a certain source.
#[derive(Debug)]
pub enum RequestAction<'a, TRq, TSrc, TBl> {
    /// A request must be emitted for the given source.
    ///
    /// The request has **not** been acknowledged when this event is emitted. You **must** call
    /// [`Start::start`] to notify the [`OptimisticSync`] that the request has been sent
    /// out.
    Start {
        /// Source where to request blocks from.
        source_id: SourceId,
        /// User data of source where to request blocks from.
        source: &'a mut TSrc,
        /// Must be used to accept the request.
        start: Start<'a, TRq, TSrc, TBl>,
        /// Height of the block to request.
        block_height: NonZeroU64,
        /// Number of blocks to request. Always smaller than the value passed through
        /// [`Config::blocks_request_granularity`].
        num_blocks: NonZeroU32,
    },

    /// The given [`RequestId`] is no longer valid.
    ///
    /// > **Note**: The request can either be cancelled, or the request can be let through but
    /// >           marked in a way that [`OptimisticSync::finish_request`] isn't called.
    Cancel {
        /// Identifier for the request. No longer valid.
        request_id: RequestId,
        /// User data associated with the request.
        user_data: TRq,
        /// Source where to request blocks from.
        source_id: SourceId,
        /// User data of source where to request blocks from.
        source: &'a mut TSrc,
    },
}

/// Must be used to accept the request.
#[must_use]
pub struct Start<'a, TRq, TSrc, TBl> {
    verification_queue: &'a mut VecDeque<VerificationQueueEntry<TRq, TBl>>,
    source: usize,
    missing_pos: usize,
    next_request_id: &'a mut RequestId,
    marker: PhantomData<&'a TSrc>,
}

impl<'a, TRq, TSrc, TBl> Start<'a, TRq, TSrc, TBl> {
    /// Updates the [`OptimisticSync`] with the fact that the request has actually been
    /// started. Returns the identifier for the request that must later be passed back to
    /// [`OptimisticSync::finish_request`].
    pub fn start(self, user_data: TRq) -> RequestId {
        let request_id = *self.next_request_id;
        self.next_request_id.0 += 1;

        self.verification_queue[self.missing_pos].ty = VerificationQueueEntryTy::Requested {
            id: request_id,
            source: self.source,
            user_data,
        };

        request_id
    }
}

impl<'a, TRq, TSrc, TBl> fmt::Debug for Start<'a, TRq, TSrc, TBl> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Start").finish()
    }
}

pub enum FinishRequestOutcome<'a, TSrc> {
    Queued,
    SourcePunished(&'a mut TSrc),
}

/// Reason why a request has failed.
pub enum RequestFail {
    /// Requested blocks aren't available from this source.
    BlocksUnavailable,
}

/// Iterator that drains requests after a source has been removed.
pub struct RequestsDrain<'a, TRq, TBl> {
    iter: iter::Fuse<alloc::collections::vec_deque::IterMut<'a, VerificationQueueEntry<TRq, TBl>>>,
    source_index: usize,
}

impl<'a, TRq, TBl> Iterator for RequestsDrain<'a, TRq, TBl> {
    type Item = (RequestId, TRq);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = self.iter.next()?;
            match entry.ty {
                VerificationQueueEntryTy::Requested { source, .. }
                    if source == self.source_index =>
                {
                    match mem::replace(&mut entry.ty, VerificationQueueEntryTy::Missing) {
                        VerificationQueueEntryTy::Requested { id, user_data, .. } => {
                            return Some((id, user_data));
                        }
                        _ => unreachable!(),
                    }
                }
                _ => {}
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, self.iter.size_hint().1)
    }
}

impl<'a, TRq, TBl> fmt::Debug for RequestsDrain<'a, TRq, TBl> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("RequestsDrain").finish()
    }
}

impl<'a, TRq, TBl> Drop for RequestsDrain<'a, TRq, TBl> {
    fn drop(&mut self) {
        // Drain all remaining elements even if the iterator is dropped eagerly.
        // This is the reason why a custom iterator type is needed, rather than using combinators.
        while let Some(_) = self.next() {}
    }
}

pub struct ProcessOne<TRq, TSrc, TBl> {
    pub expected_block_height: u64,
    pub blocks: vec::IntoIter<TBl>,
    pub report: ProcessOneReport<TRq, TSrc, TBl>,
}

#[must_use]
pub struct ProcessOneReport<TRq, TSrc, TBl> {
    parent: OptimisticSync<TRq, TSrc, TBl>,
}

impl<TRq, TSrc, TBl> ProcessOneReport<TRq, TSrc, TBl> {
    pub fn reset_to_finalized(
        mut self,
        finalized_block_number: u64,
    ) -> OptimisticSync<TRq, TSrc, TBl> {
        self.parent.cancelling_requests = true;
        self.parent.best_block_number = finalized_block_number;
        self.parent
    }

    pub fn update_block_height(
        mut self,
        new_best_block_number: u64,
    ) -> OptimisticSync<TRq, TSrc, TBl> {
        self.parent.best_block_number = new_best_block_number;
        self.parent
    }
}
