//! Tree of block headers.
//!
//! This module provides the [`NonFinalizedTree`] struct. This struct is a data structure
//! containing a tree of block headers, plus the state necessary to add new blocks to that tree.
//! Each block header additionally holds a user-chosen opaque data.
//!
//! The state in the [`NonFinalizedTree`] consists of:
//!
//! - One "latest finalized" block, and various states contained in its ancestors.
//! - Zero or more blocks that descend from the latest finalized block.
//!
//! The latest finalized block is a block that is guaranted to never be reverted. While it can
//! always be set to the genesis block of the chain, it is preferable, in order to reduce
//! memory utilization, to maintain it to a block that is as high as possible in the chain.
//!
//! > **Note**: While the GrandPa protocol provides a network-wide way to designate a block as
//! >           final, the concept of GrandPa-provided finality doesn't have to be the same as
//! >           the concept of finality in the [`NonFinalizedTree`]. For example, an API user
//! >           might decide to optimistically assume that the block whose number is
//! >           `highest_block - 5` is automatically finalized, and fall back to rebuilding a new
//! >           [`NonFinalizedTree`] if that assumption turns out to not be true.
//!
//! A block can be added to the chain by calling [`NonFinalizedTree::verify_header`].
//!

// TODO: rethink this doc ^

use crate::{
    chain::{chain_information, fork_tree},
    executor,
    finality::justification,
    header,
    verify::{self, babe},
};

use alloc::{collections::VecDeque, sync::Arc};
use core::{cmp, fmt, iter, mem};

/// Configuration for the [`NonFinalizedTree`].
#[derive(Debug, Clone)]
pub struct Config {
    /// Information about the latest finalized block and its ancestors.
    pub chain_information: chain_information::ChainInformation,

    /// Configuration for BABE, retreived from the genesis block.
    pub babe_genesis_config: babe::BabeGenesisConfiguration,

    /// Pre-allocated size of the chain, in number of non-finalized blocks.
    pub blocks_capacity: usize,
}

/// Holds state about the current state of the chain for the purpose of verifying headers.
pub struct NonFinalizedTree<T> {
    /// Header of the highest known finalized block.
    finalized_block_header: header::Header,
    /// Hash of [`NonFinalizedTree::finalized_block_header`].
    finalized_block_hash: [u8; 32],
    /// Grandpa authorities set ID of the block right after the finalized block.
    grandpa_after_finalized_block_authorities_set_id: u64,
    /// List of GrandPa authorities that need to finalize the block right after the finalized
    /// block.
    grandpa_finalized_triggered_authorities: Vec<header::GrandpaAuthority>,
    /// List of changes in the GrandPa authorities list that have been scheduled by blocks that
    /// are already finalized. These changes will for sure happen.
    /// Contrary to the equivalent field in [`ChainInformation`], this list is always known to be
    /// ordered by block height.
    // TODO: doesn't have to be a collection; refactor into a single value
    grandpa_finalized_scheduled_changes: VecDeque<chain_information::FinalizedScheduledChange>,

    /// Configuration for BABE, retreived from the genesis block.
    babe_genesis_config: babe::BabeGenesisConfiguration,

    /// See [`ChainInformation::babe_finalized_block_epoch_information`].
    babe_finalized_block_epoch_information:
        Option<Arc<(header::BabeNextEpoch, header::BabeNextConfig)>>,

    /// See [`ChainInformation::babe_finalized_next_epoch_transition`].
    babe_finalized_next_epoch_transition:
        Option<Arc<(header::BabeNextEpoch, header::BabeNextConfig)>>,

    /// If block 1 is finalized, contains its slot number.
    babe_finalized_block1_slot_number: Option<u64>,
    /// Container for non-finalized blocks.
    blocks: fork_tree::ForkTree<Block<T>>,
    /// Index within [`NonFinalizedTree::blocks`] of the current best block. `None` if and only
    /// if the fork tree is empty.
    current_best: Option<fork_tree::NodeIndex>,
}

struct Block<T> {
    /// Header of the block.
    header: header::Header,
    /// Cache of the hash of the block. Always equal to the hash of the header stored in this
    /// same struct.
    hash: [u8; 32],
    /// If this block is block #1 of the chain, contains its babe slot number. Otherwise, contains
    /// the slot number of the block #1 that is an ancestor of this block.
    babe_block1_slot_number: u64,
    /// Information about the Babe epoch the block belongs to. `None` if the block belongs to
    /// epoch #0.
    babe_current_epoch: Option<Arc<(header::BabeNextEpoch, header::BabeNextConfig)>>,
    /// Information about the Babe epoch the block belongs to.
    babe_next_epoch: Arc<(header::BabeNextEpoch, header::BabeNextConfig)>,
    /// Opaque data decided by the user.
    user_data: T,
}

impl<T> NonFinalizedTree<T> {
    /// Initializes a new queue.
    ///
    /// # Panic
    ///
    /// Panics if the chain information is incorrect.
    ///
    pub fn new(mut config: Config) -> Self {
        if config.chain_information.finalized_block_header.number >= 1 {
            assert!(config
                .chain_information
                .babe_finalized_block1_slot_number
                .is_some());
            assert!(config
                .chain_information
                .babe_finalized_next_epoch_transition
                .is_some());
        } else {
            assert_eq!(
                config
                    .chain_information
                    .grandpa_after_finalized_block_authorities_set_id,
                0
            );
            assert!(config
                .chain_information
                .babe_finalized_next_epoch_transition
                .is_none());
            assert!(config
                .chain_information
                .babe_finalized_block_epoch_information
                .is_none());
        }

        // TODO: also check that babe_finalized_block_epoch_information is None if and only if block is in epoch #0

        let finalized_block_hash = config.chain_information.finalized_block_header.hash();

        config
            .chain_information
            .grandpa_finalized_scheduled_changes
            .retain({
                let n = config.chain_information.finalized_block_header.number;
                move |sc| sc.trigger_block_height > n
            });
        config
            .chain_information
            .grandpa_finalized_scheduled_changes
            .sort_by_key(|sc| sc.trigger_block_height);

        NonFinalizedTree {
            finalized_block_header: config.chain_information.finalized_block_header,
            finalized_block_hash,
            grandpa_after_finalized_block_authorities_set_id: config
                .chain_information
                .grandpa_after_finalized_block_authorities_set_id,
            grandpa_finalized_triggered_authorities: config
                .chain_information
                .grandpa_finalized_triggered_authorities,
            grandpa_finalized_scheduled_changes: config
                .chain_information
                .grandpa_finalized_scheduled_changes
                .into_iter()
                .collect(),
            babe_genesis_config: config.babe_genesis_config,
            babe_finalized_block1_slot_number: config
                .chain_information
                .babe_finalized_block1_slot_number,
            babe_finalized_block_epoch_information: config
                .chain_information
                .babe_finalized_block_epoch_information
                .map(Arc::new),
            babe_finalized_next_epoch_transition: config
                .chain_information
                .babe_finalized_next_epoch_transition
                .map(Arc::new),
            blocks: fork_tree::ForkTree::with_capacity(config.blocks_capacity),
            current_best: None,
        }
    }

    /// Removes all non-finalized blocks from the tree.
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.current_best = None;
    }

    /// Returns the number of non-finalized blocks in the chain.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Reserves additional capacity for at least `additional` new blocks without allocating.
    pub fn reserve(&mut self, additional: usize) {
        self.blocks.reserve(additional)
    }

    /// Shrink the capacity of the chain as much as possible.
    pub fn shrink_to_fit(&mut self) {
        self.blocks.shrink_to_fit()
    }

    /// Builds a [`chain_information::ChainInformationRef`] struct that might later be used to
    /// build a new [`NonFinalizedTree`].
    pub fn as_chain_information(&self) -> chain_information::ChainInformationRef {
        chain_information::ChainInformationRef {
            finalized_block_header: (&self.finalized_block_header).into(),
            babe_finalized_block1_slot_number: self.babe_finalized_block1_slot_number,
            babe_finalized_block_epoch_information: self
                .babe_finalized_block_epoch_information
                .as_ref()
                .map(|info| ((&info.0).into(), info.1)),
            babe_finalized_next_epoch_transition: self
                .babe_finalized_next_epoch_transition
                .as_ref()
                .map(|info| ((&info.0).into(), info.1)),
            grandpa_after_finalized_block_authorities_set_id: self
                .grandpa_after_finalized_block_authorities_set_id,
            grandpa_finalized_triggered_authorities: &self.grandpa_finalized_triggered_authorities,
            grandpa_finalized_scheduled_changes: self
                .grandpa_finalized_scheduled_changes
                .iter()
                .cloned()
                .collect(),
        }
    }

    /// Returns the header of the latest finalized block.
    pub fn finalized_block_header(&self) -> header::HeaderRef {
        (&self.finalized_block_header).into()
    }

    /// Returns the hash of the latest finalized block.
    pub fn finalized_block_hash(&self) -> [u8; 32] {
        self.finalized_block_hash
    }

    /// Returns the header of the best block.
    pub fn best_block_header(&self) -> header::HeaderRef {
        if let Some(index) = self.current_best {
            (&self.blocks.get(index).unwrap().header).into()
        } else {
            (&self.finalized_block_header).into()
        }
    }

    /// Returns the hash of the best block.
    pub fn best_block_hash(&self) -> [u8; 32] {
        if let Some(index) = self.current_best {
            self.blocks.get(index).unwrap().hash
        } else {
            self.finalized_block_hash
        }
    }

    /// Verifies the given block.
    ///
    /// The verification is performed in the context of the chain. In particular, the
    /// verification will fail if the parent block isn't already in the chain.
    ///
    /// If the verification succeeds, an [`Insert`] object might be returned which can be used to
    /// then insert the block in the chain.
    #[must_use]
    pub fn verify_header(
        &mut self,
        scale_encoded_header: Vec<u8>,
    ) -> Result<HeaderVerifySuccess<T>, HeaderVerifyError> {
        match self.verify_inner(scale_encoded_header, None::<iter::Empty<Vec<u8>>>) {
            BodyOrHeader::Header(v) => v,
            BodyOrHeader::Body(_) => unreachable!(),
        }
    }

    /// Underlying implementation of both header and header+body verification.
    // TODO: should take ownership of `self` to avoid potential lifetime issues in upper layers of
    // the API
    fn verify_inner<'c, I, E>(
        &'c mut self,
        scale_encoded_header: Vec<u8>,
        body: Option<I>,
    ) -> BodyOrHeader<'c, T, I>
    where
        I: ExactSizeIterator<Item = E> + Clone,
        E: AsRef<[u8]> + Clone,
    {
        let decoded_header = match header::decode(&scale_encoded_header) {
            Ok(h) => h,
            Err(err) => {
                return if body.is_some() {
                    BodyOrHeader::Body(BodyVerifyStep1::InvalidHeader(err))
                } else {
                    BodyOrHeader::Header(Err(HeaderVerifyError::InvalidHeader(err)))
                }
            }
        };

        let hash = header::hash_from_scale_encoded_header(&scale_encoded_header);

        if self.blocks.find(|b| b.hash == hash).is_some() {
            return if body.is_some() {
                BodyOrHeader::Body(BodyVerifyStep1::Duplicate)
            } else {
                BodyOrHeader::Header(Ok(HeaderVerifySuccess::Duplicate))
            };
        }

        // Try to find the parent block in the tree of known blocks.
        // `Some` with an index of the parent within the tree of unfinalized blocks.
        // `None` means that the parent is the finalized block.
        //
        // The parent hash is first checked against `self.current_best`, as it is most likely
        // that new blocks are built on top of the current best.
        let parent_tree_index = if self.current_best.map_or(false, |best| {
            *decoded_header.parent_hash == self.blocks.get(best).unwrap().hash
        }) {
            Some(self.current_best.unwrap())
        } else if *decoded_header.parent_hash == self.finalized_block_hash {
            None
        } else {
            let parent_hash = *decoded_header.parent_hash;
            match self.blocks.find(|b| b.hash == parent_hash) {
                Some(parent) => Some(parent),
                None => {
                    return if body.is_some() {
                        BodyOrHeader::Body(BodyVerifyStep1::BadParent { parent_hash })
                    } else {
                        BodyOrHeader::Header(Err(HeaderVerifyError::BadParent { parent_hash }))
                    }
                }
            }
        };

        // Try to find the slot number of block 1.
        // If block 1 is finalized, this information is found in
        // `babe_finalized_block1_slot_number`, otherwise the information is found in the parent
        // node in the fork tree.
        let block1_slot_number = if let Some(val) = self.babe_finalized_block1_slot_number {
            debug_assert!(parent_tree_index.map_or(true, |p_idx| self
                .blocks
                .get(p_idx)
                .unwrap()
                .babe_block1_slot_number
                == val));
            Some(val)
        } else if let Some(parent_tree_index) = parent_tree_index {
            Some(
                self.blocks
                    .get(parent_tree_index)
                    .unwrap()
                    .babe_block1_slot_number,
            )
        } else {
            // Can only happen if parent is the block #0.
            assert_eq!(decoded_header.number, 1);
            None
        };

        if let Some(body) = body {
            BodyOrHeader::Body(BodyVerifyStep1::ParentRuntimeRequired(
                BodyVerifyRuntimeRequired {
                    chain: self,
                    scale_encoded_header,
                    parent_tree_index,
                    body,
                    block1_slot_number,
                },
            ))
        } else {
            let parent_block_header = if let Some(parent_tree_index) = parent_tree_index {
                &self.blocks.get(parent_tree_index).unwrap().header
            } else {
                &self.finalized_block_header
            };

            let mut process = verify::header_only::verify(verify::header_only::Config {
                babe_genesis_configuration: &self.babe_genesis_config,
                block1_slot_number,
                now_from_unix_epoch: {
                    // TODO: is it reasonable to use the stdlib here?
                    //std::time::SystemTime::UNIX_EPOCH.elapsed().unwrap()
                    // TODO: this is commented out because of Wasm support
                    core::time::Duration::new(0, 0)
                },
                block_header: decoded_header.clone(),
                parent_block_header: parent_block_header.into(),
            });

            let result = loop {
                match process {
                    verify::header_only::Verify::Finished(Ok(result)) => break result,
                    verify::header_only::Verify::Finished(Err(err)) => {
                        return BodyOrHeader::Header(Err(HeaderVerifyError::VerificationFailed(
                            err,
                        )));
                    }
                    verify::header_only::Verify::ReadyToRun(run) => process = run.run(),
                    verify::header_only::Verify::BabeEpochInformation(epoch_info_rq) => {
                        let epoch_info = if let Some(parent_tree_index) = parent_tree_index {
                            let parent = self.blocks.get(parent_tree_index).unwrap();
                            if epoch_info_rq.same_epoch_as_parent() {
                                parent.babe_current_epoch.as_ref().unwrap()
                            } else {
                                &parent.babe_next_epoch
                            }
                        } else {
                            if epoch_info_rq.same_epoch_as_parent() {
                                self.babe_finalized_block_epoch_information
                                    .as_ref()
                                    .unwrap()
                            } else {
                                self.babe_finalized_next_epoch_transition.as_ref().unwrap()
                            }
                        };

                        process = epoch_info_rq
                            .inject_epoch((From::from(&epoch_info.0), epoch_info.1))
                            .run();
                    }
                }
            };

            let is_new_best = if let Some(current_best) = self.current_best {
                is_better_block(
                    &self.blocks,
                    current_best,
                    parent_tree_index,
                    decoded_header.clone(),
                )
            } else {
                true
            };

            let babe_block1_slot_number = block1_slot_number.unwrap_or_else(|| {
                debug_assert_eq!(decoded_header.number, 1);
                result.slot_number
            });

            let babe_current_epoch = if result.babe_epoch_transition_target.is_some() {
                if let Some(parent_tree_index) = parent_tree_index {
                    Some(
                        self.blocks
                            .get(parent_tree_index)
                            .unwrap()
                            .babe_next_epoch
                            .clone(),
                    )
                } else {
                    self.babe_finalized_next_epoch_transition.clone()
                }
            } else if let Some(parent_tree_index) = parent_tree_index {
                self.blocks
                    .get(parent_tree_index)
                    .unwrap()
                    .babe_current_epoch
                    .clone()
            } else {
                self.babe_finalized_block_epoch_information.clone()
            };

            let babe_next_epoch = match (
                decoded_header.digest.babe_epoch_information(),
                parent_tree_index,
                &self.babe_finalized_next_epoch_transition,
            ) {
                (Some((ref new_epoch, Some(new_config))), _, _) => {
                    Arc::new((new_epoch.clone().into(), new_config))
                }
                (Some((ref new_epoch, None)), Some(parent_tree_index), _) => {
                    let new_config = self
                        .blocks
                        .get(parent_tree_index)
                        .unwrap()
                        .babe_next_epoch
                        .1
                        .clone();
                    Arc::new((new_epoch.clone().into(), new_config))
                }
                (Some((ref new_epoch, None)), None, Some(finalized_next)) => {
                    Arc::new((new_epoch.clone().into(), finalized_next.1))
                }
                (Some((ref new_epoch, None)), None, None) => Arc::new((
                    new_epoch.clone().into(),
                    self.babe_genesis_config.epoch0_configuration(),
                )),
                (None, Some(parent_tree_index), _) => self
                    .blocks
                    .get(parent_tree_index)
                    .unwrap()
                    .babe_next_epoch
                    .clone(),
                (None, None, _) => {
                    // Block 1 always contains a Babe epoch transition. Consequently, this block
                    // can't be reached for block 1.
                    // `babe_finalized_next_epoch_transition` is `None` only if the finalized
                    // block is 0.
                    // `babe_finalized_next_epoch_transition` is therefore always `Some`
                    // Q.E.D.
                    debug_assert_ne!(decoded_header.number, 1);
                    self.babe_finalized_next_epoch_transition.clone().unwrap()
                }
            };

            BodyOrHeader::Header(Ok(HeaderVerifySuccess::Insert {
                block_height: decoded_header.number,
                is_new_best,
                insert: Insert {
                    chain: self,
                    parent_tree_index,
                    is_new_best,
                    header: decoded_header.into(),
                    hash,
                    babe_block1_slot_number,
                    babe_current_epoch,
                    babe_next_epoch,
                },
            }))
        }
    }

    /// Verifies the given justification.
    ///
    /// The verification is performed in the context of the chain. In particular, the
    /// verification will fail if the target block isn't already in the chain.
    ///
    /// If the verification succeeds, a [`JustificationApply`] object will be returned which can
    /// be used to apply the finalization.
    #[must_use]
    pub fn verify_justification(
        &mut self,
        scale_encoded_justification: &[u8],
    ) -> Result<JustificationApply<T>, JustificationVerifyError> {
        // Turn justification into a strongly-typed struct.
        let decoded = justification::decode::decode(&scale_encoded_justification)
            .map_err(JustificationVerifyError::InvalidJustification)?;

        // Find in the list of non-finalized blocks the one targeted by the justification.
        let block_index = match self.blocks.find(|b| b.hash == *decoded.target_hash) {
            Some(idx) => idx,
            None => {
                return Err(JustificationVerifyError::UnknownTargetBlock {
                    block_number: From::from(decoded.target_number),
                    block_hash: *decoded.target_hash,
                });
            }
        };

        // If any block between the latest finalized one and the target block trigger any GrandPa
        // authorities change, then we need to finalize that triggering block (or any block
        // after or including the one that schedules these changes) before finalizing the one
        // targeted by the justification.

        // Find out the next block height where an authority change will be triggered.
        let earliest_trigger = {
            // Scheduled change that is already finalized.
            let already_finalized = self
                .grandpa_finalized_scheduled_changes
                .front()
                .map(|sc| sc.trigger_block_height);

            // First change that would be scheduled if we finalize the target block.
            let would_happen = {
                let mut trigger_height = None;
                // TODO: lot of boilerplate code here
                for node in self.blocks.root_to_node_path(block_index) {
                    let header = &self.blocks.get(node).unwrap().header;
                    for grandpa_digest_item in header.digest.logs().filter_map(|d| match d {
                        header::DigestItemRef::GrandpaConsensus(gp) => Some(gp),
                        _ => None,
                    }) {
                        match grandpa_digest_item {
                            header::GrandpaConsensusLogRef::ScheduledChange(change) => {
                                let trigger_block_height =
                                    header.number.checked_add(u64::from(change.delay)).unwrap();
                                match trigger_height {
                                    Some(_) => panic!("invalid block!"), // TODO: this problem is not checked during block verification
                                    None => trigger_height = Some(trigger_block_height),
                                }
                            }
                            _ => {} // TODO: unimplemented
                        }
                    }
                }
                trigger_height
            };

            match (already_finalized, would_happen) {
                (Some(a), Some(b)) => Some(cmp::min(a, b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        };

        // As explained above, `target_number` must be <= `earliest_trigger`, otherwise the
        // finalization is unsecure.
        if let Some(earliest_trigger) = earliest_trigger {
            if u64::from(decoded.target_number) > earliest_trigger {
                let block_to_finalize_hash = self
                    .blocks
                    .node_to_root_path(block_index)
                    .filter_map(|b| {
                        let b = self.blocks.get(b).unwrap();
                        if b.header.number == earliest_trigger {
                            Some(b.hash)
                        } else {
                            None
                        }
                    })
                    .next()
                    .unwrap();
                return Err(JustificationVerifyError::TooFarAhead {
                    justification_block_number: u64::from(decoded.target_number),
                    justification_block_hash: *decoded.target_hash,
                    block_to_finalize_number: earliest_trigger,
                    block_to_finalize_hash,
                });
            }
        }

        // Find which authorities are supposed to finalize the target block.
        let authorities_list = self
            .grandpa_finalized_scheduled_changes
            .iter()
            .filter(|sc| sc.trigger_block_height < u64::from(decoded.target_number))
            .last()
            .map(|sc| &sc.new_authorities_list)
            .unwrap_or(&self.grandpa_finalized_triggered_authorities);

        // As per above check, we know that the authorities of the target block are either the
        // same as the ones of the latest finalized block, or the ones contained in the header of
        // the latest finalized block.
        justification::verify::verify(justification::verify::Config {
            justification: decoded,
            authorities_set_id: self.grandpa_after_finalized_block_authorities_set_id,
            authorities_list: authorities_list.iter().map(|a| a.public_key),
        })
        .map_err(JustificationVerifyError::VerificationFailed)?;

        // Justification has been successfully verified!
        Ok(JustificationApply {
            chain: self,
            to_finalize: block_index,
        })
    }

    /// Sets the latest known finalized block. Trying to verify a block that isn't a descendant of
    /// that block will fail.
    ///
    /// The block must have been passed to [`NonFinalizedTree::verify_header`].
    // TODO: return the pruned blocks
    pub fn set_finalized_block(&mut self, block_hash: &[u8; 32]) -> Result<(), SetFinalizedError> {
        let block_index = match self.blocks.find(|b| b.hash == *block_hash) {
            Some(idx) => idx,
            None => return Err(SetFinalizedError::UnknownBlock),
        };

        self.set_finalized_block_inner(block_index);
        Ok(())
    }

    /// Private function that does the same as [`NonFinalizedTree::set_finalized_block`].
    fn set_finalized_block_inner(&mut self, block_index: fork_tree::NodeIndex) {
        // TODO: uncomment after https://github.com/rust-lang/rust/issues/53485
        //debug_assert!(self.grandpa_finalized_scheduled_changes.iter().is_sorted_by_key(|sc| sc.trigger_block_height));

        // Update the list of scheduled GrandPa changes and Babe epochs with the ones contained
        // in the newly-finalized blocks.
        for node in self.blocks.root_to_node_path(block_index) {
            let node = self.blocks.get(node).unwrap();
            for grandpa_digest_item in node.header.digest.logs().filter_map(|d| match d {
                header::DigestItemRef::GrandpaConsensus(gp) => Some(gp),
                _ => None,
            }) {
                match grandpa_digest_item {
                    header::GrandpaConsensusLogRef::ScheduledChange(change) => {
                        let trigger_block_height = node
                            .header
                            .number
                            .checked_add(u64::from(change.delay))
                            .unwrap();
                        self.grandpa_finalized_scheduled_changes.push_back(
                            chain_information::FinalizedScheduledChange {
                                trigger_block_height,
                                new_authorities_list: change
                                    .next_authorities
                                    .map(Into::into)
                                    .collect(),
                            },
                        );
                    }
                    _ => {} // TODO: unimplemented
                }
            }
        }

        // TODO: uncomment after https://github.com/rust-lang/rust/issues/53485
        //debug_assert!(self.grandpa_finalized_scheduled_changes.iter().is_sorted_by_key(|sc| sc.trigger_block_height));

        let new_finalized_block = self.blocks.get_mut(block_index).unwrap();

        self.babe_finalized_block_epoch_information =
            new_finalized_block.babe_current_epoch.clone();
        self.babe_finalized_next_epoch_transition =
            Some(new_finalized_block.babe_next_epoch.clone());

        mem::swap(
            &mut self.finalized_block_header,
            &mut new_finalized_block.header,
        );
        self.finalized_block_hash = self.finalized_block_header.hash();

        // Remove the changes that have been triggered by the newly-finalized blocks.
        while let Some(next_change) = self.grandpa_finalized_scheduled_changes.front() {
            if next_change.trigger_block_height <= self.finalized_block_header.number {
                let next_change = self
                    .grandpa_finalized_scheduled_changes
                    .pop_front()
                    .unwrap();
                self.grandpa_finalized_triggered_authorities = next_change.new_authorities_list;
                self.grandpa_after_finalized_block_authorities_set_id += 1;
            }
        }

        if self.babe_finalized_block1_slot_number.is_none() {
            debug_assert!(self.finalized_block_header.number >= 1);
            self.babe_finalized_block1_slot_number =
                Some(new_finalized_block.babe_block1_slot_number);
        }

        self.blocks.prune_ancestors(block_index);

        // If the current best was removed from the list, we need to update it.
        if self
            .current_best
            .map_or(true, |b| self.blocks.get(b).is_none())
        {
            // TODO: no; should try to find best block
            self.current_best = None;
        }
    }
}

impl<T> fmt::Debug for NonFinalizedTree<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_map()
            .entries(self.blocks.iter().map(|v| (&v.hash, &v.user_data)))
            .finish()
    }
}

enum BodyOrHeader<'c, T, I> {
    Header(Result<HeaderVerifySuccess<'c, T>, HeaderVerifyError>),
    Body(BodyVerifyStep1<'c, T, I>),
}

///
#[derive(Debug)]
pub enum BodyVerifyStep1<'c, T, I> {
    /// Block is already known.
    Duplicate,

    /// Error while decoding the header.
    InvalidHeader(header::Error),

    /// The parent of the block isn't known.
    BadParent {
        /// Hash of the parent block in question.
        parent_hash: [u8; 32],
    },

    /// Verification is pending. In order to continue, a [`executor::WasmVmPrototype`] of the
    /// runtime of the parent block must be provided.
    ParentRuntimeRequired(BodyVerifyRuntimeRequired<'c, T, I>),
}

/// Verification is pending. In order to continue, a [`executor::WasmVmPrototype`] of the runtime
/// of the parent block must be provided.
#[derive(Debug)]
pub struct BodyVerifyRuntimeRequired<'c, T, I> {
    chain: &'c mut NonFinalizedTree<T>,
    scale_encoded_header: Vec<u8>, // TODO: should be owned Header
    parent_tree_index: Option<fork_tree::NodeIndex>,
    body: I,
    block1_slot_number: Option<u64>,
}

impl<'c, T, I, E> BodyVerifyRuntimeRequired<'c, T, I>
where
    I: ExactSizeIterator<Item = E> + Clone,
    E: AsRef<[u8]> + Clone,
{
    pub fn resume(self, parent_runtime: executor::WasmVmPrototype) -> BodyVerifyStep2<'c, T> {
        let parent_block_header = if let Some(parent_tree_index) = self.parent_tree_index {
            &self.chain.blocks.get(parent_tree_index).unwrap().header
        } else {
            &self.chain.finalized_block_header
        };

        let process = verify::header_body::verify(verify::header_body::Config {
            parent_runtime,
            babe_genesis_configuration: &self.chain.babe_genesis_config,
            block1_slot_number: self.block1_slot_number,
            now_from_unix_epoch: {
                // TODO: is it reasonable to use the stdlib here?
                //std::time::SystemTime::UNIX_EPOCH.elapsed().unwrap()
                // TODO: this is commented out because of Wasm support
                core::time::Duration::new(0, 0)
            },
            block_header: header::decode(&self.scale_encoded_header).unwrap(),
            parent_block_header: parent_block_header.into(),
            block_body: self.body,
            top_trie_root_calculation_cache: None,
        });

        BodyVerifyStep2::from_inner(
            process,
            BodyVerifyShared {
                chain: self.chain,
                parent_tree_index: self.parent_tree_index,
            },
        )
    }
}

/// Header and body verification in progress.
pub enum BodyVerifyStep2<'c, T> {
    /// Verification is over.
    Finished {
        /// Value that was passed to [`BodyVerifyRuntimeRequired::resume`].
        parent_runtime: executor::WasmVmPrototype,
        /// Outcome of the verification.
        result: Result<Insert<'c, T>, HeaderVerifyError>,
    },
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet<'c, T>),
    /// Fetching the list of keys with a given prefix is required in order to continue.
    StoragePrefixKeys(StoragePrefixKeys<'c, T>),
    /// Fetching the key that follows a given one is required in order to continue.
    StorageNextKey(StorageNextKey<'c, T>),
}

struct BodyVerifyShared<'c, T> {
    chain: &'c mut NonFinalizedTree<T>,
    parent_tree_index: Option<fork_tree::NodeIndex>,
}

impl<'c, T> BodyVerifyStep2<'c, T> {
    fn from_inner(mut inner: verify::header_body::Verify, chain: BodyVerifyShared<'c, T>) -> Self {
        loop {
            match inner {
                verify::header_body::Verify::Finished(Ok(_)) => {
                    // Block verification is successful!
                    todo!()
                }
                verify::header_body::Verify::Finished(Err(err)) => todo!(),
                verify::header_body::Verify::ReadyToRun(i) => {
                    inner = i.run();
                    continue;
                }
                verify::header_body::Verify::BabeEpochInformation(epoch_info_rq) => {
                    todo!() // TODO:
                            /*
                            let epoch_info = if let Some(parent_tree_index) = parent_tree_index {
                                let parent = self.blocks.get(parent_tree_index).unwrap();
                                if epoch_info_rq.same_epoch_as_parent() {
                                    parent.babe_current_epoch.as_ref().unwrap()
                                } else {
                                    &parent.babe_next_epoch
                                }
                            } else {
                                if epoch_info_rq.same_epoch_as_parent() {
                                    self.babe_finalized_block_epoch_information
                                        .as_ref()
                                        .unwrap()
                                } else {
                                    self.babe_finalized_next_epoch_transition.as_ref().unwrap()
                                }
                            };

                            process = epoch_info_rq.inject_epoch(From::from(&**epoch_info)).run();*/
                }
                verify::header_body::Verify::StorageGet(inner) => {
                    return BodyVerifyStep2::StorageGet(StorageGet { chain, inner })
                }
                verify::header_body::Verify::StorageNextKey(inner) => {
                    return BodyVerifyStep2::StorageNextKey(StorageNextKey { chain, inner })
                }
                verify::header_body::Verify::StoragePrefixKeys(inner) => {
                    return BodyVerifyStep2::StoragePrefixKeys(StoragePrefixKeys { chain, inner })
                }
            }
        }
    }
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet<'c, T> {
    inner: verify::header_body::StorageGet,
    chain: BodyVerifyShared<'c, T>,
}

impl<'c, T> StorageGet<'c, T> {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    // TODO: shouldn't be mut
    pub fn key<'b>(&'b mut self) -> impl Iterator<Item = impl AsRef<[u8]> + 'b> + 'b {
        self.inner.key()
    }

    /// Returns the user data associated to the block whose storage must be accessed.
    pub fn block_user_data(&mut self) -> Option<&mut T> {
        if let Some(parent_tree_index) = self.chain.parent_tree_index {
            Some(
                &mut self
                    .chain
                    .chain
                    .blocks
                    .get_mut(parent_tree_index)
                    .unwrap()
                    .user_data,
            )
        } else {
            None
        }
    }

    /// Injects the corresponding storage value.
    // TODO: change API, see unsealed::StorageGet
    pub fn inject_value(self, value: Option<&[u8]>) -> BodyVerifyStep2<'c, T> {
        let inner = self.inner.inject_value(value);
        BodyVerifyStep2::from_inner(inner.run(), self.chain)
    }
}

/// Fetching the list of keys with a given prefix is required in order to continue.
#[must_use]
pub struct StoragePrefixKeys<'c, T> {
    inner: verify::header_body::StoragePrefixKeys,
    chain: BodyVerifyShared<'c, T>,
}

impl<'c, T> StoragePrefixKeys<'c, T> {
    /// Returns the prefix whose keys to load.
    // TODO: don't take &mut mut but &self
    pub fn prefix(&mut self) -> &[u8] {
        self.inner.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(
        self,
        keys: impl Iterator<Item = impl AsRef<[u8]>>,
    ) -> BodyVerifyStep2<'c, T> {
        let inner = self.inner.inject_keys(keys);
        BodyVerifyStep2::from_inner(inner.run(), self.chain)
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct StorageNextKey<'c, T> {
    inner: verify::header_body::StorageNextKey,
    chain: BodyVerifyShared<'c, T>,
}

impl<'c, T> StorageNextKey<'c, T> {
    /// Returns the key whose next key must be passed back.
    // TODO: don't take &mut mut but &self
    pub fn key(&mut self) -> &[u8] {
        self.inner.key()
    }

    /// Injects the key.
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> BodyVerifyStep2<'c, T> {
        let inner = self.inner.inject_key(key);
        BodyVerifyStep2::from_inner(inner.run(), self.chain)
    }
}

///
#[derive(Debug)]
pub enum HeaderVerifySuccess<'c, T> {
    /// Block is already known.
    Duplicate,
    /// Block wasn't known and is ready to be inserted.
    Insert {
        /// Height of the verified block.
        block_height: u64,
        /// True if the verified block will become the new "best" block after being inserted.
        is_new_best: bool,
        /// Use this struct to insert the block in the chain after its successful verification.
        insert: Insert<'c, T>,
    },
}

/// Mutably borrows the [`NonFinalizedTree`] and allows insert a successfully-verified block
/// into it.
#[must_use]
pub struct Insert<'c, T> {
    chain: &'c mut NonFinalizedTree<T>,
    /// Copy of the value in [`VerifySuccess::is_new_best`].
    is_new_best: bool,
    /// Index of the parent in [`NonFinalizedTree::blocks`].
    parent_tree_index: Option<fork_tree::NodeIndex>,
    header: header::Header,
    hash: [u8; 32],
    babe_block1_slot_number: u64,
    babe_current_epoch: Option<Arc<(header::BabeNextEpoch, header::BabeNextConfig)>>,
    babe_next_epoch: Arc<(header::BabeNextEpoch, header::BabeNextConfig)>,
}

impl<'c, T> Insert<'c, T> {
    /// Inserts the block with the given user data.
    pub fn insert(self, user_data: T) {
        let new_node_index = self.chain.blocks.insert(
            self.parent_tree_index,
            Block {
                header: self.header,
                hash: self.hash,
                babe_block1_slot_number: self.babe_block1_slot_number,
                babe_current_epoch: self.babe_current_epoch,
                babe_next_epoch: self.babe_next_epoch,
                user_data,
            },
        );

        if self.is_new_best {
            self.chain.current_best = Some(new_node_index);
        }
    }

    /// Destroys the object without inserting the block in the chain. Returns the block header.
    pub fn into_header(self) -> header::Header {
        self.header
    }
}

impl<'c, T> fmt::Debug for Insert<'c, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Insert").field(&self.header).finish()
    }
}

/// Error that can happen when verifying a block.
#[derive(Debug, derive_more::Display)]
pub enum HeaderVerifyError {
    /// Error while decoding the header.
    InvalidHeader(header::Error),
    /// The parent of the block isn't known.
    #[display(fmt = "The parent of the block isn't known.")]
    BadParent {
        /// Hash of the parent block in question.
        parent_hash: [u8; 32],
    },
    /// The block verification has failed. The block is invalid and should be thrown away.
    VerificationFailed(verify::header_only::Error),
    /// Babe epoch information couldn't be determined.
    // TODO: how can that happen?
    UnknownBabeEpoch,
}

// TODO: doc and all
#[must_use]
pub struct JustificationApply<'c, T> {
    chain: &'c mut NonFinalizedTree<T>,
    to_finalize: fork_tree::NodeIndex,
}

impl<'c, T> JustificationApply<'c, T> {
    /// Applies the justification, finalizing the given block.
    // TODO: return type?
    pub fn apply(self) {
        self.chain.set_finalized_block_inner(self.to_finalize)
    }
}

impl<'c, T> fmt::Debug for JustificationApply<'c, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("JustificationApply").finish()
    }
}

/// Error that can happen when verifying a justification.
#[derive(Debug, derive_more::Display)]
pub enum JustificationVerifyError {
    /// Error while decoding the justification.
    InvalidJustification(justification::decode::Error),
    /// Justification targets a block that isn't in the chain.
    #[display(fmt = "Justification targets a block that isn't in the chain.")]
    UnknownTargetBlock {
        /// Number of the block that isn't in the chain.
        block_number: u64,
        /// Hash of the block that isn't in the chain.
        block_hash: [u8; 32],
    },
    /// There exists a block in-between the latest finalized block and the block targeted by the
    /// justification that must first be finalized.
    #[display(
        fmt = "There exists a block in-between the latest finalized block and the block \
                     targeted by the justification that must first be finalized"
    )]
    TooFarAhead {
        /// Number of the block contained in the justification.
        justification_block_number: u64,
        /// Hash of the block contained in the justification.
        justification_block_hash: [u8; 32],
        /// Number of the block to finalize first.
        block_to_finalize_number: u64,
        /// Hash of the block to finalize first.
        block_to_finalize_hash: [u8; 32],
    },
    /// The justification verification has failed. The justification is invalid and should be
    /// thrown away.
    VerificationFailed(justification::verify::Error),
}

/// Error that can happen when setting the finalized block.
#[derive(Debug, derive_more::Display)]
pub enum SetFinalizedError {
    /// Block must have been passed to [`NonFinalizedTree::verify_header`] in the past.
    UnknownBlock,
}

/// Accepts as parameter a container of blocks and indices within this container.
///
/// Returns true if `maybe_new_best` on top of `maybe_new_best_parent` is a better block compared
/// to `old_best`.
fn is_better_block<T>(
    blocks: &fork_tree::ForkTree<Block<T>>,
    old_best: fork_tree::NodeIndex,
    maybe_new_best_parent: Option<fork_tree::NodeIndex>,
    maybe_new_best: header::HeaderRef,
) -> bool {
    debug_assert!(
        maybe_new_best_parent.map_or(true, |p_idx| blocks.get(p_idx).unwrap().hash
            == *maybe_new_best.parent_hash)
    );

    if maybe_new_best_parent.map_or(false, |p_idx| blocks.is_ancestor(old_best, p_idx)) {
        // A descendant is always preferred to its ancestor.
        true
    } else {
        // In order to determine whether the new block is our new best:
        //
        // - Find the common ancestor between the current best and the new block's parent.
        // - Count the number of Babe primary slot claims between the common ancestor and
        //   the current best.
        // - Count the number of Babe primary slot claims between the common ancestor and
        //   the new block's parent. Add one if the new block has a Babe primary slot
        //   claim.
        // - If the number for the new block is strictly superior, then the new block is
        //   out new best.
        //
        let (ascend, descend) = blocks.ascend_and_descend(old_best, maybe_new_best_parent.unwrap());

        // TODO: what if there's a mix of Babe and non-Babe blocks here?

        let curr_best_primary_slots: usize = ascend
            .map(|i| {
                if blocks
                    .get(i)
                    .unwrap()
                    .header
                    .digest
                    .babe_pre_runtime()
                    .map_or(false, |pr| pr.is_primary())
                {
                    1
                } else {
                    0
                }
            })
            .sum();

        let new_block_primary_slots = {
            if maybe_new_best
                .digest
                .babe_pre_runtime()
                .map_or(false, |pr| pr.is_primary())
            {
                1
            } else {
                0
            }
        };

        let parent_primary_slots: usize = descend
            .map(|i| {
                if blocks
                    .get(i)
                    .unwrap()
                    .header
                    .digest
                    .babe_pre_runtime()
                    .map_or(false, |pr| pr.is_primary())
                {
                    1
                } else {
                    0
                }
            })
            .sum();

        // Note the strictly superior. If there is an equality, we keep the current best.
        parent_primary_slots + new_block_primary_slots > curr_best_primary_slots
    }
}
