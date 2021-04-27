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

//! State machine managing the "disjoint" blocks, in other words blocks whose existence is known
//! but which can't be verified yet.
//!
//! > **Example**: The local node knows about block 5. A peer announces block 7. Since the local
//! >              node doesn't know block 6, it has to store block 7 for later, then download
//! >              block 6. The container in this module is where block 7 is temporarily stored.
//!
//! In addition to a set of blocks, this data structure also stores a set of sources of blocks,
//! and ongoing requests that related to these blocks.
//!
//! > **Note**: In the example above, it would store the request that asks for block 6 from the
//! >           network.
//!
//! # Sources
//!
//! The [`PendingBlocks`] collection stores a list of sources of blocks.
//!
//! Sources can be added by calling [`PendingBlocks::add_source`] and removed by calling
//! [`PendingBlocks::remove_source`].
//!
//! Each source has the following properties:
//!
//! - A [`SourceId`].
//! - A best block.
//! - A list of non-finalized blocks known by this source.
//! - An opaque user data, of type `TSrc`.
//!
//! # Blocks
//!
//! The [`PendingBlocks`] collection stores a list of pending blocks.
//!
//! Blocks are expected to be added to this collection whenever we hear about them from a source
//! of blocks (such as a peer) and that it is not possible to verify them immediately (because
//! their parent isn't known).
//!
//! Blocks can be added by calling [`PendingBlocks::insert_unverified_block`] and remove by
//! calling [`PendingBlocks::remove`].
//!
//! Each block stored in this collection has the following properties associated to it:
//!
//! - A height.
//! - A hash.
//! - An optional parent block hash.
//! - Whether the block is known to be bad.
//! - A opaque user data decided by the user of type `TBl`.
//!
//! This data structure is only able to link parent and children together if the heights are
//! linearly increasing. For example, if block A is the parent of block B, then the height of
//! block B must be equal to the height of block A plus one. Otherwise, this data structure will
//! not be able to detect the parent-child relationship.
//!
//! If a block is marked as bad, all its children (i.e. other blocks in the collection whose
//! parent hash is the bad block) are automatically marked as bad as well. This process is
//! recursive, such that not only direct children but all descendants of a bad block are
//! automatically marked as bad.
//!
//! # Requests
//!
//! Call [`PendingBlocks::desired_requests`] or [`PendingBlocks::source_desired_requests`] to
//! obtain the list of requests that should be started.
//!
//! Call [`PendingBlocks::add_request`] to allocate a new [`RequestId`] and add a new request.
//! Call [`PendingBlocks::finish_request`] to destroy a request after it has finished or been
//! cancelled. Note that this method doesn't require to be passed the response to that request.
//! The user is encouraged to update the state machine according to the response, but this must
//! be done manually.
//!

use super::{disjoint, sources};

use alloc::{collections::BTreeSet, vec::Vec};
use core::{
    convert::TryFrom as _,
    iter,
    num::{NonZeroU32, NonZeroU64},
};

pub use disjoint::TreeRoot;
pub use sources::SourceId;

/// Configuration for the [`PendingBlocks`].
#[derive(Debug)]
pub struct Config {
    /// Pre-allocated capacity for the number of blocks between the finalized block and the head
    /// of the chain.
    pub blocks_capacity: usize,

    /// Pre-allocated capacity for the number of sources that will be added to the collection.
    pub sources_capacity: usize,

    /// Height of the known finalized block. Can be lower than the actual value, and increased
    /// later.
    pub finalized_block_height: u64,

    /// If `true`, block bodies are downloaded and verified. If `false`, only headers are
    /// verified.
    pub verify_bodies: bool,

    /// Maximum number of simultaneous pending requests made towards the same block.
    ///
    /// Should be set according to the failure rate of requests. For example if requests have an
    /// estimated 10% chance of failing, then setting to value to `2` gives a 1% chance that
    /// downloading this block will overall fail and has to be attempted again.
    ///
    /// Also keep in mind that sources might maliciously take a long time to answer requests. A
    /// higher value makes it possible to reduce the risks of the syncing taking a long time
    /// because of malicious sources.
    ///
    /// The higher the value, the more bandwidth is potentially wasted.
    pub max_requests_per_block: NonZeroU32,

    /// List of block hashes that are known to be bad and shouldn't be downloaded or verified.
    ///
    /// > **Note**: This list is typically filled with a list of blocks found in the chain
    /// >           specifications. It is part of the "trusted setup" of the node, in other words
    /// >           the information that is passed by the user and blindly assumed to be true.
    // TODO: unused
    pub banned_blocks: Vec<[u8; 64]>,
}

/// State of a block in the data structure.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum UnverifiedBlockState {
    /// Only the height and hash of the block is known.
    HeightHashKnown,
    /// The header of the block is known, but not its body.
    HeaderKnown {
        /// Hash of the block that is parent of this one.
        parent_hash: [u8; 32],
    },
    /// The header and body of the block are both known. The block is waiting to be verified.
    HeaderBodyKnown {
        /// Hash of the block that is parent of this one.
        parent_hash: [u8; 32],
    },
}

impl UnverifiedBlockState {
    /// Returns the parent block hash stored in this instance, if any.
    pub fn parent_hash(&self) -> Option<&[u8; 32]> {
        match self {
            UnverifiedBlockState::HeightHashKnown => None,
            UnverifiedBlockState::HeaderKnown { parent_hash } => Some(parent_hash),
            UnverifiedBlockState::HeaderBodyKnown { parent_hash } => Some(parent_hash),
        }
    }
}

/// Identifier for a request in the [`PendingBlocks`].
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct RequestId(usize);

/// Collection of pending blocks and requests.
pub struct PendingBlocks<TBl, TRq, TSrc> {
    /// All sources in the collection.
    sources: sources::AllForksSources<Source<TSrc>>,

    /// Blocks whose validity couldn't be determined yet.
    blocks: disjoint::DisjointBlocks<UnverifiedBlock<TBl>>,

    /// See [`Config::verify_bodies`].
    verify_bodies: bool,

    /// Set of `(block_height, block_hash, request_id)`.
    /// Contains the list of all requests, associated to their block.
    ///
    /// Note that this doesn't contain an exhaustive list of all blocks that are targeted by a
    /// request, for the simple reason that not all blocks might be known.
    ///
    /// The `request_id` is an index in [`PendingBlocks::requests`].
    ///
    /// > **Note**: This is a more optimized way compared to adding a `Vec<RequestId>` in the
    /// >           [`UnverifiedBlock`] struct.
    blocks_requests: BTreeSet<(u64, [u8; 32], RequestId)>,

    /// Set of `(request_id, block_height, block_hash)`.
    ///
    /// Contains the same entries as [`PendingBlocks::blocks_requests`], but ordered differently.
    requested_blocks: BTreeSet<(RequestId, u64, [u8; 32])>,

    /// Set of `(source_id, request_id)`.
    /// Contains the list of requests, associated to their source.
    ///
    /// The `request_id` is an index in [`PendingBlocks::requests`].
    source_occupations: BTreeSet<(SourceId, RequestId)>,

    /// All ongoing requests.
    requests: slab::Slab<Request<TRq>>,

    /// See [`Config::max_requests_per_block`].
    /// Since it is always compared with `usize`s, converted to `usize` ahead of time.
    max_requests_per_block: usize,
}

struct UnverifiedBlock<TBl> {
    state: UnverifiedBlockState,
    user_data: TBl,
}

struct Request<TRq> {
    detail: RequestParams,
    source_id: SourceId,
    user_data: TRq,
}

#[derive(Debug)]
struct Source<TSrc> {
    /// Opaque object passed by the user.
    user_data: TSrc,
}

impl<TBl, TRq, TSrc> PendingBlocks<TBl, TRq, TSrc> {
    /// Initializes a new empty collection.
    pub fn new(config: Config) -> Self {
        PendingBlocks {
            sources: sources::AllForksSources::new(
                config.sources_capacity,
                config.finalized_block_height,
            ),
            blocks: disjoint::DisjointBlocks::with_capacity(config.blocks_capacity),
            verify_bodies: config.verify_bodies,
            blocks_requests: Default::default(),
            requested_blocks: Default::default(),
            source_occupations: Default::default(),
            requests: slab::Slab::with_capacity(
                config.blocks_capacity
                    * usize::try_from(config.max_requests_per_block.get())
                        .unwrap_or(usize::max_value()),
            ),
            max_requests_per_block: usize::try_from(config.max_requests_per_block.get())
                .unwrap_or(usize::max_value()),
        }
    }

    /// Add a new source to the container.
    ///
    /// The `user_data` parameter is opaque and decided entirely by the user. It can later be
    /// retrieved using [`PendingBlocks::source_user_data`].
    ///
    /// Returns the newly-created source entry.
    pub fn add_source(
        &mut self,
        user_data: TSrc,
        best_block_number: u64,
        best_block_hash: [u8; 32],
    ) -> SourceId {
        self.sources
            .add_source(best_block_number, best_block_hash, Source { user_data })
    }

    /// Removes the source from the [`PendingBlocks`].
    ///
    /// Returns the user data that was originally passed to [`PendingBlocks::add_source`], plus
    /// a list of all the requests that were targetting this source. These request are now
    /// invalid.
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn remove_source(
        &mut self,
        source_id: SourceId,
    ) -> (TSrc, impl Iterator<Item = (RequestId, RequestParams, TRq)>) {
        let user_data = self.sources.remove(source_id);

        let source_occupations_entries = self
            .source_occupations
            .range(
                (source_id, RequestId(usize::min_value()))
                    ..=(source_id, RequestId(usize::max_value())),
            )
            .cloned()
            .collect::<Vec<_>>();

        // TODO: optimize with a custom iterator?
        let mut pending_requests = Vec::new();

        for (_source_id, pending_request_id) in source_occupations_entries {
            debug_assert_eq!(source_id, _source_id);

            debug_assert!(self.requests.contains(pending_request_id.0));
            let request = self.requests.remove(pending_request_id.0);

            let _was_in = self.blocks_requests.remove(&(
                request.detail.first_block_height,
                request.detail.first_block_hash,
                pending_request_id,
            ));
            debug_assert!(_was_in);

            let _was_in = self.requested_blocks.remove(&(
                pending_request_id,
                request.detail.first_block_height,
                request.detail.first_block_hash,
            ));
            debug_assert!(_was_in);

            pending_requests.push((pending_request_id, request.detail, request.user_data));
        }

        (user_data.user_data, pending_requests.into_iter())
    }

    /// Registers a new block that the source is aware of.
    ///
    /// Has no effect if `height` is inferior or equal to the finalized block height.
    ///
    /// The block does not need to be known by the data structure.
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn add_known_block(&mut self, source_id: SourceId, height: u64, hash: [u8; 32]) {
        self.sources.add_known_block(source_id, height, hash);
    }

    /// Sets the best block of this source.
    ///
    /// The block does not need to be known by the data structure.
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn set_best_block(&mut self, source_id: SourceId, height: u64, hash: [u8; 32]) {
        self.sources.set_best_block(source_id, height, hash);
    }

    /// Returns true if [`PendingBlocks::add_known_block`] or [`PendingBlocks::set_best_block`]
    /// has earlier been called on this source with this height and hash, or if the source was
    /// originally created (using [`PendingBlocks::add_source`]) with this height and hash.
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    /// Panics if `height` is inferior or equal to the finalized block height. Finalized blocks
    /// are intentionally not tracked by this data structure, and panicking when asking for a
    /// potentially-finalized block prevents potentially confusing or erroneous situations.
    ///
    pub fn source_knows_non_finalized_block(
        &self,
        source_id: SourceId,
        height: u64,
        hash: &[u8; 32],
    ) -> bool {
        self.sources
            .source_knows_non_finalized_block(source_id, height, hash)
    }

    /// Returns the user data associated to the source. This is the value originally passed
    /// through [`PendingBlocks::add_source`].
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn source_user_data(&self, source_id: SourceId) -> &TSrc {
        &self.sources.user_data(source_id).user_data
    }

    /// Returns the user data associated to the source. This is the value originally passed
    /// through [`PendingBlocks::add_source`].
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn source_user_data_mut(&mut self, source_id: SourceId) -> &mut TSrc {
        &mut self.sources.user_data_mut(source_id).user_data
    }

    /// Updates the height of the finalized block.
    ///
    /// This removes from the collection, and will ignore in the future, all blocks whose height
    /// is inferior or equal to this value.
    ///
    /// # Panic
    ///
    /// Panics if the new height is inferior to the previous value.
    ///
    pub fn set_finalized_block_height(
        &mut self,
        height: u64,
    ) -> impl ExactSizeIterator<Item = TBl> {
        self.sources.set_finalized_block_height(height);
        self.blocks
            .remove_below_height(height + 1)
            .map(|(_, _, bl)| bl.user_data)
    }

    /// Inserts an unverified block in the collection.
    ///
    /// Returns the previous user data associated to this block, if any.
    pub fn insert_unverified_block(
        &mut self,
        height: u64,
        hash: [u8; 32],
        state: UnverifiedBlockState,
        user_data: TBl,
    ) -> Option<(TBl, UnverifiedBlockState)> {
        if height <= self.sources.finalized_block_height() {
            return None;
        }

        let parent_hash = state.parent_hash().map(|h| *h);
        // TODO: is it ok to just override the UnverifiedBlockState?
        self.blocks
            .insert(
                height,
                hash,
                parent_hash,
                UnverifiedBlock { state, user_data },
            )
            .map(|b| (b.user_data, b.state))
    }

    /// Returns `true` if the block with the given height and hash is in the collection.
    pub fn contains_block(&self, height: u64, hash: &[u8; 32]) -> bool {
        self.blocks.contains(height, hash)
    }

    /// Gives access to the user data stored for this block.
    ///
    /// # Panic
    ///
    /// Panics if the block wasn't present in the data structure.
    ///
    pub fn block_user_data(&self, height: u64, hash: &[u8; 32]) -> &TBl {
        &self.blocks.user_data(height, hash).unwrap().user_data
    }

    /// Gives access to the user data stored for this block.
    ///
    /// # Panic
    ///
    /// Panics if the block wasn't present in the data structure.
    ///
    pub fn block_user_data_mut(&mut self, height: u64, hash: &[u8; 32]) -> &mut TBl {
        &mut self.blocks.user_data_mut(height, hash).unwrap().user_data
    }

    /// Modifies the state of the given block.
    ///
    /// This influences the outcome of [`PendingBlocks::desired_requests`].
    ///
    /// # Panic
    ///
    /// Panics if the block wasn't present in the data structure.
    ///
    pub fn set_block_state(&mut self, height: u64, hash: &[u8; 32], state: UnverifiedBlockState) {
        if let Some(parent_hash) = state.parent_hash() {
            self.blocks.set_parent_hash(height, hash, *parent_hash);
        }

        self.blocks.user_data_mut(height, hash).unwrap().state = state;
    }

    /// Modifies the state of the given block. This is a convenience around
    /// [`PendingBlocks::set_block_state`].
    ///
    /// If the current block's state implies that the header isn't known yet, updates it to a
    /// state where the header is known.
    ///
    /// # Panic
    ///
    /// Panics if the block wasn't present in the data structure.
    ///
    pub fn set_block_header_known(&mut self, height: u64, hash: &[u8; 32], parent_hash: [u8; 32]) {
        let curr = &mut self.blocks.user_data_mut(height, hash).unwrap().state;

        match curr {
            UnverifiedBlockState::HeaderKnown {
                parent_hash: cur_ph,
            }
            | UnverifiedBlockState::HeaderBodyKnown {
                parent_hash: cur_ph,
            } if *cur_ph == parent_hash => return,
            UnverifiedBlockState::HeaderKnown { .. }
            | UnverifiedBlockState::HeaderBodyKnown { .. } => {
                panic!()
            }
            UnverifiedBlockState::HeightHashKnown => {}
        }

        *curr = UnverifiedBlockState::HeaderKnown { parent_hash };
        self.blocks.set_parent_hash(height, hash, parent_hash);
    }

    /// Modifies the state of the given block. This is a convenience around
    /// [`PendingBlocks::set_block_state`].
    ///
    /// If the current block's state implies that the header or body isn't known yet, updates it
    /// to a state where the header and body are known.
    ///
    /// # Panic
    ///
    /// Panics if the block wasn't present in the data structure.
    ///
    pub fn set_block_header_body_known(
        &mut self,
        height: u64,
        hash: &[u8; 32],
        parent_hash: [u8; 32],
    ) {
        let curr = &mut self.blocks.user_data_mut(height, hash).unwrap().state;

        match curr {
            UnverifiedBlockState::HeaderKnown {
                parent_hash: cur_ph,
            } if *cur_ph == parent_hash => {}
            UnverifiedBlockState::HeaderBodyKnown {
                parent_hash: cur_ph,
            } if *cur_ph == parent_hash => return,
            UnverifiedBlockState::HeaderKnown { .. }
            | UnverifiedBlockState::HeaderBodyKnown { .. } => {
                panic!()
            }
            UnverifiedBlockState::HeightHashKnown => {}
        }

        *curr = UnverifiedBlockState::HeaderBodyKnown { parent_hash };
        self.blocks.set_parent_hash(height, hash, parent_hash);
    }

    /// Removes the given block from the collection.
    ///
    /// > **Note**: Use this method after a block has been successfully verified, or in order to
    /// >           remove uninteresting blocks if there are too many blocks in the collection.
    ///
    /// # Panic
    ///
    /// Panics if the block wasn't present in the data structure.
    ///
    pub fn remove(&mut self, height: u64, hash: &[u8; 32]) -> TBl {
        self.blocks.remove(height, hash).user_data
    }

    /// Marks the given block and all its known children as "bad".
    ///
    /// If a child of this block is later added to the collection, it is also automatically
    /// marked as bad.
    ///
    /// # Panic
    ///
    /// Panics if the block wasn't present in the data structure.
    ///
    #[track_caller]
    pub fn set_block_bad(&mut self, height: u64, hash: &[u8; 32]) {
        self.blocks.set_block_bad(height, hash);
    }

    /// Returns the number of blocks stored in the data structure.
    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
    }

    /// Returns the list of blocks whose parent hash is known but absent from the list of disjoint
    /// blocks. These blocks can potentially be verified.
    ///
    /// All the returned block are guaranteed to be in a "header known" state. If
    /// [`Config::verify_bodies`] if `true`, they they are also guaranteed to be in a "body known"
    /// state.
    pub fn unverified_leaves(&'_ self) -> impl Iterator<Item = TreeRoot> + '_ {
        self.blocks.good_tree_roots().filter(move |pending| {
            match self
                .blocks
                .user_data(pending.block_number, &pending.block_hash)
                .unwrap()
                .state
            {
                UnverifiedBlockState::HeightHashKnown => false,
                UnverifiedBlockState::HeaderKnown { .. } => !self.verify_bodies,
                UnverifiedBlockState::HeaderBodyKnown { .. } => true,
            }
        })
    }

    /// Inserts a new request in the data structure.
    ///
    /// > **Note**: The request doesn't necessarily have to match a request returned by
    /// >           [`PendingBlocks::desired_requests`] or
    /// >           [`PendingBlocks::source_desired_requests`].
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn add_request(
        &mut self,
        source_id: SourceId,
        detail: RequestParams,
        user_data: TRq,
    ) -> RequestId {
        assert!(self.sources.contains(source_id));

        let request_id = RequestId(self.requests.insert(Request {
            detail,
            source_id,
            user_data,
        }));

        self.source_occupations.insert((source_id, request_id));

        debug_assert_eq!(self.source_occupations.len(), self.requests.len());

        // Add in `blocks_requests` and `requested_blocks` an entry for each known block.
        let mut iter = (detail.first_block_height, detail.first_block_hash);
        loop {
            self.blocks_requests.insert((iter.0, iter.1, request_id));
            self.requested_blocks.insert((request_id, iter.0, iter.1));

            match self.blocks.parent_hash(iter.0, &iter.1) {
                Some(p) => iter = (iter.0 - 1, *p),
                None => break,
            }
        }

        request_id
    }

    /// Marks a request as finished.
    ///
    /// Returns the parameters that were passed to [`PendingBlocks::add_request`].
    ///
    /// The next call to [`PendingBlocks::desired_requests`] might return the same request again.
    /// In order to avoid that, you are encouraged to update the state of the sources and blocks
    /// in the container with the outcome of the request.
    ///
    /// # Panic
    ///
    /// Panics if the [`RequestId`] is invalid.
    ///
    #[track_caller]
    pub fn finish_request(&mut self, request_id: RequestId) -> (RequestParams, SourceId, TRq) {
        assert!(self.requests.contains(request_id.0));
        let request = self.requests.remove(request_id.0);

        let blocks_to_remove = self
            .requested_blocks
            .range(
                (request_id, u64::min_value(), [0; 32])
                    ..=(request_id, u64::max_value(), [0xff; 32]),
            )
            .cloned()
            .collect::<Vec<_>>();

        for (request_id, block_height, block_hash) in blocks_to_remove {
            let _was_in = self
                .blocks_requests
                .remove(&(block_height, block_hash, request_id));
            debug_assert!(_was_in);

            let _was_in = self
                .requested_blocks
                .remove(&(request_id, block_height, block_hash));
            debug_assert!(_was_in);
        }

        let _was_in = self
            .source_occupations
            .remove(&(request.source_id, request_id));
        debug_assert!(_was_in);

        debug_assert_eq!(self.source_occupations.len(), self.requests.len());
        debug_assert_eq!(self.blocks_requests.len(), self.requested_blocks.len());

        (request.detail, request.source_id, request.user_data)
    }

    /// Returns the source that the given request is being performed on.
    ///
    /// # Panic
    ///
    /// Panics if the [`RequestId`] is out of range.
    ///
    pub fn request_source(&self, request_id: RequestId) -> SourceId {
        self.requests.get(request_id.0).unwrap().source_id
    }

    /// Returns a list of requests that are considered obsolete and can be removed using
    /// [`PendingBlocks::finish_request`].
    ///
    /// A request becomes obsolete if the state of the request blocks changes in such a way that
    /// they don't need to be requested anymore. The response to the request will be useless.
    ///
    /// > **Note**: It is in no way mandatory to actually call this function and cancel the
    /// >           requests that are returned.
    pub fn obsolete_requests(&'_ self) -> impl Iterator<Item = RequestId> + '_ {
        // TODO: more than that?
        self.requests
            .iter()
            .filter(move |(_, rq)| {
                rq.detail.first_block_height <= self.sources.finalized_block_height()
            })
            .map(|(id, _)| RequestId(id))
    }

    /// Returns the details of a request to start towards a source.
    ///
    /// This method doesn't modify the state machine in any way. [`PendingBlocks::add_request`]
    /// must be called in order for the request to actually be marked as started.
    ///
    /// No request concerning the finalized block (as set using
    /// [`PendingBlocks::set_finalized_block_height`]) or below will ever be returned.
    pub fn desired_requests(&'_ self) -> impl Iterator<Item = DesiredRequest> + '_ {
        self.desired_requests_inner(None)
    }

    /// Returns the details of a request to start towards the source.
    ///
    /// This method is similar to [`PendingBlocks::desired_requests`].
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn source_desired_requests(
        &'_ self,
        source_id: SourceId,
    ) -> impl Iterator<Item = RequestParams> + '_ {
        self.desired_requests_inner(Some(source_id)).map(move |rq| {
            debug_assert_eq!(rq.source_id, source_id);
            rq.request_params
        })
    }

    /// Inner implementation of [`PendingBlocks::desired_requests`] and
    /// [`PendingBlocks::source_desired_requests`].
    ///
    /// If `force_source` is `Some`, only the given source will be considered.
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    fn desired_requests_inner(
        &'_ self,
        force_source: Option<SourceId>,
    ) -> impl Iterator<Item = DesiredRequest> + '_ {
        // TODO: request the best block of each source if necessary

        // List of blocks whose header is known but not its body.
        let unknown_body_iter = if self.verify_bodies {
            either::Left(
                self.blocks
                    .iter()
                    .filter(move |(_, _, block_info)| match &block_info.state {
                        UnverifiedBlockState::HeaderKnown { .. } => true,
                        _ => false,
                    })
                    .map(|(height, hash, _)| (height, hash)),
            )
        } else {
            either::Right(iter::empty())
        };

        // List of blocks whose header isn't known.
        let unknown_header_iter = self
            .blocks
            .unknown_blocks()
            .filter(move |(unknown_block_height, _)| {
                // Don't request the finalized block or below.
                *unknown_block_height > self.sources.finalized_block_height()
            })
            .inspect(move |(unknown_block_height, unknown_block_hash)| {
                // Sanity check.
                debug_assert!(match self
                    .blocks
                    .user_data(*unknown_block_height, unknown_block_hash)
                    .map(|ud| &ud.state)
                {
                    None | Some(UnverifiedBlockState::HeightHashKnown) => true,
                    Some(UnverifiedBlockState::HeaderKnown { .. })
                    | Some(UnverifiedBlockState::HeaderBodyKnown { .. }) => false,
                })
            });

        // Combine the two block iterators and find sources.
        // There isn't any overlap between the two iterators.
        unknown_body_iter
            .chain(unknown_header_iter)
            .filter(move |(unknown_block_height, unknown_block_hash)| {
                // Cap by `max_requests_per_block`.
                let num_existing_requests = self
                    .blocks_requests
                    .range(
                        (
                            *unknown_block_height,
                            **unknown_block_hash,
                            RequestId(usize::min_value()),
                        )
                            ..=(
                                *unknown_block_height,
                                **unknown_block_hash,
                                RequestId(usize::max_value()),
                            ),
                    )
                    .count();

                debug_assert!(num_existing_requests <= self.max_requests_per_block);
                num_existing_requests < self.max_requests_per_block
            })
            .flat_map(move |(unknown_block_height, unknown_block_hash)| {
                // Try to find all appropriate sources.
                let possible_sources = if let Some(force_source) = force_source {
                    either::Left(iter::once(force_source).filter(move |id| {
                        self.sources.source_knows_non_finalized_block(
                            *id,
                            unknown_block_height,
                            unknown_block_hash,
                        )
                    }))
                } else {
                    either::Right(
                        self.sources
                            .knows_non_finalized_block(unknown_block_height, unknown_block_hash),
                    )
                };

                possible_sources
                    .filter(move |source_id| {
                        // Don't start any request towards this source if there's another request
                        // for the same block from the same source.
                        !self
                            .blocks_requests
                            .range(
                                (
                                    unknown_block_height,
                                    *unknown_block_hash,
                                    RequestId(usize::min_value()),
                                )
                                    ..=(
                                        unknown_block_height,
                                        *unknown_block_hash,
                                        RequestId(usize::max_value()),
                                    ),
                            )
                            .any(|(_, _, request_id)| {
                                self.requests[request_id.0].source_id == *source_id
                            })
                    })
                    .map(move |source_id| {
                        debug_assert!(self.sources.source_knows_non_finalized_block(
                            source_id,
                            unknown_block_height,
                            unknown_block_hash
                        ));

                        DesiredRequest {
                            source_id,
                            source_num_existing_requests: self
                                .source_occupations
                                .range(
                                    (source_id, RequestId(usize::min_value()))
                                        ..=(source_id, RequestId(usize::max_value())),
                                )
                                .count(),
                            request_params: RequestParams {
                                first_block_hash: *unknown_block_hash,
                                first_block_height: unknown_block_height,
                                num_blocks: NonZeroU64::new(
                                    unknown_block_height - self.sources.finalized_block_height(),
                                )
                                .unwrap(),
                            },
                        }
                    })
            })
    }
}

/// See [`PendingBlocks::desired_requests`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DesiredRequest {
    /// Source onto which to start this request.
    pub source_id: SourceId,
    /// Number of requests that the source is already performing.
    pub source_num_existing_requests: usize,
    /// Details of the request.
    pub request_params: RequestParams,
}

/// Information about a blocks request to be performed on a source.
///
/// The source should return information about the block indicated with
/// [`RequestParams::first_block_height`] and [`RequestParams::first_block_hash`] and its
/// ancestors. In total, [`RequestParams::num_blocks`] should be provided by the source.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestParams {
    /// Height of the first block to request.
    pub first_block_height: u64,

    /// Hash of the first block to request.
    pub first_block_hash: [u8; 32],

    /// Number of blocks the request should return.
    ///
    /// Note that this is only an indication, and the source is free to give fewer blocks
    /// than requested.
    pub num_blocks: NonZeroU64,
}
