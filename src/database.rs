//! Filesystem-backed database containing all the information about a chain.

use std::path::Path;

/// An open database. Holds file descriptors.
pub struct Database {
    inner: parity_db::Db,
}

/// Columns indices in the key-value store.
///
/// > **Note**: Don't get fooled by the word "column". Each so-called column is similar to what
/// >           one would call a "table" in a traditional database system. Each "column" is an
/// >           individual key-value store.
// TODO: document all these
mod columns {
    /// Holds a predefined list of keys. See the `meta_keys` module below.
    pub const META: u8 = 0;
    /// Storage of the best block. The keys and values are the same as for the storage itself.
    pub const STATE: u8 = 1;
    pub const STATE_META: u8 = 2;
    /// Maps block numbers or block hashes to keys used to index into other columns.
    /// Each block in the database has an entry that maps its hash to its key (for indexing in
    /// other columns). Blocks that are part of the canonical chain (usually defined as the longest
    /// chain) also have an additional entry that maps their number (encoded as big endian) to
    /// this same key.
    pub const KEY_LOOKUP: u8 = 3;
    /// Each block in database has a record. Values are SCALE-encoded block headers.
    /// Look into [`KEY_LOOKUP`] to find the key corresponding to a certain block.
    pub const HEADER: u8 = 4;
    /// Each block in database has a record. Values are SCALE-encoded block bodies. A block body
    /// is a `Vec<Vec<u8>>` where each inner `Vec` is an opaque extrinsic.
    /// Look into [`KEY_LOOKUP`] to find the key corresponding to a certain block.
    pub const BODY: u8 = 5;
    /// Each finalized block in database has a record. Values are an opaque proof of the finality
    /// of the block.
    /// Look into [`KEY_LOOKUP`] to find the key corresponding to a certain block.
    // TODO: what is the exact format of a justification?
    pub const JUSTIFICATION: u8 = 6;
    pub const CHANGES_TRIE: u8 = 7;
    pub const AUX: u8 = 8;
    pub const OFFCHAIN: u8 = 9;
    pub const CACHE: u8 = 10;
}

/// Keys found in the `META` column.
mod meta_keys {
    /// Type of storage ("full" or "light").
    pub const TYPE: &[u8; 4] = b"type";
    /// Key (for indexing other columns) of the latest best block.
    pub const BEST_BLOCK: &[u8; 4] = b"best";
    /// Key (for indexing other columns) of the latest finalized block.
    pub const FINALIZED_BLOCK: &[u8; 5] = b"final";
    /// Meta information prefix for list-based caches.
    // TODO: explain better ^
    pub const CACHE_META_PREFIX: &[u8; 5] = b"cache";
    /// Meta information for changes tries key.
    // TODO: explain better ^
    pub const CHANGES_TRIES_META: &[u8; 5] = b"ctrie";
    /// 32 bytes of the hash of the genesis block. Note that the genesis block can be found in the
    /// database as well.
    pub const GENESIS_HASH: &[u8; 3] = b"gen";
    /// A SCALE-encoded `Vec<(u32, Vec<[u8; 32]>)>`. Contains a list of tuples of block number and
    /// list of block hashes with that number. All the blocks in this list are leaves, in other
    /// words they have no children. Only includes blocks who descend from the latest finalized
    /// block.
    // TODO: not actually sure of the exact data type
    pub const LEAF_PREFIX: &[u8; 4] = b"leaf";
    /// Children prefix list key.
    // TODO: explain better ^
    pub const CHILDREN_PREFIX: &[u8; 8] = b"children";
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Self {
        // TODO:
        let inner = {
            let mut options = parity_db::Options::with_columns(path.as_ref(), 11);
            options.sync = true;
            let mut column_options = &mut options.columns[usize::from(columns::STATE)];
            column_options.ref_counted = true;
            column_options.preimage = true;
            column_options.uniform = true;
            parity_db::Db::open(&options).unwrap()
        };

        Database { inner }
    }

    /// Returns the hash of the genesis block.
    pub fn genesis_block_hash(&self) -> Result<[u8; 32], AccessError> {
        let genesis_hash = self
            .inner
            .get(columns::META, meta_keys::GENESIS_HASH)?
            .ok_or(AccessError::Corrupted)?;
        if genesis_hash.len() != 32 {
            return Err(AccessError::Corrupted);
        }
        let mut out = [0; 32];
        out.copy_from_slice(&genesis_hash);
        Ok(out)
    }

    /// Gives access to a block, given its hash. Return `None` if the block isn't in the database.
    pub fn block_by_hash(&self, hash: &[u8; 32]) -> Result<Option<BlockAccess>, AccessError> {
        let lookup_key = match self.inner.get(columns::KEY_LOOKUP, hash)? {
            Some(k) => k,
            None => return Ok(None),
        };

        Ok(Some(BlockAccess {
            database: self,
            lookup_key,
        }))
    }

    /// Gives access to a block, given its number. Return `None` if the block isn't in the database.
    pub fn block_by_number(&self, number: u32) -> Result<Option<BlockAccess>, AccessError> {
        let lookup_key = match self.inner.get(columns::KEY_LOOKUP, &number.to_be_bytes())? {
            Some(k) => k,
            None => return Ok(None),
        };

        Ok(Some(BlockAccess {
            database: self,
            lookup_key,
        }))
    }

    /// Gives access to the current best block in the database.
    pub fn best_block(&self) -> Result<BlockAccess, AccessError> {
        let lookup_key = self
            .inner
            .get(columns::META, meta_keys::BEST_BLOCK)?
            .ok_or(AccessError::Corrupted)?;
        Ok(BlockAccess {
            database: self,
            lookup_key,
        })
    }

    /// Gives access to the latest finalized best block in the database.
    pub fn finalized_block(&self) -> Result<BlockAccess, AccessError> {
        let lookup_key = self
            .inner
            .get(columns::META, meta_keys::FINALIZED_BLOCK)?
            .ok_or(AccessError::Corrupted)?;
        Ok(BlockAccess {
            database: self,
            lookup_key,
        })
    }
}

/// Access to the information about a block in the database.
///
/// This struct refers to a certain block with a specific hash. If for example this struct is
/// obtained by calling [`Database::best_block`], then it will keep referring to the same block
/// even if the best block is modified.
///
/// It is guaranteed that blocks don't get removed from the database as long as a [`BlockAccess`]
/// referring to it is alive.
// TODO: pin the block while this struct is alive so it doesn't get destroyed
pub struct BlockAccess<'a> {
    /// Parent structure.
    database: &'a Database,
    /// Opaque lookup key for this block.
    /// While this key is usually very short, there is no point in inlining as the underlying
    /// database returns it as a `Vec<u8>` anyway.
    lookup_key: Vec<u8>,
}

impl<'a> BlockAccess<'a> {
    /// Returns the header of the block.
    // TODO: this method is debatable considering modules isolation
    pub fn header(&self) -> Result<crate::block::Header, AccessError> {
        let header_encoded = self.scale_encoded_header()?;
        let decoded = parity_scale_codec::DecodeAll::decode_all(&header_encoded)
            .map_err(|_| AccessError::Corrupted)?;
        Ok(decoded)
    }

    /// Returns the header of the block, in SCALE-encoding.
    ///
    /// The returned data is properly encoded, provided the database is not corrupted.
    pub fn scale_encoded_header(&self) -> Result<Vec<u8>, AccessError> {
        let header_encoded = self
            .database
            .inner
            .get(columns::HEADER, &self.lookup_key)?
            .ok_or(AccessError::Corrupted)?;
        Ok(header_encoded)
    }

    /// Returns the body of the block.
    ///
    /// On success, the returned `Vec` is a list of opaque extrinsics.
    pub fn body(&self) -> Result<Vec<Vec<u8>>, AccessError> {
        let encoded_body = self
            .database
            .inner
            .get(columns::BODY, &self.lookup_key)?
            .ok_or(AccessError::Corrupted)?;
        let decoded = parity_scale_codec::DecodeAll::decode_all(&encoded_body)
            .map_err(|_| AccessError::Corrupted)?;
        Ok(decoded)
    }
}

/// Error while accessing the database.
#[derive(Debug, derive_more::Display, derive_more::From)]
pub enum AccessError {
    /// Error in the underlying database system.
    #[display(fmt = "Error in the underlying database system: {}", _0)]
    // TODO: hide the underlying type?
    Database(parity_db::Error),
    /// Database content could be read but doesn't match the expected format.
    Corrupted,
}
