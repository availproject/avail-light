//! Storage of abstract data, accessible from the runtime.

use alloc::sync::Arc;
use fnv::FnvBuildHasher;
use hashbrown::HashMap;
use primitive_types::H256;

/// Main storage entry point for abstract data.
pub struct Storage {
    /// For each block hash, stores its state.
    blocks: HashMap<H256, BlockState, FnvBuildHasher>,
}

struct BlockState {
    storage: Arc<BlockStorage>,
}

/// Access to a block within the storage.
pub struct Block<'a> {
    /// Entry in the [`Storage::blocks`] hashmap.
    entry: hashbrown::hash_map::Entry<'a, H256, BlockState, FnvBuildHasher>,
}

/// Storage for an individual block.
#[derive(Debug, Clone)]
pub struct BlockStorage {
    top_trie: HashMap<Vec<u8>, Vec<u8>, FnvBuildHasher>,
    children:  HashMap<Vec<u8>, Child>,
}

#[derive(Debug, Clone)]
struct Child {
    trie: HashMap<Vec<u8>, Vec<u8>, FnvBuildHasher>,
}

impl Storage {
    /// Creates a new empty storage.
    pub fn empty() -> Self {
        Storage {
            blocks: HashMap::default(),
        }
    }

    pub fn block_access(&mut self, hash: &H256) -> Block {
        Block {
            entry: self.blocks.entry(hash.clone())
        }
    }
}

impl<'a> Block<'a> {
    pub fn get_storage(&self) -> Option<Arc<BlockStorage>> {
        if let hashbrown::hash_map::Entry::Occupied(e) = &self.entry {
            Some(e.get().storage.clone())
        } else {
            None
        }
    }

    /*pub fn insert(self, state: BlockState) {
        let _was_in = self.storage.blocks.insert(self.hash.clone(), Arc::new(state));
        debug_assert!(_was_in.is_none());
    }*/
}
