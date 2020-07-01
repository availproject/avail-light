//! Filesystem-backed database containing all the information about a chain.
//!
//! This module handles the persistent storage of the chain on disk.

// TODO: gate this module in no_std contexts

use blake2::digest::{Input as _, VariableOutput as _};
use core::ops;
use parity_scale_codec::DecodeAll as _;
use sled::Transactional as _;

pub use open::{open, Config, DatabaseOpen};

mod open;

/// An open database. Holds file descriptors.
pub struct Database {
    database: sled::Db,

    /// Tree named "meta" in the database.
    /// Contains all the meta-information about the content.
    ///
    /// Keys in that tree are:
    ///
    /// - `best`: Hash of the best block.
    ///
    meta_tree: sled::Tree,

    /// Tree named "block_hashes_by_number" in the database.
    /// Contains all the meta-information about the content.
    ///
    /// Keys in that tree are 64-bits-big-endian block numbers, and values are 32-bytes block
    /// hashes (without any encoding).
    block_hashes_by_number_tree: sled::Tree,

    /// Tree named "block_headers" in the database.
    ///
    /// Keys are block hashes, and values are SCALE-encoded block headers.
    block_headers_tree: sled::Tree,

    /// Tree named "storage_top_trie" in the database.
    ///
    /// Keys are storage keys, and values are storage values.
    storage_top_trie_tree: sled::Tree,
}

impl Database {
    /// Returns the hash of the block in the database whose storage is currently accessible.
    pub fn best_block_hash(&self) -> Result<[u8; 32], AccessError> {
        match self.meta_tree.get(b"best")? {
            Some(val) => {
                if val.len() == 32 {
                    let mut out = [0; 32];
                    out.copy_from_slice(&val);
                    Ok(out)
                } else {
                    Err(AccessError::Corrupted(
                        CorruptedError::BestBlockHashBadLength,
                    ))
                }
            }
            None => Err(AccessError::Corrupted(
                CorruptedError::BestBlockHashNotFound,
            )),
        }
    }

    /// Returns the number of the block in the database whose storage is currently accessible.
    pub fn best_block_number(&self) -> Result<u64, AccessError> {
        let header = self
            .block_scale_encoded_header(&self.best_block_hash()?)?
            .ok_or(AccessError::Corrupted(
                CorruptedError::BestBlockHeaderNotInDatabase,
            ))?;
        // TODO: ideally we wouldn't use this `crate::block` module
        let decoded = crate::block::Header::decode_all(header.as_ref())
            .map_err(|err| AccessError::Corrupted(CorruptedError::BestBlockHeaderCorrupted(err)))?;
        Ok(decoded.number)
    }

    /// Returns the SCALE-encoded header of the given block, or `None` if the block is unknown.
    pub fn block_scale_encoded_header(
        &self,
        block_hash: &[u8; 32],
    ) -> Result<Option<VarLenBytes>, AccessError> {
        Ok(self.block_headers_tree.get(block_hash)?.map(VarLenBytes))
    }

    /// Returns the hash of the block given its number.
    // TODO: comment about reorgs
    pub fn block_hash_by_number(
        &self,
        block_number: u64,
    ) -> Result<Option<[u8; 32]>, AccessError> {
        let hash = self.block_hashes_by_number_tree.get(&u64::to_be_bytes(block_number)[..])?;
        let hash = match hash {
            Some(h) => h,
            None => return Ok(None),
        };

        if hash.len() != 32 {
            return Err(AccessError::Corrupted(CorruptedError::BlockHashLenInHashNumberMapping));
        }

        let mut out = [0u8; 32];
        out.copy_from_slice(&hash);
        Ok(Some(out))
    }

    /// Insert a new best block in the database.
    ///
    /// `current_best` will be atomically compared with the current best block header. If there
    /// is a mismatch, the new block will not be inserted and an
    /// [`InsertNewBestError::ObsoleteCurrentHead`] error will be returned.
    pub fn insert_new_best(
        &self,
        current_best: [u8; 32],
        new_best_scale_header: &[u8],
        // TODO: pass body by reference
        new_best_body: impl Iterator<Item = Vec<u8>>,
        // TODO: pass trie by references
        storage_top_trie_changes: impl Iterator<Item = (Vec<u8>, Option<Vec<u8>>)> + Clone,
    ) -> Result<(), InsertNewBestError> {
        // Calculate the hash of the new best block.
        let new_best_hash = {
            let mut out = [0; 32];
            let mut hasher = blake2::VarBlake2b::new_keyed(&[], 32);
            hasher.input(new_best_scale_header);
            hasher.variable_result(|result| {
                debug_assert_eq!(result.len(), 32);
                out.copy_from_slice(result)
            });
            out
        };

        let new_block_number = {
            // TODO: weird way to get the block number
            let header = crate::block::Header::decode_all(&new_best_scale_header).unwrap();
            header.number
        };

        // Try to apply changes. This is done atomically through a transaction.
        let result = (
            &self.block_hashes_by_number_tree,
            &self.block_headers_tree,
            &self.storage_top_trie_tree,
            &self.meta_tree,
        )
            .transaction(move |(block_hashes_by_number, block_headers, storage_top_trie, meta)| {
                if meta.get(b"best")?.map_or(true, |v| v != &current_best[..]) {
                    return Err(sled::ConflictableTransactionError::Abort(()));
                }

                for (key, value) in storage_top_trie_changes.clone() {
                    if let Some(value) = value {
                        storage_top_trie.insert(key, value)?;
                    } else {
                        storage_top_trie.remove(key)?;
                    }
                }

                // TODO: insert body

                block_hashes_by_number.insert(&u64::to_be_bytes(new_block_number)[..], &new_best_hash[..])?;

                block_headers.insert(&new_best_hash[..], new_best_scale_header)?;
                meta.insert(b"best", &new_best_hash[..])?;
                Ok(())
            });

        match result {
            Ok(()) => Ok(()),
            // Note that `sled` at the moment doesn't let you pass an error to `Abort` when
            // applying a transaction to multiple trees at once. Since we only have one possible
            // cause for errors anyway, we treat `()` as `ObsoleteCurrentHead`.
            Err(sled::TransactionError::Abort(())) => Err(InsertNewBestError::ObsoleteCurrentHead),
            Err(sled::TransactionError::Storage(err)) => {
                Err(InsertNewBestError::Access(AccessError::Database(err)))
            }
        }
    }

    pub fn storage_top_trie_keys(&self, block: [u8; 32]) -> Result<Vec<VarLenBytes>, AccessError> {
        // TODO: is this atomic? probably not :-/
        // TODO: block isn't checked
        Ok(self
            .storage_top_trie_tree
            .iter()
            .keys()
            .map(|v| v.map(VarLenBytes))
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn storage_top_trie_get(
        &self,
        block: [u8; 32],
        key: &[u8],
    ) -> Result<Option<VarLenBytes>, AccessError> {
        // TODO: block isn't checked
        Ok(self.storage_top_trie_tree.get(key)?.map(VarLenBytes))
    }
}

/// Bytes in the database.
// Note: serves to hide the `sled::IVec` type.
pub struct VarLenBytes(sled::IVec);

impl ops::Deref for VarLenBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

/// Error while accessing some information.
#[derive(Debug, derive_more::Display, derive_more::From)]
pub enum AccessError {
    /// Couldn't access the database.
    #[display(fmt = "Couldn't access the database: {}", _0)]
    Database(sled::Error),

    /// Database could be accessed, but its content is invalid.
    ///
    /// While these corruption errors are probably unrecoverable, the inner error might however
    /// be useful for debugging purposes.
    Corrupted(CorruptedError),
}

/// Error while isnerting new best block.
#[derive(Debug, derive_more::Display, derive_more::From)]
pub enum InsertNewBestError {
    ObsoleteCurrentHead,
    Access(AccessError),
}

/// Error in the content of the database.
#[derive(Debug, derive_more::Display)]
pub enum CorruptedError {
    BestBlockHashNotFound,
    BestBlockHashBadLength,
    BestBlockHeaderNotInDatabase,
    BestBlockHeaderCorrupted(parity_scale_codec::Error),
    BlockHashLenInHashNumberMapping,
}
