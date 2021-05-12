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

//! *All-forks* header and body syncing.
//!
//! # Overview
//!
//! This state machine holds:
//!
//! - A list of sources of blocks, maintained by the API user.
//!  - For each source, a list of blocks hashes known by the source.
//! - The latest known finalized block.
//! - A tree of valid non-finalized blocks that all descend from the latest known finalized block.
//! - (if full mode) A list of block headers whose body is currently being downloaded.
//! - A list of block header waiting to be verified and whose ancestry with the latest finalized
//!   block is currently unknown.
//!
//! The state machine has the objective to synchronize the tree of non-finalized blocks with its
//! equivalent on the sources added by the API user.
//!
//! Because it is not possible to predict which block in this tree is going to be finalized in
//! the future, the entire tree needs to be synchronized.
//!
//! > **Example**: If the latest finalized block is block number 4, and the tree contains blocks
//! >              5, 6, and 7, and a source announces a block 5 that is different from the
//! >              locally-known block 5, a block request will be emitted for this block 5, even
//! >              if it is certain that this "other" block 5 will not become the local best
//! >              block. This is necessary in case it is this other block 5 that will end up
//! >              being finalized.
//!
//! # Full vs non-full
//!
//! The [`Config::full`] option configures whether the state machine only holds headers of the
//! non-finalized blocks (`full` equal to `false`), or the headers, and bodies, and storage
//! (`full` equal to `true`).
//!
//! In full mode, .
//!
//! # Bounded and unbounded containers
//!
//! It is important to limit the memory usage of this state machine no matter how the
//! potentially-malicious sources behave.
//!
//! The state in this state machine can be put into three categories:
//!
//! - Each source of blocks has a certain fixed-size state associated to it (containing for
//!   instance its best block number and height). Each source also has up to one in-flight
//!   request, which might incur more memory usage. Managing this additional request is out of
//!   scope of this module. The user of this module is expected to limit the number of
//!   simultaneous sources.
//!
//! - A set of verified blocks that descend from the latest finalized block. This set is
//!   unbounded. The consensus and finalization algorithms of the chain are supposed to limit
//!   the number of possible blocks in this set.
//!
//! - A set of blocks that can't be verified yet. Receiving a block announce inserts an element
//!   in this set. In order to handle situations where a malicious source announces lots of
//!   invalid blocks, this set must be bounded. Once it has reached a certain size, the blocks
//!   with the highest block number are discarded if their parent is also in this set or being
//!   downloaded from a source.
//!
//! Consequently, and assuming that the number of simultaneous sources is bounded, and that
//! the consensus and finalization algorithms of the chain are properly configured, malicious
//! sources can't indefinitely grow the state in this state machine.
//! Malicious sources, however, can potentially increase the number of block requests required to
//! download a long fork. This is, at most, an annoyance, and not a vulnerability.
//!

// TODO: finish ^

use crate::{
    chain::{blocks_tree, chain_information},
    header, verify,
};

use alloc::vec::Vec;
use core::{num::NonZeroU32, time::Duration};

mod disjoint;
mod pending_blocks;
mod sources;

pub use pending_blocks::{RequestId, RequestParams, SourceId};

/// Configuration for the [`AllForksSync`].
#[derive(Debug)]
pub struct Config {
    /// Information about the latest finalized block and its ancestors.
    pub chain_information: chain_information::ChainInformation,

    /// Pre-allocated capacity for the number of block sources.
    pub sources_capacity: usize,

    /// Pre-allocated capacity for the number of blocks between the finalized block and the head
    /// of the chain.
    ///
    /// Should be set to the maximum number of block between two consecutive justifications.
    pub blocks_capacity: usize,

    /// Maximum number of blocks of unknown ancestry to keep in memory. A good default is 1024.
    ///
    /// When a potential long fork is detected, its blocks are downloaded progressively in
    /// descending order until a common ancestor is found.
    /// Unfortunately, an attack could generate fake very long forks in order to make the node
    /// consume a lot of memory keeping track of the blocks in that fork.
    /// In order to avoid this, a limit is added to the number of blocks of unknown ancestry that
    /// are kept in memory.
    ///
    /// Note that the download of long forks will always work no matter this limit. In the worst
    /// case scenario, the same blocks will be downloaded multiple times. There is an implicit
    /// minimum size equal to the number of sources that have been added to the state machine.
    ///
    /// Increasing this value has no drawback, except for increasing the maximum possible memory
    /// consumption of this state machine.
    //
    // Implementation note: the size of `disjoint_headers` can temporarily grow above this limit
    // due to the internal processing of the state machine.
    pub max_disjoint_headers: usize,

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

    /// If true, the block bodies and storage are also synchronized.
    pub full: bool,
}

pub struct AllForksSync<TBl, TRq, TSrc> {
    /// Data structure containing the non-finalized blocks.
    ///
    /// If [`Config::full`], this only contains blocks whose header *and* body have been verified.
    chain: blocks_tree::NonFinalizedTree<Block<TBl>>,

    /// Extra fields. In a separate structure in order to be moved around.
    inner: Inner<TRq, TSrc>,
}

/// Extra fields. In a separate structure in order to be moved around.
struct Inner<TRq, TSrc> {
    blocks: pending_blocks::PendingBlocks<PendingBlock, TRq, TSrc>,
}

struct PendingBlock {
    header: Option<header::Header>,
    body: Option<Vec<Vec<u8>>>,
    justification: Option<Vec<u8>>,
}

struct Block<TBl> {
    header: header::Header,
    user_data: TBl,
}

impl<TBl, TRq, TSrc> AllForksSync<TBl, TRq, TSrc> {
    /// Initializes a new [`AllForksSync`].
    pub fn new(config: Config) -> Self {
        let finalized_block_height = config.chain_information.finalized_block_header.number;

        let chain = blocks_tree::NonFinalizedTree::new(blocks_tree::Config {
            chain_information: config.chain_information,
            blocks_capacity: config.blocks_capacity,
        });

        Self {
            chain,
            inner: Inner {
                blocks: pending_blocks::PendingBlocks::new(pending_blocks::Config {
                    blocks_capacity: config.blocks_capacity,
                    finalized_block_height,
                    max_requests_per_block: config.max_requests_per_block,
                    sources_capacity: config.sources_capacity,
                    verify_bodies: config.full,
                    banned_blocks: Vec::new(), // TODO:
                }),
            },
        }
    }

    /// Builds a [`chain_information::ChainInformationRef`] struct corresponding to the current
    /// latest finalized block. Can later be used to reconstruct a chain.
    pub fn as_chain_information(&self) -> chain_information::ChainInformationRef {
        self.chain.as_chain_information()
    }

    /// Returns the header of the finalized block.
    pub fn finalized_block_header(&self) -> header::HeaderRef {
        self.chain.as_chain_information().finalized_block_header
    }

    /// Returns the header of the best block.
    ///
    /// > **Note**: This value is provided only for informative purposes. Keep in mind that this
    /// >           best block might be reverted in the future.
    pub fn best_block_header(&self) -> header::HeaderRef {
        self.chain.best_block_header()
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

    /// Inform the [`AllForksSync`] of a new potential source of blocks.
    ///
    /// The `user_data` parameter is opaque and decided entirely by the user. It can later be
    /// retrieved using [`AllForksSync::source_user_data`].
    ///
    /// Returns the newly-created source entry, plus optionally a request that should be started
    /// towards this source.
    pub fn add_source(
        &mut self,
        user_data: TSrc,
        best_block_number: u64,
        best_block_hash: [u8; 32],
    ) -> SourceId {
        let source_id = self
            .inner
            .blocks
            .add_source(user_data, best_block_number, best_block_hash);

        let needs_verification = best_block_number > self.chain.finalized_block_header().number
            && self
                .chain
                .non_finalized_block_by_hash(&best_block_hash)
                .is_none();
        let is_in_disjoints_list = self
            .inner
            .blocks
            .contains_block(best_block_number, &best_block_hash);
        debug_assert!(!(!needs_verification && is_in_disjoints_list));

        if needs_verification && !is_in_disjoints_list {
            self.inner.blocks.insert_unverified_block(
                best_block_number,
                best_block_hash,
                pending_blocks::UnverifiedBlockState::HeightHashKnown,
                PendingBlock {
                    header: None,
                    body: None,
                    justification: None,
                },
            );
        }

        source_id
    }

    /// Removes the source from the [`AllForksSync`].
    ///
    /// Removing the source implicitly cancels the request that is associated to it (if any).
    ///
    /// Returns the user data that was originally passed to [`AllForksSync::add_source`], plus
    /// an `Option`.
    /// If this `Option` is `Some`, it contains a request that must be started towards the source
    /// indicated by the [`SourceId`].
    ///
    /// > **Note**: For example, if the source that has just been removed was performing an
    /// >           ancestry search, the `Option` might contain that same ancestry search.
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn remove_source(
        &mut self,
        source_id: SourceId,
    ) -> (TSrc, impl Iterator<Item = (RequestId, RequestParams, TRq)>) {
        self.inner.blocks.remove_source(source_id)
    }

    /// Returns the list of sources in this state machine.
    pub fn sources(&'_ self) -> impl ExactSizeIterator<Item = SourceId> + '_ {
        self.inner.blocks.sources()
    }

    /// Returns true if the source has earlier announced the block passed as parameter or one of
    /// its descendants.
    ///
    /// Also returns true if the requested block is inferior or equal to the known finalized block
    /// and the source has announced a block higher or equal to the known finalized block.
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
        self.inner
            .blocks
            .source_knows_non_finalized_block(source_id, height, hash)
    }

    /// Returns the list of sources for which [`AllForksSync::source_knows_non_finalized_block`]
    /// would return `true`.
    ///
    /// # Panic
    ///
    /// Panics if `height` is inferior or equal to the finalized block height. Finalized blocks
    /// are intentionally not tracked by this data structure, and panicking when asking for a
    /// potentially-finalized block prevents potentially confusing or erroneous situations.
    ///
    pub fn knows_non_finalized_block<'a>(
        &'a self,
        height: u64,
        hash: &[u8; 32],
    ) -> impl Iterator<Item = SourceId> + 'a {
        self.inner.blocks.knows_non_finalized_block(height, hash)
    }

    /// Returns the current best block of the given source.
    ///
    /// This corresponds either the latest call to [`AllForksSync::block_announce`] where
    /// `is_best` was `true`, or to the parameter passed to [`AllForksSync::add_source`].
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is invalid.
    ///
    pub fn source_best_block(&self, source_id: SourceId) -> (u64, &[u8; 32]) {
        self.inner.blocks.source_best_block(source_id)
    }

    /// Returns the user data associated to the source. This is the value originally passed
    /// through [`AllForksSync::add_source`].
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn source_user_data(&self, source_id: SourceId) -> &TSrc {
        self.inner.blocks.source_user_data(source_id)
    }

    /// Returns the user data associated to the source. This is the value originally passed
    /// through [`AllForksSync::add_source`].
    ///
    /// # Panic
    ///
    /// Panics if the [`SourceId`] is out of range.
    ///
    pub fn source_user_data_mut(&mut self, source_id: SourceId) -> &mut TSrc {
        self.inner.blocks.source_user_data_mut(source_id)
    }

    /// Returns the details of a request to start towards a source.
    ///
    /// This method doesn't modify the state machine in any way. [`AllForksSync::add_request`]
    /// must be called in order for the request to actually be marked as started.
    pub fn desired_requests(&'_ self) -> impl Iterator<Item = (SourceId, RequestParams)> + '_ {
        // TODO: need to periodically query for justifications of non-finalized blocks that change GrandPa authorities

        // TODO: allow multiple requests towards the same source?

        self.inner
            .blocks
            .desired_requests()
            .filter(|rq| rq.source_num_existing_requests == 0)
            .filter(move |rq| {
                !self
                    .chain
                    .contains_non_finalized_block(&rq.request_params.first_block_hash)
            })
            .map(|rq| (rq.source_id, rq.request_params))
    }

    /// Inserts a new request in the data structure.
    ///
    /// > **Note**: The request doesn't necessarily have to match a request returned by
    /// >           [`AllForksSync::desired_requests`].
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
        self.inner.blocks.add_request(source_id, detail, user_data)
    }

    /// Returns a list of requests that are considered obsolete and can be removed using
    /// [`AllForksSync::finish_ancestry_search`] or similar.
    ///
    /// A request becomes obsolete if the state of the request blocks changes in such a way that
    /// they don't need to be requested anymore. The response to the request will be useless.
    ///
    /// > **Note**: It is in no way mandatory to actually call this function and cancel the
    /// >           requests that are returned.
    pub fn obsolete_requests(&'_ self) -> impl Iterator<Item = RequestId> + '_ {
        self.inner.blocks.obsolete_requests()
    }

    /// Call in response to a response being finished.
    ///
    /// The headers are expected to be sorted in decreasing order. The first element of the
    /// iterator should be the block with the hash that was referred by
    /// [`RequestParams::first_block_hash`]. Each subsequent element is then expected to
    /// be the parent of the previous one.
    ///
    /// It is legal for the iterator to be shorter than the number of blocks that were requested
    /// through [`RequestParams::num_blocks`].
    ///
    /// # Panic
    ///
    /// Panics if the [`RequestId`] is invalid.
    ///
    pub fn finish_ancestry_search(
        &mut self,
        request_id: RequestId,
        received_blocks: Result<
            impl Iterator<Item = RequestSuccessBlock<impl AsRef<[u8]>, impl AsRef<[u8]>>>,
            (),
        >,
    ) -> AncestrySearchResponseOutcome {
        // Sets the `occupation` of `source_id` back to `AllSync`.
        let (
            pending_blocks::RequestParams {
                first_block_hash: requested_block_hash,
                first_block_height: requested_block_height,
                ..
            },
            source_id,
            _, // TODO: unused
        ) = self.inner.blocks.finish_request(request_id);

        // The body of this function mostly consists in verifying that the received answer is
        // correct.
        // TODO: shouldn't that be done in the networking? ^

        // Set to true below if any block is inserted in `disjoint_headers`.
        let mut any_progress = false;

        // The next block in the list of headers should have a hash equal to this one.
        let mut expected_next_hash = requested_block_hash;

        // Iterate through the headers. If the request has failed, treat it the same way as if
        // no blocks were returned.
        for (index_in_response, received_block) in received_blocks.into_iter().flatten().enumerate()
        {
            let scale_encoded_header = received_block.scale_encoded_header.as_ref();

            // Compare expected with actual hash.
            // This ensure that each header being processed is the parent of the previous one.
            if expected_next_hash != header::hash_from_scale_encoded_header(scale_encoded_header) {
                break;
            }

            // Invalid headers are skipped. The next iteration will likely fail when comparing
            // actual with expected hash, but we give it a chance.
            let decoded_header = match header::decode(scale_encoded_header) {
                Ok(h) => h,
                Err(_) => continue,
            };

            match self.block_from_source(
                source_id,
                &expected_next_hash,
                decoded_header.clone(),
                None,
                received_block
                    .scale_encoded_justification
                    .as_ref()
                    .map(|j| j.as_ref()),
                false,
            ) {
                HeaderFromSourceOutcome::HeaderVerify => {
                    return AncestrySearchResponseOutcome::Verify;
                }
                HeaderFromSourceOutcome::TooOld => {
                    // Block is below the finalized block number.
                    // Ancestry searches never request any block earlier than the finalized block
                    // number. `TooOld` can happen if the source is misbehaving, but also if the
                    // finalized block has been updated between the moment the request was emitted
                    // and the moment the response is received.
                    debug_assert_eq!(index_in_response, 0);
                    break;
                }
                HeaderFromSourceOutcome::NotFinalizedChain => {
                    // Block isn't part of the finalized chain.
                    // This doesn't necessarily mean that the source and the local node disagree
                    // on the finalized chain. It is possible that the finalized block has been
                    // updated between the moment the request was emitted and the moment the
                    // response is received.
                    return AncestrySearchResponseOutcome::NotFinalizedChain {
                        discarded_unverified_block_headers: Vec::new(), // TODO:
                    };
                }
                HeaderFromSourceOutcome::AlreadyInChain => {
                    // Block is already in chain. Can happen if a different response or
                    // announcement has arrived and been processed between the moment the request
                    // was emitted and the moment the response is received.
                    debug_assert_eq!(index_in_response, 0);
                    return AncestrySearchResponseOutcome::AllAlreadyInChain;
                }
                HeaderFromSourceOutcome::Disjoint => {
                    // Block of unknown ancestry. Continue looping.
                    any_progress = true;
                    expected_next_hash = *decoded_header.parent_hash;
                }
            }
        }

        // TODO: restore
        /*// If this is reached, then the ancestry search was inconclusive. Only disjoint blocks
        // have been received.
        if !any_progress {
            // TODO: distinguish errors from empty requests?
            // Avoid sending the same request to the same source over and over again.
            self.inner
                .blocks
                .source_mut(source_id)
                .unwrap()
                .remove_known_block(requested_block_height, requested_block_hash);
        }*/

        AncestrySearchResponseOutcome::Inconclusive
    }

    /// Update the source with a newly-announced block.
    ///
    /// > **Note**: This information is normally reported by the source itself. In the case of a
    /// >           a networking peer, call this when the source sent a block announce.
    ///
    /// # Panic
    ///
    /// Panics if `source_id` is invalid.
    ///
    pub fn block_announce(
        &mut self,
        source_id: SourceId,
        announced_scale_encoded_header: Vec<u8>,
        is_best: bool,
    ) -> BlockAnnounceOutcome {
        let announced_header = match header::decode(&announced_scale_encoded_header) {
            Ok(h) => h,
            Err(error) => return BlockAnnounceOutcome::InvalidHeader(error),
        };

        let announced_header_hash = announced_header.hash();

        match self.block_from_source(
            source_id,
            &announced_header_hash,
            announced_header,
            None,
            None,
            is_best,
        ) {
            HeaderFromSourceOutcome::HeaderVerify => BlockAnnounceOutcome::HeaderVerify,
            HeaderFromSourceOutcome::TooOld => BlockAnnounceOutcome::TooOld,
            HeaderFromSourceOutcome::AlreadyInChain => BlockAnnounceOutcome::AlreadyInChain,
            HeaderFromSourceOutcome::NotFinalizedChain => BlockAnnounceOutcome::NotFinalizedChain,
            HeaderFromSourceOutcome::Disjoint => BlockAnnounceOutcome::Disjoint,
        }
    }

    /// Process the next block in the queue of verification.
    ///
    /// This method takes ownership of the [`AllForksSync`] and starts a verification
    /// process. The [`AllForksSync`] is yielded back at the end of this process.
    pub fn process_one(self) -> ProcessOne<TBl, TRq, TSrc> {
        let block = self
            .inner
            .blocks
            .unverified_leaves()
            .filter(|block| {
                block.parent_block_hash == self.chain.finalized_block_hash()
                    || self
                        .chain
                        .contains_non_finalized_block(&block.parent_block_hash)
            })
            .next();

        if let Some(block) = block {
            ProcessOne::HeaderVerify(HeaderVerify {
                parent: self,
                block_to_verify: block,
            })
        } else {
            ProcessOne::AllSync { sync: self }
        }
    }

    /// Called when a source reports a header and an optional body, either through a block
    /// announce, an ancestry search result, or a block request, and so on.
    ///
    /// `known_to_be_source_best` being `true` means that we are sure that this is the best block
    /// of the source. `false` means "it is not", but also "maybe", "unknown", and similar.
    ///
    /// # Panic
    ///
    /// Panics if `source_id` is invalid.
    ///
    fn block_from_source(
        &mut self,
        source_id: SourceId,
        header_hash: &[u8; 32],
        header: header::HeaderRef,
        body: Option<Vec<Vec<u8>>>,
        justification: Option<&[u8]>,
        known_to_be_source_best: bool,
    ) -> HeaderFromSourceOutcome {
        debug_assert_eq!(header.hash(), *header_hash);

        // Code below does `header.number - 1`. Make sure that `header.number` isn't 0.
        if header.number == 0 {
            return HeaderFromSourceOutcome::TooOld;
        }

        // No matter what is done below, start by updating the view the state machine maintains
        // for this source.
        if known_to_be_source_best {
            self.inner
                .blocks
                .set_best_block(source_id, header.number, *header_hash);
        } else {
            self.inner
                .blocks
                .add_known_block(source_id, header.number, *header_hash);
        }

        // Source also knows the parent of the announced block.
        self.inner
            .blocks
            .add_known_block(source_id, header.number - 1, *header.parent_hash);

        // It is assumed that all sources will eventually agree on the same finalized chain. If
        // the block number is lower or equal than the locally-finalized block number, it is
        // assumed that this source is simply late compared to the local node, and that the block
        // that has been received is either part of the finalized chain or belongs to a fork that
        // will get discarded by this source in the future.
        if header.number <= self.chain.finalized_block_header().number {
            return HeaderFromSourceOutcome::TooOld;
        }

        // If the block is already part of the local tree of blocks, nothing more to do.
        if self.chain.contains_non_finalized_block(&header_hash) {
            return HeaderFromSourceOutcome::AlreadyInChain;
        }

        // At this point, we have excluded blocks that are already part of the chain or too old.
        // We insert the block in the list of unverified blocks so as to treat all blocks the
        // same.
        if !self.inner.blocks.contains_block(header.number, header_hash) {
            self.inner.blocks.insert_unverified_block(
                header.number,
                *header_hash,
                if body.is_some() {
                    pending_blocks::UnverifiedBlockState::HeaderBodyKnown {
                        parent_hash: *header.parent_hash,
                    }
                } else {
                    pending_blocks::UnverifiedBlockState::HeaderKnown {
                        parent_hash: *header.parent_hash,
                    }
                },
                PendingBlock {
                    body,
                    header: Some(header.clone().into()),
                    justification: justification.map(|j| j.to_vec()),
                },
            );
        } else {
            if body.is_some() {
                self.inner.blocks.set_block_header_body_known(
                    header.number,
                    header_hash,
                    *header.parent_hash,
                );
            } else {
                self.inner.blocks.set_block_header_known(
                    header.number,
                    header_hash,
                    *header.parent_hash,
                );
            }

            let block_user_data = self
                .inner
                .blocks
                .block_user_data_mut(header.number, header_hash);
            if block_user_data.header.is_none() {
                block_user_data.header = Some(header.clone().into()); // TODO: copying bytes :-/
            }
            // TODO: what if body was already known, but differs from what is stored?
            if block_user_data.body.is_none() {
                if let Some(body) = body {
                    block_user_data.body = Some(body);
                }
            }
        }

        // TODO: what if the pending block already contains a justification and it is not the
        //       same as here? since justifications aren't immediately verified, it is possible
        //       for a malicious peer to send us bad justifications

        // Block is not part of the finalized chain.
        if header.number == self.chain.finalized_block_header().number + 1
            && *header.parent_hash != self.chain.finalized_block_hash()
        {
            // TODO: remove_verify_failed
            return HeaderFromSourceOutcome::NotFinalizedChain;
        }

        if *header.parent_hash == self.chain.finalized_block_hash()
            || self
                .chain
                .non_finalized_block_by_hash(header.parent_hash)
                .is_some()
        {
            // TODO: ambiguous naming
            return HeaderFromSourceOutcome::HeaderVerify;
        }

        // TODO: if pending_blocks.num_blocks() > some_max { remove uninteresting block }

        HeaderFromSourceOutcome::Disjoint
    }

    /*/// Call in response to a [`BlockAnnounceOutcome::BlockBodyDownloadStart`].
    ///
    /// # Panic
    ///
    /// Panics if the [`RequestId`] is invalid.
    ///
    pub fn block_body_response(
        mut self,
        now_from_unix_epoch: Duration,
        request_id: RequestId,
        block_body: impl Iterator<Item = impl AsRef<[u8]>>,
    ) -> (BlockBodyVerify<TBl, TRq, TSrc>, Option<Request>) {
        // TODO: unfinished

        todo!()

        /*// TODO: update occupation

        // Removes traces of the request from the state machine.
        let block_header_hash = if let Some((h, _)) = self
            .inner
            .pending_body_downloads
            .iter_mut()
            .find(|(_, (_, s))| *s == Some(source_id))
        {
            let hash = *h;
            let header = self.inner.pending_body_downloads.remove(&hash).unwrap().0;
            (header, hash)
        } else {
            panic!()
        };

        // Sanity check.
        debug_assert_eq!(block_header_hash.1, block_header_hash.0.hash());

        // If not full, there shouldn't be any block body download happening in the first place.
        debug_assert!(self.inner.full);

        match self
            .chain
            .verify_body(
                block_header_hash.0.scale_encoding()
                    .fold(Vec::new(), |mut a, b| { a.extend_from_slice(b.as_ref()); a }), now_from_unix_epoch) // TODO: stupid extra allocation
        {
            blocks_tree::BodyVerifyStep1::BadParent { .. }
            | blocks_tree::BodyVerifyStep1::InvalidHeader(..)
            | blocks_tree::BodyVerifyStep1::Duplicate(_) => unreachable!(),
            blocks_tree::BodyVerifyStep1::ParentRuntimeRequired(_runtime_req) => {
                todo!()
            }
        }*/
    }*/
}

/// Struct to pass back when a block request has succeeded.
#[derive(Debug)]
pub struct RequestSuccessBlock<THdr, TJs> {
    /// SCALE-encoded header returned by the remote.
    pub scale_encoded_header: THdr,
    /// SCALE-encoded justification returned by the remote.
    pub scale_encoded_justification: Option<TJs>,
}

/// Outcome of calling [`AllForksSync::block_from_source`].
///
/// Not public.
enum HeaderFromSourceOutcome {
    /// Header is ready to be verified.
    HeaderVerify,

    /// Announced block is too old to be part of the finalized chain.
    ///
    /// It is assumed that all sources will eventually agree on the same finalized chain. Blocks
    /// whose height is inferior to the height of the latest known finalized block should simply
    /// be ignored. Whether or not this old block is indeed part of the finalized block isn't
    /// verified, and it is assumed that the source is simply late.
    TooOld,
    /// Announced block has already been successfully verified and is part of the non-finalized
    /// chain.
    AlreadyInChain,
    /// Announced block is known to not be a descendant of the finalized block.
    NotFinalizedChain,
    /// Header cannot be verified now, and has been stored for later.
    Disjoint,
}

/// Outcome of calling [`AllForksSync::block_announce`].
pub enum BlockAnnounceOutcome {
    /// Header is ready to be verified.
    HeaderVerify,

    /// Announced block is too old to be part of the finalized chain.
    ///
    /// It is assumed that all sources will eventually agree on the same finalized chain. Blocks
    /// whose height is inferior to the height of the latest known finalized block should simply
    /// be ignored. Whether or not this old block is indeed part of the finalized block isn't
    /// verified, and it is assumed that the source is simply late.
    TooOld,
    /// Announced block has already been successfully verified and is part of the non-finalized
    /// chain.
    AlreadyInChain,
    /// Announced block is known to not be a descendant of the finalized block.
    NotFinalizedChain,
    /// Header cannot be verified now, and has been stored for later.
    Disjoint,
    /// Failed to decode announce header.
    InvalidHeader(header::Error),
}

/// Outcome of calling [`AllForksSync::finish_ancestry_search`].
pub enum AncestrySearchResponseOutcome {
    /// Ready to start verifying one or more headers returned in the ancestry search.
    // TODO: might not actually mean that ProcessOne isn't AllSync; confusing
    Verify,

    /// Source has given blocks that aren't part of the finalized chain.
    ///
    /// This doesn't necessarily mean that the source is malicious or uses a different chain. It
    /// is possible for this to legitimately happen, for example if the finalized chain has been
    /// updated while the ancestry search was in progress.
    NotFinalizedChain {
        /// List of block headers that were pending verification and that have now been discarded
        /// since it has been found out that they don't belong to the finalized chain.
        discarded_unverified_block_headers: Vec<Vec<u8>>,
    },

    /// Couldn't verify any of the blocks of the ancestry search. Some or all of these blocks
    /// have been stored in the local machine for later.
    Inconclusive,

    /// All blocks in the ancestry search response were already in the list of verified blocks.
    ///
    /// This can happen if a block announce or different ancestry search response has been
    /// processed in between the request and response.
    AllAlreadyInChain,
}

/// Header verification to be performed.
///
/// Internally holds the [`AllForksSync`].
pub struct HeaderVerify<TBl, TRq, TSrc> {
    parent: AllForksSync<TBl, TRq, TSrc>,
    /// Block that can be verified.
    block_to_verify: pending_blocks::TreeRoot,
}

impl<TBl, TRq, TSrc> HeaderVerify<TBl, TRq, TSrc> {
    /// Returns the height of the block to be verified.
    pub fn height(&self) -> u64 {
        self.block_to_verify.block_number
    }

    /// Returns the hash of the block to be verified.
    pub fn hash(&self) -> &[u8; 32] {
        &self.block_to_verify.block_hash
    }

    /// Perform the verification.
    pub fn perform(
        mut self,
        now_from_unix_epoch: Duration,
        user_data: TBl,
    ) -> HeaderVerifyOutcome<TBl, TRq, TSrc> {
        let to_verify_scale_encoded_header = self
            .parent
            .inner
            .blocks
            .block_user_data(
                self.block_to_verify.block_number,
                &self.block_to_verify.block_hash,
            )
            .header
            .as_ref()
            .unwrap()
            .scale_encoding_vec();

        let result = match self
            .parent
            .chain
            .verify_header(to_verify_scale_encoded_header, now_from_unix_epoch)
        {
            Ok(blocks_tree::HeaderVerifySuccess::Insert {
                insert,
                is_new_best,
                ..
            }) => {
                // TODO: cloning the header :-/
                let block = Block {
                    header: insert.header().into(),
                    user_data,
                };
                insert.insert(block);
                Ok(is_new_best)
            }
            Err(blocks_tree::HeaderVerifyError::VerificationFailed(error)) => {
                Err((error, user_data))
            }
            Ok(blocks_tree::HeaderVerifySuccess::Duplicate)
            | Err(blocks_tree::HeaderVerifyError::BadParent { .. })
            | Err(blocks_tree::HeaderVerifyError::InvalidHeader(_)) => unreachable!(),
        };

        // Remove the verified block from `pending_blocks`.
        let justification = if result.is_ok() {
            let outcome = self.parent.inner.blocks.remove(
                self.block_to_verify.block_number,
                &self.block_to_verify.block_hash,
            );
            outcome.justification
        } else {
            self.parent.inner.blocks.set_block_bad(
                self.block_to_verify.block_number,
                &self.block_to_verify.block_hash,
            );
            None
        };

        let justification_verification = if let Some(justification) = justification {
            match self.parent.chain.verify_justification(&justification) {
                Ok(success) => {
                    let finalized = success
                        .apply()
                        .map(|b| (b.header, b.user_data))
                        .collect::<Vec<_>>();
                    self.parent
                        .inner
                        .blocks
                        .set_finalized_block_height(finalized.last().unwrap().0.number);
                    JustificationVerification::NewFinalized(finalized)
                }
                Err(err) => JustificationVerification::JustificationVerificationError(err),
            }
        } else {
            JustificationVerification::NoJustification
        };

        match result {
            Ok(is_new_best) => HeaderVerifyOutcome::Success {
                is_new_best,
                justification_verification,
                sync: self.parent,
            },
            Err((error, user_data)) => HeaderVerifyOutcome::Error {
                sync: self.parent,
                error,
                user_data,
            },
        }
    }

    /// Do not actually proceed with the verification.
    pub fn cancel(self) -> AllForksSync<TBl, TRq, TSrc> {
        self.parent
    }
}

/// State of the processing of blocks.
pub enum ProcessOne<TBl, TRq, TSrc> {
    /// No processing is necessary.
    ///
    /// Calling [`AllForksSync::process_one`] again is unnecessary.
    AllSync {
        /// The state machine.
        /// The [`AllForksSync::process_one`] method takes ownership of the [`AllForksSync`]. This
        /// field yields it back.
        sync: AllForksSync<TBl, TRq, TSrc>,
    },

    /// A header is ready for verification.
    HeaderVerify(HeaderVerify<TBl, TRq, TSrc>),
}

/// Outcome of calling [`HeaderVerify::perform`].
pub enum HeaderVerifyOutcome<TBl, TRq, TSrc> {
    /// Header has been successfully verified.
    Success {
        /// True if the newly-verified block is considered the new best block.
        is_new_best: bool,
        /// If a justification was attached to this block, it has also been verified. Contains the
        /// outcome.
        justification_verification: JustificationVerification<TBl>,
        /// State machine yielded back. Use to continue the processing.
        sync: AllForksSync<TBl, TRq, TSrc>,
    },

    /// Header verification failed.
    Error {
        /// State machine yielded back. Use to continue the processing.
        sync: AllForksSync<TBl, TRq, TSrc>,
        /// Error that happened.
        error: verify::header_only::Error,
        /// User data that was passed to [`HeaderVerify::perform`] and is unused.
        user_data: TBl,
    },
}

/// Information about the verification of a justification that was stored for this block.
#[derive(Debug)]
pub enum JustificationVerification<TBl> {
    /// No information about finality
    NoJustification,
    /// A justification was available for the newly-verified block, but it failed to verify.
    JustificationVerificationError(blocks_tree::JustificationVerifyError),
    /// Justification verification successful. The block and all its ancestors is now finalized.
    NewFinalized(Vec<(header::Header, TBl)>),
}

impl<TBl> JustificationVerification<TBl> {
    /// Returns `true` for [`JustificationVerification::NewFinalized`].
    pub fn is_success(&self) -> bool {
        matches!(self, JustificationVerification::NewFinalized(_))
    }
}

/// State of the processing of blocks.
pub enum BlockBodyVerify<TBl, TRq, TSrc> {
    #[doc(hidden)]
    Foo(core::marker::PhantomData<(TBl, TRq, TSrc)>),
    // TODO: finish
    /*/// Processing of the block is over.
    ///
    /// There might be more blocks remaining. Call [`AllForksSync::process_one`] again.
    NewBest {
        /// The state machine.
        /// The [`AllForksSync::process_one`] method takes ownership of the
        /// [`AllForksSync`]. This field yields it back.
        sync: AllForksSync<TBl, TRq, TSrc>,

        new_best_number: u64,
        new_best_hash: [u8; 32],
    },

    /// Processing of the block is over. The block has been finalized.
    ///
    /// There might be more blocks remaining. Call [`AllForksSync::process_one`] again.
    Finalized {
        /// The state machine.
        /// The [`AllForksSync::process_one`] method takes ownership of the
        /// [`AllForksSync`]. This field yields it back.
        sync: AllForksSync<TBl, TRq, TSrc>,

        /// Blocks that have been finalized. Includes the block that has just been verified.
        finalized_blocks: Vec<Block<TBl>>,
    },

    /// Loading a storage value of the finalized block is required in order to continue.
    FinalizedStorageGet(StorageGet<TBl, TRq, TSrc>),

    /// Fetching the list of keys of the finalized block with a given prefix is required in order
    /// to continue.
    FinalizedStoragePrefixKeys(StoragePrefixKeys<TBl, TRq, TSrc>),

    /// Fetching the key of the finalized block storage that follows a given one is required in
    /// order to continue.
    FinalizedStorageNextKey(StorageNextKey<TBl, TRq, TSrc>),*/
}
