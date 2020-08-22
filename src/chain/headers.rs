//! Chain of block headers.
//!
//! This module provides the [`Chain`] struct. It contains the state necessary to maintain a
//! chain of block headers.
//!
//! The state in the [`Chain`] consists of:
//!
//! - One "latest finalized" block.
//! - Zero or more blocks that descend from the latest finalized block.
//!
//! The latest finalized block represents the block that we know will never be reverted. While it
//! can always be set to the genesis block of the chain, it is preferable, in order to reduce
//! memory utilization, to maintain it to a block that is as high as possible in the chain.
//!
//! A block can be added to the chain by calling [`Chain::verify`]. You are
//! encouraged to regularly update the latest finalized block using
//! [`Chain::set_finalized_block`].
//!
//! > **Note**: While the GrandPa protocol provides a network-wide way to designate a block as
//! >           final, the concept of GrandPa-provided finality doesn't have to be the same as
//! >           the finality decided for the [`Chain`]. For example, an API user
//! >           might decide that the block whose number is `latest_block - 5` will always be
//! >           final, and rebuild a new [`Chain`] if that assumption turned out to
//! >           not be true.

// TODO: rethink this doc ^

use crate::{babe, finality::justification, fork_tree, header, verify};
use core::fmt;

/// Configuration for the [`Chain`].
// TODO: #[derive(Debug)]
pub struct Config {
    /// SCALE encoding of the header of the highest known finalized block.
    ///
    /// Once the queue is created, it is as if you had called
    /// [`Chain::set_finalized_block`] with this block.
    // TODO: should be an owned decoded header?
    pub finalized_block_header: Vec<u8>,

    /// If the number in [`Config::finalized_block_header`] is superior or equal to 1, then this
    /// field must contain the slot number of the block whose number is 1 and is an ancestor of
    /// the finalized block.
    pub babe_finalized_block1_slot_number: Option<u64>,

    /// Known Babe epoch transitions coming from the finalized block and its ancestors.
    pub babe_known_epoch_information: Vec<(u64, babe::EpochInformation)>,

    /// Configuration for BABE, retreived from the genesis block.
    pub babe_genesis_config: babe::BabeGenesisConfiguration,

    /// Pre-allocated size of the chain, in number of blocks.
    pub capacity: usize,
}

/// Holds state about the current state of the chain for the purpose of verifying headers.
pub struct Chain<T> {
    /// SCALE encoding of the header of the highest known finalized block.
    // TODO: should be an owned decoded header
    finalized_block_header: Vec<u8>,
    /// Hash of [`Chain::finalized_block_header`].
    finalized_block_hash: [u8; 32],

    /// Configuration for BABE, retreived from the genesis block.
    babe_genesis_config: babe::BabeGenesisConfiguration,

    /// Known Babe epoch transitions coming from the finalized block and its ancestors.
    babe_known_epoch_information: Vec<(u64, babe::EpochInformation)>,

    babe_epoch_info_cache: lru::LruCache<u64, babe::EpochInformation>,
    /// If block 1 is finalized, contains its slot number.
    babe_finalized_block1_slot_number: Option<u64>,
    /// Container for non-finalized blocks.
    blocks: fork_tree::ForkTree<Block<T>>,
    /// Index within [`Chain::blocks`] of the current best block. `None` if and only
    /// if the fork tree is empty.
    current_best: Option<fork_tree::NodeIndex>,
}

struct Block<T> {
    // TODO: should be owned header
    scale_encoded_header: Vec<u8>,
    hash: [u8; 32],
    /// If this block is block #1 of the chain, contains its babe slot number. Otherwise, contains
    /// the slot number of the block #1 that is an ancestor of this block.
    babe_block1_slot_number: u64,
    /// If the block contains a Babe epoch change information, this is it.
    babe_epoch_change: Option<babe::EpochChangeInformation>,
    /// Opaque data decided by the user.
    user_data: T,
}

impl<T> Chain<T> {
    /// Initializes a new queue.
    pub fn new(config: Config) -> Self {
        let finalized_header = header::decode(&config.finalized_block_header).unwrap();
        if finalized_header.number >= 1 {
            assert!(config.babe_finalized_block1_slot_number.is_some());
        }

        let finalized_block_hash =
            header::hash_from_scale_encoded_header(&config.finalized_block_header);

        Chain {
            finalized_block_header: config.finalized_block_header,
            finalized_block_hash,
            babe_genesis_config: config.babe_genesis_config,
            babe_known_epoch_information: config.babe_known_epoch_information,
            babe_epoch_info_cache: lru::LruCache::new(4),
            babe_finalized_block1_slot_number: config.babe_finalized_block1_slot_number,
            blocks: fork_tree::ForkTree::with_capacity(config.capacity),
            current_best: None,
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
    pub fn verify(
        &mut self,
        scale_encoded_header: Vec<u8>,
    ) -> Result<VerifySuccess<T>, VerifyError> {
        let decoded_header = match header::decode(&scale_encoded_header) {
            Ok(h) => h,
            Err(err) => {
                return Err(VerifyError {
                    scale_encoded_header,
                    detail: VerifyErrorDetail::InvalidHeader(err),
                })
            }
        };

        let hash = header::hash_from_scale_encoded_header(&scale_encoded_header);

        if self.blocks.find(|b| b.hash == hash).is_some() {
            return Ok(VerifySuccess::Duplicate {
                scale_encoded_header,
            });
        }

        // Try to find the parent block in the tree of known blocks.
        // `Some` with an index of the parent within the tree of unfinalized blocks.
        // `None` means that the parent is the finalized block.
        //
        // The parent hash is first checked against `self.current_best`, as it is most likely that
        // new blocks are built on top of the current best.
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
                    return Err(VerifyError {
                        scale_encoded_header,
                        detail: VerifyErrorDetail::BadParent { parent_hash },
                    })
                }
            }
        };

        let parent_block_header = if let Some(parent_tree_index) = parent_tree_index {
            header::decode(
                &self
                    .blocks
                    .get(parent_tree_index)
                    .unwrap()
                    .scale_encoded_header,
            )
            .unwrap()
        } else {
            header::decode(&self.finalized_block_header).unwrap()
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

        // Now perform the actual block verification.
        let import_success = {
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
                        return Err(VerifyError {
                            scale_encoded_header,
                            detail: VerifyErrorDetail::VerificationFailed(err),
                        });
                    }
                    verify::header_only::Verify::ReadyToRun(run) => process = run.run(),
                    verify::header_only::Verify::EpochInformation(epoch_info_rq) => {
                        if let Some(info) = self
                            .babe_known_epoch_information
                            .iter()
                            .find(|(e_num, _)| *e_num == epoch_info_rq.epoch_number())
                        {
                            process = epoch_info_rq.inject_epoch(&info.1).run();
                        } else if let Some(parent_tree_index) = parent_tree_index {
                            if let Some(info) = self
                                .blocks
                                .node_to_root_path(parent_tree_index)
                                .map(|ni| &self.blocks.get(ni).unwrap().babe_epoch_change)
                                .filter_map(|ei| ei.as_ref())
                                .find(|e| e.info_epoch_number == epoch_info_rq.epoch_number())
                            {
                                process = epoch_info_rq.inject_epoch(&info.info).run();
                            } else {
                                return Err(VerifyError {
                                    scale_encoded_header,
                                    detail: VerifyErrorDetail::UnknownBabeEpoch,
                                });
                            }
                        } else {
                            return Err(VerifyError {
                                scale_encoded_header,
                                detail: VerifyErrorDetail::UnknownBabeEpoch,
                            });
                        }
                    }
                }
            }
        };

        // Verification is successful!

        // Determine if block would be new best.
        let is_new_best = if let Some(current_best) = self.current_best {
            if parent_tree_index.map_or(false, |p_idx| self.blocks.is_ancestor(current_best, p_idx))
            {
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
                let (ascend, descend) = self
                    .blocks
                    .ascend_and_descend(current_best, parent_tree_index.unwrap());

                let curr_best_primary_slots: usize = ascend
                    .map(|i| {
                        let decoded =
                            header::decode(&self.blocks.get(i).unwrap().scale_encoded_header)
                                .unwrap();
                        let decoded = babe::header_info::header_information(decoded).unwrap();
                        if decoded.pre_runtime.is_primary() {
                            1
                        } else {
                            0
                        }
                    })
                    .sum();

                let new_block_primary_slots = {
                    let decoded =
                        babe::header_info::header_information(decoded_header.clone()).unwrap();
                    if decoded.pre_runtime.is_primary() {
                        1
                    } else {
                        0
                    }
                };

                let parent_primary_slots: usize = descend
                    .map(|i| {
                        let decoded =
                            header::decode(&self.blocks.get(i).unwrap().scale_encoded_header)
                                .unwrap();
                        let decoded = babe::header_info::header_information(decoded).unwrap();
                        if decoded.pre_runtime.is_primary() {
                            1
                        } else {
                            0
                        }
                    })
                    .sum();

                // Note the strictly superior. If there is an equality, we keep the current best.
                parent_primary_slots + new_block_primary_slots > curr_best_primary_slots
            }
        } else {
            debug_assert_eq!(self.blocks.len(), 1);
            true
        };

        let babe_block1_slot_number = block1_slot_number.unwrap_or_else(|| {
            debug_assert_eq!(decoded_header.number, 1);
            import_success.slot_number
        });

        Ok(VerifySuccess::Insert {
            is_new_best,
            insert: Insert {
                chain: self,
                parent_tree_index,
                is_new_best,
                scale_encoded_header,
                hash,
                babe_block1_slot_number,
                babe_epoch_change: import_success.babe_epoch_change,
            },
        })
    }

    /// Verifies the given justification.
    ///
    /// The verification is performed in the context of the chain. In particular, the
    /// verification will fail if the target block isn't already in the chain.
    ///
    /// If the verification succeeds, a [`JustificationApply`] object might be returned which can
    /// be used to then insert the block in the chain.
    pub fn verify_justification(
        &mut self,
        scale_encoded_justification: &[u8],
    ) -> Result<(), JustificationVerifyError> {
        let decoded = justification::decode::decode(&scale_encoded_justification)
            .map_err(JustificationVerifyError::InvalidJustification)?;

        let verif_result = justification::verify::verify(justification::verify::Config {
            justification: decoded,
            authorities_set_id: 0,                           // TODO:
            authorities_list: std::iter::empty::<Vec<u8>>(), // TODO:
        })
        .map_err(JustificationVerifyError::VerificationFailed)?;

        todo!()
    }

    /// Sets the latest known finalized block. Trying to verify a block that isn't a descendant of
    /// that block will fail.
    ///
    /// The block must have been passed to [`Chain::verify`].
    pub fn set_finalized_block(&mut self, block_hash: &[u8; 32]) -> Result<(), SetFinalizedError> {
        todo!()
    }
}

impl<T> fmt::Debug for Chain<T>
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
pub enum VerifySuccess<'a, T> {
    /// Block is already known.
    Duplicate {
        /// Header that was requested to be verified.
        scale_encoded_header: Vec<u8>,
    },
    /// Block wasn't known and is ready to be inserted.
    Insert {
        /// True if the verified block will become the new "best" block after being inserted.
        is_new_best: bool,
        /// Use this struct to insert the block in the chain after its successful verification.
        insert: Insert<'a, T>,
    },
}

/// Mutably borrows the [`Chain`] and allows insert a successfully-verified block into it.
#[must_use]
pub struct Insert<'a, T> {
    chain: &'a mut Chain<T>,
    /// Copy of the value in [`VerifySuccess::is_new_best`].
    is_new_best: bool,
    /// Index of the parent in [`Chain::blocks`].
    parent_tree_index: Option<fork_tree::NodeIndex>,
    scale_encoded_header: Vec<u8>,
    hash: [u8; 32],
    babe_block1_slot_number: u64,
    babe_epoch_change: Option<babe::EpochChangeInformation>,
}

impl<'a, T> Insert<'a, T> {
    /// Inserts the block with the given user data.
    pub fn insert(self, user_data: T) -> InsertSuccess<'a> {
        let new_node_index = self.chain.blocks.insert(
            self.parent_tree_index,
            Block {
                scale_encoded_header: self.scale_encoded_header,
                hash: self.hash,
                babe_block1_slot_number: self.babe_block1_slot_number,
                babe_epoch_change: self.babe_epoch_change,
                user_data,
            },
        );

        if self.is_new_best {
            self.chain.current_best = Some(new_node_index);
        }

        let scale_encoded_header = &self
            .chain
            .blocks
            .get(new_node_index)
            .unwrap()
            .scale_encoded_header[..];
        InsertSuccess {
            scale_encoded_header,
            header: header::decode(scale_encoded_header).unwrap(),
        }
    }

    /// Destroys the object without inserting the block in the chain. Returns the SCALE-encoded
    /// block header.
    pub fn into_scale_encoded_header(self) -> Vec<u8> {
        self.scale_encoded_header
    }
}

impl<'a, T> fmt::Debug for Insert<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Insert").finish()
    }
}

/// Verification is successful.
pub struct InsertSuccess<'a> {
    /// Same value as the parameter passed to [`Chain::verify`].
    pub scale_encoded_header: &'a [u8],
    /// Same value as the parameter passed to [`Chain::verify`].
    pub header: header::HeaderRef<'a>,
}

/// Error that can happen when verifying a block.
#[derive(Debug, derive_more::Display)]
#[display(fmt = "{}", detail)]
pub struct VerifyError {
    /// Same value as the parameter passed to [`Chain::verify`].
    pub scale_encoded_header: Vec<u8>,
    /// Reason for the failure.
    pub detail: VerifyErrorDetail,
}

/// Error that can happen when verifying a block.
#[derive(Debug, derive_more::Display)]
pub enum VerifyErrorDetail {
    /// Error while decoding the header.
    InvalidHeader(header::Error),
    /// The parent of the block isn't known.
    #[display(fmt = "The parent of the block isn't known.")]
    BadParent {
        /// Hash of the current block in question.
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
pub struct JustificationApply<'a, T> {
    chain: &'a mut Chain<T>,
}

impl<'a, T> JustificationApply<'a, T> {
    /// Applies the justification, finalizing the given block.
    // TODO: return type?
    pub fn apply(self) {
        todo!()
    }
}

impl<'a, T> fmt::Debug for JustificationApply<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("JustificationApply").finish()
    }
}

/// Error that can happen when verifying a justification.
#[derive(Debug, derive_more::Display)]
pub enum JustificationVerifyError {
    /// Error while decoding the justification.
    InvalidJustification(justification::decode::Error),
    /// The justification verification has failed. The justification is invalid and should be
    /// thrown away.
    VerificationFailed(justification::verify::Error),
}

/// Error that can happen when setting the finalized block.
#[derive(Debug, derive_more::Display)]
pub enum SetFinalizedError {
    /// Block must have been passed to [`Chain::verify`] in the past.
    UnknownBlock,
}
