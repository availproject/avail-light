//! Storage of abstract data, accessible from the runtime.

use alloc::sync::Arc;
use fnv::FnvBuildHasher;
use hashbrown::HashMap;
use primitive_types::H256;

/// Main storage entry point for abstract data.
pub struct Storage {
    /// For each block hash, stores its state.
    blocks: HashMap<H256, Arc<BlockState>, FnvBuildHasher>,
}

pub enum Block<'a> {
    Present(Arc<BlockState>),
    Missing(MissingBlock<'a>),
}

pub struct MissingBlock<'a> {
    hash: &'a H256,
    storage: &'a mut Storage,
}

/// Storage for an individual block.
#[derive(Debug, Clone)]
pub struct BlockState {
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

    pub fn block_access<'a>(&'a mut self, hash: &'a H256) -> Block<'a> {
        if let Some(block_access) = self.blocks.get(hash) {
            Block::Present(block_access.clone())
        } else {
            Block::Missing(MissingBlock {
                hash,
                storage: self,
            })
        }
    }
}

impl<'a> MissingBlock<'a> {
    pub fn insert(self, state: BlockState) {
        let _was_in = self.storage.blocks.insert(self.hash.clone(), Arc::new(state));
        debug_assert!(_was_in.is_none());
    }
}
