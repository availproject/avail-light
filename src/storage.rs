//! Data structure containing all blocks in the chain.

use alloc::sync::Arc;
use fnv::FnvBuildHasher;
use hashbrown::HashMap;

use primitive_types::H256;

/// Main storage entry point for abstract data.
// TODO: probably useless, remove
pub struct Storage {
    /// For each block hash, stores its state.
    blocks: HashMap<H256, BlockState, FnvBuildHasher>,
}

#[derive(Default)]
struct BlockState {
    storage: Option<Arc<BlockStorage>>,
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
    children: HashMap<Vec<u8>, Child, FnvBuildHasher>,
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

    /// Returns an object representing an access to the given block in the storage.
    ///
    /// Since every single hash can potentially be valid, this function always succeeds whatever
    /// hash you pass and lets you insert a corresponding block.
    pub fn block(&mut self, hash: &H256) -> Block {
        Block {
            entry: self.blocks.entry(hash.clone()),
        }
    }

    // TODO: temporary hack to free some memory
    pub fn remove_storage(&mut self, block: &H256) {
        self.blocks.remove(block);
    }
}

impl<'a> Block<'a> {
    /// Returns an access to the storage of this block, if known.
    pub fn storage(&self) -> Option<Arc<BlockStorage>> {
        if let hashbrown::hash_map::Entry::Occupied(e) = &self.entry {
            e.get().storage.as_ref().map(|s| s.clone())
        } else {
            None
        }
    }

    // TODO: should be &mut self normally
    pub fn set_storage(self, block_storage: BlockStorage) -> Result<(), ()> {
        // TODO: check proper hash of block_storage

        self.entry.or_insert_with(|| BlockState::default()).storage = Some(Arc::new(block_storage));
        Ok(())
    }

    /// Returns an access to the hash of this block, if known.
    pub fn header(&self) -> Option<()> {
        unimplemented!()
    }

    /*pub fn insert(self, state: BlockState) {
        let _was_in = self.storage.blocks.insert(self.hash.clone(), Arc::new(state));
        debug_assert!(_was_in.is_none());
    }*/
}

impl BlockStorage {
    /// Builds a new empty [`BlockStorage`].
    pub fn empty() -> BlockStorage {
        BlockStorage {
            top_trie: HashMap::default(),
            children: HashMap::default(),
        }
    }

    pub fn insert(&mut self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
        self.top_trie
            .insert(key.as_ref().to_owned(), value.as_ref().to_owned());
    }

    pub fn storage_keys<'a>(&'a self) -> impl Iterator<Item = impl AsRef<[u8]> + 'a> + 'a {
        self.top_trie.keys()
    }

    pub fn get<'a>(&'a self, key: &[u8]) -> Option<impl AsRef<[u8]> + 'a> {
        self.top_trie.get(key)
    }

    pub fn remove(&mut self, key: &[u8]) {
        self.top_trie.remove(key);
    }

    /// Returns the key, in lexicographic order, following the one passed as parameter.
    pub fn next_key<'a>(&'a self, key: &[u8]) -> Option<impl AsRef<[u8]> + 'a> {
        // TODO: not optimized
        self.top_trie.keys().filter(|k| &***k > key).min()
    }

    /// Returns the value of the `:code` key, containing the Wasm code.
    pub fn code_key<'a>(&'a self) -> Option<impl AsRef<[u8]> + 'a> {
        const CODE: &[u8] = b":code";
        self.get(CODE)
    }
}
