//! Filesystem-backed database containing all the information about a chain.
//!
//! This module handles the persistent storage of the chain on disk.

#![cfg(feature = "database")]
#![cfg_attr(docsrs, doc(cfg(feature = "database")))]

use crate::header;

use blake2::digest::{Input as _, VariableOutput as _};
use core::{convert::TryFrom as _, fmt, iter, ops};
use parity_scale_codec::DecodeAll as _;
use sled::Transactional as _;

pub use open::{open, Config, DatabaseOpen};

mod open;

/// An open database. Holds file descriptors.
pub struct Database {
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

    /// Tree named "block_bodies" in the database.
    ///
    /// Keys are block hashes, and values are SCALE-encoded `Vec`s containing the extrinsics. Each
    /// extrinsic is itself a SCALE-encoded `Vec<u8>`.
    block_bodies_tree: sled::Tree,

    /// Tree named "storage_top_trie" in the database.
    ///
    /// Keys are storage keys, and values are storage values.
    storage_top_trie_tree: sled::Tree,

    /// For each BABE epoch number encoded, the list of block hashes containing the information
    /// about that epoch.
    ///
    /// Keys in that tree are 64-bits-big-endian epoch numbers, and values are a concatenation of
    /// 32-bytes block hashes (without any encoding). In other words, the length of values is
    /// always a multiple of 32.
    babe_epochs_tree: sled::Tree,
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
        let decoded = header::decode(header.as_ref())
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

    /// Returns the list of extrinsics of the given block, or `None` if the block is unknown.
    pub fn block_extrinsics(
        &self,
        block_hash: &[u8; 32],
    ) -> Result<Option<impl ExactSizeIterator<Item = Vec<u8>>>, AccessError> {
        let body = match self.block_bodies_tree.get(block_hash)? {
            Some(b) => b,
            None => return Ok(None),
        };

        let decoded = <Vec<Vec<u8>> as parity_scale_codec::DecodeAll>::decode_all(body.as_ref())
            .map_err(|err| AccessError::Corrupted(CorruptedError::BlockBodyCorrupted(err)))?;
        Ok(Some(decoded.into_iter()))
    }

    /// Returns the hash of the block given its number.
    // TODO: comment about reorgs
    pub fn block_hash_by_number(&self, block_number: u64) -> Result<Option<[u8; 32]>, AccessError> {
        let hash = self
            .block_hashes_by_number_tree
            .get(&u64::to_be_bytes(block_number)[..])?;
        let hash = match hash {
            Some(h) => h,
            None => return Ok(None),
        };

        if hash.len() != 32 {
            return Err(AccessError::Corrupted(
                CorruptedError::BlockHashLenInHashNumberMapping,
            ));
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
        contains_babe_epoch_information: Option<u64>,
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
            let header = header::decode(&new_best_scale_header).unwrap();
            header.number
        };

        let encoded_body = {
            // TODO: optimize by not building an intermediary `Vec`
            let body = new_best_body.collect::<Vec<_>>();
            parity_scale_codec::Encode::encode(&body)
        };

        // Try to apply changes. This is done atomically through a transaction.
        let result = (
            &self.block_hashes_by_number_tree,
            &self.block_headers_tree,
            &self.block_bodies_tree,
            &self.storage_top_trie_tree,
            &self.meta_tree,
            &self.babe_epochs_tree,
        )
            .transaction(
                move |(
                    block_hashes_by_number,
                    block_headers,
                    block_bodies,
                    storage_top_trie,
                    meta,
                    babe_epochs,
                )| {
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

                    block_hashes_by_number
                        .insert(&u64::to_be_bytes(new_block_number)[..], &new_best_hash[..])?;

                    block_headers.insert(&new_best_hash[..], new_best_scale_header)?;
                    block_bodies.insert(&new_best_hash[..], &encoded_body[..])?;

                    if let Some(epoch_number) = contains_babe_epoch_information {
                        let epoch_number = u64::to_be_bytes(epoch_number);
                        if let Some(existing) = babe_epochs.get(&epoch_number)? {
                            let mut new_value = existing.as_ref().to_vec();
                            new_value.extend_from_slice(&new_best_hash[..]);
                            babe_epochs.insert(&epoch_number, new_value)?;
                        } else {
                            babe_epochs.insert(&epoch_number, &new_best_hash[..])?;
                        }
                    }

                    meta.insert(b"best", &new_best_hash[..])?;
                    Ok(())
                },
            );

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

    pub fn babe_epoch_information_block_hashes(
        &self,
        epoch_number: u64,
    ) -> Result<BabeEpochInformationBlockHashes, AccessError> {
        let value = self
            .babe_epochs_tree
            .get(&u64::to_be_bytes(epoch_number)[..])?;
        if let Some(value) = &value {
            if value.is_empty() || value.len() % 32 != 0 {
                return Err(AccessError::Corrupted(
                    CorruptedError::BabeEpochInformationWrongLength,
                ));
            }
        }
        Ok(BabeEpochInformationBlockHashes(value))
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

/// List of block hashes that contain an epoch information.
pub struct BabeEpochInformationBlockHashes(Option<sled::IVec>);

impl BabeEpochInformationBlockHashes {
    /// Returns the list of block hashes.
    pub fn iter<'a>(&'a self) -> impl ExactSizeIterator<Item = &[u8; 32]> + 'a {
        if let Some(value) = &self.0 {
            either::Either::Left(value.chunks(32).map(|s| <&[u8; 32]>::try_from(s).unwrap()))
        } else {
            either::Either::Right(iter::empty())
        }
    }
}

impl fmt::Debug for BabeEpochInformationBlockHashes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: map hashes with Hex encoding
        f.debug_list().entries(self.iter()).finish()
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
    BestBlockHeaderCorrupted(header::Error),
    BlockHashLenInHashNumberMapping,
    BlockBodyCorrupted(parity_scale_codec::Error),
    BabeEpochInformationWrongLength,
}
