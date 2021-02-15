// Substrate-lite
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

//! Collection of "pending" blocks, in other words blocks whose existence is known but which
//! can't be verified yet. Used for the `all_forks` syncing.
//!
//! Example: the local node knows about block 5. A peer announces block 7. Since the local node
//! doesn't know block 6, it has to store block 7 for later, then download block 6. The container
//! in this module is where block 7 is temporarily stored.
//!
//! In addition to a set of blocks, this data structure also stores a set of ongoing requests that
//! related to these blocks. In the example above, it would store the request that asks for block
//! 6 from the network.
//!
//! # Blocks
//!
//! The [`PendingBlocks`] collection stores a list of pending blocks.
//!
//! Blocks are expected to be added to this collection whenever we hear about them from a source
//! of blocks (such as a peer) and that it is not possible to verify them immediately (because
//! their parent isn't known).
//!
//! Each block has zero, one, or more *requests* associated to it. When a request is associated
//! to a block, it means that we expect the response of the request to contain needed information
//! about the block in question.
//!
//! Blocks can only be removed in three different ways:
//!
//! - Calling [`OccupiedBlockEntry::remove_verify_success`] marks a block as valid and removes
//! it, as it is not pending anymore.
//! - Calling [`OccupiedBlockEntry::remove_verify_failed`] marks a block and all its descendants
//! as invalid. This may or may not remove the block itself and all its descendants.
//! - Calling [`OccupiedBlockEntry::remove_uninteresting`] removes a block in order to reduce
//! the memory usage of the data structure.
//!
//! # Requests
//!
//! In addition to a list of blocks, this data structure also stores a list of ongoing requests.
//! Each block has zero, one, or more requests associated to it.
//!
//! Call [`PendingBlocks::desired_queries`] to find the list of queries that *should* be started.
//! Call [`OccupiedBlockEntry::add_descending_request`] to allocate a new [`RequestId`] and add
//! a new request. This has the effect of changing the outcome of
//! [`PendingBlocks::desired_queries`].
//! Call [`PendingBlocks::finish_request`] to destroy a request after it has finished.
//!

use crate::header;

use alloc::collections::{BTreeMap, BTreeSet};
use core::{
    convert::TryFrom as _,
    num::{NonZeroU32, NonZeroU64},
};

/// Configuration for the [`PendingBlocks`].
#[derive(Debug)]
pub struct Config {
    /// Pre-allocated capacity for the number of blocks between the finalized block and the head
    /// of the chain.
    pub blocks_capacity: usize,

    /// Maximum number of simultaneous pending requests made towards the same block.
    ///
    /// Should be set according to the failure rate of requests. For example if requests have a
    /// 10% chance of failing, then setting to value to `2` gives a 1% chance that downloading
    /// this block will overall fail and has to be attempted again.
    ///
    /// Also keep in mind that sources might maliciously take a long time to answer requests. A
    /// higher value makes it possible to reduce the risks of the syncing taking a long time
    /// because of malicious sources.
    ///
    /// The higher the value, the more bandwidth is potentially wasted.
    pub max_requests_per_block: NonZeroU32,
}

/// Identifier for a request in the [`PendingBlocks`].
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct RequestId(usize);

/// Collection of pending blocks and requests.
pub struct PendingBlocks<TBl, TRq> {
    /// All blocks in the collection. Keys are the block height and hash.
    blocks: BTreeMap<(u64, [u8; 32]), Block<TBl>>,

    /// Set of `(block_height, block_hash, request_id)`.
    /// Contains the list of all requests, associated to their block.
    ///
    /// The `request_id` is an index in [`PendingBlocks::requests`].
    ///
    /// > **Note**: This is a more optimized way compared to adding a `Vec<RequestId>` in the
    /// >           [`Block`] struct.
    blocks_requests: BTreeSet<(u64, [u8; 32], RequestId)>,

    /// All ongoing requests.
    requests: slab::Slab<Request<TRq>>,

    /// See [`Config::max_requests_per_block`].
    /// Since it is always compared with `usize`s, converted to `usize` ahead of time.
    max_requests_per_block: usize,
}

struct Block<TBl> {
    user_data: TBl,
    inner: BlockInner,
}

enum BlockInner {
    /// Header of the block isn't known.
    Unknown,
    /// Header of the block is known.
    UnverifiedHeader {
        /// Header of the block.
        scale_encoded_header: Vec<u8>,
    },
    /// Block is considered as bad by the API user. It is stored in the collection in order to be
    /// able to immediately rejecting children of this block without having to re-download it.
    KnownBad { parent_hash: [u8; 32] },
}

impl BlockInner {
    /// Returns the parent hash of this block, if it is known.
    fn parent_hash(&self) -> Option<&[u8; 32]> {
        match self {
            BlockInner::Unknown => None,
            BlockInner::KnownBad { parent_hash } => Some(parent_hash),
            BlockInner::UnverifiedHeader {
                scale_encoded_header,
            } => {
                let decoded = header::decode(&scale_encoded_header).unwrap();
                Some(decoded.parent_hash)
            }
        }
    }
}

struct Request<TRq> {
    user_data: TRq,
    /// Block that this request is targetting.
    target_block: (u64, [u8; 32]),
}

impl<TBl, TRq> PendingBlocks<TBl, TRq> {
    /// Initializes a new empty collection of blocks.
    pub fn new(config: Config) -> Self {
        PendingBlocks {
            blocks: Default::default(),
            blocks_requests: Default::default(),
            requests: slab::Slab::with_capacity(
                config.blocks_capacity
                    * usize::try_from(config.max_requests_per_block.get())
                        .unwrap_or(usize::max_value()),
            ),
            max_requests_per_block: usize::try_from(config.max_requests_per_block.get())
                .unwrap_or(usize::max_value()),
        }
    }

    /// Returns the number of blocks stored in the data structure.
    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
    }

    /// Marks the request as finished.
    ///
    /// Returns the height and hash block of the first block that requested.
    ///
    /// The next call to [`PendingBlocks::desired_queries`] might return the same request again.
    /// In order to avoid that, you are encouraged to update the list of blocks in the container
    /// with the outcome of the request.
    ///
    /// # Panic
    ///
    /// Panics if the [`RequestId`] is invalid.
    ///
    pub fn finish_request(&mut self, request_id: RequestId) -> (TRq, u64, [u8; 32]) {
        let request = self.requests.remove(request_id.0);

        let _was_in = self.blocks_requests.remove(&(
            request.target_block.0,
            request.target_block.1,
            request_id,
        ));
        debug_assert!(_was_in);

        (
            request.user_data,
            request.target_block.0,
            request.target_block.1,
        )
    }

    /// Returns an iterator yielding blocks that should be requested from the sources of blocks.
    ///
    /// Considering this state machine is unable to differentiate between blocks that are
    /// completely unknown and blocks that are known by not stored in the [`PendingBlocks`], this
    /// method will also return queries for blocks that belong to that second category.
    ///
    /// In order to avoid this, you are encouraged to remove from the [`PendingBlocks`] any block
    /// that can be marked as verified or bad prior to calling this method.
    ///
    /// Guaranteed to always return blocks that are in the data structure.
    pub fn desired_queries<'a>(&'a self) -> impl Iterator<Item = DesiredQuery> + 'a {
        // TODO: this is O(n); maybe do something more optimized once it's fully working and has unit tests
        self.blocks
            .iter()
            .filter(|(_, s)| matches!(s.inner, BlockInner::Unknown))
            .filter_map(
                move |(&(ref unknown_block_height, ref unknown_block_hash), _)| {
                    let num_existing_requests = self
                        .blocks_requests
                        .range(
                            (
                                *unknown_block_height,
                                *unknown_block_hash,
                                RequestId(usize::min_value()),
                            )
                                ..=(
                                    *unknown_block_height,
                                    *unknown_block_hash,
                                    RequestId(usize::max_value()),
                                ),
                        )
                        .count();

                    debug_assert!(num_existing_requests <= self.max_requests_per_block);
                    if num_existing_requests == self.max_requests_per_block {
                        return None;
                    }

                    Some(DesiredQuery {
                        first_block_hash: *unknown_block_hash,
                        first_block_height: *unknown_block_height,
                        num_blocks: NonZeroU64::new(128).unwrap(), // TODO: *unknown_block_height - ...
                    })
                },
            )

        // TODO: all query the parents of `UnverifiedHeader` blocks if they're not present
    }

    /// Gives access to a block, either present or absent.
    pub fn block_mut(&mut self, height: u64, hash: [u8; 32]) -> BlockEntry<TBl, TRq> {
        let key = (height, hash);
        if self.blocks.contains_key(&key) {
            BlockEntry::Occupied(OccupiedBlockEntry { parent: self, key })
        } else {
            BlockEntry::Vacant(VacantBlockEntry { parent: self, key })
        }
    }

    /*/// Passed a known entry in `blocks`. Removes this entry and any known children of this block.
    ///
    /// # Panic
    ///
    /// Panics if `(number, hash)` isn't an entry in [`PendingBlocks::blocks]`.
    ///
    fn discard_chain(&mut self, number: u64, hash: [u8; 32]) -> Vec<(u64, [u8; 32])> {
        // TODO: keep a list of banned blocks for later? this is required by chain specs anyway

        // The implementation consists in iterating over the increasing block number, and removing
        // all blocks whose parent was removed at the previous iteration.

        // Return value of the function.
        let mut result = Vec::with_capacity(64);

        // List of blocks to discard at the next iteration.
        let mut blocks_to_discard = Vec::with_capacity(16);
        blocks_to_discard.push(hash);

        for number in number.. {
            // The `for` loop would be infinite unless we put an explicit `break`.
            if blocks_to_discard.is_empty() {
                break;
            }

            // Find in `disjoint_headers` any block whose parent is in `blocks_to_discard`.
            let blocks_to_discard_next = {
                let mut blocks_to_discard_next = Vec::with_capacity(16);
                for ((_, hash), block) in self
                    .blocks
                    .range((number + 1, [0; 32])..=(number + 1, [0xff; 32]))
                {
                    let decoded = header::decode(&block.scale_encoded_header).unwrap();
                    if blocks_to_discard.iter().any(|b| b == decoded.parent_hash) {
                        blocks_to_discard_next.push(*hash);
                    }
                }
                blocks_to_discard_next
            };

            // Now discard `blocks_to_discard`.
            for to_discard in mem::replace(&mut blocks_to_discard, blocks_to_discard_next) {
                let mut discarded_block = self.blocks.remove(&(number, to_discard)).unwrap();

                let requests_ids = self
                    .blocks_requests
                    .range(
                        (number, to_discard, RequestId(usize::min_value()))
                            ..=(number, to_discard, RequestId(usize::max_value())),
                    )
                    .map(|(_, _, id)| *id)
                    .collect::<Vec<_>>();

                for request_id in requests_ids {
                    let _was_in = self
                        .blocks_requests
                        .remove(&(number, to_discard, request_id));
                    debug_assert!(_was_in);

                    let request = self.requests.remove(&request_id.0).unwrap();
                }

                result.push((number, to_discard, discarded_block));
            }
        }

        result
    }*/

    /// Returns the list of children of the given block that are in the collection.
    fn children_mut<'a>(
        &'a mut self,
        height: u64,
        hash: &'a [u8; 32],
    ) -> impl Iterator<Item = ((u64, [u8; 32]), &mut Block<TBl>)> + 'a {
        self.blocks
            .range_mut((height + 1, [0x0; 32])..=(height + 1, [0xff; 32]))
            .filter(move |(_, block)| block.inner.parent_hash() == Some(hash))
            .map(|(k, v)| (*k, v))
    }
}

/// See [`PendingBlocks::desired_queries`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct DesiredQuery {
    /// Height of the first block to request.
    pub first_block_height: u64,

    /// Hash of the first block to request.
    pub first_block_hash: [u8; 32],

    /// Number of blocks the request should return.
    ///
    /// Note that this is only an indication, and the source is free to give fewer blocks
    /// than requested. If that happens, the state machine might later send out further
    /// ancestry search requests to complete the chain.
    pub num_blocks: NonZeroU64,
}

/// Access to a block, either present or absent, w.r.t to the container.
pub enum BlockEntry<'a, TBl, TRq> {
    /// Block is present.
    Occupied(OccupiedBlockEntry<'a, TBl, TRq>),
    /// Block is absent.
    Vacant(VacantBlockEntry<'a, TBl, TRq>),
}

impl<'a, TBl, TRq> BlockEntry<'a, TBl, TRq> {
    /// Ensures that this block is in the collection. If `Vacant`, calls
    /// [`VacantBlockEntry::insert`].
    pub fn or_insert(self, user_data: TBl) -> OccupiedBlockEntry<'a, TBl, TRq> {
        match self {
            BlockEntry::Occupied(e) => e,
            BlockEntry::Vacant(e) => e.insert(user_data),
        }
    }

    /// Returns `Some` for the `Occupied` variant, and `None` otherwise.
    pub fn into_occupied(self) -> Option<OccupiedBlockEntry<'a, TBl, TRq>> {
        match self {
            BlockEntry::Occupied(e) => Some(e),
            BlockEntry::Vacant(_) => None,
        }
    }
}

/// Access to a block already present in the container.
pub struct OccupiedBlockEntry<'a, TBl, TRq> {
    parent: &'a mut PendingBlocks<TBl, TRq>,
    key: (u64, [u8; 32]),
}

impl<'a, TBl, TRq> OccupiedBlockEntry<'a, TBl, TRq> {
    /// Returns the user data associated to the block. This is the value originally passed
    /// through [`VacantBlockEntry::insert`].
    pub fn user_data(&mut self) -> &mut TBl {
        let block = self.parent.blocks.get_mut(&self.key).unwrap();
        &mut block.user_data
    }

    /// Returns the user data associated to the block. This is the value originally passed
    /// through [`VacantBlockEntry::insert`].
    pub fn into_user_data(self) -> &'a mut TBl {
        let block = self.parent.blocks.get_mut(&self.key).unwrap();
        &mut block.user_data
    }

    /// Sets the header of the block.
    pub fn update_header(&mut self, header: Vec<u8>) {
        debug_assert_eq!(header::hash_from_scale_encoded_header(&header), self.key.1);

        let block = self.parent.blocks.get_mut(&self.key).unwrap();
        match &block.inner {
            // Transition from `Unknown` to `UnverifiedHeader`.
            BlockInner::Unknown => {
                block.inner = BlockInner::UnverifiedHeader {
                    scale_encoded_header: header,
                };
            }

            // Header doesn't interest us anymore.
            BlockInner::KnownBad { .. } => {}

            // Header already known. Do a basic sanity check.
            BlockInner::UnverifiedHeader {
                scale_encoded_header: already_in,
            } => {
                // Since headers are indexed by their hash, a mismatch here would mean that two
                // different headers have the same hash.
                debug_assert_eq!(already_in, &header);
            }
        };
    }

    /// Removes the block from the collection, as it has now been successfully verified.
    ///
    /// Returns the list of blocks that are children of this one, and are thus ready to be
    /// verified as well.
    pub fn remove_verify_success(self) -> RemoveVerifySuccessOutcome<TBl, TRq> {
        let children_iter = self.parent.children_mut(self.key.0, &self.key.1);

        // List of blocks that are now ready to be verified.
        let mut verify_next = Vec::with_capacity({
            let hint = children_iter.size_hint();
            hint.1.unwrap_or(hint.0)
        });

        for ((child_height, child_hash), child) in children_iter {
            debug_assert_eq!(child_height, self.key.0 + 1);
            match &child.inner {
                // `Unknown` blocks can't be returned by `self.children()` since we don't know
                // their ancestry.
                BlockInner::Unknown => unreachable!(),

                // Nothing more to do for blocks already verified.
                BlockInner::KnownBad { .. } => {}

                BlockInner::UnverifiedHeader {
                    scale_encoded_header,
                } => {
                    // TODO: don't clone?
                    verify_next.push((child_height, child_hash, scale_encoded_header.clone()));
                }
            }
        }

        // Actually remove from list.
        let removed_block: Block<_> = self.parent.blocks.remove(&self.key).unwrap();

        let cancelled_requests = {
            let ids = self
                .parent
                .blocks_requests
                .range(
                    (self.key.0, self.key.1, RequestId(usize::min_value()))
                        ..=(self.key.0, self.key.1, RequestId(usize::max_value())),
                )
                .map(|(_, _, id)| *id)
                .collect::<Vec<_>>();

            let mut cancelled_requests = Vec::with_capacity(ids.len());
            for id in ids {
                self.parent
                    .blocks_requests
                    .remove(&(self.key.0, self.key.1, id));
                let request = self.parent.requests.remove(id.0);
                debug_assert_eq!(request.target_block, self.key);
                cancelled_requests.push((id, request.user_data));
            }
            cancelled_requests
        };

        RemoveVerifySuccessOutcome {
            verify_next,
            user_data: removed_block.user_data,
            cancelled_requests,
        }
    }

    /// Removes the block from the collection, as its verification has failed.
    ///
    /// In addition to removing the block itself, all its descendants are marked as "bad" and
    /// might be removed as well.
    pub fn remove_verify_failed(self) {
        // Actually remove from list.
        let removed_block: Block<_> = self.parent.blocks.remove(&self.key).unwrap();

        let cancelled_requests = {
            let ids = self
                .parent
                .blocks_requests
                .range(
                    (self.key.0, self.key.1, RequestId(usize::min_value()))
                        ..=(self.key.0, self.key.1, RequestId(usize::max_value())),
                )
                .map(|(_, _, id)| *id)
                .collect::<Vec<_>>();

            let mut cancelled_requests = Vec::with_capacity(ids.len());
            for id in ids {
                self.parent
                    .blocks_requests
                    .remove(&(self.key.0, self.key.1, id));
                let request = self.parent.requests.remove(id.0);
                debug_assert_eq!(request.target_block, self.key);
                cancelled_requests.push((id, request.user_data));
            }
            cancelled_requests
        };

        // TODO: remove children of the block as well
        // TODO: remove ongoing requests?
        todo!()
    }

    /// Removes the block from the collection in order to leave space.
    pub fn remove_uninteresting(self) {
        // TODO: temporary implementation
        self.remove_verify_failed();
    }

    /// Adds a new request to the collection. Allocates a new [`RequestId`].
    ///
    /// The request is expected to fetch this block and `num_requested_blocks - 1` ancestors.
    ///
    /// The main effect of this method is to adjust the outcome of calling
    /// [`PendingBlocks::desired_queries`].
    pub fn add_descending_request(
        &mut self,
        user_data: TRq,
        num_requested_blocks: NonZeroU64,
    ) -> RequestId {
        let request_id = RequestId(self.parent.requests.insert(Request {
            target_block: self.key,
            user_data,
        }));

        self.parent
            .blocks_requests
            .insert((self.key.0, self.key.1, request_id));

        request_id
    }
}

/// Outcome of calling `remove_verify_success`.
#[must_use]
pub struct RemoveVerifySuccessOutcome<TBl, TRq> {
    /// List of blocks whose parent was the block that has just been successfully verified.
    ///
    /// These blocks are ready to be verified.
    ///
    /// Contains the block height, hash, and SCALE-encoded header.
    pub verify_next: Vec<(u64, [u8; 32], Vec<u8>)>,

    /// User data of the block that has been removed.
    pub user_data: TBl,

    /// List of requests that concerned the block that has been successfully verified. These
    /// request IDs are now invalid.
    pub cancelled_requests: Vec<(RequestId, TRq)>,
}

/// Access to a block absent from the container.
pub struct VacantBlockEntry<'a, TBl, TRq> {
    parent: &'a mut PendingBlocks<TBl, TRq>,
    key: (u64, [u8; 32]),
}

impl<'a, TBl, TRq> VacantBlockEntry<'a, TBl, TRq> {
    /// Inserts the block in the collection, passing a user data.
    pub fn insert(self, user_data: TBl) -> OccupiedBlockEntry<'a, TBl, TRq> {
        self.parent.blocks.insert(
            self.key,
            Block {
                user_data,
                inner: BlockInner::Unknown,
            },
        );

        OccupiedBlockEntry {
            parent: self.parent,
            key: self.key,
        }
    }
}
