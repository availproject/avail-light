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
//! returns a [`DatabaseOpen`] enum. This enum will contain either a [`SqliteFullDatabase`] object,
//! representing an access to the database, or a [`DatabaseEmpty`] if the database didn't exist or
//! is empty. If that is the case, use [`DatabaseEmpty::initialize`] in order to populate it and
//! obtain a [`SqliteFullDatabase`].
//!
//! Use [`SqliteFullDatabase::insert`] to insert a new block in the database. The block is assumed
//! to have been successfully verified prior to insertion. An error is returned if this block is
//! already in the database or isn't a descendant or ancestor of the latest finalized block.
//!
//! Use [`SqliteFullDatabase::set_finalized`] to mark a block already in the database as finalized.
//! Any block that isn't an ancestor or descendant will be removed. Reverting finalization is
//! not supported.
//!
//! In order to minimize disk usage, it is not possible to efficiently retrieve the storage items
//! of blocks that are ancestors of the finalized block. When a block is finalized, the storage of
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
//! The SQL schema of the database, with explanatory comments, can be found in `open.rs`.
//!

// TODO: better docs

#![cfg(feature = "database-sqlite")]
#![cfg_attr(docsrs, doc(cfg(feature = "database-sqlite")))]

use crate::{chain::chain_information, header, util};

use core::{
    convert::TryFrom,
    fmt,
    iter::{self, FromIterator},
    num::NonZeroU64,
};
use parking_lot::Mutex;

pub use open::{open, Config, ConfigTy, DatabaseEmpty, DatabaseOpen};

mod open;

/// An open database. Holds file descriptors.
pub struct SqliteFullDatabase {
    /// The SQLite connection.
    ///
    /// The database is constantly within a transaction.
    /// When the database is opened, `BEGIN TRANSACTION` is immediately run. We periodically
    /// call `COMMIT; BEGIN_TRANSACTION` when deemed necessary. `COMMIT` is basically the
    /// equivalent of `fsync`, and must be called carefully in order to not lose too much speed.
    database: Mutex<sqlite::Connection>,
}

impl SqliteFullDatabase {
    /// Returns the hash of the block in the database whose storage is currently accessible.
    pub fn best_block_hash(&self) -> Result<[u8; 32], AccessError> {
        let connection = self.database.lock();

        let val = meta_get_blob(&connection, "best")?
            .ok_or(AccessError::Corrupted(CorruptedError::MissingMetaKey))?;
        if val.len() == 32 {
            let mut out = [0; 32];
            out.copy_from_slice(&val);
            Ok(out)
        } else {
            Err(AccessError::Corrupted(CorruptedError::InvalidBlockHashLen))
        }
    }

    /// Returns the hash of the finalized block in the database.
    pub fn finalized_block_hash(&self) -> Result<[u8; 32], AccessError> {
        let database = self.database.lock();
        finalized_hash(&database)
    }

    /// Returns the SCALE-encoded header of the given block, or `None` if the block is unknown.
    ///
    /// > **Note**: If this method is called twice times in a row with the same block hash, it
    /// >           is possible for the first time to return `Some` and the second time to return
    /// >           `None`, in case the block has since been removed from the database.
    pub fn block_scale_encoded_header(
        &self,
        block_hash: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, AccessError> {
        let connection = self.database.lock();

        let mut statement = connection
            .prepare(r#"SELECT header FROM blocks WHERE hash = ?"#)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)?;
        statement.bind(1, &block_hash[..]).unwrap();

        if !matches!(statement.next().unwrap(), sqlite::State::Row) {
            return Ok(None);
        }

        let value = statement
            .read::<Vec<u8>>(0)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)?;
        Ok(Some(value))
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
        let connection = self.database.lock();

        let mut statement = connection
            .prepare(r#"SELECT extrinsic FROM blocks_body WHERE hash = ? ORDER BY idx ASC"#)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)?;

        // TODO: doesn't detect if block is absent

        statement.bind(1, &block_hash[..]).unwrap();

        let mut out = Vec::new();
        while matches!(statement.next().unwrap(), sqlite::State::Row) {
            let extrinsic = statement
                .read::<Vec<u8>>(0)
                .map_err(InternalError)
                .map_err(CorruptedError::Internal)?;
            out.push(extrinsic);
        }
        Ok(Some(out.into_iter()))
    }

    /// Returns the hashes of the blocks given a block number.
    pub fn block_hash_by_number(
        &self,
        block_number: u64,
    ) -> Result<impl ExactSizeIterator<Item = [u8; 32]>, AccessError> {
        let block_number = match i64::try_from(block_number) {
            Ok(n) => n,
            Err(_) => return Ok(either::Right(iter::empty())),
        };

        let connection = self.database.lock();

        let mut statement = connection
            .prepare(r#"SELECT hash FROM blocks WHERE number = ?"#)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)?;
        statement.bind(1, block_number).unwrap();

        let mut out = Vec::new();
        while matches!(statement.next().unwrap(), sqlite::State::Row) {
            let hash = statement.read::<Vec<u8>>(0).unwrap();
            out.push(
                <[u8; 32]>::try_from(&hash[..])
                    .map_err(|_| AccessError::Corrupted(CorruptedError::InvalidBlockHashLen))?,
            );
        }

        Ok(either::Left(out.into_iter()))
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
        let connection = self.database.lock();
        if finalized_hash(&connection)? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        let finalized_block_header = block_header(&connection, &finalized_block_hash)?
            .ok_or(AccessError::Corrupted(CorruptedError::MissingBlockHeader))?;

        let finality = match (
            grandpa_authorities_set_id(&connection)?,
            grandpa_finalized_triggered_authorities(&connection)?,
            grandpa_finalized_scheduled_change(&connection)?,
        ) {
            (
                Some(after_finalized_block_authorities_set_id),
                finalized_triggered_authorities,
                finalized_scheduled_change,
            ) => chain_information::ChainInformationFinality::Grandpa {
                after_finalized_block_authorities_set_id,
                finalized_triggered_authorities,
                finalized_scheduled_change,
            },
            (None, auth, None) if auth.is_empty() => {
                chain_information::ChainInformationFinality::Outsourced
            }
            _ => {
                return Err(FinalizedAccessError::Access(AccessError::Corrupted(
                    CorruptedError::ConsensusAlgorithmMix,
                )))
            }
        };

        let consensus = match (
            meta_get_number(&connection, "aura_slot_duration")?,
            meta_get_number(&connection, "babe_slots_per_epoch")?,
            meta_get_blob(&connection, "babe_finalized_next_epoch")?,
        ) {
            (None, Some(slots_per_epoch), Some(finalized_next_epoch)) => {
                let slots_per_epoch = expect_nz_u64(slots_per_epoch)?;
                let finalized_next_epoch_transition =
                    decode_babe_epoch_information(&finalized_next_epoch)?;
                let finalized_block_epoch_information =
                    meta_get_blob(&connection, "babe_finalized_epoch")?
                        .map(|v| decode_babe_epoch_information(&v))
                        .transpose()?;
                chain_information::ChainInformationConsensus::Babe {
                    finalized_block_epoch_information,
                    finalized_next_epoch_transition,
                    slots_per_epoch,
                }
            }
            (Some(slot_duration), None, None) => {
                let slot_duration = expect_nz_u64(slot_duration)?;
                let finalized_authorities_list = aura_finalized_authorities(&connection)?;
                chain_information::ChainInformationConsensus::Aura {
                    finalized_authorities_list,
                    slot_duration,
                }
            }
            (None, None, None) => chain_information::ChainInformationConsensus::AllAuthorized,
            _ => {
                return Err(FinalizedAccessError::Access(AccessError::Corrupted(
                    CorruptedError::ConsensusAlgorithmMix,
                )))
            }
        };

        Ok(chain_information::ChainInformation {
            finalized_block_header,
            consensus,
            finality,
        })
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

        // Locking is performed as late as possible.
        let connection = self.database.lock();

        // Make sure that the block to insert isn't already in the database.
        if has_block(&connection, &block_hash)? {
            return Err(InsertError::Duplicate);
        }

        // Make sure that the parent of the block to insert is in the database.
        if !has_block(&connection, header.parent_hash)? {
            return Err(InsertError::MissingParent);
        }

        // If the height of the block to insert is <= the latest finalized, it doesn't
        // belong to the finalized chain and would be pruned.
        // TODO: what if we don't immediately insert the entire finalized chain, but populate it later? should that not be a use case?
        if header.number <= finalized_num(&connection)? {
            return Err(InsertError::FinalizedNephew);
        }

        let mut statement = connection
            .prepare(
                "INSERT INTO blocks(number, hash, header, justification) VALUES (?, ?, ?, NULL)",
            )
            .unwrap();
        statement
            .bind(1, i64::try_from(header.number).unwrap())
            .unwrap();
        statement.bind(2, &block_hash[..]).unwrap();
        statement.bind(3, &scale_encoded_header[..]).unwrap();
        statement.next().unwrap();

        let mut statement = connection
            .prepare("INSERT INTO blocks_body(hash, idx, extrinsic) VALUES (?, ?, ?)")
            .unwrap();
        for (index, item) in body.enumerate() {
            statement.bind(1, &block_hash[..]).unwrap();
            statement.bind(2, i64::try_from(index).unwrap()).unwrap();
            statement.bind(3, item.as_ref()).unwrap();
            statement.next().unwrap();
            statement.reset().unwrap();
        }

        // Insert the storage changes.
        let mut statement = connection
            .prepare("INSERT INTO non_finalized_changes(hash, key, value) VALUES (?, ?, ?)")
            .unwrap();
        for (key, value) in storage_top_trie_changes {
            statement.bind(1, &block_hash[..]).unwrap();
            statement.bind(2, key.as_ref()).unwrap();
            if let Some(value) = value {
                statement.bind(3, value.as_ref()).unwrap();
            } else {
                // Binds NULL.
                statement.bind(3, ()).unwrap();
            }
            statement.next().unwrap();
            statement.reset().unwrap();
        }

        // Various other updates.
        if is_new_best {
            meta_set_blob(&connection, "best", &block_hash)?;
        }

        Ok(())
    }

    /// Changes the finalized block to the given one.
    ///
    /// The block must have been previously inserted using [`SqliteFullDatabase::insert`], otherwise
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
        let connection = self.database.lock();

        // Fetch the header of the block to finalize.
        let new_finalized_header = block_header(&connection, &new_finalized_block_hash)?
            .ok_or(SetFinalizedError::UnknownBlock)?;

        // Fetch the current finalized block.
        let current_finalized = finalized_num(&connection)?;

        // If the block to finalize is at the same height as the already-finalized
        // block, considering that the database only contains one block per height on
        // the finalized chain, and that the presence of the block to finalize in
        // the database has already been verified, it is guaranteed that the block
        // to finalize is already the one already finalized.
        if new_finalized_header.number == current_finalized {
            return Ok(());
        }

        // Cannot set the finalized block to a past block. The database can't support
        // reverting finalization.
        if new_finalized_header.number < current_finalized {
            return Err(SetFinalizedError::RevertForbidden);
        }

        // At this point, we are sure that the operation will succeed unless the database is
        // corrupted.
        // Update the finalized block in meta.
        meta_set_number(&connection, "finalized", new_finalized_header.number)?;

        // Take each block height between `header.number` and `current_finalized + 1`
        // and remove blocks that aren't an ancestor of the new finalized block.
        {
            // For each block height between the old finalized and new finalized,
            // remove all blocks except the one whose hash is `expected_hash`.
            // `expected_hash` always designates a block in the finalized chain.
            let mut expected_hash = *new_finalized_block_hash;

            for height in (current_finalized + 1..=new_finalized_header.number).rev() {
                let blocks_list = block_hashes_by_number(&connection, height)?;

                let mut expected_block_found = false;
                for hash_at_height in blocks_list {
                    if hash_at_height == expected_hash {
                        expected_block_found = true;
                        continue;
                    }

                    // Remove the block from the database.
                    purge_block(&connection, &hash_at_height)?;
                }

                // `expected_hash` not found in the list of blocks with this number.
                if !expected_block_found {
                    return Err(SetFinalizedError::Access(AccessError::Corrupted(
                        CorruptedError::BrokenChain,
                    )));
                }

                // Update `expected_hash` to point to the parent of the current
                // `expected_hash`.
                expected_hash = {
                    let header = block_header(&connection, &expected_hash)?.ok_or(
                        SetFinalizedError::Access(AccessError::Corrupted(
                            CorruptedError::BrokenChain,
                        )),
                    )?;
                    header.parent_hash
                };
            }
        }

        // Take each block height starting from `header.number + 1` and remove blocks
        // that aren't a descendant of the newly-finalized block.
        let mut allowed_parents = vec![*new_finalized_block_hash];
        for height in new_finalized_header.number + 1.. {
            let mut next_iter_allowed_parents = Vec::with_capacity(allowed_parents.len());

            let blocks_list = block_hashes_by_number(&connection, height)?;
            if blocks_list.is_empty() {
                break;
            }

            for block_hash in blocks_list {
                let header = block_header(&connection, &block_hash)?
                    .ok_or(AccessError::Corrupted(CorruptedError::MissingBlockHeader))?;
                if allowed_parents.iter().any(|p| *p == header.parent_hash) {
                    next_iter_allowed_parents.push(block_hash);
                    continue;
                }

                purge_block(&connection, &block_hash)?;
            }

            allowed_parents = next_iter_allowed_parents;
        }

        // Now update the finalized block storage.
        for height in current_finalized + 1..=new_finalized_header.number {
            let block_hash =
                {
                    let list = block_hashes_by_number(&connection, height)?;
                    debug_assert_eq!(list.len(), 1);
                    list.into_iter().next().ok_or(SetFinalizedError::Access(
                        AccessError::Corrupted(CorruptedError::MissingBlockHeader),
                    ))?
                };

            let block_header =
                block_header(&connection, &block_hash)?.ok_or(SetFinalizedError::Access(
                    AccessError::Corrupted(CorruptedError::MissingBlockHeader),
                ))?;

            let mut statement = connection
                .prepare(
                    "DELETE FROM finalized_storage_top_trie
                WHERE key IN (
                    SELECT key FROM non_finalized_changes WHERE hash = ? AND value IS NULL
                );",
                )
                .unwrap();
            statement.bind(1, &block_hash[..]).unwrap();
            statement.next().unwrap();

            let mut statement = connection
                .prepare(
                    "INSERT OR REPLACE INTO finalized_storage_top_trie(key, value)
                SELECT key, value
                FROM non_finalized_changes 
                WHERE non_finalized_changes.hash = ? AND non_finalized_changes.value IS NOT NULL",
                )
                .unwrap();
            statement.bind(1, &block_hash[..]).unwrap();
            statement.next().unwrap();

            // Remove the entries from `non_finalized_changes` as they are now finalized.
            let mut statement = connection
                .prepare("DELETE FROM non_finalized_changes WHERE hash = ?")
                .unwrap();
            statement.bind(1, &block_hash[..]).unwrap();
            statement.next().unwrap();

            // TODO: the code below is very verbose and redundant with other similar code in smoldot ; could be improved

            if let Some((new_epoch, next_config)) = block_header.digest.babe_epoch_information() {
                let epoch = meta_get_blob(&connection, "babe_finalized_next_epoch")?.unwrap(); // TODO: don't unwrap
                let decoded_epoch = decode_babe_epoch_information(&epoch)?;
                connection.execute(r#"INSERT OR REPLACE INTO meta(key, value_blob) SELECT "babe_finalized_epoch", value_blob FROM meta WHERE key = "babe_finalized_next_epoch""#).unwrap();

                let slot_number = block_header
                    .digest
                    .babe_pre_runtime()
                    .unwrap()
                    .slot_number();
                let slots_per_epoch =
                    expect_nz_u64(meta_get_number(&connection, "babe_slots_per_epoch")?.unwrap())?; // TODO: don't unwrap

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

                meta_set_blob(
                    &connection,
                    "babe_finalized_next_epoch",
                    &encode_babe_epoch_information(From::from(&new_epoch)),
                )?;
            }

            // TODO: implement Aura

            if grandpa_authorities_set_id(&connection)?.is_some() {
                for grandpa_digest_item in block_header.digest.logs().filter_map(|d| match d {
                    header::DigestItemRef::GrandpaConsensus(gp) => Some(gp),
                    _ => None,
                }) {
                    match grandpa_digest_item {
                        header::GrandpaConsensusLogRef::ScheduledChange(change) => {
                            assert_eq!(change.delay, 0); // TODO: not implemented if != 0

                            connection
                                .execute("DELETE FROM grandpa_triggered_authorities")
                                .unwrap();

                            let mut statement = connection.prepare("INSERT INTO grandpa_triggered_authorities(idx, public_key, weight) VALUES(?, ?, ?)").unwrap();
                            for (index, item) in change.next_authorities.enumerate() {
                                statement
                                    .bind(1, i64::from_ne_bytes(index.to_ne_bytes()))
                                    .unwrap();
                                statement.bind(2, &item.public_key[..]).unwrap();
                                statement
                                    .bind(3, i64::from_ne_bytes(item.weight.get().to_ne_bytes()))
                                    .unwrap();
                                statement.next().unwrap();
                                statement.reset().unwrap();
                            }

                            connection.execute(r#"UPDATE meta SET value_number = value_number + 1 WHERE key = "grandpa_authorities_set_id""#).unwrap();
                        }
                        _ => {} // TODO: unimplemented
                    }
                }
            }
        }

        // It is possible that the best block has been pruned.
        // TODO: ^ yeah, how do we handle that exactly ^ ?

        // Make sure that everything is saved to disk after this point.
        flush(&connection)?;

        Ok(())
    }

    /// Returns all the keys and values in the storage of the finalized block.
    ///
    /// In order to avoid race conditions, the known finalized block hash must be passed as
    /// parameter. If the finalized block in the database doesn't match the hash passed as
    /// parameter, most likely because it has been updated in a parallel thread, a
    /// [`FinalizedAccessError::Obsolete`] error is returned.
    ///
    /// The return value must implement the `FromIterator` trait, being passed an iterator that
    /// produces tuples of keys and values.
    pub fn finalized_block_storage_top_trie<T: FromIterator<(Vec<u8>, Vec<u8>)>>(
        &self,
        finalized_block_hash: &[u8; 32],
    ) -> Result<T, FinalizedAccessError> {
        let connection = self.database.lock();

        if finalized_hash(&connection)? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        let mut statement = connection
            .prepare(r#"SELECT key, value FROM finalized_storage_top_trie"#)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)
            .map_err(FinalizedAccessError::Access)?;

        let out: T = iter::from_fn(|| {
            if !matches!(statement.next().unwrap(), sqlite::State::Row) {
                return None;
            }

            let key = statement.read::<Vec<u8>>(0).unwrap();
            let value = statement.read::<Vec<u8>>(1).unwrap();
            Some((key, value))
        })
        .collect();

        Ok(out)
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
    ) -> Result<Option<Vec<u8>>, FinalizedAccessError> {
        let connection = self.database.lock();

        if finalized_hash(&connection)? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        let mut statement = connection
            .prepare(r#"SELECT value FROM finalized_storage_top_trie WHERE key = ?"#)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)
            .map_err(FinalizedAccessError::Access)?;
        statement.bind(1, key).unwrap();

        if !matches!(statement.next().unwrap(), sqlite::State::Row) {
            return Ok(None);
        }

        let value = statement
            .read::<Vec<u8>>(0)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)
            .map_err(FinalizedAccessError::Access)?;
        Ok(Some(value))
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
    ) -> Result<Option<Vec<u8>>, FinalizedAccessError> {
        let connection = self.database.lock();

        if finalized_hash(&connection)? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        let mut statement = connection
            .prepare(r#"SELECT key FROM finalized_storage_top_trie WHERE key > ? ORDER BY key ASC LIMIT 1"#)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)
            .map_err(FinalizedAccessError::Access)?;
        statement.bind(1, key).unwrap();

        if !matches!(statement.next().unwrap(), sqlite::State::Row) {
            return Ok(None);
        }

        let key = statement
            .read::<Vec<u8>>(0)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)
            .map_err(FinalizedAccessError::Access)?;
        Ok(Some(key))
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
    ) -> Result<Vec<Vec<u8>>, FinalizedAccessError> {
        let connection = self.database.lock();

        if finalized_hash(&connection)? != *finalized_block_hash {
            return Err(FinalizedAccessError::Obsolete);
        }

        let mut statement = connection
            .prepare(r#"SELECT key FROM finalized_storage_top_trie WHERE key >= ?"#)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)
            .map_err(FinalizedAccessError::Access)?;
        statement.bind(1, prefix).unwrap();

        let mut out = Vec::new();
        while matches!(statement.next().unwrap(), sqlite::State::Row) {
            let key = statement
                .read::<Vec<u8>>(0)
                .map_err(InternalError)
                .map_err(CorruptedError::Internal)
                .map_err(AccessError::Corrupted)
                .map_err(FinalizedAccessError::Access)?;

            // TODO: hack because I don't know how to ask sqlite to do that
            if !(key.starts_with(prefix)) {
                continue;
            }

            out.push(key);
        }

        Ok(out)
    }
}

impl fmt::Debug for SqliteFullDatabase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SqliteFullDatabase").finish()
    }
}

impl Drop for SqliteFullDatabase {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = self.database.get_mut().execute("PRAGMA optimize;");
            let _ = self.database.get_mut().execute("COMMIT");
        } else {
            // Rolling back if we're unwind is not the worst idea, in case we were in the middle
            // of an update.
            // We might roll back too much, but it is not considered a problem.
            let _ = self.database.get_mut().execute("ROLLBACK");
        }
    }
}

/// Error while accessing some information.
// TODO: completely replace with just CorruptedError?
#[derive(Debug, derive_more::Display, derive_more::From)]
pub enum AccessError {
    /// Database could be accessed, but its content is invalid.
    ///
    /// While these corruption errors are probably unrecoverable, the inner error might however
    /// be useful for debugging purposes.
    Corrupted(CorruptedError),
}

/// Error while calling [`SqliteFullDatabase::insert`].
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

/// Error while calling [`SqliteFullDatabase::set_finalized`].
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
    /// Block numbers are expected to be 64 bits.
    // TODO: remove this and use stronger schema
    InvalidNumber,
    /// Finalized block number stored in the database doesn't match any block.
    InvalidFinalizedNum,
    /// A block hash is expected to be 32 bytes. This isn't the case.
    InvalidBlockHashLen,
    /// The parent of a block in the database couldn't be found in that same database.
    BrokenChain,
    /// Missing a key in the `meta` table.
    MissingMetaKey,
    /// Some parts of the database refer to a block by its hash, but the block's constituents
    /// couldn't be found.
    MissingBlockHeader,
    /// The header of a block in the database has failed to decode.
    BlockHeaderCorrupted(header::Error),
    /// Multiple different consensus algorithms are mixed within the database.
    ConsensusAlgorithmMix,
    /// The information about a Babe epoch found in the database has failed to decode.
    InvalidBabeEpochInformation,
    Internal(InternalError),
}

/// Low-level database error, such as an error while accessing the file system.
#[derive(Debug, derive_more::Display)]
pub struct InternalError(sqlite::Error);

fn meta_get_blob(database: &sqlite::Connection, key: &str) -> Result<Option<Vec<u8>>, AccessError> {
    let mut statement = database
        .prepare(r#"SELECT value_blob FROM meta WHERE key = ?"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;
    statement.bind(1, key).unwrap();

    if !matches!(statement.next().unwrap(), sqlite::State::Row) {
        return Ok(None);
    }

    let value = statement
        .read::<Vec<u8>>(0)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;
    Ok(Some(value))
}

fn meta_get_number(database: &sqlite::Connection, key: &str) -> Result<Option<u64>, AccessError> {
    let mut statement = database
        .prepare(r#"SELECT value_number FROM meta WHERE key = ?"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;
    statement.bind(1, key).unwrap();

    if !matches!(statement.next().unwrap(), sqlite::State::Row) {
        return Ok(None);
    }

    let value = statement
        .read::<i64>(0)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;
    Ok(Some(u64::from_ne_bytes(value.to_ne_bytes())))
}

fn meta_set_blob(
    database: &sqlite::Connection,
    key: &str,
    value: &[u8],
) -> Result<(), AccessError> {
    let mut statement = database
        .prepare(r#"INSERT OR REPLACE INTO meta(key, value_blob) VALUES (?, ?)"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;
    statement.bind(1, key).unwrap();
    statement.bind(2, value).unwrap();
    statement.next().unwrap();
    Ok(())
}

fn meta_set_number(
    database: &sqlite::Connection,
    key: &str,
    value: u64,
) -> Result<(), AccessError> {
    let mut statement = database
        .prepare(r#"INSERT OR REPLACE INTO meta(key, value_number) VALUES (?, ?)"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;
    statement.bind(1, key).unwrap();
    statement
        .bind(2, i64::from_ne_bytes(value.to_ne_bytes()))
        .unwrap();
    statement.next().unwrap();
    Ok(())
}

fn has_block(database: &sqlite::Connection, hash: &[u8]) -> Result<bool, AccessError> {
    let mut statement = database
        .prepare(r#"SELECT COUNT(*) FROM blocks WHERE hash = ?"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;
    statement.bind(1, hash).unwrap();

    if !matches!(statement.next().unwrap(), sqlite::State::Row) {
        panic!()
    }

    Ok(statement.read::<i64>(0).unwrap() != 0)
}

// TODO: the fact that the meta table stores blobs makes it impossible to use joins ; fix that
fn finalized_num(database: &sqlite::Connection) -> Result<u64, AccessError> {
    meta_get_number(database, "finalized")?
        .ok_or(AccessError::Corrupted(CorruptedError::MissingMetaKey))
}

fn finalized_hash(database: &sqlite::Connection) -> Result<[u8; 32], AccessError> {
    let mut statement = database
        .prepare(r#"SELECT hash FROM blocks WHERE number = (SELECT value_number FROM meta WHERE key = "finalized")"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;

    if !matches!(statement.next().unwrap(), sqlite::State::Row) {
        return Err(AccessError::Corrupted(CorruptedError::InvalidFinalizedNum).into());
    }

    let value = statement
        .read::<Vec<u8>>(0)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;

    if value.len() == 32 {
        let mut out = [0; 32];
        out.copy_from_slice(&value);
        Ok(out)
    } else {
        Err(AccessError::Corrupted(CorruptedError::InvalidBlockHashLen).into())
    }
}

fn block_hashes_by_number(
    database: &sqlite::Connection,
    number: u64,
) -> Result<Vec<[u8; 32]>, AccessError> {
    let number = match i64::try_from(number) {
        Ok(n) => n,
        Err(_) => return Ok(Vec::new()),
    };

    let mut statement = database
        .prepare(r#"SELECT hash FROM blocks WHERE number = ?"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;
    statement.bind(1, number).unwrap();

    let mut out = Vec::new();
    while matches!(statement.next().unwrap(), sqlite::State::Row) {
        let value = statement
            .read::<Vec<u8>>(0)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)?;

        out.push(
            <[u8; 32]>::try_from(&value[..])
                .map_err(|_| AccessError::Corrupted(CorruptedError::InvalidBlockHashLen))?,
        );
    }

    Ok(out)
}

fn block_header(
    database: &sqlite::Connection,
    hash: &[u8; 32],
) -> Result<Option<header::Header>, AccessError> {
    let mut statement = database
        .prepare(r#"SELECT header FROM blocks WHERE hash = ?"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;
    statement.bind(1, &hash[..]).unwrap();

    if !matches!(statement.next().unwrap(), sqlite::State::Row) {
        return Ok(None);
    }

    let encoded = statement
        .read::<Vec<u8>>(0)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;

    match header::decode(&encoded) {
        Ok(h) => Ok(Some(h.into())),
        Err(err) => Err(AccessError::Corrupted(CorruptedError::BlockHeaderCorrupted(err)).into()),
    }
}

fn flush(database: &sqlite::Connection) -> Result<(), AccessError> {
    database.execute("COMMIT; BEGIN TRANSACTION;").unwrap();
    Ok(())
}

fn purge_block(database: &sqlite::Connection, hash: &[u8; 32]) -> Result<(), AccessError> {
    let mut statement = database
        .prepare(
            "DELETE FROM non_finalized_changes WHERE hash = :hash;
        DELETE FROM blocks_body WHERE hash = :hash;
        DELETE FROM blocks WHERE hash = :hash;",
        )
        .unwrap();
    statement.bind_by_name(":hash", &hash[..]).unwrap();
    statement.next().unwrap();

    Ok(())
}

fn grandpa_authorities_set_id(database: &sqlite::Connection) -> Result<Option<u64>, AccessError> {
    meta_get_number(database, "grandpa_authorities_set_id")
}

fn grandpa_finalized_triggered_authorities(
    database: &sqlite::Connection,
) -> Result<Vec<header::GrandpaAuthority>, AccessError> {
    let mut statement = database
        .prepare(r#"SELECT public_key, weight FROM grandpa_triggered_authorities ORDER BY idx ASC"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;

    let mut out = Vec::new();
    while matches!(statement.next().unwrap(), sqlite::State::Row) {
        let public_key = statement
            .read::<Vec<u8>>(0)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)?;

        let public_key = <[u8; 32]>::try_from(&public_key[..])
            .map_err(|_| CorruptedError::InvalidBlockHashLen)
            .map_err(AccessError::Corrupted)?;

        let weight = statement
            .read::<i64>(1)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)?;

        let weight = NonZeroU64::new(u64::from_ne_bytes(weight.to_ne_bytes()))
            .ok_or(CorruptedError::InvalidNumber)
            .map_err(AccessError::Corrupted)?;
        out.push(header::GrandpaAuthority { public_key, weight });
    }

    Ok(out)
}

fn grandpa_finalized_scheduled_change(
    database: &sqlite::Connection,
) -> Result<Option<(u64, Vec<header::GrandpaAuthority>)>, AccessError> {
    if let Some(height) = meta_get_number(database, "grandpa_scheduled_target")? {
        // TODO: duplicated from above except different table name
        let mut statement = database
            .prepare(
                r#"SELECT public_key, weight FROM grandpa_scheduled_authorities ORDER BY idx ASC"#,
            )
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)?;

        let mut out = Vec::new();
        while matches!(statement.next().unwrap(), sqlite::State::Row) {
            let public_key = statement
                .read::<Vec<u8>>(0)
                .map_err(InternalError)
                .map_err(CorruptedError::Internal)
                .map_err(AccessError::Corrupted)?;

            let public_key = <[u8; 32]>::try_from(&public_key[..])
                .map_err(|_| CorruptedError::InvalidBlockHashLen)
                .map_err(AccessError::Corrupted)?;

            let weight = statement
                .read::<i64>(1)
                .map_err(InternalError)
                .map_err(CorruptedError::Internal)
                .map_err(AccessError::Corrupted)?;

            let weight = NonZeroU64::new(u64::from_ne_bytes(weight.to_ne_bytes()))
                .ok_or(CorruptedError::InvalidNumber)
                .map_err(AccessError::Corrupted)?;
            out.push(header::GrandpaAuthority { public_key, weight });
        }

        Ok(Some((height, out)))
    } else {
        Ok(None)
    }
}

fn expect_nz_u64(value: u64) -> Result<NonZeroU64, AccessError> {
    NonZeroU64::new(value)
        .ok_or(CorruptedError::InvalidNumber)
        .map_err(AccessError::Corrupted)
}

fn aura_finalized_authorities(
    database: &sqlite::Connection,
) -> Result<Vec<header::AuraAuthority>, AccessError> {
    let mut statement = database
        .prepare(r#"SELECT public_key FROM aura_finalized_authorities ORDER BY idx ASC"#)
        .map_err(InternalError)
        .map_err(CorruptedError::Internal)
        .map_err(AccessError::Corrupted)?;

    let mut out = Vec::new();
    while matches!(statement.next().unwrap(), sqlite::State::Row) {
        let public_key = statement
            .read::<Vec<u8>>(0)
            .map_err(InternalError)
            .map_err(CorruptedError::Internal)
            .map_err(AccessError::Corrupted)?;

        let public_key = <[u8; 32]>::try_from(&public_key[..])
            .map_err(|_| CorruptedError::InvalidBlockHashLen)
            .map_err(AccessError::Corrupted)?;

        out.push(header::AuraAuthority { public_key });
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

fn decode_babe_epoch_information(
    value: &[u8],
) -> Result<chain_information::BabeEpochInformation, AccessError> {
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
}
