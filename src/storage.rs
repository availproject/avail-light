//! Data structure containing all blocks in the chain.

use alloc::sync::Arc;
use fnv::FnvBuildHasher;
use hashbrown::HashMap;
use parity_scale_codec::Decode as _;
use primitive_types::H256;

/// Main storage entry point for abstract data.
pub struct Storage {
    /// Disk storage for the data. `None` if the data shouldn't be saved on disk.
    disk_database: Option<parity_db::Db>,

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

mod columns {
    pub const META: u8 = 0;
    pub const STATE: u8 = 1;
    pub const STATE_META: u8 = 2;
    /// maps hashes to lookup keys and numbers to canon hashes.
    pub const KEY_LOOKUP: u8 = 3;
    pub const HEADER: u8 = 4;
    pub const BODY: u8 = 5;
    pub const JUSTIFICATION: u8 = 6;
    pub const CHANGES_TRIE: u8 = 7;
    pub const AUX: u8 = 8;
    /// Offchain workers local storage
    pub const OFFCHAIN: u8 = 9;
    pub const CACHE: u8 = 10;
}

impl Storage {
    /// Creates a new empty storage.
    pub fn empty() -> Self {
        // TODO:
        let disk_database = {
            let mut options = parity_db::Options::with_columns(
                std::path::Path::new(
                    "/home/pierre/.local/share/substrate/chains/flamingfir7/paritydb",
                ),
                11,
            );
            let mut column_options = &mut options.columns[usize::from(columns::STATE)];
            column_options.ref_counted = true;
            column_options.preimage = true;
            column_options.uniform = true;
            parity_db::Db::open(&options).unwrap()
        };

        let genesis_hash = disk_database.get(columns::META, b"gen").unwrap().unwrap();
        println!("genesis hash = {:?}", genesis_hash);

        let best_block_lookup = disk_database.get(columns::META, b"best").unwrap().unwrap();
        let best_block = disk_database
            .get(columns::HEADER, &best_block_lookup)
            .unwrap()
            .unwrap();
        println!("best block = {:?}", best_block);
        let decoded = crate::block::Header::decode(&mut &best_block[..]).unwrap();
        println!("decoded block = {:?}", decoded);

        Storage {
            disk_database: Some(disk_database),
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
    pub fn set_storage(mut self, block_storage: BlockStorage) -> Result<(), ()> {
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

    /// Returns the value of the `:code` key, containing the Wasm code.
    pub fn code_key<'a>(&'a self) -> Option<impl AsRef<[u8]> + 'a> {
        const CODE: &[u8] = b":code";
        self.top_trie.get(CODE)
    }
}
