//! Caching layer for the storage.

use alloc::{collections::BTreeMap, sync::Arc};
use fnv::FnvBuildHasher;

pub struct StorageCache {
    /// The keys of this map are `(block_hash, storage_key)`, and the values are the storage
    /// values.
    top_trie_entries: BTreeMap<([u8; 32], Vec<u8>), Vec<u8>>,
}

/// Access to a block within the storage cache.
pub struct Block<'a> {
    /// Entry in the [`StorageCache::blocks`] map.
    entry: &'a mut BTreeMap<([u8; 32], Vec<u8>), Vec<u8>>,
}

impl StorageCache {
    /// Creates a new empty storage cache.
    pub fn new() -> Self {
        Storage {
            top_trie_entries: Default::default(),
        }
    }

    /// Returns an object representing an access to the given block in the storage cache.
    ///
    /// Since every single hash can potentially be valid, this function always succeeds whatever
    /// hash you pass and lets you insert a corresponding block.
    pub fn block(&mut self, hash: &[u8; 32]) -> Block {
        Block {
            entry: self.blocks.entry(hash.clone()),
        }
    }
}

impl<'a> Block<'a> {
    pub fn get_storage_or(&mut self, key: &[u8], or_insert: impl FnOnce() -> Vec<u8>) -> &[u8] {
        unimplemented!()  // TODO:
    }
}
