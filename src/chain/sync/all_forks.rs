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

use alloc::collections::VecDeque;
use core::{
    iter, mem,
    num::{NonZeroU32, NonZeroU64},
    time::Duration,
};

mod pending_blocks;
mod sources;

pub use sources::SourceId;

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

pub struct AllForksSync<TSrc, TBl> {
    /// Data structure containing the non-finalized blocks.
    ///
    /// If [`Inner::full`], this only contains blocks whose header *and* body have been verified.
    chain: blocks_tree::NonFinalizedTree<Block<TBl>>,

    /// Extra fields. In a separate structure in order to be moved around.
    inner: Inner<TSrc>,
}

/// Extra fields. In a separate structure in order to be moved around.
struct Inner<TSrc> {
    /// See [`Config::full`].
    full: bool,

    /// List of sources. Controlled by the API user.
    // TODO: somehow limit the size of this container, to avoid growing forever if the source continuously announces fake blocks?
    sources: sources::AllForksSources<Source<TSrc>>,

    /// List of blocks whose existence is known but can't be verified yet.
    /// Also contains a list of ongoing requests. The requests are kept up-to-date with the
    /// information in [`Inner::sources`].
    pending_blocks: pending_blocks::PendingBlocks<(), ()>,
}

struct Block<TBl> {
    user_data: TBl,
}

/// Extra fields specific to each blocks source.
struct Source<TSrc> {
    /// What the source is busy doing.
    occupation: Option<pending_blocks::RequestId>,
    user_data: TSrc,
}

impl<TSrc, TBl> AllForksSync<TSrc, TBl> {
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
                full: config.full,
                sources: sources::AllForksSources::new(
                    config.sources_capacity,
                    finalized_block_height,
                ),
                pending_blocks: pending_blocks::PendingBlocks::new(pending_blocks::Config {
                    blocks_capacity: config.blocks_capacity,
                    max_requests_per_block: config.max_requests_per_block,
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
    /// retrieved using [`SourceMutAccess::user_data`].
    ///
    /// Returns the newly-created source entry, plus optionally a request that should be started
    /// towards this source.
    pub fn add_source(
        &mut self,
        user_data: TSrc,
        best_block_number: u64,
        best_block_hash: [u8; 32],
    ) -> (SourceMutAccess<TSrc, TBl>, Option<Request>) {
        let new_source = self.inner.sources.add_source(
            Source {
                occupation: None,
                user_data,
            },
            best_block_number,
            best_block_hash,
        );

        if best_block_number > self.chain.finalized_block_header().number
            && self
                .chain
                .non_finalized_block_by_hash(&best_block_hash)
                .is_none()
        {
            self.inner
                .pending_blocks
                .block_mut(best_block_number, best_block_hash)
                .or_insert(());
        }

        let source_id = new_source.id();
        let request = self.source_next_request(source_id);

        (
            SourceMutAccess {
                parent: self,
                source_id,
            },
            request,
        )
    }

    /// Grants access to a source, using its identifier.
    pub fn source_mut(&mut self, id: SourceId) -> Option<SourceMutAccess<TSrc, TBl>> {
        if self.inner.sources.source_mut(id).is_some() {
            Some(SourceMutAccess {
                parent: self,
                source_id: id,
            })
        } else {
            None
        }
    }

    /// Call in response to a [`Request::AncestrySearch`].
    ///
    /// The headers are expected to be sorted in decreasing order. The first element of the
    /// iterator should be the block with the hash passed through
    /// [`Request::AncestrySearch::first_block_hash`]. Each subsequent element is then expected to
    /// be the parent of the previous one.
    ///
    /// It is legal for the iterator to be shorter than the number of blocks that were requested
    /// through [`Request::AncestrySearch::num_blocks`].
    ///
    /// # Panic
    ///
    /// Panics if the source wasn't known locally as downloading something.
    ///
    pub fn ancestry_search_response(
        mut self,
        source_id: SourceId,
        scale_encoded_headers: Result<impl Iterator<Item = impl AsRef<[u8]>>, ()>,
    ) -> AncestrySearchResponseOutcome<TSrc, TBl> {
        // Sets the `occupation` of `source_id` back to `Idle`.
        let (_, requested_block_height, requested_block_hash) = match mem::take(
            &mut self
                .inner
                .sources
                .source_mut(source_id)
                .unwrap()
                .user_data()
                .occupation,
        ) {
            Some(request_id) => self.inner.pending_blocks.finish_request(request_id),
            None => panic!(),
        };

        // Set to true below if any block is inserted in `disjoint_headers`.
        let mut any_progress = false;

        // The next block in the list of headers should have a hash equal to this one.
        let mut expected_next_hash = requested_block_hash;

        // Iterate through the headers. If the request has failed, treat it the same way as if
        // no blocks were returned.
        for (index_in_response, scale_encoded_header) in scale_encoded_headers
            .into_iter()
            .flat_map(|l| l)
            .enumerate()
        {
            let scale_encoded_header = scale_encoded_header.as_ref();

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

            match self.header_from_source(
                source_id,
                &expected_next_hash,
                decoded_header.clone(),
                false,
            ) {
                HeaderFromSourceOutcome::HeaderVerify(this) => {
                    return AncestrySearchResponseOutcome::Verify(this);
                }
                HeaderFromSourceOutcome::TooOld(this) => {
                    // Block is below the finalized block number.
                    // Ancestry searches never request any block earlier than the finalized block
                    // number. `TooOld` can happen if the source is misbehaving, but also if the
                    // finalized block has been updated between the moment the request was emitted
                    // and the moment the response is received.
                    debug_assert_eq!(index_in_response, 0);
                    self = this;
                    break;
                }
                HeaderFromSourceOutcome::NotFinalizedChain(this) => {
                    // Block isn't part of the finalized chain.
                    // This doesn't necessarily mean that the source and the local node disagree
                    // on the finalized chain. It is possible that the finalized block has been
                    // updated between the moment the request was emitted and the moment the
                    // response is received.
                    self = this;

                    let next_request = self.source_next_request(source_id);
                    return AncestrySearchResponseOutcome::NotFinalizedChain {
                        sync: self,
                        next_request,
                        discarded_unverified_block_headers: Vec::new(), // TODO:
                    };
                }
                HeaderFromSourceOutcome::AlreadyInChain(mut this) => {
                    // Block is already in chain. Can happen if a different response or
                    // announcement has arrived and been processed between the moment the request
                    // was emitted and the moment the response is received.
                    debug_assert_eq!(index_in_response, 0);
                    let next_request = this.source_next_request(source_id);
                    return AncestrySearchResponseOutcome::AllAlreadyInChain {
                        sync: this,
                        next_request,
                    };
                }
                HeaderFromSourceOutcome::Disjoint(this) => {
                    // Block of unknown ancestry. Continue looping.
                    any_progress = true;
                    self = this;
                    expected_next_hash = *decoded_header.parent_hash;
                }
            }
        }

        // If this is reached, then the ancestry search was inconclusive. Only disjoint blocks
        // have been received.
        if !any_progress {
            // TODO: distinguish errors from empty requests?
            // Avoid sending the same request to the same source over and over again.
            self.inner
                .sources
                .source_mut(source_id)
                .unwrap()
                .remove_known_block(requested_block_height, requested_block_hash);
        }

        let next_request = self.source_next_request(source_id);
        AncestrySearchResponseOutcome::Inconclusive {
            sync: self,
            next_request,
        }
    }

    /// Finds a request that the given source could start performing.
    ///
    /// If `Some` is returned, updates `self` and returns the request that must be started.
    #[must_use]
    fn source_next_request(&mut self, source_id: SourceId) -> Option<Request> {
        let mut source_access = self.inner.sources.source_mut(source_id).unwrap();

        // Don't start more than one request at a time.
        // TODO: in the future, allow multiple requests
        if source_access.user_data().occupation.is_some() {
            return None;
        }

        // Find a query that is appropriate for this source.
        let query_to_start = self
            .inner
            .pending_blocks
            .desired_queries()
            .find(|desired_query| {
                // The source can only operate on blocks that it knows about.
                // `continue` if this block isn't known by this source.
                source_access.knows_block(
                    desired_query.first_block_height,
                    &desired_query.first_block_hash,
                )
            })?;

        // Start the request.
        let request_id: pending_blocks::RequestId = self
            .inner
            .pending_blocks
            .block_mut(
                query_to_start.first_block_height,
                query_to_start.first_block_hash,
            )
            .into_occupied()
            .unwrap()
            .add_descending_request((), query_to_start.num_blocks);
        source_access.user_data().occupation = Some(request_id);

        Some(Request::AncestrySearch {
            first_block_hash: query_to_start.first_block_hash,
            num_blocks: query_to_start.num_blocks,
        })
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
        self,
        source_id: SourceId,
        announced_scale_encoded_header: Vec<u8>,
        is_best: bool,
    ) -> BlockAnnounceOutcome<TSrc, TBl> {
        let announced_header = match header::decode(&announced_scale_encoded_header) {
            Ok(h) => h,
            Err(error) => return BlockAnnounceOutcome::InvalidHeader { sync: self, error },
        };

        let announced_header_hash = announced_header.hash();

        match self.header_from_source(source_id, &announced_header_hash, announced_header, is_best)
        {
            HeaderFromSourceOutcome::HeaderVerify(verify) => {
                BlockAnnounceOutcome::HeaderVerify(verify)
            }
            HeaderFromSourceOutcome::TooOld(sync) => BlockAnnounceOutcome::TooOld(sync),
            HeaderFromSourceOutcome::AlreadyInChain(sync) => {
                BlockAnnounceOutcome::AlreadyInChain(sync)
            }
            HeaderFromSourceOutcome::NotFinalizedChain(sync) => {
                BlockAnnounceOutcome::NotFinalizedChain(sync)
            }
            HeaderFromSourceOutcome::Disjoint(mut sync) => {
                let next_request = sync.source_next_request(source_id);
                BlockAnnounceOutcome::Disjoint { sync, next_request }
            }
        }
    }

    /// Called when a source reports a header, either through a block announce, an ancestry
    /// search result, or a block header query.
    ///
    /// `known_to_be_source_best` being `true` means that we are sure that this is the best block
    /// of the source. `false` means "it is not", but also "maybe", "unknown", and similar.
    ///
    /// # Panic
    ///
    /// Panics if `source_id` is invalid.
    ///
    fn header_from_source(
        mut self,
        source_id: SourceId,
        header_hash: &[u8; 32],
        header: header::HeaderRef,
        known_to_be_source_best: bool,
    ) -> HeaderFromSourceOutcome<TSrc, TBl> {
        debug_assert_eq!(header.hash(), *header_hash);

        // No matter what is done below, start by updating the view the local state machine
        // maintains for this source.
        let mut source_access = self.inner.sources.source_mut(source_id).unwrap();
        if known_to_be_source_best {
            source_access.set_best_block(header.number, header.hash());
        } else {
            source_access.add_known_block(header.number, header.hash());
        }
        source_access.add_known_block(header.number - 1, *header.parent_hash);

        // It is assumed that all sources will eventually agree on the same finalized chain. If
        // the block number is lower or equal than the locally-finalized block number, it is
        // assumed that this source is simply late compared to the local node, and that the block
        // that has been received is either part of the finalized chain or belongs to a fork that
        // will get discarded by this source in the future.
        if header.number <= self.chain.finalized_block_header().number {
            return HeaderFromSourceOutcome::TooOld(self);
        }

        // If the block is already part of the local tree of blocks, nothing more to do.
        if self
            .chain
            .non_finalized_block_by_hash(&header_hash)
            .is_some()
        {
            return HeaderFromSourceOutcome::AlreadyInChain(self);
        }

        // TODO: somehow optimize? the encoded block is normally known from it being decoded
        let scale_encoded_header = header.scale_encoding_vec();

        // TODO: if pending_blocks.num_blocks() > some_max { remove uninteresting block }

        let mut block_access = self
            .inner
            .pending_blocks
            .block_mut(header.number, *header_hash)
            .or_insert(()); // TODO: don't always invert

        if *header.parent_hash == self.chain.finalized_block_hash()
            || self
                .chain
                .non_finalized_block_by_hash(header.parent_hash)
                .is_some()
        {
            // Parent is in the `NonFinalizedTree`, meaning it is possible to verify it.
            // TODO: don't do this
            block_access.update_header(scale_encoded_header.clone()); // TODO: clone :(

            HeaderFromSourceOutcome::HeaderVerify(HeaderVerify {
                parent: self,
                source_id,
                verifiable_blocks: iter::once((header.number, *header_hash, scale_encoded_header))
                    .collect(),
            })
        } else if header.number == self.chain.finalized_block_header().number + 1 {
            // Checked above.
            debug_assert_ne!(*header.parent_hash, self.chain.finalized_block_hash());

            // Announced block is not part of the finalized chain.
            block_access.remove_verify_failed();
            HeaderFromSourceOutcome::NotFinalizedChain(self)
        } else {
            // Parent is not in the `NonFinalizedTree`. It is unknown whether this block belongs
            // to the same finalized chain as the one known locally, but we expect that it is the
            // case.
            block_access.update_header(scale_encoded_header);

            // Also insert the parent in the list of pending blocks.
            self.inner
                .pending_blocks
                .block_mut(header.number - 1, *header.parent_hash)
                .or_insert(());

            HeaderFromSourceOutcome::Disjoint(self)
        }
    }

    /*/// Call in response to a [`BlockAnnounceOutcome::BlockBodyDownloadStart`].
    ///
    /// # Panic
    ///
    /// Panics if the source wasn't known locally as downloading something.
    ///
    pub fn block_body_response(
        mut self,
        now_from_unix_epoch: Duration,
        source_id: SourceId,
        block_body: impl Iterator<Item = impl AsRef<[u8]>>,
    ) -> (BlockBodyVerify<TSrc, TBl>, Option<Request>) {
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

/// Request that should be performed towards a source.
#[must_use]
pub enum Request {
    /// An ancestry search is necessary in situations where there are links missing between some
    /// block headers and the local chain of valid blocks. It consists in asking the source for
    /// its block headers in descending order starting from `first_block_height`. The answer will
    /// make it possible for the local state machine to determine how the chain is connected.
    ///
    /// > **Note**: This situation can happen for instance after a network split (also called
    /// >           *netsplit*) ends. During the split, some nodes have produced one chain, while
    /// >           some other nodes have produced a different chain.
    AncestrySearch {
        /// Hash of the first block to request.
        first_block_hash: [u8; 32],

        /// Number of blocks the request should return.
        ///
        /// Note that this is only an indication, and the source is free to give fewer blocks
        /// than requested. If that happens, the state machine might later send out further
        /// ancestry search requests to complete the chain.
        num_blocks: NonZeroU64,
    },

    /// The header of the block with the given hash is requested.
    HeaderRequest {
        /// Height of the block.
        ///
        /// > **Note**: This value is passed because it is always known, but the hash alone is
        /// >           expected to be enough to fetch the block header.
        number: u64,

        /// Hash of the block whose header to obtain.
        hash: [u8; 32],
    },

    /// The body of the block with the given hash is requested.
    ///
    /// Can only happen if [`Config::full`].
    BodyRequest {
        /// Height of the block.
        ///
        /// > **Note**: This value is passed because it is always known, but the hash alone is
        /// >           expected to be enough to fetch the block body.
        number: u64,

        /// Hash of the block whose body to obtain.
        hash: [u8; 32],
    },
}

/// Access to a source in a [`AllForksSync`]. Obtained through [`AllForksSync::source_mut`].
pub struct SourceMutAccess<'a, TSrc, TBl> {
    parent: &'a mut AllForksSync<TSrc, TBl>,

    /// Guaranteed to be a valid entry in [`Inner::sources`].
    source_id: SourceId,
}

impl<'a, TSrc, TBl> SourceMutAccess<'a, TSrc, TBl> {
    /// Returns the identifier of this source.
    pub fn id(&self) -> SourceId {
        self.source_id
    }

    /// Returns true if the source has earlier announced the block passed as parameter or one of
    /// its descendants.
    // TODO: document precisely what it means
    // TODO: shouldn't take &mut self but just &self
    pub fn knows_block(&mut self, height: u64, hash: &[u8; 32]) -> bool {
        self.parent
            .inner
            .sources
            .source_mut(self.source_id)
            .unwrap()
            .knows_block(height, hash)
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
    pub fn remove(self) -> (TSrc, Option<(SourceId, Request)>) {
        let source = self
            .parent
            .inner
            .sources
            .source_mut(self.source_id)
            .unwrap()
            .remove();

        if let Some(request_id) = source.occupation {
            // TODO: self.parent.inner.pending_blocks.finish_request(request_id);
            todo!()
        }

        // TODO: None hardcoded
        (source.user_data, None)
    }

    /// Returns the user data associated to the source. This is the value originally passed
    /// through [`AllForksSync::add_source`].
    pub fn user_data(&mut self) -> &mut TSrc {
        let source = self
            .parent
            .inner
            .sources
            .source_mut(self.source_id)
            .unwrap();
        &mut source.into_user_data().user_data
    }

    /// Returns the user data associated to the source. This is the value originally passed
    /// through [`AllForksSync::add_source`].
    pub fn into_user_data(self) -> &'a mut TSrc {
        let source = self
            .parent
            .inner
            .sources
            .source_mut(self.source_id)
            .unwrap();
        &mut source.into_user_data().user_data
    }
}

/// Outcome of calling [`AllForksSync::header_from_source`].
///
/// Not public.
enum HeaderFromSourceOutcome<TSrc, TBl> {
    /// Header is ready to be verified.
    HeaderVerify(HeaderVerify<TSrc, TBl>),

    /// Announced block is too old to be part of the finalized chain.
    ///
    /// It is assumed that all sources will eventually agree on the same finalized chain. Blocks
    /// whose height is inferior to the height of the latest known finalized block should simply
    /// be ignored. Whether or not this old block is indeed part of the finalized block isn't
    /// verified, and it is assumed that the source is simply late.
    TooOld(AllForksSync<TSrc, TBl>),
    /// Announced block has already been successfully verified and is part of the non-finalized
    /// chain.
    AlreadyInChain(AllForksSync<TSrc, TBl>),
    /// Announced block is known to not be a descendant of the finalized block.
    NotFinalizedChain(AllForksSync<TSrc, TBl>),
    /// Header cannot be verified now, and has been stored for later.
    Disjoint(AllForksSync<TSrc, TBl>),
}

/// Outcome of calling [`AllForksSync::block_announce`].
pub enum BlockAnnounceOutcome<TSrc, TBl> {
    /// Header is ready to be verified.
    HeaderVerify(HeaderVerify<TSrc, TBl>),

    /// Announced block is too old to be part of the finalized chain.
    ///
    /// It is assumed that all sources will eventually agree on the same finalized chain. Blocks
    /// whose height is inferior to the height of the latest known finalized block should simply
    /// be ignored. Whether or not this old block is indeed part of the finalized block isn't
    /// verified, and it is assumed that the source is simply late.
    TooOld(AllForksSync<TSrc, TBl>),
    /// Announced block has already been successfully verified and is part of the non-finalized
    /// chain.
    AlreadyInChain(AllForksSync<TSrc, TBl>),
    /// Announced block is known to not be a descendant of the finalized block.
    NotFinalizedChain(AllForksSync<TSrc, TBl>),
    /// Header cannot be verified now, and has been stored for later.
    Disjoint {
        sync: AllForksSync<TSrc, TBl>,
        /// Next request that the same source should now perform.
        next_request: Option<Request>,
    },
    /// Failed to decode announce header.
    InvalidHeader {
        sync: AllForksSync<TSrc, TBl>,
        error: header::Error,
    },
}

/// Outcome of calling [`AllForksSync::ancestry_search_response`].
pub enum AncestrySearchResponseOutcome<TSrc, TBl> {
    /// Ready to start verifying one or more headers returned in the ancestry search.
    Verify(HeaderVerify<TSrc, TBl>),

    /// Source has given blocks that aren't part of the finalized chain.
    ///
    /// This doesn't necessarily mean that the source is malicious or uses a different chain. It
    /// is possible for this to legitimately happen, for example if the finalized chain has been
    /// updated while the ancestry search was in progress.
    NotFinalizedChain {
        sync: AllForksSync<TSrc, TBl>,

        /// Next request that the same source should now perform.
        next_request: Option<Request>,

        /// List of block headers that were pending verification and that have now been discarded
        /// since it has been found out that they don't belong to the finalized chain.
        discarded_unverified_block_headers: Vec<Vec<u8>>,
    },

    /// Couldn't verify any of the blocks of the ancestry search. Some or all of these blocks
    /// have been stored in the local machine for later.
    Inconclusive {
        sync: AllForksSync<TSrc, TBl>,

        /// Next request that the same source should now perform.
        next_request: Option<Request>,
    },

    /// All blocks in the ancestry search response were already in the list of verified blocks.
    ///
    /// This can happen if a block announce or different ancestry search response has been
    /// processed in between the request and response.
    AllAlreadyInChain {
        sync: AllForksSync<TSrc, TBl>,

        /// Next request that the same source should now perform.
        next_request: Option<Request>,
    },
}

/// Header verification to be performed.
///
/// Internally holds the [`AllForksSync`].
pub struct HeaderVerify<TSrc, TBl> {
    parent: AllForksSync<TSrc, TBl>,
    /// Source that gave the first block that allows verification.
    source_id: SourceId,
    /// List of blocks to verify. Must never be empty.
    verifiable_blocks: VecDeque<(u64, [u8; 32], Vec<u8>)>,
}

impl<TSrc, TBl> HeaderVerify<TSrc, TBl> {
    /// Source the blocks to verify belong to.
    pub fn source_id(&self) -> SourceId {
        self.source_id
    }

    /// Perform the verification.
    pub fn perform(
        mut self,
        now_from_unix_epoch: Duration,
        user_data: TBl,
    ) -> HeaderVerifyOutcome<TSrc, TBl> {
        // `verifiable_blocks` must never be empty.
        let (to_verify_height, to_verify_hash, to_verify_scale_encoded_header) =
            self.verifiable_blocks.pop_front().unwrap();

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
                insert.insert(Block { user_data });
                Ok(is_new_best)
            }
            Err(blocks_tree::HeaderVerifyError::VerificationFailed(error)) => {
                Err((error, user_data))
            }
            Ok(blocks_tree::HeaderVerifySuccess::Duplicate)
            | Err(blocks_tree::HeaderVerifyError::BadParent { .. })
            | Err(blocks_tree::HeaderVerifyError::InvalidHeader(_)) => unreachable!(),
        };

        if result.is_ok() {
            let outcome = self
                .parent
                .inner
                .pending_blocks
                .block_mut(to_verify_height, to_verify_hash)
                .into_occupied()
                .unwrap()
                .remove_verify_success();
            // TODO: properly cancel the requests found in `outcome`
            self.verifiable_blocks.extend(outcome.verify_next);
        } else {
            self.parent
                .inner
                .pending_blocks
                .block_mut(to_verify_height, to_verify_hash)
                .into_occupied()
                .unwrap()
                .remove_verify_failed();
        }

        match (result, self.verifiable_blocks.is_empty()) {
            (Ok(is_new_best), false) => HeaderVerifyOutcome::SuccessContinue {
                is_new_best,
                next_block: HeaderVerify {
                    parent: self.parent,
                    source_id: self.source_id,
                    verifiable_blocks: self.verifiable_blocks,
                },
            },
            (Ok(is_new_best), true) => {
                let next_request = self.parent.source_next_request(self.source_id);
                HeaderVerifyOutcome::Success {
                    is_new_best,
                    sync: self.parent,
                    next_request,
                }
            }
            (Err((error, user_data)), false) => HeaderVerifyOutcome::ErrorContinue {
                next_block: HeaderVerify {
                    parent: self.parent,
                    source_id: self.source_id,
                    verifiable_blocks: self.verifiable_blocks,
                },
                error,
                user_data,
            },
            (Err((error, user_data)), true) => {
                let next_request = self.parent.source_next_request(self.source_id);
                HeaderVerifyOutcome::Error {
                    sync: self.parent,
                    error,
                    user_data,
                    next_request,
                }
            }
        }
    }

    // Note: no `cancel` method is provided, as it would leave the `AllForksSync` in a weird
    // state.
}

/// Outcome of calling [`HeaderVerify::perform`].
pub enum HeaderVerifyOutcome<TSrc, TBl> {
    /// Header has been successfully verified.
    Success {
        /// True if the newly-verified block is considered the new best block.
        is_new_best: bool,
        /// State machine yielded back. Use to continue the processing.
        sync: AllForksSync<TSrc, TBl>,
        /// Next request that must be performed on the source.
        next_request: Option<Request>,
    },

    /// Header has been successfully verified. A follow-up header is ready to be verified.
    SuccessContinue {
        /// True if the newly-verified block is considered the new best block.
        is_new_best: bool,
        /// Next verification.
        next_block: HeaderVerify<TSrc, TBl>,
    },

    /// Header verification failed.
    Error {
        /// State machine yielded back. Use to continue the processing.
        sync: AllForksSync<TSrc, TBl>,
        /// Error that happened.
        error: verify::header_only::Error,
        /// User data that was passed to [`HeaderVerify::perform`] and is unused.
        user_data: TBl,
        /// Next request that must be performed on the source.
        next_request: Option<Request>,
    },

    /// Header verification failed. A follow-up header is ready to be verified.
    ErrorContinue {
        /// Next verification.
        next_block: HeaderVerify<TSrc, TBl>,
        /// Error that happened.
        error: verify::header_only::Error,
        /// User data that was passed to [`HeaderVerify::perform`] and is unused.
        user_data: TBl,
    },
}

/// State of the processing of blocks.
pub enum BlockBodyVerify<TSrc, TBl> {
    #[doc(hidden)]
    Foo(core::marker::PhantomData<(TSrc, TBl)>),
    // TODO: finish
    /*/// Processing of the block is over.
    ///
    /// There might be more blocks remaining. Call [`AllForksSync::process_one`] again.
    NewBest {
        /// The state machine.
        /// The [`AllForksSync::process_one`] method takes ownership of the
        /// [`AllForksSync`]. This field yields it back.
        sync: AllForksSync<TSrc, TBl>,

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
        sync: AllForksSync<TSrc, TBl>,

        /// Blocks that have been finalized. Includes the block that has just been verified.
        finalized_blocks: Vec<Block<TBl>>,
    },

    /// Loading a storage value of the finalized block is required in order to continue.
    FinalizedStorageGet(StorageGet<TSrc, TBl>),

    /// Fetching the list of keys of the finalized block with a given prefix is required in order
    /// to continue.
    FinalizedStoragePrefixKeys(StoragePrefixKeys<TSrc, TBl>),

    /// Fetching the key of the finalized block storage that follows a given one is required in
    /// order to continue.
    FinalizedStorageNextKey(StorageNextKey<TSrc, TBl>),*/
}
