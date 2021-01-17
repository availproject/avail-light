// Smoldot
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

//! Extension module containing the API and implementation of everything related to finality.

use super::*;

impl<T> NonFinalizedTree<T> {
    /// Verifies the given justification.
    ///
    /// The verification is performed in the context of the chain. In particular, the
    /// verification will fail if the target block isn't already in the chain.
    ///
    /// If the verification succeeds, a [`JustificationApply`] object will be returned which can
    /// be used to apply the finalization.
    // TODO: expand the documentation about how blocks with authorities changes have to be finalized before any further block can be finalized
    pub fn verify_justification(
        &mut self,
        scale_encoded_justification: &[u8],
    ) -> Result<JustificationApply<T>, JustificationVerifyError> {
        self.inner
            .as_mut()
            .unwrap()
            .verify_justification(scale_encoded_justification)
    }

    /// Sets the latest known finalized block. Trying to verify a block that isn't a descendant of
    /// that block will fail.
    ///
    /// The block must have been passed to [`NonFinalizedTree::verify_header`].
    ///
    /// Returns an iterator containing the now-finalized blocks in decreasing block numbers. In
    /// other words, the first element of the iterator is always the block whose hash is the
    /// `block_hash` passed as parameter.
    ///
    /// > **Note**: This function returns blocks in decreasing block number, because any other
    /// >           ordering would incur a performance cost. While returning blocks in increasing
    /// >           block number would often be more convenient, the overhead of doing so is
    /// >           moved to the user.
    ///
    /// The pruning is completely performed, even if the iterator is dropped eagerly.
    pub fn set_finalized_block(
        &mut self,
        block_hash: &[u8; 32],
    ) -> Result<SetFinalizedBlockIter<T>, SetFinalizedError> {
        let inner = self.inner.as_mut().unwrap();

        let block_index = match inner.blocks.find(|b| b.hash == *block_hash) {
            Some(idx) => idx,
            None => return Err(SetFinalizedError::UnknownBlock),
        };

        Ok(inner.set_finalized_block(block_index))
    }
}

impl<T> NonFinalizedTreeInner<T> {
    /// See [`NonFinalizedTree::verify_justification`].
    fn verify_justification(
        &mut self,
        scale_encoded_justification: &[u8],
    ) -> Result<JustificationApply<T>, JustificationVerifyError> {
        match &self.finality {
            Finality::Outsourced => Err(JustificationVerifyError::AlgorithmHasNoJustification),
            Finality::Grandpa {
                after_finalized_block_authorities_set_id,
                finalized_scheduled_change,
                finalized_triggered_authorities,
            } => {
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
                // TODO: rethink and reexplain this ^

                // Find out the next block height where an authority change will be triggered.
                let earliest_trigger = {
                    // Scheduled change that is already finalized.
                    let scheduled = finalized_scheduled_change.as_ref().map(|(n, _)| *n);

                    // First change that would be scheduled if we finalize the target block.
                    let would_happen = {
                        let mut trigger_height = None;
                        // TODO: lot of boilerplate code here
                        for node in self.blocks.root_to_node_path(block_index) {
                            let header = &self.blocks.get(node).unwrap().header;
                            for grandpa_digest_item in
                                header.digest.logs().filter_map(|d| match d {
                                    header::DigestItemRef::GrandpaConsensus(gp) => Some(gp),
                                    _ => None,
                                })
                            {
                                match grandpa_digest_item {
                                    header::GrandpaConsensusLogRef::ScheduledChange(change) => {
                                        let trigger_block_height = header
                                            .number
                                            .checked_add(u64::from(change.delay))
                                            .unwrap();
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

                    match (scheduled, would_happen) {
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
                let authorities_list = finalized_scheduled_change
                    .as_ref()
                    .filter(|(trigger_height, _)| {
                        *trigger_height < u64::from(decoded.target_number)
                    })
                    .map(|(_, list)| list)
                    .unwrap_or(finalized_triggered_authorities);

                // As per above check, we know that the authorities of the target block are either the
                // same as the ones of the latest finalized block, or the ones contained in the header of
                // the latest finalized block.
                justification::verify::verify(justification::verify::Config {
                    justification: decoded,
                    authorities_set_id: *after_finalized_block_authorities_set_id,
                    authorities_list: authorities_list.iter().map(|a| a.public_key),
                })
                .map_err(JustificationVerifyError::VerificationFailed)?;

                // Justification has been successfully verified!
                Ok(JustificationApply {
                    chain: self,
                    to_finalize: block_index,
                })
            }
        }
    }

    /// Implementation of [`NonFinalizedTree::set_finalized_block`].
    fn set_finalized_block(
        &mut self,
        block_index: fork_tree::NodeIndex,
    ) -> SetFinalizedBlockIter<T> {
        let target_block_height = self.blocks.get_mut(block_index).unwrap().header.number;

        match &mut self.finality {
            Finality::Outsourced => {}
            Finality::Grandpa {
                after_finalized_block_authorities_set_id,
                finalized_scheduled_change,
                finalized_triggered_authorities,
            } => {
                // Update the scheduled GrandPa change with the latest scheduled-but-non-finalized change
                // that could be found.
                *finalized_scheduled_change = None;
                for node in self.blocks.root_to_node_path(block_index) {
                    let node = self.blocks.get(node).unwrap();
                    //node.header.number
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
                                if trigger_block_height > target_block_height {
                                    *finalized_scheduled_change = Some((
                                        trigger_block_height,
                                        change.next_authorities.map(Into::into).collect(),
                                    ));
                                } else {
                                    *finalized_triggered_authorities =
                                        change.next_authorities.map(Into::into).collect();
                                    *after_finalized_block_authorities_set_id += 1;
                                }
                            }
                            _ => {} // TODO: unimplemented
                        }
                    }
                }
            }
        }

        let new_finalized_block = self.blocks.get_mut(block_index).unwrap();

        match (
            &mut self.finalized_consensus,
            &new_finalized_block.consensus,
        ) {
            (
                FinalizedConsensus::Aura {
                    authorities_list, ..
                },
                BlockConsensus::Aura {
                    authorities_list: new_list,
                },
            ) => {
                *authorities_list = new_list.clone();
            }
            (
                FinalizedConsensus::Babe {
                    block_epoch_information,
                    next_epoch_transition,
                    ..
                },
                BlockConsensus::Babe {
                    current_epoch,
                    next_epoch,
                },
            ) => {
                *block_epoch_information = current_epoch.clone();
                *next_epoch_transition = next_epoch.clone();
            }
            // Any mismatch of consensus engines between the chain and the newly-finalized block
            // should have been detected when the block got added to the chain.
            _ => unreachable!(),
        }

        mem::swap(
            &mut self.finalized_block_header,
            &mut new_finalized_block.header,
        );
        self.finalized_block_hash = self.finalized_block_header.hash();

        SetFinalizedBlockIter {
            iter: self.blocks.prune_ancestors(block_index),
            current_best: &mut self.current_best,
        }
    }
}

/// Returned by [`NonFinalizedTree::verify_justification`] on success.
///
/// As long as [`JustificationApply::apply`] isn't called, the underlying [`NonFinalizedTree`]
/// isn't modified.
#[must_use]
pub struct JustificationApply<'c, T> {
    chain: &'c mut NonFinalizedTreeInner<T>,
    to_finalize: fork_tree::NodeIndex,
}

impl<'c, T> JustificationApply<'c, T> {
    /// Applies the justification, finalizing the given block.
    ///
    /// This function, including its return type, behaves in the same way as
    /// [`NonFinalizedTree::set_finalized_block`].
    pub fn apply(self) -> SetFinalizedBlockIter<'c, T> {
        self.chain.set_finalized_block(self.to_finalize)
    }

    /// Returns the user data of the block about to be justified.
    pub fn block_user_data(&mut self) -> &mut T {
        &mut self
            .chain
            .blocks
            .get_mut(self.to_finalize)
            .unwrap()
            .user_data
    }

    /// Returns true if the block to be finalized is the current best block.
    pub fn is_current_best_block(&self) -> bool {
        Some(self.to_finalize) == self.chain.current_best
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
    /// Finality mechanism used by the chain doesn't use justifications.
    AlgorithmHasNoJustification,
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

/// Iterator producing the newly-finalized blocks removed from the state when the finalized block
/// is updated.
pub struct SetFinalizedBlockIter<'a, T> {
    iter: fork_tree::PruneAncestorsIter<'a, Block<T>>,
    current_best: &'a mut Option<fork_tree::NodeIndex>,
}

impl<'a, T> Iterator for SetFinalizedBlockIter<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let pruned = self.iter.next()?;
            if Some(pruned.index) == *self.current_best {
                *self.current_best = None;
            }
            if !pruned.is_prune_target_ancestor {
                continue;
            }
            break Some(pruned.user_data.user_data);
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<'a, T> Drop for SetFinalizedBlockIter<'a, T> {
    fn drop(&mut self) {
        // Make sure the iteration goes to the end.
        for _ in self {}

        // TODO: update current_best with the new best block
    }
}

/// Error that can happen when setting the finalized block.
#[derive(Debug, derive_more::Display)]
pub enum SetFinalizedError {
    /// Block must have been passed to [`NonFinalizedTree::verify_header`] in the past.
    UnknownBlock,
}
