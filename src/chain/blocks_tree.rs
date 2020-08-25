//! Chain of block headers.
//!
//! This module provides the [`NonFinalizedTree`] struct. It contains the state necessary to maintain a
//! chain of block headers.
//!
//! The state in the [`NonFinalizedTree`] consists of:
//!
//! - One "latest finalized" block.
//! - Zero or more blocks that descend from the latest finalized block.
//!
//! The latest finalized block represents the block that we know will never be reverted. While it
//! can always be set to the genesis block of the chain, it is preferable, in order to reduce
//! memory utilization, to maintain it to a block that is as high as possible in the chain.
//!
//! A block can be added to the chain by calling [`NonFinalizedTree::verify_header`]. You are
//! encouraged to regularly update the latest finalized block using
//! [`NonFinalizedTree::set_finalized_block`].
//!
//! > **Note**: While the GrandPa protocol provides a network-wide way to designate a block as
//! >           final, the concept of GrandPa-provided finality doesn't have to be the same as
//! >           the finality decided for the [`NonFinalizedTree`]. For example, an API user
//! >           might decide that the block whose number is `latest_block - 5` will always be
//! >           final, and rebuild a new [`NonFinalizedTree`] if that assumption turned out to
//! >           not be true.

// TODO: rethink this doc ^

use crate::{
    chain::fork_tree,
    executor,
    finality::justification,
    header,
    verify::{self, babe},
};

use alloc::collections::VecDeque;
use core::{cmp, fmt, iter, mem};

/// Configuration for the [`NonFinalizedTree`].
#[derive(Debug)]
pub struct Config {
    /// SCALE encoding of the header of the highest known finalized block.
    ///
    /// Once the queue is created, it is as if you had called
    /// [`NonFinalizedTree::set_finalized_block`] with this block.
    // TODO: should be an owned decoded header?
    pub finalized_block_header: Vec<u8>,

    /// If the number in [`Config::finalized_block_header`] is superior or equal to 1, then this
    /// field must contain the slot number of the block whose number is 1 and is an ancestor of
    /// the finalized block.
    pub babe_finalized_block1_slot_number: Option<u64>,

    /// Known Babe epoch transitions coming from the finalized block and its ancestors.
    // TODO: turn into iterator
    pub babe_known_epoch_information: Vec<(u64, header::BabeNextEpoch)>,

    /// Configuration for BABE, retreived from the genesis block.
    pub babe_genesis_config: babe::BabeGenesisConfiguration,

    /// Grandpa authorities set ID of the block right after finalized block.
    ///
    /// If the finalized block is the genesis, should be 0. Otherwise,
    // TODO: document how to know this
    pub grandpa_after_finalized_block_authorities_set_id: u64,

    /// List of GrandPa authorities that need to finalize the block right after the finalized
    /// block.
    pub grandpa_finalized_triggered_authorities: Vec<header::GrandpaAuthority>,

    /// List of changes in the GrandPa authorities list that have been scheduled by blocks that
    /// are already finalized but not triggered yet. These changes will for sure happen.
    pub grandpa_finalized_scheduled_changes: Vec<FinalizedScheduledChange>,

    /// Pre-allocated size of the chain, in number of non-finalized blocks.
    pub blocks_capacity: usize,
}

#[derive(Debug)]
pub struct FinalizedScheduledChange {
    pub trigger_block_height: u64,
    pub new_authorities_list: Vec<header::GrandpaAuthority>,
}

/// Holds state about the current state of the chain for the purpose of verifying headers.
pub struct NonFinalizedTree<T> {
    /// SCALE encoding of the header of the highest known finalized block.
    // TODO: should be an owned decoded header
    finalized_block_header: Vec<u8>,
    /// Hash of [`NonFinalizedTree::finalized_block_header`].
    finalized_block_hash: [u8; 32],
    /// Grandpa authorities set ID of the block right after the finalized block.
    grandpa_after_finalized_block_authorities_set_id: u64,
    /// List of GrandPa authorities that need to finalize the block right after the finalized
    /// block.
    grandpa_finalized_triggered_authorities: Vec<header::GrandpaAuthority>,
    /// List of changes in the GrandPa authorities list that have been scheduled by blocks that
    /// are already finalized. These changes will for sure happen.
    /// Contrary to the equivalent field in [`Config`], this list is always known to be ordered
    /// by block height.
    // TODO: doesn't have to be a collection; refactor into a single value
    grandpa_finalized_scheduled_changes: VecDeque<FinalizedScheduledChange>,

    /// Configuration for BABE, retreived from the genesis block.
    babe_genesis_config: babe::BabeGenesisConfiguration,

    /// Known Babe epoch transitions coming from the finalized block and its ancestors.
    /// Contrary to the equivalent field in [`Config`], this list is always known to be ordered
    /// by epoch number.
    // TODO: doesn't have to be a collection; refactor into a single value
    babe_known_epoch_information: VecDeque<(u64, header::BabeNextEpoch)>,

    /// If block 1 is finalized, contains its slot number.
    babe_finalized_block1_slot_number: Option<u64>,
    /// Container for non-finalized blocks.
    blocks: fork_tree::ForkTree<Block<T>>,
    /// Index within [`NonFinalizedTree::blocks`] of the current best block. `None` if and only
    /// if the fork tree is empty.
    current_best: Option<fork_tree::NodeIndex>,
}

struct Block<T> {
    // TODO: should be owned header
    scale_encoded_header: Vec<u8>,
    /// Cache of the hash of the block. Always equal to the hash of the header stored in this
    /// same struct.
    hash: [u8; 32],
    /// If this block is block #1 of the chain, contains its babe slot number. Otherwise, contains
    /// the slot number of the block #1 that is an ancestor of this block.
    babe_block1_slot_number: u64,
    /// If the block contains a Babe epoch change information, contains the epoch number of the
    /// change, and the information about this epoch).
    babe_epoch_change: Option<(u64, header::BabeNextEpoch)>,
    /// Babe epoch number the block belongs to.
    babe_epoch_number: u64,
    /// Opaque data decided by the user.
    user_data: T,
}

impl<T> NonFinalizedTree<T> {
    /// Initializes a new queue.
    pub fn new(mut config: Config) -> Self {
        let finalized_header = header::decode(&config.finalized_block_header).unwrap();
        if finalized_header.number >= 1 {
            assert!(config.babe_finalized_block1_slot_number.is_some());
        } else {
            assert_eq!(config.grandpa_after_finalized_block_authorities_set_id, 0);
        }

        let finalized_block_hash =
            header::hash_from_scale_encoded_header(&config.finalized_block_header);

        config
            .grandpa_finalized_scheduled_changes
            .retain(|sc| sc.trigger_block_height > finalized_header.number);
        config
            .grandpa_finalized_scheduled_changes
            .sort_by_key(|sc| sc.trigger_block_height);
        config
            .babe_known_epoch_information
            .sort_by_key(|(epoch_num, _)| *epoch_num);

        NonFinalizedTree {
            finalized_block_header: config.finalized_block_header,
            finalized_block_hash,
            grandpa_after_finalized_block_authorities_set_id: config
                .grandpa_after_finalized_block_authorities_set_id,
            grandpa_finalized_triggered_authorities: config.grandpa_finalized_triggered_authorities,
            grandpa_finalized_scheduled_changes: config
                .grandpa_finalized_scheduled_changes
                .into_iter()
                .collect(),
            babe_genesis_config: config.babe_genesis_config,
            babe_known_epoch_information: config.babe_known_epoch_information.into_iter().collect(),
            babe_finalized_block1_slot_number: config.babe_finalized_block1_slot_number,
            blocks: fork_tree::ForkTree::with_capacity(config.blocks_capacity),
            current_best: None,
        }
    }

    /// Returns the number of non-finalized blocks in the chain.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Shrink the capacity of the chain as much as possible.
    pub fn shrink_to_fit(&mut self) {
        self.blocks.shrink_to_fit()
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
        scale_encoded_header: &[u8],
    ) -> Result<HeaderVerifySuccess<T>, HeaderVerifyError> {
        match self.verify_inner(scale_encoded_header, None::<iter::Empty<Vec<u8>>>) {
            BodyVerifyStep1::InvalidHeader(err) => Err(HeaderVerifyError::InvalidHeader(err)),
            BodyVerifyStep1::ParentRuntimeRequired(_) => unreachable!(),
            BodyVerifyStep1::Duplicate => Ok(HeaderVerifySuccess::Duplicate),
            _ => todo!(),
        }
    }

    /// Underlying implementation of both header and header+body verification.
    fn verify_inner<'c, 'h, I, E>(
        &'c mut self,
        scale_encoded_header: &'h [u8],
        body: Option<I>,
    ) -> BodyVerifyStep1<'c, 'h, T, I>
    where
        I: ExactSizeIterator<Item = E> + Clone,
        E: AsRef<[u8]> + Clone,
    {
        let decoded_header = match header::decode(&scale_encoded_header) {
            Ok(h) => h,
            Err(err) => return BodyVerifyStep1::InvalidHeader(err),
        };

        let hash = header::hash_from_scale_encoded_header(&scale_encoded_header);

        if self.blocks.find(|b| b.hash == hash).is_some() {
            return BodyVerifyStep1::Duplicate;
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
                None => return BodyVerifyStep1::BadParent { parent_hash },
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
            BodyVerifyStep1::ParentRuntimeRequired(BodyVerifyRuntimeRequired {
                chain: self,
                header: decoded_header,
                parent_tree_index,
                body,
                block1_slot_number,
            })
        } else {
            todo!()
        }
    }

    /*
    /// Underlying implementation of both header and header+body verification.
    fn verify_inner(
        &mut self,
        scale_encoded_header: Vec<u8>,
        body: Option<impl ExactSizeIterator<Item = impl AsRef<[u8]> + Clone> + Clone>,
    ) -> Result<HeaderVerifySuccess<T>, HeaderVerifyError> {
        // Now perform the actual block verification.
        let import_success = if let Some(body) = body {
            // TODO: finish here
            let mut process = verify::header_body::verify(verify::header_body::Config {
                parent_runtime: todo!(),
                babe_genesis_configuration: &self.babe_genesis_config,
                block1_slot_number,
                now_from_unix_epoch: {
                    // TODO: is it reasonable to use the stdlib here?
                    //std::time::SystemTime::UNIX_EPOCH.elapsed().unwrap()
                    // TODO: this is commented out because of Wasm support
                    core::time::Duration::new(0, 0)
                },
                block_header: decoded_header.clone(),
                parent_block_header,
                block_body: body,
                top_trie_root_calculation_cache: None,
            });

            todo!()
        } else {
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
                parent_block_header,
            });

            loop {
                match process {
                    verify::header_only::Verify::Finished(Ok(result)) => break result,
                    verify::header_only::Verify::Finished(Err(err)) => {
                        return Err(HeaderVerifyError {
                            scale_encoded_header,
                            detail: HeaderVerifyErrorDetail::VerificationFailed(err),
                        });
                    }
                    verify::header_only::Verify::ReadyToRun(run) => process = run.run(),
                    verify::header_only::Verify::BabeEpochInformation(epoch_info_rq) => {
                        if let Some(info) = self
                            .babe_known_epoch_information
                            .iter()
                            .rev()
                            .find(|(e_num, _)| *e_num == epoch_info_rq.epoch_number())
                        {
                            process = epoch_info_rq.inject_epoch(From::from(&info.1)).run();
                        } else if let Some(parent_tree_index) = parent_tree_index {
                            if let Some(info) = self
                                .blocks
                                .node_to_root_path(parent_tree_index)
                                .map(|ni| &self.blocks.get(ni).unwrap().babe_epoch_change)
                                .filter_map(|ei| ei.as_ref())
                                .find(|e| e.0 == epoch_info_rq.epoch_number())
                            {
                                process = epoch_info_rq.inject_epoch(From::from(&info.1)).run();
                            } else {
                                return Err(HeaderVerifyError {
                                    scale_encoded_header,
                                    detail: HeaderVerifyErrorDetail::UnknownBabeEpoch,
                                });
                            }
                        } else {
                            return Err(HeaderVerifyError {
                                scale_encoded_header,
                                detail: HeaderVerifyErrorDetail::UnknownBabeEpoch,
                            });
                        }
                    }
                }
            }
        };

        // Verification is successful!

        // Determine if block would be new best.
        let is_new_best = if let Some(current_best) = self.current_best {
            is_better_block(&self.blocks, current_best, parent_tree_index, decoded_header)
        } else {
            debug_assert_eq!(self.blocks.len(), 1);
            true
        };

        let babe_block1_slot_number = block1_slot_number.unwrap_or_else(|| {
            debug_assert_eq!(decoded_header.number, 1);
            import_success.slot_number
        });

        let babe_epoch_change = import_success
            .babe_epoch_change
            .map(|e| (e.info_epoch_number, e.info.into()));

        let babe_epoch_number = import_success.epoch_number;

        Ok(HeaderVerifySuccess::Insert {
            is_new_best,
            insert: Insert {
                chain: self,
                parent_tree_index,
                is_new_best,
                scale_encoded_header,
                hash,
                babe_block1_slot_number,
                babe_epoch_change,
                babe_epoch_number,
            },
        })
    }*/

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
            None => return Err(JustificationVerifyError::UnknownTargetBlock),
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
                    let header =
                        header::decode(&self.blocks.get(node).unwrap().scale_encoded_header)
                            .unwrap();
                    for grandpa_digest_item in header.digest.logs().filter_map(|d| match d {
                        header::DigestItemRef::GrandpaConsensus(gp) => Some(gp),
                        _ => None,
                    }) {
                        match grandpa_digest_item {
                            header::GrandpaConsensusLogRef::ScheduledChange(change) => {
                                let trigger_block_height =
                                    header.number.checked_add(u64::from(change.delay)).unwrap();
                                match trigger_height {
                                    Some(_) => panic!("invalid block!"), // TODO: do better? also, this problem is not checked during block verification
                                    None => trigger_height = Some(trigger_block_height),
                                }
                            }
                            _ => unimplemented!(), // TODO: unimplemented
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

        // As explained above, `target_number` must be < `earliest_trigger`, otherwise the
        // finalization is unsecure.
        if let Some(earliest_trigger) = earliest_trigger {
            if u64::from(decoded.target_number) >= earliest_trigger {
                let block_to_finalize_hash = self
                    .blocks
                    .node_to_root_path(block_index)
                    .filter_map(|b| {
                        let b = self.blocks.get(b).unwrap();
                        if header::decode(&b.scale_encoded_header).unwrap().number
                            == earliest_trigger
                        {
                            Some(b.hash)
                        } else {
                            None
                        }
                    })
                    .next()
                    .unwrap();
                return Err(JustificationVerifyError::TooFarAhead {
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

        // Update the list of scheduled GrandPa changes with the ones contained in the
        // newly-finalized blocks.
        for node in self.blocks.root_to_node_path(block_index) {
            let node = self.blocks.get(node).unwrap();
            let decoded = header::decode(&node.scale_encoded_header).unwrap();
            for grandpa_digest_item in decoded.digest.logs().filter_map(|d| match d {
                header::DigestItemRef::GrandpaConsensus(gp) => Some(gp),
                _ => None,
            }) {
                match grandpa_digest_item {
                    header::GrandpaConsensusLogRef::ScheduledChange(change) => {
                        let trigger_block_height =
                            decoded.number.checked_add(u64::from(change.delay)).unwrap();
                        self.grandpa_finalized_scheduled_changes.push_back(
                            FinalizedScheduledChange {
                                trigger_block_height,
                                new_authorities_list: change
                                    .next_authorities
                                    .map(Into::into)
                                    .collect(),
                            },
                        );
                    }
                    _ => unimplemented!(), // TODO: unimplemented
                }
            }
        }

        // TODO: uncomment after https://github.com/rust-lang/rust/issues/53485
        //debug_assert!(self.grandpa_finalized_scheduled_changes.iter().is_sorted_by_key(|sc| sc.trigger_block_height));

        let new_finalized_block = self.blocks.get_mut(block_index).unwrap();
        self.finalized_block_header =
            mem::replace(&mut new_finalized_block.scale_encoded_header, Vec::new());
        self.finalized_block_hash =
            header::hash_from_scale_encoded_header(&self.finalized_block_header);

        let new_finalized_block_decoded = header::decode(&self.finalized_block_header).unwrap();

        // Remove the changes that have been triggered by the newly-finalized blocks.
        while let Some(next_change) = self.grandpa_finalized_scheduled_changes.front() {
            if next_change.trigger_block_height <= new_finalized_block_decoded.number {
                let next_change = self
                    .grandpa_finalized_scheduled_changes
                    .pop_front()
                    .unwrap();
                self.grandpa_finalized_triggered_authorities = next_change.new_authorities_list;
                self.grandpa_after_finalized_block_authorities_set_id += 1;
            }
        }

        if self.babe_finalized_block1_slot_number.is_none() {
            debug_assert!(new_finalized_block_decoded.number >= 1);
            self.babe_finalized_block1_slot_number =
                Some(new_finalized_block.babe_block1_slot_number);
        }

        let new_finalized_block_babe_epoch_number = new_finalized_block.babe_epoch_number;

        self.blocks.prune_ancestors(block_index);

        // If the current best was removed from the list, we need to update it.
        if self
            .current_best
            .map_or(true, |b| self.blocks.get(b).is_none())
        {
            // TODO: no; should try to find best block
            self.current_best = None;
        }

        // Purge the Babe epoch information from now-useless epochs.
        self.babe_known_epoch_information
            .retain(|(num, _)| *num >= new_finalized_block_babe_epoch_number);
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

///
#[derive(Debug)]
pub enum BodyVerifyStep1<'c, 'h, T, I> {
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
    ParentRuntimeRequired(BodyVerifyRuntimeRequired<'c, 'h, T, I>),
}

/// Verification is pending. In order to continue, a [`executor::WasmVmPrototype`] of the runtime
/// of the parent block must be provided.
#[derive(Debug)]
pub struct BodyVerifyRuntimeRequired<'c, 'h, T, I> {
    chain: &'c mut NonFinalizedTree<T>,
    header: header::HeaderRef<'h>,
    parent_tree_index: Option<fork_tree::NodeIndex>,
    body: I,
    block1_slot_number: Option<u64>,
}

impl<'c, 'h, T, I, E> BodyVerifyRuntimeRequired<'c, 'h, T, I>
where
    I: ExactSizeIterator<Item = E> + Clone,
    E: AsRef<[u8]> + Clone,
{
    pub fn resume(self, parent_runtime: executor::WasmVmPrototype) -> BodyVerifyStep2<'c, 'h, T> {
        let parent_block_header = if let Some(parent_tree_index) = self.parent_tree_index {
            header::decode(
                &self
                    .chain
                    .blocks
                    .get(parent_tree_index)
                    .unwrap()
                    .scale_encoded_header,
            )
            .unwrap()
        } else {
            header::decode(&self.chain.finalized_block_header).unwrap()
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
            block_header: self.header.clone(),
            parent_block_header,
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
pub enum BodyVerifyStep2<'c, 'h, T> {
    /// Verification is over.
    Finished {
        /// Value that was passed to [`BodyVerifyRuntimeRequired::resume`].
        parent_runtime: executor::WasmVmPrototype,
        /// Outcome of the verification.
        result: Result<Insert<'c, T>, HeaderVerifyError>,
    },
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet<'c, 'h, T>),
    /// Fetching the list of keys with a given prefix is required in order to continue.
    StoragePrefixKeys(StoragePrefixKeys<'c, 'h, T>),
    /// Fetching the key that follows a given one is required in order to continue.
    StorageNextKey(StorageNextKey<'c, 'h, T>),
}

struct BodyVerifyShared<'c, T> {
    chain: &'c mut NonFinalizedTree<T>,
    parent_tree_index: Option<fork_tree::NodeIndex>,
}

impl<'c, 'h, T> BodyVerifyStep2<'c, 'h, T> {
    fn from_inner(
        mut inner: verify::header_body::Verify<'h>,
        chain: BodyVerifyShared<'c, T>,
    ) -> Self {
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
                verify::header_body::Verify::BabeEpochInformation(_) => {
                    /*
                        if let Some(info) = self
                            .babe_known_epoch_information
                            .iter()
                            .rev()
                            .find(|(e_num, _)| *e_num == epoch_info_rq.epoch_number())
                        {
                            process = epoch_info_rq.inject_epoch(From::from(&info.1)).run();
                        } else if let Some(parent_tree_index) = parent_tree_index {
                            if let Some(info) = self
                                .blocks
                                .node_to_root_path(parent_tree_index)
                                .map(|ni| &self.blocks.get(ni).unwrap().babe_epoch_change)
                                .filter_map(|ei| ei.as_ref())
                                .find(|e| e.0 == epoch_info_rq.epoch_number())
                            {
                                process = epoch_info_rq.inject_epoch(From::from(&info.1)).run();
                            } else {
                                return Err(HeaderVerifyError {
                                    scale_encoded_header,
                                    detail: HeaderVerifyErrorDetail::UnknownBabeEpoch,
                                });
                            }
                        } else {
                            return Err(HeaderVerifyError {
                                scale_encoded_header,
                                detail: HeaderVerifyErrorDetail::UnknownBabeEpoch,
                            });
                        }
                    */
                    todo!()
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
pub struct StorageGet<'c, 'h, T> {
    inner: verify::header_body::StorageGet<'h>,
    chain: BodyVerifyShared<'c, T>,
}

impl<'c, 'h, T> StorageGet<'c, 'h, T> {
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
    pub fn inject_value(self, value: Option<&[u8]>) -> BodyVerifyStep2<'c, 'h, T> {
        let inner = self.inner.inject_value(value);
        BodyVerifyStep2::from_inner(inner.run(), self.chain)
    }
}

/// Fetching the list of keys with a given prefix is required in order to continue.
#[must_use]
pub struct StoragePrefixKeys<'c, 'h, T> {
    inner: verify::header_body::StoragePrefixKeys<'h>,
    chain: BodyVerifyShared<'c, T>,
}

impl<'c, 'h, T> StoragePrefixKeys<'c, 'h, T> {
    /// Returns the prefix whose keys to load.
    // TODO: don't take &mut mut but &self
    pub fn prefix(&mut self) -> &[u8] {
        self.inner.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(
        self,
        keys: impl Iterator<Item = impl AsRef<[u8]>>,
    ) -> BodyVerifyStep2<'c, 'h, T> {
        let inner = self.inner.inject_keys(keys);
        BodyVerifyStep2::from_inner(inner.run(), self.chain)
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct StorageNextKey<'c, 'h, T> {
    inner: verify::header_body::StorageNextKey<'h>,
    chain: BodyVerifyShared<'c, T>,
}

impl<'c, 'h, T> StorageNextKey<'c, 'h, T> {
    /// Returns the key whose next key must be passed back.
    // TODO: don't take &mut mut but &self
    pub fn key(&mut self) -> &[u8] {
        self.inner.key()
    }

    /// Injects the key.
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> BodyVerifyStep2<'c, 'h, T> {
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
    scale_encoded_header: Vec<u8>,
    hash: [u8; 32],
    babe_block1_slot_number: u64,
    babe_epoch_change: Option<(u64, header::BabeNextEpoch)>,
    babe_epoch_number: u64,
}

impl<'c, T> Insert<'c, T> {
    /// Inserts the block with the given user data.
    pub fn insert(self, user_data: T) {
        let new_node_index = self.chain.blocks.insert(
            self.parent_tree_index,
            Block {
                scale_encoded_header: self.scale_encoded_header,
                hash: self.hash,
                babe_block1_slot_number: self.babe_block1_slot_number,
                babe_epoch_change: self.babe_epoch_change,
                babe_epoch_number: self.babe_epoch_number,
                user_data,
            },
        );

        if self.is_new_best {
            self.chain.current_best = Some(new_node_index);
        }
    }

    /// Destroys the object without inserting the block in the chain. Returns the SCALE-encoded
    /// block header.
    pub fn into_scale_encoded_header(self) -> Vec<u8> {
        self.scale_encoded_header
    }
}

impl<'c, T> fmt::Debug for Insert<'c, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Insert").finish()
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
    UnknownTargetBlock,
    /// There exists a block in-between the latest finalized block and the block targeted by the
    /// justification that must first be finalized.
    #[display(
        fmt = "There exists a block in-between the latest finalized block and the block \
                     targeted by the justification that must first be finalized"
    )]
    TooFarAhead {
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
    blocks: &mut fork_tree::ForkTree<Block<T>>,
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
                let decoded = header::decode(&blocks.get(i).unwrap().scale_encoded_header).unwrap();
                if decoded
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
                let decoded = header::decode(&blocks.get(i).unwrap().scale_encoded_header).unwrap();
                if decoded
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
