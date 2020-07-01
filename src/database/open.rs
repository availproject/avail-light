//! Database opening code.
//!
//! Contains everything related to the opening and initialization of the database.

use super::{AccessError, Database};

use blake2::digest::{Input as _, VariableOutput as _};
use sled::Transactional as _;
use std::path::Path;

/// Opens the database using the given [`Config`].
///
/// Note that this doesn't return a [`Database`], but rather a [`DatabaseOpen`].
// TODO: hide inner error type?
pub fn open(config: Config) -> Result<DatabaseOpen, sled::Error> {
    let database = sled::Config::default()
        // We put a `/v1/` behind the path in case we change the schema.
        .path(config.path.join("v1"))
        .open()?;

    let meta_tree = database.open_tree(b"meta")?;
    let block_hashes_by_number_tree = database.open_tree(b"block_hashes_by_number")?;
    let block_headers_tree = database.open_tree(b"block_headers")?;
    let storage_top_trie_tree = database.open_tree(b"storage_top_trie")?;

    Ok(if meta_tree.get(b"best")?.is_some() {
        DatabaseOpen::Open(Database {
            database,
            block_hashes_by_number_tree,
            meta_tree,
            block_headers_tree,
            storage_top_trie_tree,
        })
    } else {
        DatabaseOpen::Empty(DatabaseEmpty {
            database,
            block_hashes_by_number_tree,
            meta_tree,
            block_headers_tree,
            storage_top_trie_tree,
        })
    })
}

/// Configuration for the database.
#[derive(Debug)]
pub struct Config<'a> {
    /// Path to the directory containing the database.
    pub path: &'a Path,
}

/// Either existing database or database prototype.
pub enum DatabaseOpen {
    /// A database already existed and has now been opened.
    Open(Database),

    /// Either a database has just been created, or there existed a database but it is empty.
    ///
    /// > **Note**: The situation where a database existed but is empty can happen if you have
    /// >           previously called [`open`] then dropped the [`DatabaseOpen`] object without
    /// >           filling the newly-created database with data.
    Empty(DatabaseEmpty),
}

/// An open database. Holds file descriptors.
pub struct DatabaseEmpty {
    /// Inner database object.
    database: sled::Db,

    /// See the similar field in [`Database`].
    meta_tree: sled::Tree,

    /// See the similar field in [`Database`].
    block_hashes_by_number_tree: sled::Tree,

    /// See the similar field in [`Database`].
    block_headers_tree: sled::Tree,

    /// See the similar field in [`Database`].
    storage_top_trie_tree: sled::Tree,
}

impl DatabaseEmpty {
    /// Inserts the genesis block in the database prototype in order to turn it into an actual
    /// database.
    pub fn insert_genesis_block<'a>(
        self,
        genesis_block_header: &[u8],
        storage_top_trie_entries: impl Iterator<Item = (&'a [u8], &'a [u8])> + Clone,
    ) -> Result<Database, AccessError> {
        // Calculate the hash of the new best block.
        let block_hash = {
            let mut out = [0; 32];
            let mut hasher = blake2::VarBlake2b::new_keyed(&[], 32);
            hasher.input(genesis_block_header);
            hasher.variable_result(|result| {
                debug_assert_eq!(result.len(), 32);
                out.copy_from_slice(result)
            });
            out
        };

        // Try to apply changes. This is done atomically through a transaction.
        let result = (
            &self.block_hashes_by_number_tree,
            &self.block_headers_tree,
            &self.storage_top_trie_tree,
            &self.meta_tree,
        )
            .transaction(move |(block_hashes_by_number, block_headers, storage_top_trie, meta)| {
                for (key, value) in storage_top_trie_entries.clone() {
                    storage_top_trie.insert(key, value)?;
                }

                block_hashes_by_number.insert(&0u64.to_be_bytes()[..], &block_hash[..])?;

                block_headers.insert(&block_hash[..], genesis_block_header)?;
                meta.insert(b"best", &block_hash[..])?;
                Ok(())
            });

        match result {
            Ok(()) => Ok(Database {
                database: self.database,
                block_hashes_by_number_tree: self.block_hashes_by_number_tree,
                meta_tree: self.meta_tree,
                block_headers_tree: self.block_headers_tree,
                storage_top_trie_tree: self.storage_top_trie_tree,
            }),
            Err(sled::TransactionError::Abort(())) => unreachable!(),
            Err(sled::TransactionError::Storage(err)) => Err(AccessError::Database(err)),
            Err(_) => unimplemented!(),
        }
    }
}
