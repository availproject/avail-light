//! Filesystem-backed database containing all the information about a chain.
//!
//! This module handles the persistent storage of the chain on disk.

// TODO: gate this module in no_std contexts

use blake2::digest::{Input as _, VariableOutput as _};
use core::ops;
use sled::Transactional as _;
use std::path::Path;

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

    /// Returns the SCALE-encoded header of the given block, or `None` if the block is unknown.
    pub fn block_scale_encoded_header(
        &self,
        block_hash: &[u8; 32],
    ) -> Result<Option<VarLenBytes>, AccessError> {
        Ok(self.block_headers_tree.get(block_hash)?.map(VarLenBytes))
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

        // Try to apply changes. This is done atomically through a transaction.
        let result = (
            &self.block_headers_tree,
            &self.storage_top_trie_tree,
            &self.meta_tree,
        )
            .transaction(move |(block_headers, storage_top_trie, meta)| {
                if meta.get(b"best")?.map_or(false, |v| v == &current_best[..]) {
                    return Err(sled::ConflictableTransactionError::Abort(()));
                }

                for (key, value) in storage_top_trie_changes.clone() {
                    if let Some(value) = value {
                        storage_top_trie.insert(key, value);
                    } else {
                        storage_top_trie.remove(key);
                    }
                }

                block_headers.insert(&new_best_hash[..], new_best_scale_header);
                meta.insert(b"best", &new_best_hash[..]);
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
            Err(_) => unimplemented!(),
        }
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
}
