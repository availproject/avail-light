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

//! Collection of "disjoint" blocks, in other words blocks whose existence is known but which
//! can't be verified yet.
//!
//! > **Example**: The local node knows about block 5. A peer announces block 7. Since the local
//! >              node doesn't know block 6, it has to store block 7 for later, then download
//! >              block 6. The container in this module is where block 7 is temporarily stored.
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

use alloc::{
    collections::{btree_map::Entry, BTreeMap},
    vec,
    vec::Vec,
};
use core::{fmt, iter, mem};

/// Collection of pending blocks.
pub struct DisjointBlocks<TBl> {
    /// All blocks in the collection. Keys are the block height and hash.
    blocks: BTreeMap<(u64, [u8; 32]), Block<TBl>>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct Block<TBl> {
    parent_hash: Option<[u8; 32]>,
    bad: bool,
    user_data: TBl,
}

impl<TBl> DisjointBlocks<TBl> {
    /// Initializes a new empty collection of blocks.
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// Initializes a new collection of blocks with the given capacity.
    pub fn with_capacity(_capacity: usize) -> Self {
        DisjointBlocks {
            blocks: BTreeMap::default(),
        }
    }

    /// Returns `true` if this data structure doesn't contain any block.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Returns the number of blocks stored in the data structure.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns the list of blocks in the collection.
    pub fn iter(&'_ self) -> impl Iterator<Item = (u64, &[u8; 32], &'_ TBl)> + '_ {
        self.blocks
            .iter()
            .map(|((he, ha), bl)| (*he, ha, &bl.user_data))
    }

    /// Returns `true` if the block with the given height and hash is in the collection.
    pub fn contains(&self, height: u64, hash: &[u8; 32]) -> bool {
        self.blocks.contains_key(&(height, *hash))
    }

    /// Inserts the block in the collection, passing a user data.
    ///
    /// If a `parent_hash` is passed, and the parent is known to be bad, the newly-inserted block
    /// is immediately marked as bad as well.
    ///
    /// Returns the previous user data associated to this block, if any.
    pub fn insert(
        &mut self,
        height: u64,
        hash: [u8; 32],
        parent_hash: Option<[u8; 32]>,
        user_data: TBl,
    ) -> Option<TBl> {
        let parent_is_bad = match (height.checked_sub(1), parent_hash) {
            (Some(parent_height), Some(parent_hash)) => self
                .blocks
                .get(&(parent_height, parent_hash))
                .map_or(false, |b| b.bad),
            _ => false,
        };

        // Insertion is done "manually" in order to not override the value of `bad` if the block
        // is already in the collection.
        match self.blocks.entry((height, hash)) {
            Entry::Occupied(entry) => {
                let entry = entry.into_mut();

                match (parent_hash, &mut entry.parent_hash) {
                    (Some(parent_hash), &mut Some(ph)) => debug_assert_eq!(ph, parent_hash),
                    (Some(parent_hash), ph @ &mut None) => *ph = Some(parent_hash),
                    (None, _) => {}
                }

                Some(mem::replace(&mut entry.user_data, user_data))
            }
            Entry::Vacant(entry) => {
                entry.insert(Block {
                    parent_hash,
                    bad: parent_is_bad,
                    user_data,
                });

                None
            }
        }
    }

    /// Removes the block from the collection.
    ///
    /// # Panic
    ///
    /// Panics if the block with the given height and hash hasn't been inserted before.
    ///
    #[track_caller]
    pub fn remove(&mut self, height: u64, hash: &[u8; 32]) -> TBl {
        self.blocks.remove(&(height, *hash)).unwrap().user_data
    }

    /// Removes from the collection the blocks whose height is strictly inferior to the given
    /// value, and returns them.
    pub fn remove_below_height(
        &mut self,
        threshold: u64,
    ) -> impl ExactSizeIterator<Item = (u64, [u8; 32], TBl)> {
        let above_threshold = self.blocks.split_off(&(threshold, [0; 32]));
        let below_threshold = mem::replace(&mut self.blocks, above_threshold);
        below_threshold
            .into_iter()
            .map(|((he, ha), v)| (he, ha, v.user_data))
    }

    /// Returns the user data associated to the block. This is the value originally passed
    /// through [`DisjointBlocks::insert`].
    ///
    /// Returns `None` if the block hasn't been inserted before.
    pub fn user_data(&self, height: u64, hash: &[u8; 32]) -> Option<&TBl> {
        Some(&self.blocks.get(&(height, *hash))?.user_data)
    }

    /// Returns the user data associated to the block. This is the value originally passed
    /// through [`DisjointBlocks::insert`].
    ///
    /// Returns `None` if the block hasn't been inserted before.
    pub fn user_data_mut(&mut self, height: u64, hash: &[u8; 32]) -> Option<&mut TBl> {
        Some(&mut self.blocks.get_mut(&(height, *hash))?.user_data)
    }

    /// Returns the parent hash of the given block.
    ///
    /// Returns `None` if either the block or its parent isn't known.
    pub fn parent_hash(&self, height: u64, hash: &[u8; 32]) -> Option<&[u8; 32]> {
        self.blocks
            .get(&(height, *hash))
            .and_then(|b| b.parent_hash.as_ref())
    }

    /// Returns the list of blocks whose height is `height + 1` and whose parent hash is the
    /// given block.
    pub fn children(
        &'_ self,
        height: u64,
        hash: &[u8; 32],
    ) -> impl Iterator<Item = (u64, &[u8; 32], &'_ TBl)> + '_ {
        let hash = *hash;
        self.blocks
            .range((height + 1, [0; 32])..=(height + 1, [0xff; 32]))
            .filter(move |((_maybe_child_height, _), maybe_child)| {
                debug_assert_eq!(*_maybe_child_height, height + 1);
                maybe_child.parent_hash.as_ref() == Some(&hash)
            })
            .map(|((he, ha), bl)| (*he, ha, &bl.user_data))
    }

    /// Sets the parent hash of the given block.
    ///
    /// If the parent is in the collection and known to be bad, the block is marked as bad as
    /// well.
    ///
    /// # Panic
    ///
    /// Panics if the block with the given height and hash hasn't been inserted before.
    ///
    #[track_caller]
    pub fn set_parent_hash(&mut self, height: u64, hash: &[u8; 32], parent_hash: [u8; 32]) {
        let parent_is_bad = match height.checked_sub(1) {
            Some(parent_height) => self
                .blocks
                .get(&(parent_height, parent_hash))
                .map_or(false, |b| b.bad),
            None => false,
        };

        let block = self.blocks.get_mut(&(height, *hash)).unwrap();

        match &mut block.parent_hash {
            &mut Some(ph) => debug_assert_eq!(ph, parent_hash),
            ph @ &mut None => *ph = Some(parent_hash),
        }

        if parent_is_bad {
            block.bad = true;
        }
    }

    /// Marks the given block and all its known children as "bad".
    ///
    /// If a child of this block is later added to the collection, it is also automatically
    /// marked as bad.
    ///
    /// # Panic
    ///
    /// Panics if the block with the given height and hash hasn't been inserted before.
    ///
    #[track_caller]
    pub fn set_block_bad(&mut self, height: u64, hash: &[u8; 32]) {
        // The implementation of this method is far from being optimized, but it is by far the
        // easiest way to implement it, and this operation is expected to happen rarely.

        // Initially contains the concerned block, then will contain the children of the concerned
        // block, then the grand-children, then the grand-grand-children, and so on.
        let mut blocks = vec![(height, *hash)];

        while !blocks.is_empty() {
            let mut children = Vec::new();

            for (height, hash) in blocks {
                self.blocks.get_mut(&(height, hash)).unwrap().bad = true;

                // Iterate over all blocks whose height is `height + 1` to try find children.
                for ((_maybe_child_height, maybe_child_hash), maybe_child) in self
                    .blocks
                    .range((height + 1, [0; 32])..=(height + 1, [0xff; 32]))
                {
                    debug_assert_eq!(*_maybe_child_height, height + 1);
                    if maybe_child.parent_hash.as_ref() == Some(&hash) {
                        children.push((height + 1, *maybe_child_hash));
                    }
                }
            }

            blocks = children;
        }
    }

    /// Returns the list of blocks whose parent hash is known but the parent itself is absent from
    /// the list of disjoint blocks. These blocks can potentially be verified.
    pub fn good_tree_roots(&'_ self) -> impl Iterator<Item = TreeRoot> + '_ {
        self.blocks
            .iter()
            .filter(|(_, block)| !block.bad)
            .filter_map(move |((height, hash), block)| {
                let parent_hash = block.parent_hash.as_ref()?;

                // Return `None` if parent is in the list of blocks.
                if self.blocks.contains_key(&(*height - 1, *parent_hash)) {
                    return None;
                }

                Some(TreeRoot {
                    block_hash: *hash,
                    block_number: *height,
                    parent_block_hash: *parent_hash,
                })
            })
    }

    /// Returns an iterator yielding blocks that are known to exist but which either haven't been
    /// inserted, or whose parent hash isn't known.
    ///
    /// More precisely, the iterator returns:
    ///
    /// - Blocks that have been inserted in this data structure but whose parent hash is unknown.
    /// - Parents of blocks that have been inserted in this data structure and whose parent hash
    /// is known and whose parent is missing from the data structure.
    ///
    /// > **Note**: Blocks in the second category might include blocks that are already known by
    /// >           the user of this data structure. To avoid this, you are encouraged to remove
    /// >           from the [`DisjointBlocks`] any block that can be verified prior to calling
    /// >           this method.
    ///
    /// The blocks yielded by the iterator are always ordered by ascending height.
    pub fn unknown_blocks(&'_ self) -> impl Iterator<Item = (u64, &'_ [u8; 32])> + '_ {
        // Blocks whose parent hash isn't known.
        let mut iter1 = self
            .blocks
            .iter()
            .filter(|(_, s)| !s.bad)
            .filter(|(_, s)| s.parent_hash.is_none())
            .map(|((n, h), _)| (*n, h))
            .peekable();

        // Blocks whose hash is referenced as the parent of a block, but are missing from the
        // collection.
        let mut iter2 = self
            .blocks
            .iter()
            .filter(|(_, s)| !s.bad)
            .filter_map(|((n, _), s)| s.parent_hash.as_ref().map(|h| (n - 1, h)))
            .filter(move |(n, h)| !self.blocks.contains_key(&(*n, **h)))
            .peekable();

        // A custom combinator is used in order to order elements between `iter1` and `iter2`
        // by ascending block height.
        iter::from_fn(move || match (iter1.peek(), iter2.peek()) {
            (Some((b1, _)), Some((b2, _))) if b1 > b2 => iter2.next(),
            (Some(_), Some(_)) => iter1.next(),
            (Some(_), None) => iter1.next(),
            (None, Some(_)) => iter2.next(),
            (None, None) => None,
        })
    }
}

impl<TBl: fmt::Debug> fmt::Debug for DisjointBlocks<TBl> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.blocks, f)
    }
}

/// See [`DisjointBlocks::good_tree_roots`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct TreeRoot {
    /// Hash of the block that is a tree root.
    pub block_hash: [u8; 32],
    /// Height of the block that is a tree root.
    pub block_number: u64,
    /// Hash of the parent of the block.
    pub parent_block_hash: [u8; 32],
}

#[cfg(test)]
mod tests {
    #[test]
    fn insert_doesnt_override_bad() {
        // Calling `insert` with a block that already exists doesn't reset its "bad" state.

        let mut collection = super::DisjointBlocks::new();

        assert!(collection.insert(1, [1; 32], Some([0; 32]), ()).is_none());
        assert_eq!(collection.unknown_blocks().count(), 1);

        collection.set_block_bad(1, &[1; 32]);
        assert_eq!(collection.unknown_blocks().count(), 0);

        assert!(collection.insert(1, [1; 32], Some([0; 32]), ()).is_some());
        assert_eq!(collection.unknown_blocks().count(), 0);
    }

    #[test]
    fn insert_doesnt_override_parent() {
        // Calling `insert` with a block that already exists doesn't reset its parent.

        let mut collection = super::DisjointBlocks::new();

        assert!(collection.insert(1, [1; 32], Some([0; 32]), ()).is_none());
        assert_eq!(
            collection.unknown_blocks().collect::<Vec<_>>(),
            vec![(0, &[0; 32])]
        );

        assert!(collection.insert(1, [1; 32], None, ()).is_some());
        assert_eq!(
            collection.unknown_blocks().collect::<Vec<_>>(),
            vec![(0, &[0; 32])]
        );
    }

    #[test]
    fn set_parent_hash_updates_bad() {
        // Calling `set_parent_hash` where the parent is a known a bad block marks the block as
        // bad as well.

        let mut collection = super::DisjointBlocks::new();

        collection.insert(1, [1; 32], Some([0; 32]), ());
        assert_eq!(collection.unknown_blocks().count(), 1);
        collection.set_block_bad(1, &[1; 32]);
        assert_eq!(collection.unknown_blocks().count(), 0);

        collection.insert(2, [2; 32], None, ());
        assert_eq!(collection.unknown_blocks().count(), 1);

        collection.set_parent_hash(2, &[2; 32], [1; 32]);
        assert_eq!(collection.unknown_blocks().count(), 0);
    }

    #[test]
    fn insert_updates_bad() {
        // Calling `insert` where the parent is a known a bad block marks the block as
        // bad as well.

        let mut collection = super::DisjointBlocks::new();

        collection.insert(1, [1; 32], Some([0; 32]), ());
        collection.set_block_bad(1, &[1; 32]);
        assert_eq!(collection.unknown_blocks().count(), 0);

        collection.insert(2, [2; 32], Some([1; 32]), ());
        assert_eq!(collection.unknown_blocks().count(), 0);

        // Control sample.
        collection.insert(1, [0x80; 32], Some([0; 32]), ());
        assert_eq!(collection.unknown_blocks().count(), 1);
    }
}
