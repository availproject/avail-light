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

//! Collection of sources used for the `all_forks` syncing.
//!
//! Each source stored in the [`AllForksSources`] is associated to:
//!
//! - A [`SourceId`].
//! - A best block.
//! - A list of non-finalized blocks known by this source.
//! - An opaque user data, of type `TSrc`.
//!

use alloc::{collections::BTreeSet, vec::Vec};
use core::fmt;

/// Identifier for a source in the [`AllForksSources`].
//
// Implementation note: the `u64` values are never re-used, making it possible to avoid clearing
// obsolete SourceIds in the `AllForksSources` state machine.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct SourceId(u64);

/// Collection of sources and which blocks they know about.
pub struct AllForksSources<TSrc> {
    /// Actual list of sources.
    sources: hashbrown::HashMap<SourceId, Source<TSrc>, fnv::FnvBuildHasher>,

    /// Identifier to allocate to the next source. Identifiers are never reused, which allows
    /// keeping obsolete identifiers in the internal state.
    next_source_id: SourceId,

    /// Stores `(source, block hash)` tuples. Each tuple is an information about the fact that
    /// this source knows about the given block. Only contains blocks whose height is strictly
    /// superior to [`AllForksSources::finalized_block_height`].
    known_blocks1: BTreeSet<(SourceId, u64, [u8; 32])>,

    /// Contains the same entries as [`AllForksSources::known_blocks1`], but in reverse.
    known_blocks2: BTreeSet<(u64, [u8; 32], SourceId)>,

    /// Height of the finalized block. All sources whose best block number is superior to this
    /// value is expected to know the entire finalized chain.
    finalized_block_height: u64,
}

impl<TSrc> AllForksSources<TSrc> {
    /// Creates a new container. Must be passed the height of the known finalized block.
    pub fn new(sources_capacity: usize, finalized_block_height: u64) -> Self {
        AllForksSources {
            sources: hashbrown::HashMap::with_capacity_and_hasher(
                sources_capacity,
                Default::default(),
            ),
            next_source_id: SourceId(0),
            known_blocks1: Default::default(),
            known_blocks2: Default::default(),
            finalized_block_height,
        }
    }

    /// Returns the number of sources in the data structure.
    pub fn num_sources(&self) -> usize {
        self.sources.len()
    }

    /// Returns the number of unique blocks in the data structure.
    pub fn num_blocks(&self) -> usize {
        // TODO: optimize; shouldn't be O(n)
        self.known_blocks2
            .iter()
            .fold((0, None), |(uniques, prev), next| match (prev, next) {
                (Some((pn, ph)), (nn, nh, _)) if pn == *nn && ph == *nh => {
                    (uniques, Some((pn, ph)))
                }
                (_, (nn, nh, _)) => (uniques + 1, Some((*nn, *nh))),
            })
            .0
    }

    /// Returns true if the data structure is empty.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Add a new source to the container.
    ///
    /// The `user_data` parameter is opaque and decided entirely by the user. It can later be
    /// retrieved using [`SourceMutAccess::user_data`].
    ///
    /// Returns the newly-created source entry.
    pub fn add_source(
        &mut self,
        user_data: TSrc,
        best_block_number: u64,
        best_block_hash: [u8; 32],
    ) -> SourceMutAccess<TSrc> {
        let new_id = {
            let id = self.next_source_id;
            self.next_source_id.0 += 1;
            id
        };

        self.sources.insert(
            new_id,
            Source {
                best_block_number,
                best_block_hash,
                user_data,
            },
        );

        if best_block_number > self.finalized_block_height {
            self.known_blocks1
                .insert((new_id, best_block_number, best_block_hash));
            self.known_blocks2
                .insert((best_block_number, best_block_hash, new_id));
        }

        SourceMutAccess {
            parent: self,
            source_id: new_id,
        }
    }

    /// Removes a block from the list of blocks the sources are aware of.
    ///
    /// > **Note**: Use this method to prevent the data structure from growing undefinitely.
    pub fn remove_known_block(&mut self, height: u64, hash: [u8; 32]) {
        let sources = self
            .known_blocks2
            .range(
                (height, hash, SourceId(u64::min_value()))
                    ..=(height, hash, SourceId(u64::max_value())),
            )
            .map(|(_, _, source)| *source)
            .collect::<Vec<_>>();

        for source_id in sources {
            self.known_blocks2.remove(&(height, hash, source_id));
            let _was_in = self.known_blocks1.remove(&(source_id, height, hash));
            debug_assert!(_was_in);
        }
    }

    /// Updates the height of the finalized block.
    ///
    /// This removes from the collection all blocks whose height is inferior or equal to this
    /// value.
    ///
    /// # Panic
    ///
    /// Panics if the new height is inferior to the previous value.
    ///
    pub fn set_finalized_block_height(&mut self, height: u64) {
        assert!(height >= self.finalized_block_height);

        debug_assert_eq!(
            self.known_blocks2
                .range(
                    (0, [0; 32], SourceId(u64::min_value()))
                        ..=(
                            self.finalized_block_height,
                            [0xff; 32],
                            SourceId(u64::max_value())
                        ),
                )
                .count(),
            0
        );

        let entries = self
            .known_blocks2
            .range(
                (0, [0; 32], SourceId(u64::min_value()))
                    ..=(height, [0xff; 32], SourceId(u64::max_value())),
            )
            .cloned()
            .collect::<Vec<_>>();

        for (height, hash, source_id) in entries {
            self.known_blocks2.remove(&(height, hash, source_id));
            let _was_in = self.known_blocks1.remove(&(source_id, height, hash));
            debug_assert!(_was_in);
        }

        self.finalized_block_height = height;
    }

    /// Grants access to a source, using its identifier.
    pub fn source_mut(&mut self, id: SourceId) -> Option<SourceMutAccess<TSrc>> {
        if self.sources.contains_key(&id) {
            Some(SourceMutAccess {
                parent: self,
                source_id: id,
            })
        } else {
            None
        }
    }
}

impl<TSrc: fmt::Debug> fmt::Debug for AllForksSources<TSrc> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("AllForksSources")
            .field("sources", &self.sources)
            .field("finalized_block_height", &self.finalized_block_height)
            .finish()
    }
}

/// Extra fields specific to each blocks source.
///
/// `best_block_number`/`best_block_hash` must be present in the known blocks.
#[derive(Debug)]
struct Source<TSrc> {
    best_block_number: u64,
    best_block_hash: [u8; 32],
    user_data: TSrc,
}

/// Access to a source in a [`AllForksSources`]. Obtained through [`AllForksSources::source_mut`].
pub struct SourceMutAccess<'a, TSrc> {
    parent: &'a mut AllForksSources<TSrc>,

    /// Guaranteed to be a valid entry in [`AllForksSources::sources`].
    source_id: SourceId,
}

impl<'a, TSrc> SourceMutAccess<'a, TSrc> {
    /// Returns the identifier of this source.
    pub fn id(&self) -> SourceId {
        self.source_id
    }

    /// Registers a new block that the source is aware of.
    ///
    /// Has no effect if `height` is inferior or equal to the finalized block height.
    pub fn add_known_block(&mut self, height: u64, hash: [u8; 32]) {
        if height > self.parent.finalized_block_height {
            self.parent
                .known_blocks1
                .insert((self.source_id, height, hash));
            self.parent
                .known_blocks2
                .insert((height, hash, self.source_id));
        }
    }

    /// Removes a block from the list of blocks the source is aware of.
    ///
    /// Has no effect if the source didn't know this block.
    ///
    /// > **Note**: Can be used when a request is sent to a node, and the node answers that it
    /// >           doesn't know about the requested block contrary to previously believed.
    pub fn remove_known_block(&mut self, height: u64, hash: [u8; 32]) {
        let _was_in1 = self
            .parent
            .known_blocks1
            .remove(&(self.source_id, height, hash));
        let _was_in2 = self
            .parent
            .known_blocks2
            .remove(&(height, hash, self.source_id));
        debug_assert_eq!(_was_in1, _was_in2);
    }

    /// Sets the best block of this source.
    pub fn set_best_block(&mut self, height: u64, hash: [u8; 32]) {
        self.add_known_block(height, hash);

        let source = self.parent.sources.get_mut(&self.source_id).unwrap();
        source.best_block_number = height;
        source.best_block_hash = hash;
    }

    /// Returns true if [`SourceMutAccess::add_known_block`] or [`SourceMutAccess::set_best_block`]
    /// has earlier been called on this source with this height and hash, or if the source was
    /// originally created (using [`AllForksSources::add_source`]) with this height and hash.
    ///
    /// # Panic
    ///
    /// Panics if `height` is inferior or equal to the finalized block height. Finalized blocks
    /// are intentionally not tracked by this data structure, and panicking prevents confusing
    /// situations.
    ///
    pub fn knows_block(&self, height: u64, hash: &[u8; 32]) -> bool {
        assert!(height > self.parent.finalized_block_height);
        self.parent
            .known_blocks1
            .contains(&(self.source_id, height, *hash))
    }

    /// Removes the source from the [`AllForksSources`].
    ///
    /// Returns the user data that was originally passed to [`AllForksSources::add_source`].
    pub fn remove(self) -> TSrc {
        let source = self.parent.sources.remove(&self.source_id).unwrap();

        // Purge `known_blocks1` and `known_blocks2`.
        let known_blocks = self
            .parent
            .known_blocks1
            .range((self.source_id, 0, [0; 32])..=(self.source_id, u64::max_value(), [0xff; 32]))
            .map(|(_, n, h)| (*n, *h))
            .collect::<Vec<_>>();
        for (height, hash) in known_blocks {
            let _was_in1 = self
                .parent
                .known_blocks1
                .remove(&(self.source_id, height, hash));
            let _was_in2 = self
                .parent
                .known_blocks2
                .remove(&(height, hash, self.source_id));
            debug_assert!(_was_in1);
            debug_assert!(_was_in2);
        }

        source.user_data
    }

    /// Returns the user data associated to the source. This is the value originally passed
    /// through [`AllForksSources::add_source`].
    pub fn user_data(&mut self) -> &mut TSrc {
        let source = self.parent.sources.get_mut(&self.source_id).unwrap();
        &mut source.user_data
    }

    /// Returns the user data associated to the source. This is the value originally passed
    /// through [`AllForksSources::add_source`].
    pub fn into_user_data(self) -> &'a mut TSrc {
        let source = self.parent.sources.get_mut(&self.source_id).unwrap();
        &mut source.user_data
    }
}

impl<'a, TSrc: fmt::Debug> fmt::Debug for SourceMutAccess<'a, TSrc> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("SourceMutAccess")
            .field(&self.source_id)
            .field(&self.parent.sources.get(&self.source_id).unwrap().user_data)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn basic_works() {
        let mut sources = super::AllForksSources::new(256, 10);
        assert!(sources.is_empty());
        assert_eq!(sources.num_blocks(), 0);

        let source1 = sources.add_source((), 12, [1; 32]).id();
        assert!(!sources.is_empty());
        assert_eq!(sources.num_sources(), 1);
        assert_eq!(sources.num_blocks(), 1);
        assert!(sources
            .source_mut(source1)
            .unwrap()
            .knows_block(12, &[1; 32]));

        sources
            .source_mut(source1)
            .unwrap()
            .set_best_block(13, [2; 32]);
        assert_eq!(sources.num_blocks(), 2);
        assert!(sources
            .source_mut(source1)
            .unwrap()
            .knows_block(12, &[1; 32]));
        assert!(sources
            .source_mut(source1)
            .unwrap()
            .knows_block(13, &[2; 32]));

        sources.remove_known_block(13, [2; 32]);
        assert_eq!(sources.num_blocks(), 1);
        assert!(sources
            .source_mut(source1)
            .unwrap()
            .knows_block(12, &[1; 32]));
        assert!(!sources
            .source_mut(source1)
            .unwrap()
            .knows_block(13, &[2; 32]));

        sources.set_finalized_block_height(12);
        assert_eq!(sources.num_blocks(), 0);

        let () = sources.source_mut(source1).unwrap().remove();
        assert!(sources.is_empty());
        assert_eq!(sources.num_sources(), 0);
    }
}
