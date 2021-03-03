// Smoldot
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Filesystem-backed database containing all the information about a chain.
//!
//! This module handles the persistent storage of the chain on disk.
//!
//! # Usage
//!
//! Use the [`open()`] function to create a new database or open an existing one. [`open()`]
//! returns a [`DatabaseOpen`] enum. This enum will contain either a [`SledFullDatabase`] object,
//! representing an access to the database, or a [`DatabaseEmpty`] if the database didn't exist or
//! is empty. If that is the case, use [`DatabaseEmpty::initialize`] in order to populate it and
//! obtain a [`SledFullDatabase`].
//!
//! Use [`SledFullDatabase::insert`] to insert a new block in the database. The block is assumed
//! to have been successfully verified prior to insertion. An error is returned if this block is
//! already in the database or isn't a descendant or ancestor of the latest finalized block.
//!
//! Use [`SledFullDatabase::set_finalized`] to mark a block already in the database as finalized.
//! Any block that isn't an ancestor or descendant will be removed. Reverting finalization is
//! not possible.
//!
//! Due to the database's schema, it is not possible to efficiently retrieve the storage items of
//! blocks that are ancestors of the finalized block. When a block is finalized, the storage of
//! its ancestors is lost, and the only way to reconstruct it is to execute all blocks starting
//! from the genesis to the desired one.
//!
//! # About errors handling
//!
//! Most of the functions and methods in this module return a `Result` containing notably an
//! [`AccessError`]. This kind of errors can happen if the operating system returns an error when
//! accessing the file system, or if the database has been corrupted, for example by the user
//! manually modifying it.
//!
//! There isn't much that can be done to properly handle an [`AccessError`]. The only reasonable
//! solutions are either to stop the program, or to delete the entire database and recreate it.
//!
//! # Schema
//!
//! Each section below corresponds to a "tree" in the sled database.
//!
//! ## meta
//!
//! Contains all the meta-information about the content.
//!
//! Keys in that tree are:
//!
//! - `best`: Hash of the best block.
//!
//! - `finalized`: Height of the finalized block, as a 64bits big endian number.
//!
//! - `grandpa_authorities_set_id`: A 64bits big endian number representing the id of the
//! authorities set that must finalize the block right after the finalized block. The value is
//! 0 at the genesis block, and increased by 1 at every authorities change. Missing if and only
//! if the chain doesn't use Grandpa.
//!
//! - `grandpa_triggered_authorities`: List of public keys and weights of the GrandPa
//! authorities that must finalize the children of the finalized block. Consists in 40bytes
//! values concatenated together, each value being a 32bytes ed25519 public key and a 8bytes
//! little endian weight. Missing if and only if the chain doesn't use Grandpa.
//!
//! - `grandpa_scheduled_target`: A 64bits big endian number representing the block where the
//! authorities found in `grandpa_scheduled_authorities` will be triggered. Blocks whose height
//! is strictly higher than this value must be finalized using the new set of authorities. This
//! authority change must have been scheduled in or before the finalized block. Missing if no
//! change is scheduled or if the chain doesn't use Grandpa.
//!
//! - `grandpa_scheduled_authorities`: List of public keys and weights of the GrandPa
//! authorities that will be triggered at the block found in `grandpa_scheduled_target`.
//! Consists in 40bytes values concatenated together, each value being a 32bytes ed25519
//! public key and a 8bytes little endian weight. Missing if no change is scheduled or if the
//! chain doesn't use Grandpa.
//!
//! - `aura_slot_duration`: A 64bits big endian number indicating the duration of an Aura
//! slot. Missing if and only if the chain doesn't use Aura.
//!
//! - `aura_finalized_authorities`: List of public keys of the Aura authorities that must
//! author the children of the finalized block. Consists in 32bytes values concatenated
//! together. Missing if and only if the chain doesn't use Aura.
//!
//! - `babe_slots_per_epoch`: A 64bits big endian number indicating the number of slots per
//! Babe epoch. Missing if and only if the chain doesn't use Babe.
//!
//! - `babe_finalized_epoch`: SCALE encoding of a structure that contains the information
//! about the Babe epoch used for the finalized block. Missing if and only if the finalized
//! block is block #0 or the chain doesn't use Babe.
//!
//! - `babe_finalized_next_epoch`: SCALE encoding of a structure that contains the information
//! about the Babe epoch that follows the one described by `babe_finalized_epoch`. If the
//! finalized block is block #0, then this contains information about epoch #0. Missing if and
//! only if the chain doesn't use Babe.
//!
//! ## block_hashes_by_number
//!
//! For each possible block number, stores a list of block hashes having that number.
//!
//! If the key is inferior or equal to the value in `finalized`, guaranteed to only contain on
//! block.
//!
//! Keys in that tree are 64-bits-big-endian block numbers, and values are a concatenation of
//! 32-bytes block hashes (without any encoding). If the value is for example 96 bytes long,
//! that means there are 3 blocks in the database with that block number.
//!
//! Never contains any empty value.
//!
//! ## block_headers
//!
//! Contains an entry for every known block that is an ancestor or descendant of the finalized
//! block.
//! When the finalized block is updated, entries that aren't ancestors or descendants of the new
//! finalized block are automatically purged.
//!
//! Keys are block hashes, and values are SCALE-encoded block headers.
//!
//! ## block_bodies
//!
//! Entries are the same as for `block_headers_tree`.
//!
//! Keys are block hashes, and values are SCALE-encoded `Vec`s containing the extrinsics. Each
//! extrinsic is itself a SCALE-encoded `Vec<u8>`.
//!
//! ## block_justifications
//!
//! Entries are a subset of the ones of `block_headers_tree`.
//! Not all blocks have a justification.
//! Only finalized blocks have a justification.
//!
//! Keys are block hashes, and values are SCALE-encoded `Vec`s containing the justification.
//!
//! ## storage_top_trie
//!
//! Contains the key-value storage at the finalized block.
//!
//! Keys are storage keys, and values are storage values.
//!
//! ## non_finalized_changes_keys
//!
//! For each hash of non-finalized block, contains the list of keys in the storage that this
//! block modifies.
//!
//! Keys are a 32 bytes block hash. Values are a list of SCALE-encoded `Vec<u8>` concatenated
//! together. In other words, each value is a length (SCALE-compact-encoded), a key of that
//! length, a length, a key of that length, and so on.
//!
//! ## non_finalized_changes
//!
//! For each element in `non_finalized_changes_keys_tree`, contains the new value for this
//! storage modification. If an entry is found in `non_finalized_changes_keys_tree` and not in
//! `non_finalized_changes_tree`, that means that the storage entry must be removed.
//!
//! Keys are a 32 bytes block hash followed with a storage key.

// TODO: better docs

#![cfg(feature = "database-sled")]
#![cfg_attr(docsrs, doc(cfg(feature = "database-sled")))]

use crate::{chain::chain_information, header, util};

use core::{convert::TryFrom, fmt, iter, num::NonZeroU64, ops};
use sled::Transactional as _;

pub use open::{open, Config, ConfigTy, DatabaseEmpty, DatabaseOpen};

mod open;

/// An open database. Holds file descriptors.
pub struct SledFullDatabase {
    /// Tree named "meta" in the database. See the module-level documentation for more info.
    meta_tree: sled::Tree,

    /// Tree named "block_hashes_by_number" in the database. See the module-level documentation
    /// for more info.
    block_hashes_by_number_tree: sled::Tree,

    /// Tree named "block_headers" in the database. See the module-level documentation for more
    /// info.
    block_headers_tree: sled::Tree,

    /// Tree named "block_bodies" in the database. See the module-level documentation for more
    /// info.
    block_bodies_tree: sled::Tree,

    /// Tree named "block_justifications" in the database. See the module-level documentation for
    /// more info.
    // TODO: never inserted
    block_justifications_tree: sled::Tree,

    /// Tree named "storage_top_trie" in the database. See the module-level documentation for more
    /// info.
    finalized_storage_top_trie_tree: sled::Tree,

    /// Tree named "non_finalized_changes_keys" in the database. See the module-level documentation
    /// for more info.
    non_finalized_changes_keys_tree: sled::Tree,

    /// Tree named "non_finalized_changes" in the database. See the module-level documentation for
    /// more info.
    non_finalized_changes_tree: sled::Tree,
}

impl SledFullDatabase {
    /// Returns the hash of the block in the database whose storage is currently accessible.
    pub fn best_block_hash(&self) -> Result<[u8; 32], AccessError> {
        match self.meta_tree.get(b"best").map_err(SledError)? {
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

    /// Returns the hash of the finalized block in the database.
    pub fn finalized_block_hash(&self) -> Result<[u8; 32], AccessError> {
        let result = (&self.block_hashes_by_number_tree, &self.meta_tree).transaction(
            move |(block_hashes_by_number, meta)| finalized_hash(&meta, &block_hashes_by_number),
        );

        match result {
            Ok(hash) => Ok(hash),
            Err(sled::transaction::TransactionError::Abort(err)) => Err(err),
            Err(sled::transaction::TransactionError::Storage(err)) => {
                Err(AccessError::Database(SledError(err)))
            }
        }
    }

    /// Returns the SCALE-encoded header of the given block, or `None` if the block is unknown.
    ///
    /// > **Note**: If this method is called twice times in a row with the same block hash, it
    /// >           is possible for the first time to return `Some` and the second time to return
    /// >           `None`, in case the block has since been removed from the database.
    pub fn block_scale_encoded_header(
        &self,
        block_hash: &[u8; 32],
    ) -> Result<Option<VarLenBytes>, AccessError> {
        Ok(self
            .block_headers_tree
            .get(block_hash)
            .map_err(SledError)?
            .map(VarLenBytes))
    }

    /// Returns the list of extrinsics of the given block, or `None` if the block is unknown.
    ///
    /// > **Note**: The list of extrinsics of a block is also known as its *body*.
    ///
    /// > **Note**: If this method is called twice times in a row with the same block hash, it
    /// >           is possible for the first time to return `Some` and the second time to return
    /// >           `None`, in case the block has since been removed from the database.
    pub fn block_extrinsics(
        &self,
        block_hash: &[u8; 32],
    ) -> Result<Option<impl ExactSizeIterator<Item = Vec<u8>>>, AccessError> {
        let body = match self.block_bodies_tree.get(block_hash).map_err(SledError)? {
            Some(b) => b,
            None => return Ok(None),
        };

        let decoded = <Vec<Vec<u8>> as parity_scale_codec::DecodeAll>::decode_all(body.as_ref())
            .map_err(|err| AccessError::Corrupted(CorruptedError::BlockBodyCorrupted(err)))?;
        Ok(Some(decoded.into_iter()))
    }

    /// Returns the hashes of the blocks given a block number.
    pub fn block_hash_by_number(
        &self,
        block_number: u64,
    ) -> Result<impl ExactSizeIterator<Item = [u8; 32]>, AccessError> {
        let hash = self
            .block_hashes_by_number_tree
            .get(&u64::to_be_bytes(block_number)[..])
            .map_err(SledError)?;
        let hash = match hash {
            Some(h) => h,
            None => return Ok(either::Left(iter::empty())),
        };

        if hash.is_empty() || (hash.len() % 32) != 0 {
            return Err(AccessError::Corrupted(
                CorruptedError::BlockHashLenInHashNumberMapping,
            ));
        }

        struct Iter {
            hash: sled::IVec,
            cursor: usize,
        }

        impl Iterator for Iter {
            type Item = [u8; 32];

            fn next(&mut self) -> Option<Self::Item> {
                if self.hash.len() <= self.cursor {
                    let h =
                        <[u8; 32]>::try_from(&self.hash[self.cursor..(self.cursor + 32)]).unwrap();
                    self.cursor += 32;
                    Some(h)
                } else {
                    None
                }
            }

            fn size_hint(&self) -> (usize, Option<usize>) {
                debug_assert_eq!(self.cursor % 32, 0);
                let len = (self.hash.len() - self.cursor) / 32;
                (len, Some(len))
            }
        }

        impl ExactSizeIterator for Iter {}

        Ok(either::Right(Iter { hash, cursor: 0 }))
    }

    /// Returns a [`chain_information::ChainInformation`] struct containing the information about
    /// the current finalized state of the chain.
    ///
    /// This method is relatively expensive and should preferably not be called repeatedly.
    ///
    /// In order to avoid race conditions, the known finalized block hash must be passed as
    /// parameter. If the finalized block in the database doesn't match the hash passed as
    /// parameter, most likely because it has been updated in a parallel thread, a
    /// [`FinalizedAccessError::Obsolete`] error is returned.
    pub fn to_chain_information(
        &self,
        finalized_block_hash: &[u8; 32],
    ) -> Result<chain_information::ChainInformation, FinalizedAccessError> {
        let result = (
            &self.meta_tree,
            &self.block_hashes_by_number_tree,
            &self.block_headers_tree,
        )
            .transaction(move |(meta, block_hashes_by_number, block_headers)| {
                let finalized_block_header =
                    finalized_block_header(&meta, &block_hashes_by_number, &block_headers)?;
                if finalized_block_header.hash() != *finalized_block_hash {
                    return Err(sled::transaction::ConflictableTransactionError::Abort(
                        FinalizedAccessError::Obsolete,
                    ));
                }

                let finality = match (
                    grandpa_authorities_set_id(&meta)?,
                    grandpa_finalized_triggered_authorities(&meta)?,
                    grandpa_finalized_scheduled_change(&meta)?,
                ) {
                    (
                        Some(after_finalized_block_authorities_set_id),
                        Some(finalized_triggered_authorities),
                        finalized_scheduled_change,
                    ) => chain_information::ChainInformationFinality::Grandpa {
                        after_finalized_block_authorities_set_id,
                        finalized_triggered_authorities,
                        finalized_scheduled_change,
                    },
                    (None, None, None) => chain_information::ChainInformationFinality::Outsourced,
                    _ => {
                        return Err(sled::transaction::ConflictableTransactionError::Abort(
                            FinalizedAccessError::Access(AccessError::Corrupted(
                                CorruptedError::ConsensusAlgorithm,
                            )),
                        ))
                    }
                };

                let consensus = match (
                    meta.get(b"aura_finalized_authorities")?,
                    meta.get(b"aura_slot_duration")?,
                    meta.get(b"babe_slots_per_epoch")?,
                    meta.get(b"babe_finalized_next_epoch")?,
                ) {
                    (None, None, Some(slots_per_epoch), Some(finalized_next_epoch)) => {
                        let slots_per_epoch = expect_be_nz_u64(&slots_per_epoch)?;
                        let finalized_next_epoch_transition =
                            decode_babe_epoch_information(&finalized_next_epoch)?;
                        let finalized_block_epoch_information = meta
                            .get(b"babe_finalized_epoch")?
                            .map(|v| decode_babe_epoch_information(&v))
                            .transpose()?;
                        chain_information::ChainInformationConsensus::Babe {
                            finalized_block_epoch_information,
                            finalized_next_epoch_transition,
                            slots_per_epoch,
                        }
                    }
                    (Some(finalized_authorities), Some(slot_duration), None, None) => {
                        let slot_duration = expect_be_nz_u64(&slot_duration)?;
                        let finalized_authorities_list =
                            decode_aura_authorities_list(&finalized_authorities)?;
                        chain_information::ChainInformationConsensus::Aura {
                            finalized_authorities_list,
                            slot_duration,
                        }
                    }
                    (None, None, None, None) => {
                        chain_information::ChainInformationConsensus::AllAuthorized
                    }
                    _ => {
                        return Err(sled::transaction::ConflictableTransactionError::Abort(
                            FinalizedAccessError::Access(AccessError::Corrupted(
                                CorruptedError::ConsensusAlgorithm,
                            )),
                        ))
                    }
                };

                Ok(chain_information::ChainInformation {
                    finalized_block_header,
                    consensus,
                    finality,
                })
            });

        match result {
            Ok(info) => Ok(info),
            Err(sled::transaction::TransactionError::Abort(err)) => Err(err),
            Err(sled::transaction::TransactionError::Storage(err)) => Err(
                FinalizedAccessError::Access(AccessError::Database(SledError(err))),
            ),
        }
    }

    /// Insert a new block in the database.
    ///
    /// Must pass the header and body of the block, and the changes to the storage that this block
    /// performs relative to its parent.
    ///
    /// Blocks must be inserted in the correct order. An error is returned if the parent of the
    /// newly-inserted block isn't present in the database.
    pub fn insert(
        &self,
        scale_encoded_header: &[u8],
        is_new_best: bool,
        body: impl ExactSizeIterator<Item = impl AsRef<[u8]>>,
        storage_top_trie_changes: impl Iterator<Item = (impl AsRef<[u8]>, Option<impl AsRef<[u8]>>)>
            + Clone,
    ) -> Result<(), InsertError> {
        // Calculate the hash of the new best block.
        let block_hash = header::hash_from_scale_encoded_header(scale_encoded_header);

        // Decode the header, as we will need various information from it.
        let header = header::decode(&scale_encoded_header).map_err(InsertError::BadHeader)?;

        // Value to put in `block_bodies_tree`. See the documentation of that field.
        let encoded_body = {
            let mut val = Vec::new();
            val.extend_from_slice(util::encode_scale_compact_usize(body.len()).as_ref());
            for item in body {
                let item = item.as_ref();
                val.extend_from_slice(util::encode_scale_compact_usize(item.len()).as_ref());
                val.extend_from_slice(item);
            }
            val
        };

        // Value to put in `non_finalized_changes_keys_tree`. See the documentation of that field.
        let changed_keys =
            storage_top_trie_changes
                .clone()
                .fold(Vec::new(), |mut list, (key, _)| {
                    let key = key.as_ref();
                    list.extend_from_slice(util::encode_scale_compact_usize(key.len()).as_ref());
                    list.extend_from_slice(key);
                    list
                });

        // Try to apply changes. This is done atomically through a transaction.
        let result = (
            &self.meta_tree,
            &self.block_hashes_by_number_tree,
            &self.block_headers_tree,
            &self.block_bodies_tree,
            &self.non_finalized_changes_keys_tree,
            &self.non_finalized_changes_tree,
        )
            .transaction(
                move |(
                    meta,
                    block_hashes_by_number,
                    block_headers,
                    block_bodies,
                    non_finalized_changes_keys,
                    non_finalized_changes,
                )| {
                    // Make sure that the block to insert isn't already in the database.
                    if block_headers.get(&block_hash)?.is_some() {
                        return Err(sled::transaction::ConflictableTransactionError::Abort(
                            InsertError::Duplicate,
                        ));
                    }

                    // Make sure that the parent of the block to insert is in the database.
                    if block_headers.get(&header.parent_hash)?.is_none() {
                        return Err(sled::transaction::ConflictableTransactionError::Abort(
                            InsertError::MissingParent,
                        ));
                    }

                    // If the height of the block to insert is <= the latest finalized, it doesn't
                    // belong to the finalized chain and would be pruned.
                    // TODO: what if we don't immediately insert the entire finalized chain, but populate it later? should that not be a use case?
                    if header.number <= finalized_num(&meta)? {
                        return Err(sled::transaction::ConflictableTransactionError::Abort(
                            InsertError::FinalizedNephew,
                        ));
                    }

                    // Append the block hash to `block_hashes_by_number`.
                    if let Some(curr) =
                        block_hashes_by_number.get(&u64::to_be_bytes(header.number)[..])?
                    {
                        let mut new_val = Vec::with_capacity(curr.len() + 32);
                        new_val.extend_from_slice(&curr);
                        new_val.extend_from_slice(&block_hash);
                        block_hashes_by_number
                            .insert(&u64::to_be_bytes(header.number)[..], &new_val[..])?;
                    } else {
                        block_hashes_by_number
                            .insert(&u64::to_be_bytes(header.number)[..], &block_hash[..])?;
                    }

                    // Insert the storage changes.
                    for (key, value) in storage_top_trie_changes.clone() {
                        // This block body unfortunately contains many memory copies, but the API
                        // of the `sled` library doesn't give us the choice.
                        let key = key.as_ref();

                        let insert_key = {
                            let mut k = Vec::with_capacity(32 + key.len());
                            k.extend_from_slice(&block_hash[..]);
                            k.extend_from_slice(key);
                            k
                        };

                        if let Some(value) = value {
                            non_finalized_changes.insert(&insert_key[..], value.as_ref())?;
                        }
                    }

                    // Various other updates.
                    non_finalized_changes_keys.insert(&block_hash[..], &changed_keys[..])?;
                    block_headers.insert(&block_hash[..], scale_encoded_header)?;
                    block_bodies.insert(&block_hash[..], &encoded_body[..])?;
                    if is_new_best {
                        meta.insert(b"best", &block_hash[..])?;
                    }

                    Ok(())
                },
            );

        match result {
            Ok(()) => Ok(()),
            Err(sled::transaction::TransactionError::Abort(err)) => Err(err),
            Err(sled::transaction::TransactionError::Storage(err)) => {
                Err(InsertError::Access(AccessError::Database(SledError(err))))
            }
        }
    }

    /// Changes the finalized block to the given one.
    ///
    /// The block must have been previously inserted using [`SledFullDatabase::insert`], otherwise
    /// an error is returned.
    ///
    /// Blocks are expected to be valid in context of the chain. Inserting an invalid block can
    /// result in the database being corrupted.
    ///
    /// The block must be a descendant of the current finalized block. Reverting finalization is
    /// forbidden, as the database intentionally discards some information when finality is
    /// applied.
    pub fn set_finalized(
        &self,
        new_finalized_block_hash: &[u8; 32],
    ) -> Result<(), SetFinalizedError> {
        let trees_list = (
            &self.meta_tree,
            &self.block_hashes_by_number_tree,
            &self.block_headers_tree,
            &self.block_bodies_tree,
            &self.finalized_storage_top_trie_tree,
            &self.non_finalized_changes_keys_tree,
            &self.non_finalized_changes_tree,
        );

        let result = trees_list.transaction(move |trees_access| {
            let (
                meta,
                block_hashes_by_number,
                block_headers,
                block_bodies,
                finalized_storage_top_trie,
                non_finalized_changes_keys,
                non_finalized_changes,
            ) = trees_access;

            // Fetch the header of the block to finalize.
            let scale_encoded_header = block_headers
                .get(&new_finalized_block_hash)?
                .ok_or(SetFinalizedError::UnknownBlock)
                .map_err(sled::transaction::ConflictableTransactionError::Abort)?;

            // Headers are checked before being inserted. If the decoding fails, it means
            // that the database is somehow corrupted.
            let header = header::decode(&scale_encoded_header)
                .map_err(|err| {
                    SetFinalizedError::Access(AccessError::Corrupted(
                        CorruptedError::BlockHeaderCorrupted(err),
                    ))
                })
                .map_err(sled::transaction::ConflictableTransactionError::Abort)?;

            // Fetch the current finalized block.
            let current_finalized = {
                let bytes = meta
                    .insert(b"finalized", &u64::to_be_bytes(header.number)[..])?
                    .ok_or(AccessError::Corrupted(
                        CorruptedError::FinalizedBlockNumberNotFound,
                    ))
                    .map_err(SetFinalizedError::Access)
                    .map_err(sled::transaction::ConflictableTransactionError::Abort)?;
                u64::from_be_bytes(<[u8; 8]>::try_from(&bytes[..]).map_err(|_| {
                    sled::transaction::ConflictableTransactionError::Abort(
                        SetFinalizedError::Access(AccessError::Corrupted(
                            CorruptedError::FinalizedBlockNumberOutOfRange,
                        )),
                    )
                })?)
            };

            // If the block to finalize is at the same height as the already-finalized
            // block, considering that the database only contains one block per height on
            // the finalized chain, and that the presence of the block to finalize in
            // the database has already been verified, it is guaranteed that the block
            // to finalize is already the one already finalized.
            if header.number == current_finalized {
                return Ok(());
            }

            // Cannot set the finalized block to a past block. The database can't support
            // reverting finalization.
            if header.number < current_finalized {
                return Err(sled::transaction::ConflictableTransactionError::Abort(
                    SetFinalizedError::RevertForbidden,
                ));
            }

            // Take each block height between `header.number` and `current_finalized + 1`
            // and remove blocks that aren't an ancestor of the new finalized block.
            {
                // For each block height between the old finalized and new finalized,
                // remove all blocks except the one whose hash is `expected_hash`.
                // `expected_hash` always designates a block in the finalized chain.
                let mut expected_hash = *new_finalized_block_hash;

                for height in (current_finalized + 1..=header.number).rev() {
                    let blocks_list = block_hashes_by_number
                        .insert(&u64::to_be_bytes(height)[..], &expected_hash[..])?
                        .ok_or(sled::transaction::ConflictableTransactionError::Abort(
                            SetFinalizedError::Access(AccessError::Corrupted(
                                CorruptedError::BrokenChain,
                            )),
                        ))?;
                    let mut expected_block_found = false;
                    for hash_at_height in blocks_list.chunks(32) {
                        if hash_at_height == expected_hash {
                            expected_block_found = true;
                            continue;
                        }

                        // Remove the block from the database.
                        purge_block(
                            TryFrom::try_from(hash_at_height).unwrap(),
                            block_headers,
                            block_bodies,
                            non_finalized_changes_keys,
                            non_finalized_changes,
                        )?;
                    }

                    // `expected_hash` not found in the list of blocks with this number.
                    if !expected_block_found {
                        return Err(sled::transaction::ConflictableTransactionError::Abort(
                            SetFinalizedError::Access(AccessError::Corrupted(
                                CorruptedError::BrokenChain,
                            )),
                        ));
                    }

                    // Update `expected_hash` to point to the parent of the current
                    // `expected_hash`.
                    expected_hash = {
                        let scale_encoded_header = block_headers
                            .get(&expected_hash)?
                            .ok_or(SetFinalizedError::Access(AccessError::Corrupted(
                                CorruptedError::BrokenChain,
                            )))
                            .map_err(sled::transaction::ConflictableTransactionError::Abort)?;
                        let header = header::decode(&scale_encoded_header)
                            .map_err(|err| {
                                SetFinalizedError::Access(AccessError::Corrupted(
                                    CorruptedError::BlockHeaderCorrupted(err),
                                ))
                            })
                            .map_err(sled::transaction::ConflictableTransactionError::Abort)?;
                        *header.parent_hash
                    };
                }
            }

            // Take each block height starting from `header.number + 1` and remove blocks
            // that aren't a descendant of the new finalized block.
            for height in header.number + 1.. {
                let blocks_list = match block_hashes_by_number.get(&u64::to_be_bytes(height)[..])? {
                    Some(l) => l,
                    None => break,
                };

                todo!()
            }

            // Now update the finalized block storage.
            for height in current_finalized + 1..=header.number {
                let block_hash = block_hashes_by_number
                    .get(&u64::to_be_bytes(height)[..])?
                    .ok_or(sled::transaction::ConflictableTransactionError::Abort(
                        SetFinalizedError::Access(AccessError::Corrupted(
                            CorruptedError::BrokenChain,
                        )),
                    ))?;

                let block_header = block_headers.get(&block_hash)?.ok_or(
                    sled::transaction::ConflictableTransactionError::Abort(
                        SetFinalizedError::Access(AccessError::Corrupted(
                            CorruptedError::BlockHeaderNotInDatabase,
                        )),
                    ),
                )?;
                let block_header = header::decode(&block_header).map_err(|err| {
                    sled::transaction::ConflictableTransactionError::Abort(
                        SetFinalizedError::Access(AccessError::Corrupted(
                            CorruptedError::BlockHeaderCorrupted(err),
                        )),
                    )
                })?;

                let changed_keys = {
                    non_finalized_changes_keys.remove(&block_hash)?.ok_or(
                        sled::transaction::ConflictableTransactionError::Abort(
                            SetFinalizedError::Access(AccessError::Corrupted(
                                CorruptedError::BrokenChain,
                            )),
                        ),
                    )?
                };

                for key in decode_non_finalized_changes_keys(&changed_keys)? {
                    let mut entry_key = Vec::with_capacity(block_hash.len() + key.len());
                    entry_key.extend_from_slice(&block_hash);
                    entry_key.extend_from_slice(&key);

                    if let Some(value) = non_finalized_changes.remove(entry_key)? {
                        finalized_storage_top_trie.insert(key, value)?;
                    } else {
                        finalized_storage_top_trie.remove(key)?;
                    }
                }

                // TODO: the code below is very verbose and redundant with other similar code in smoldot ; could be improved

                if let Some((new_epoch, next_config)) = block_header.digest.babe_epoch_information()
                {
                    let epoch = meta.get(b"babe_finalized_next_epoch")?.unwrap(); // TODO: don't unwrap
                    let decoded_epoch = decode_babe_epoch_information(&epoch)?;
                    meta.insert(b"babe_finalized_epoch", epoch)?;

                    let slot_number = block_header
                        .digest
                        .babe_pre_runtime()
                        .unwrap()
                        .slot_number();
                    let slots_per_epoch =
                        expect_be_nz_u64(&meta.get(b"babe_slots_per_epoch")?.unwrap())?; // TODO: don't unwrap

                    let new_epoch = if let Some(next_config) = next_config {
                        chain_information::BabeEpochInformation {
                            epoch_index: decoded_epoch.epoch_index.checked_add(1).unwrap(),
                            start_slot_number: Some(
                                decoded_epoch
                                    .start_slot_number
                                    .unwrap_or(slot_number)
                                    .checked_add(slots_per_epoch.get())
                                    .unwrap(),
                            ),
                            authorities: new_epoch.authorities.map(Into::into).collect(),
                            randomness: *new_epoch.randomness,
                            c: next_config.c,
                            allowed_slots: next_config.allowed_slots,
                        }
                    } else {
                        chain_information::BabeEpochInformation {
                            epoch_index: decoded_epoch.epoch_index.checked_add(1).unwrap(),
                            start_slot_number: Some(
                                decoded_epoch
                                    .start_slot_number
                                    .unwrap_or(slot_number)
                                    .checked_add(slots_per_epoch.get())
                                    .unwrap(),
                            ),
                            authorities: new_epoch.authorities.map(Into::into).collect(),
                            randomness: *new_epoch.randomness,
                            c: decoded_epoch.c,
                            allowed_slots: decoded_epoch.allowed_slots,
                        }
                    };

                    meta.insert(
                        b"babe_finalized_next_epoch",
                        encode_babe_epoch_information(From::from(&new_epoch)),
                    )?;
                }

                // TODO: implement Aura

                if grandpa_authorities_set_id(&meta)?.is_some() {
                    for grandpa_digest_item in block_header.digest.logs().filter_map(|d| match d {
                        header::DigestItemRef::GrandpaConsensus(gp) => Some(gp),
                        _ => None,
                    }) {
                        match grandpa_digest_item {
                            header::GrandpaConsensusLogRef::ScheduledChange(change) => {
                                assert_eq!(change.delay, 0); // TODO: not implemented if != 0
                                meta.insert(
                                    b"grandpa_triggered_authorities",
                                    encode_grandpa_authorities_list(change.next_authorities),
                                )?;

                                let curr_set_id = expect_be_u64(
                                    &meta.get(b"grandpa_authorities_set_id")?.unwrap(),
                                )?; // TODO: don't unwrap
                                meta.insert(
                                    b"grandpa_authorities_set_id",
                                    &(curr_set_id + 1).to_be_bytes()[..],
                                )?;
                            }
                            _ => {} // TODO: unimplemented
                        }
                    }
                }
            }

            // It is possible that the best block has been pruned.
            // TODO: ^ yeah, how do we handle that exactly ^ ?

            Ok(())
        });

        match result {
            Ok(()) => Ok(()),
            Err(sled::transaction::TransactionError::Abort(err)) => Err(err),
            Err(sled::transaction::TransactionError::Storage(err)) => Err(
                SetFinalizedError::Access(AccessError::Database(SledError(err))),
            ),
        }
    }

    /// Returns the value associated to a key in the storage of the finalized block.
    ///
    /// In order to avoid race conditions, the known finalized block hash must be passed as
    /// parameter. If the finalized block in the database doesn't match the hash passed as
    /// parameter, most likely because it has been updated in a parallel thread, a
    /// [`FinalizedAccessError::Obsolete`] error is returned.
    pub fn finalized_block_storage_top_trie_get(
        &self,
        finalized_block_hash: &[u8; 32],
        key: &[u8],
    ) -> Result<Option<VarLenBytes>, FinalizedAccessError> {
        // TODO: use a transaction rather than checking once before and once after?
        if self.finalized_block_hash()? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        let ret = self
            .finalized_storage_top_trie_tree
            .get(key)
            .map_err(SledError)
            .map_err(AccessError::Database)
            .map_err(FinalizedAccessError::Access)?
            .map(VarLenBytes);

        if self.finalized_block_hash()? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        Ok(ret)
    }

    /// Returns the key in the storage of the finalized block that immediately follows the key
    /// passed as parameter.
    ///
    /// In order to avoid race conditions, the known finalized block hash must be passed as
    /// parameter. If the finalized block in the database doesn't match the hash passed as
    /// parameter, most likely because it has been updated in a parallel thread, a
    /// [`FinalizedAccessError::Obsolete`] error is returned.
    pub fn finalized_block_storage_top_trie_next_key(
        &self,
        finalized_block_hash: &[u8; 32],
        key: &[u8],
    ) -> Result<Option<VarLenBytes>, FinalizedAccessError> {
        // TODO: use a transaction rather than checking once before and once after?
        if self.finalized_block_hash()? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        let ret = self
            .finalized_storage_top_trie_tree
            .get_gt(key)
            .map_err(SledError)
            .map_err(AccessError::Database)
            .map_err(FinalizedAccessError::Access)?
            .map(|(k, _)| VarLenBytes(k));

        if self.finalized_block_hash()? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        Ok(ret)
    }

    /// Returns the list of keys of the storage of the finalized block that start with the given
    /// prefix. Pass `&[]` for the prefix to get the list of all keys.
    ///
    /// In order to avoid race conditions, the known finalized block hash must be passed as
    /// parameter. If the finalized block in the database doesn't match the hash passed as
    /// parameter, most likely because it has been updated in a parallel thread, a
    /// [`FinalizedAccessError::Obsolete`] error is returned.
    pub fn finalized_block_storage_top_trie_keys(
        &self,
        finalized_block_hash: &[u8; 32],
        prefix: &[u8],
    ) -> Result<Vec<VarLenBytes>, FinalizedAccessError> {
        // TODO: use a transaction rather than checking once before and once after?
        if self.finalized_block_hash()? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        // TODO: implement better?
        let mut prefix_after = prefix.to_vec();
        prefix_after.push(0xff);

        let ret = self
            .finalized_storage_top_trie_tree
            .range(prefix..=(&prefix_after[..]))
            .map(|v| v.map(|(k, _)| VarLenBytes(k)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(SledError)
            .map_err(AccessError::Database)
            .map_err(FinalizedAccessError::Access)?;

        if self.finalized_block_hash()? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        Ok(ret)
    }
}

impl fmt::Debug for SledFullDatabase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SledFullDatabase").finish()
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
    Database(SledError),

    /// Database could be accessed, but its content is invalid.
    ///
    /// While these corruption errors are probably unrecoverable, the inner error might however
    /// be useful for debugging purposes.
    Corrupted(CorruptedError),
}

/// Low-level database error, such as an error while accessing the file system.
#[derive(Debug, derive_more::Display)]
pub struct SledError(sled::Error);

/// Error while calling [`SledFullDatabase::insert`].
#[derive(Debug, derive_more::Display, derive_more::From)]
pub enum InsertError {
    /// Error accessing the database.
    Access(AccessError),
    /// Block was already in the database.
    Duplicate,
    /// Error when decoding the header to import.
    BadHeader(header::Error),
    /// Parent of the block to insert isn't in the database.
    MissingParent,
    /// Block isn't a descendant of the latest finalized block.
    FinalizedNephew,
}

/// Error while calling [`SledFullDatabase::set_finalized`].
#[derive(Debug, derive_more::Display, derive_more::From)]
pub enum SetFinalizedError {
    /// Error accessing the database.
    Access(AccessError),
    /// New finalized block isn't in the database.
    UnknownBlock,
    /// New finalized block must be a child of the previous finalized block.
    RevertForbidden,
}

/// Error while accessing the storage of the finalized block.
#[derive(Debug, derive_more::Display, derive_more::From)]
pub enum FinalizedAccessError {
    /// Error accessing the database.
    Access(AccessError),
    /// Block hash passed as parameter is no longer the finalized block.
    Obsolete,
}

/// Error in the content of the database.
// TODO: document and see if any entry is unused
#[derive(Debug, derive_more::Display)]
pub enum CorruptedError {
    /// The parent of a block in the database couldn't be found in the database.
    BrokenChain,
    BestBlockHashNotFound,
    FinalizedBlockNumberNotFound,
    FinalizedBlockNumberOutOfRange,
    BestBlockHashBadLength,
    /// A block that is known to be in the database in missing from the list of block headers.
    BlockHeaderNotInDatabase,
    BlockHeaderCorrupted(header::Error),
    BlockHashLenInHashNumberMapping,
    BlockBodyCorrupted(parity_scale_codec::Error),
    NonFinalizedChangesMissing,
    InvalidBabeEpochInformation,
    InvalidGrandpaAuthoritiesSetId,
    InvalidGrandpaTriggeredAuthoritiesScheduledHeight,
    InvalidGrandpaAuthoritiesList,
    InvalidNumber,
    /// Database stores information about more than one consensus algorithm, or some critical
    /// information is missing.
    ConsensusAlgorithm,
}

fn finalized_num<E: From<AccessError>>(
    meta: &sled::transaction::TransactionalTree,
) -> Result<u64, sled::transaction::ConflictableTransactionError<E>> {
    let value = meta
        .get(b"finalized")?
        .ok_or(AccessError::Corrupted(
            CorruptedError::FinalizedBlockNumberNotFound,
        ))
        .map_err(From::from)
        .map_err(sled::transaction::ConflictableTransactionError::Abort)?;
    expect_be_u64(&value)
}

fn finalized_hash<E: From<AccessError>>(
    meta: &sled::transaction::TransactionalTree,
    block_hashes_by_number: &sled::transaction::TransactionalTree,
) -> Result<[u8; 32], sled::transaction::ConflictableTransactionError<E>> {
    let num = finalized_num(meta)?;
    let hash = block_hashes_by_number
        .get(num.to_be_bytes())?
        .ok_or(AccessError::Corrupted(
            CorruptedError::FinalizedBlockNumberOutOfRange,
        ))
        .map_err(From::from)
        .map_err(sled::transaction::ConflictableTransactionError::Abort)?;
    if hash.len() == 32 {
        let mut out = [0; 32];
        out.copy_from_slice(&hash);
        Ok(out)
    } else {
        Err(sled::transaction::ConflictableTransactionError::Abort(
            AccessError::Corrupted(CorruptedError::BlockHashLenInHashNumberMapping).into(),
        ))
    }
}

fn finalized_block_header<E: From<AccessError>>(
    meta: &sled::transaction::TransactionalTree,
    block_hashes_by_number: &sled::transaction::TransactionalTree,
    block_headers: &sled::transaction::TransactionalTree,
) -> Result<header::Header, sled::transaction::ConflictableTransactionError<E>> {
    let hash = finalized_hash(meta, block_hashes_by_number)?;

    let encoded = block_headers
        .get(&hash)?
        .ok_or(CorruptedError::BlockHeaderNotInDatabase)
        .map_err(AccessError::Corrupted)
        .map_err(From::from)
        .map_err(sled::transaction::ConflictableTransactionError::Abort)?;

    match header::decode(&encoded) {
        Ok(h) => Ok(h.into()),
        Err(err) => Err(sled::transaction::ConflictableTransactionError::Abort(
            AccessError::Corrupted(CorruptedError::BlockHeaderCorrupted(err)).into(),
        )),
    }
}

fn grandpa_authorities_set_id<E: From<AccessError>>(
    meta: &sled::transaction::TransactionalTree,
) -> Result<Option<u64>, sled::transaction::ConflictableTransactionError<E>> {
    let value = match meta.get(b"grandpa_authorities_set_id")? {
        Some(v) => v,
        None => return Ok(None),
    };

    Ok(Some(expect_be_u64(&value)?))
}

fn grandpa_finalized_triggered_authorities<E: From<AccessError>>(
    meta: &sled::transaction::TransactionalTree,
) -> Result<Option<Vec<header::GrandpaAuthority>>, sled::transaction::ConflictableTransactionError<E>>
{
    let value = match meta.get(b"grandpa_triggered_authorities")? {
        Some(v) => v,
        None => return Ok(None),
    };

    Ok(Some(decode_grandpa_authorities_list(&value)?))
}

fn grandpa_finalized_scheduled_change<E: From<AccessError>>(
    meta: &sled::transaction::TransactionalTree,
) -> Result<
    Option<(u64, Vec<header::GrandpaAuthority>)>,
    sled::transaction::ConflictableTransactionError<E>,
> {
    match (
        meta.get(b"grandpa_scheduled_authorities")?,
        meta.get(b"grandpa_scheduled_target")?,
    ) {
        (Some(authorities), Some(height)) => {
            let authorities = decode_grandpa_authorities_list(&authorities)?;
            let height = expect_be_u64(&height)?;
            Ok(Some((height, authorities)))
        }
        (None, None) => Ok(None),
        _ => Err(sled::transaction::ConflictableTransactionError::Abort(
            AccessError::Corrupted(CorruptedError::InvalidGrandpaAuthoritiesList).into(),
        )),
    }
}

/// Removes all traces of the block with the given hash from the database.
///
/// It is assumed that the block exists and that it is not finalized, otherwise a
/// [`CorruptedError`] is returned.
// TODO: what if `hash` == best block?
fn purge_block<E: From<AccessError>>(
    hash: &[u8; 32],
    block_headers: &sled::transaction::TransactionalTree,
    block_bodies: &sled::transaction::TransactionalTree,
    non_finalized_changes_keys: &sled::transaction::TransactionalTree,
    non_finalized_changes: &sled::transaction::TransactionalTree,
) -> Result<(), sled::transaction::ConflictableTransactionError<E>> {
    // TODO: check that the block was indeed in there
    block_bodies.remove(hash)?;
    block_headers.remove(hash)?;

    // TODO: block_hashes_by_number?

    let changes_keys = match non_finalized_changes_keys.remove(hash)? {
        Some(k) => decode_non_finalized_changes_keys(&k)?,
        None => {
            return Err(sled::transaction::ConflictableTransactionError::Abort(
                AccessError::Corrupted(CorruptedError::InvalidGrandpaAuthoritiesList).into(), // TODO: bad error
            ));
        }
    };

    for key in changes_keys {
        let mut entry_key = Vec::with_capacity(hash.len() + key.len());
        entry_key.extend_from_slice(hash);
        entry_key.extend_from_slice(&key);

        // Entries can legitimately be missing from `non_finalized_changes`.
        non_finalized_changes.remove(entry_key)?;
    }

    Ok(())
}

/// Decodes a value found in [`SledFullDatabase::non_finalized_changes_keys_tree`].
fn decode_non_finalized_changes_keys<E: From<AccessError>>(
    value: &sled::IVec,
) -> Result<Vec<Vec<u8>>, sled::transaction::ConflictableTransactionError<E>> {
    let result = nom::combinator::all_consuming(nom::multi::many0(nom::combinator::map(
        nom::multi::length_data(util::nom_scale_compact_usize),
        |data| data.to_vec(),
    )))(&value[..])
    .map(|(_, v)| v)
    .map_err(|_: nom::Err<nom::error::Error<&[u8]>>| ());

    result
        .map_err(|()| CorruptedError::InvalidBabeEpochInformation)
        .map_err(AccessError::Corrupted)
        .map_err(From::from)
        .map_err(sled::transaction::ConflictableTransactionError::Abort)
}

fn expect_be_u64<E: From<AccessError>>(
    value: &sled::IVec,
) -> Result<u64, sled::transaction::ConflictableTransactionError<E>> {
    <[u8; 8]>::try_from(&**value)
        .map(u64::from_be_bytes)
        .map_err(|_| CorruptedError::InvalidNumber)
        .map_err(AccessError::Corrupted)
        .map_err(From::from)
        .map_err(sled::transaction::ConflictableTransactionError::Abort)
}

fn expect_be_nz_u64<E: From<AccessError>>(
    value: &sled::IVec,
) -> Result<NonZeroU64, sled::transaction::ConflictableTransactionError<E>> {
    let num = expect_be_u64(value)?;
    NonZeroU64::new(num)
        .ok_or(CorruptedError::InvalidNumber)
        .map_err(AccessError::Corrupted)
        .map_err(From::from)
        .map_err(sled::transaction::ConflictableTransactionError::Abort)
}

fn encode_aura_authorities_list(list: header::AuraAuthoritiesIter) -> Vec<u8> {
    let mut out = Vec::with_capacity(list.len() * 32);
    for authority in list {
        out.extend_from_slice(authority.public_key);
    }
    debug_assert_eq!(out.len(), out.capacity());
    out
}

fn decode_aura_authorities_list<E: From<AccessError>>(
    value: &sled::IVec,
) -> Result<Vec<header::AuraAuthority>, sled::transaction::ConflictableTransactionError<E>> {
    if value.len() % 32 != 0 {
        return Err(sled::transaction::ConflictableTransactionError::Abort(
            AccessError::Corrupted(CorruptedError::InvalidGrandpaAuthoritiesList).into(),
        ));
    }

    Ok(value
        .chunks(32)
        .map(|chunk| {
            let public_key = <[u8; 32]>::try_from(chunk).unwrap();
            header::AuraAuthority { public_key }
        })
        .collect())
}

fn encode_grandpa_authorities_list(list: header::GrandpaAuthoritiesIter) -> Vec<u8> {
    let mut out = Vec::with_capacity(list.len() * 40);
    for authority in list {
        out.extend_from_slice(authority.public_key);
        out.extend_from_slice(&authority.weight.get().to_le_bytes()[..]);
    }
    debug_assert_eq!(out.len(), out.capacity());
    out
}

fn decode_grandpa_authorities_list<E: From<AccessError>>(
    value: &sled::IVec,
) -> Result<Vec<header::GrandpaAuthority>, sled::transaction::ConflictableTransactionError<E>> {
    if value.len() % 40 != 0 {
        return Err(sled::transaction::ConflictableTransactionError::Abort(
            AccessError::Corrupted(CorruptedError::InvalidGrandpaAuthoritiesList).into(),
        ));
    }

    let mut out = Vec::with_capacity(value.len() / 40);
    for chunk in value.chunks(40) {
        let public_key = <[u8; 32]>::try_from(&chunk[..32]).unwrap();
        let weight = u64::from_le_bytes(<[u8; 8]>::try_from(&chunk[32..]).unwrap());
        let weight = NonZeroU64::new(weight)
            .ok_or(CorruptedError::InvalidGrandpaAuthoritiesList)
            .map_err(AccessError::Corrupted)
            .map_err(From::from)
            .map_err(sled::transaction::ConflictableTransactionError::Abort)?;
        out.push(header::GrandpaAuthority { public_key, weight });
    }

    Ok(out)
}

fn encode_babe_epoch_information(info: chain_information::BabeEpochInformationRef) -> Vec<u8> {
    let mut out = Vec::with_capacity(69 + info.authorities.len() * 40);
    out.extend_from_slice(&info.epoch_index.to_le_bytes());
    if let Some(start_slot_number) = info.start_slot_number {
        out.extend_from_slice(&[1]);
        out.extend_from_slice(&start_slot_number.to_le_bytes());
    } else {
        out.extend_from_slice(&[0]);
    }
    out.extend_from_slice(util::encode_scale_compact_usize(info.authorities.len()).as_ref());
    for authority in info.authorities {
        out.extend_from_slice(authority.public_key);
        out.extend_from_slice(&authority.weight.to_le_bytes());
    }
    out.extend_from_slice(info.randomness);
    out.extend_from_slice(&info.c.0.to_le_bytes());
    out.extend_from_slice(&info.c.1.to_le_bytes());
    out.extend_from_slice(match info.allowed_slots {
        header::BabeAllowedSlots::PrimarySlots => &[0],
        header::BabeAllowedSlots::PrimaryAndSecondaryPlainSlots => &[1],
        header::BabeAllowedSlots::PrimaryAndSecondaryVrfSlots => &[2],
    });
    out
}

fn decode_babe_epoch_information<E: From<AccessError>>(
    value: &sled::IVec,
) -> Result<
    chain_information::BabeEpochInformation,
    sled::transaction::ConflictableTransactionError<E>,
> {
    let result = nom::combinator::all_consuming(nom::combinator::map(
        nom::sequence::tuple((
            nom::number::complete::le_u64,
            util::nom_option_decode(nom::number::complete::le_u64),
            nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |num_elems| {
                nom::multi::many_m_n(
                    num_elems,
                    num_elems,
                    nom::combinator::map(
                        nom::sequence::tuple((
                            nom::bytes::complete::take(32u32),
                            nom::number::complete::le_u64,
                        )),
                        move |(public_key, weight)| header::BabeAuthority {
                            public_key: TryFrom::try_from(public_key).unwrap(),
                            weight,
                        },
                    ),
                )
            }),
            nom::bytes::complete::take(32u32),
            nom::sequence::tuple((nom::number::complete::le_u64, nom::number::complete::le_u64)),
            nom::branch::alt((
                nom::combinator::map(nom::bytes::complete::tag(&[0]), |_| {
                    header::BabeAllowedSlots::PrimarySlots
                }),
                nom::combinator::map(nom::bytes::complete::tag(&[1]), |_| {
                    header::BabeAllowedSlots::PrimaryAndSecondaryPlainSlots
                }),
                nom::combinator::map(nom::bytes::complete::tag(&[2]), |_| {
                    header::BabeAllowedSlots::PrimaryAndSecondaryVrfSlots
                }),
            )),
        )),
        |(epoch_index, start_slot_number, authorities, randomness, c, allowed_slots)| {
            chain_information::BabeEpochInformation {
                epoch_index,
                start_slot_number,
                authorities,
                randomness: TryFrom::try_from(randomness).unwrap(),
                c,
                allowed_slots,
            }
        },
    ))(&value)
    .map(|(_, v)| v)
    .map_err(|_: nom::Err<nom::error::Error<&[u8]>>| ());

    result
        .map_err(|()| CorruptedError::InvalidBabeEpochInformation)
        .map_err(AccessError::Corrupted)
        .map_err(From::from)
        .map_err(sled::transaction::ConflictableTransactionError::Abort)
}
