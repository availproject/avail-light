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
use core::iter;

/// Accepts as parameter a container of blocks and indices within this container.
///
/// Returns true if `maybe_new_best` on top of `maybe_new_best_parent` is a better block compared
/// to `old_best`.
///
/// `maybe_new_best_parent` is to be `None` if the parent is the finalized block that is the
/// parent of all the leaves of the tree.
pub(super) fn is_better_block<T>(
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
        let (ascend, descend) = if let Some(maybe_new_best_parent) = maybe_new_best_parent {
            let (asc, desc) = blocks.ascend_and_descend(old_best, maybe_new_best_parent);
            (either::Left(asc), either::Left(desc))
        } else {
            (
                either::Right(blocks.node_to_root_path(old_best)),
                either::Right(iter::empty()),
            )
        };

        // TODO: update for Aura?
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
