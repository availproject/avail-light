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

//! Extension module containing the best block determination.

use super::*;
use core::{cmp::Ordering, iter};

/// Accepts as parameter a container of blocks and indices within this container.
///
/// Returns `Ordering::Greater` if `maybe_new_best` on top of `maybe_new_best_parent` is a better
/// block compared to `old_best`.
///
/// `maybe_new_best_parent` is to be `None` if the parent is the finalized block that is the
/// parent of all the leaves of the tree.
///
/// The implementation assumes that all blocks of the chain use the same consensus algorithm. No
/// output is guaranteed if this is not the case.
pub(super) fn is_better_block<T>(
    blocks: &fork_tree::ForkTree<Block<T>>,
    old_best: fork_tree::NodeIndex,
    maybe_new_best_parent: Option<fork_tree::NodeIndex>,
    maybe_new_best: header::HeaderRef,
) -> Ordering {
    debug_assert!(
        maybe_new_best_parent.map_or(true, |p_idx| blocks.get(p_idx).unwrap().hash
            == *maybe_new_best.parent_hash)
    );

    // A descendant is always preferred to its ancestor.
    if maybe_new_best_parent.map_or(false, |p_idx| blocks.is_ancestor(old_best, p_idx)) {
        return Ordering::Greater;
    };

    // In order to determine whether the new block is our new best:
    //
    // - Find the common ancestor between the current best and the new block's parent.
    // - Count the number of Babe primary slot claims between the common ancestor and the current
    //   best.
    // - Count the number of Babe primary slot claims between the common ancestor and the new
    //   block's parent. Add one if the new block has a Babe primary slot claim.
    // - If the number for the new block is strictly superior, then the new block is our new best.
    //
    // For algorithms other than Babe, all blocks simply count as one, such that the longest
    // chain is the preferred one.
    //
    // The code below assumes that all blocks use the same consensus algorithm. It is not
    // meaningful to compare the score of an Aura chain and the score of a Babe chain, for
    // example.
    let (ascend, descend) = if let Some(maybe_new_best_parent) = maybe_new_best_parent {
        let (asc, desc) = blocks.ascend_and_descend(old_best, maybe_new_best_parent);
        (either::Left(asc), either::Left(desc))
    } else {
        (
            either::Right(blocks.node_to_root_path(old_best)),
            either::Right(iter::empty()),
        )
    };

    let curr_best_chain_score: usize = ascend
        .map(|i| {
            if let Some(pr) = blocks.get(i).unwrap().header.digest.babe_pre_runtime() {
                if pr.is_primary() {
                    1
                } else {
                    0
                }
            } else {
                1
            }
        })
        .sum();

    let candidate_score = {
        if let Some(pr) = maybe_new_best.digest.babe_pre_runtime() {
            if pr.is_primary() {
                1
            } else {
                0
            }
        } else {
            1
        }
    };

    let candidate_chain_score: usize = descend
        .map(|i| {
            if let Some(pr) = blocks.get(i).unwrap().header.digest.babe_pre_runtime() {
                if pr.is_primary() {
                    1
                } else {
                    0
                }
            } else {
                1
            }
        })
        .sum();

    (candidate_chain_score + candidate_score).cmp(&curr_best_chain_score)
}
