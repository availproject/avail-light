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

//! Database opening code.
//!
//! Contains everything related to the opening and initialization of the database.

use super::{encode_babe_epoch_information, AccessError, SqliteFullDatabase};
use crate::chain::chain_information;

use std::{convert::TryFrom as _, path::Path};

/// Opens the database using the given [`Config`].
///
/// Note that this doesn't return a [`SqliteFullDatabase`], but rather a [`DatabaseOpen`].
pub fn open(config: Config) -> Result<DatabaseOpen, super::InternalError> {
    let flags = sqlite::OpenFlags::new()
        .set_create()
        .set_read_write()
        // The "no mutex" option opens SQLite in "multi-threaded" mode, meaning that it can safely
        // be used from multiple threads as long as we don't access the connection from multiple
        // threads *at the same time*. Since we put the connection behind a `Mutex`, and that the
        // underlying library implements `!Sync` for `Connection` as a safety measure anyway, it
        // is safe to enable this option.
        // See https://www.sqlite.org/threadsafe.html
        .set_no_mutex();

    let database = match config.ty {
        ConfigTy::Disk(path) => {
            // We put a `/v1/` behind the path in case we change the schema.
            let path = path.join("v1").join("database.sqlite");
            sqlite::Connection::open_with_flags(path, flags)
        }
        ConfigTy::Memory => sqlite::Connection::open_with_flags(":memory:", flags),
    }
    .map_err(super::InternalError)?;

    database
        .execute(
            r#"
-- See https://sqlite.org/pragma.html and https://www.sqlite.org/wal.html
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA locking_mode = EXCLUSIVE;
PRAGMA auto_vacuum = FULL;
PRAGMA encoding = 'UTF-8';
PRAGMA trusted_schema = false; 

/*
Contains all the "global" values in the database.
A value must be present either in `value_blob` or `value_number` depending on the type of data.

Keys in that table:

 - `best` (blob): Hash of the best block.

 - `finalized` (number): Height of the finalized block, as a 64bits big endian number.

 - `grandpa_authorities_set_id` (number): A 64bits big endian number representing the id of the
 authorities set that must finalize the block right after the finalized block. The value is
 0 at the genesis block, and increased by 1 at every authorities change. Missing if and only
 if the chain doesn't use Grandpa.

 - `grandpa_scheduled_target` (number): A 64bits big endian number representing the block where the
 authorities found in `grandpa_scheduled_authorities` will be triggered. Blocks whose height
 is strictly higher than this value must be finalized using the new set of authorities. This
 authority change must have been scheduled in or before the finalized block. Missing if no
 change is scheduled or if the chain doesn't use Grandpa.

 - `aura_slot_duration` (number): A 64bits big endian number indicating the duration of an Aura
 slot. Missing if and only if the chain doesn't use Aura.

 - `babe_slots_per_epoch` (number): A 64bits big endian number indicating the number of slots per
 Babe epoch. Missing if and only if the chain doesn't use Babe.

 - `babe_finalized_epoch` (blob): SCALE encoding of a structure that contains the information
 about the Babe epoch used for the finalized block. Missing if and only if the finalized
 block is block #0 or the chain doesn't use Babe.

 - `babe_finalized_next_epoch` (blob): SCALE encoding of a structure that contains the information
 about the Babe epoch that follows the one described by `babe_finalized_epoch`. If the
 finalized block is block #0, then this contains information about epoch #0. Missing if and
 only if the chain doesn't use Babe.

*/
CREATE TABLE IF NOT EXISTS meta(
    key STRING NOT NULL PRIMARY KEY,
    value_blob BLOB,
    value_number INTEGER,
    -- Either `value_blob` or `value_number` must be NULL but not both.
    CHECK((value_blob IS NULL OR value_number IS NULL) AND (value_blob IS NOT NULL OR value_number IS NOT NULL))
);

/*
List of all known blocks, indexed by their hash or number.
*/
CREATE TABLE IF NOT EXISTS blocks(
    hash BLOB NOT NULL PRIMARY KEY,
    number INTEGER NOT NULL,
    header BLOB NOT NULL,
    justification BLOB,
    UNIQUE(number, hash),
    CHECK(length(hash) == 32)
);
CREATE INDEX IF NOT EXISTS blocks_by_number ON blocks(number);

/*
Each block has a body made from 0+ extrinsics (in practice, there's always at least one extrinsic,
but the database supports 0). This table contains these extrinsics.
The `idx` field contains the index between `0` and `num_extrinsics - 1`. The values in `idx` must
be contiguous for each block.
*/
CREATE TABLE IF NOT EXISTS blocks_body(
    hash BLOB NOT NULL,
    idx INTEGER NOT NULL,
    extrinsic BLOB NOT NULL,
    UNIQUE(hash, idx),
    CHECK(length(hash) == 32),
    FOREIGN KEY (hash) REFERENCES blocks(hash) ON UPDATE CASCADE ON DELETE CASCADE
);

/*
Storage at the highest block that is considered finalized.
*/
CREATE TABLE IF NOT EXISTS finalized_storage_top_trie(
    key BLOB NOT NULL PRIMARY KEY,
    value BLOB NOT NULL
);

/*
For non-finalized blocks (i.e. blocks that descend from the finalized block), contains changes
that this block performs on the storage.
When a block gets finalized, these changes get merged into `finalized_storage_top_trie`.
*/
CREATE TABLE IF NOT EXISTS non_finalized_changes(
    hash BLOB NOT NULL,
    key BLOB NOT NULL,
    -- `value` is NULL if the block removes the key from the storage, and NON-NULL if it inserts
    -- or replaces the value at the key.
    value BLOB,
    UNIQUE(hash, key),
    CHECK(length(hash) == 32),
    FOREIGN KEY (hash) REFERENCES blocks(hash) ON UPDATE CASCADE ON DELETE CASCADE
);

/*
List of public keys and weights of the GrandPa authorities that will be triggered at the block
found in `grandpa_scheduled_target` (see `meta`). Empty if the chain doesn't use Grandpa.
*/
CREATE TABLE IF NOT EXISTS grandpa_triggered_authorities(
    idx INTEGER NOT NULL PRIMARY KEY,
    public_key BLOB NOT NULL,
    weight INTEGER NOT NULL,
    CHECK(length(public_key) == 32)
);

/*
List of public keys and weights of the GrandPa authorities that must finalize the children of the
finalized block. Empty if the chain doesn't use Grandpa.
 */
CREATE TABLE IF NOT EXISTS grandpa_scheduled_authorities(
    idx INTEGER NOT NULL PRIMARY KEY,
    public_key BLOB NOT NULL,
    weight INTEGER NOT NULL,
    CHECK(length(public_key) == 32)
);

/*
List of public keys of the Aura authorities that must author the children of the finalized block.
 */
CREATE TABLE IF NOT EXISTS aura_finalized_authorities(
    idx INTEGER NOT NULL PRIMARY KEY,
    public_key BLOB NOT NULL,
    CHECK(length(public_key) == 32)
);

    "#,
        )
        .map_err(super::InternalError)?;

    let is_empty = {
        let mut statement = database
            .prepare("SELECT COUNT(*) FROM meta WHERE key = ?")
            .unwrap();
        statement.bind(1, "best").unwrap();
        statement.next().unwrap();
        statement.read::<i64>(0).unwrap() == 0
    };

    // The database is *always* within a transaction.
    database.execute("BEGIN TRANSACTION").unwrap();

    Ok(if !is_empty {
        DatabaseOpen::Open(SqliteFullDatabase {
            database: parking_lot::Mutex::new(database),
        })
    } else {
        DatabaseOpen::Empty(DatabaseEmpty { database })
    })
}

/// Configuration for the database.
#[derive(Debug)]
pub struct Config<'a> {
    /// Type of database.
    pub ty: ConfigTy<'a>,
}

/// Type of database.
#[derive(Debug)]
pub enum ConfigTy<'a> {
    /// Store the database on disk. Path to the directory containing the database.
    Disk(&'a Path),
    /// Store the database in memory. The database is discarded on destruction.
    Memory,
}

/// Either existing database or database prototype.
pub enum DatabaseOpen {
    /// A database already existed and has now been opened.
    Open(SqliteFullDatabase),

    /// Either a database has just been created, or there existed a database but it is empty.
    ///
    /// > **Note**: The situation where a database existed but is empty can happen if you have
    /// >           previously called [`open`] then dropped the [`DatabaseOpen`] object without
    /// >           filling the newly-created database with data.
    Empty(DatabaseEmpty),
}

/// An open database. Holds file descriptors.
pub struct DatabaseEmpty {
    /// See the similar field in [`SqliteFullDatabase`].
    database: sqlite::Connection,
}

impl DatabaseEmpty {
    /// Inserts the given [`chain_information::ChainInformationRef`] in the database prototype in
    /// order to turn it into an actual database.
    ///
    /// Must also pass the body, justification, and state of the storage of the finalized block.
    pub fn initialize<'a>(
        self,
        chain_information: impl Into<chain_information::ChainInformationRef<'a>>,
        finalized_block_body: impl ExactSizeIterator<Item = &'a [u8]>,
        finalized_block_justification: Option<Vec<u8>>,
        finalized_block_storage_top_trie_entries: impl Iterator<Item = (&'a [u8], &'a [u8])> + Clone,
    ) -> Result<SqliteFullDatabase, AccessError> {
        let chain_information = chain_information.into();

        let finalized_block_hash = chain_information.finalized_block_header.hash();

        let scale_encoded_finalized_block_header = chain_information
            .finalized_block_header
            .scale_encoding()
            .fold(Vec::new(), |mut a, b| {
                a.extend_from_slice(b.as_ref());
                a
            });

        {
            let mut statement = self
                .database
                .prepare("INSERT INTO finalized_storage_top_trie(key, value) VALUES(?, ?)")
                .unwrap();
            for (key, value) in finalized_block_storage_top_trie_entries.clone() {
                statement.bind(1, key).unwrap();
                statement.bind(2, value).unwrap();
                statement.next().unwrap();
                statement.reset().unwrap();
            }
        }

        {
            let mut statement = self
                .database
                .prepare(
                    "INSERT INTO blocks(hash, number, header, justification) VALUES(?, ?, ?, ?)",
                )
                .unwrap();
            statement.bind(1, &finalized_block_hash[..]).unwrap();
            statement
                .bind(
                    2,
                    i64::try_from(chain_information.finalized_block_header.number).unwrap(),
                )
                .unwrap();
            statement
                .bind(3, &scale_encoded_finalized_block_header[..])
                .unwrap();
            if let Some(finalized_block_justification) = &finalized_block_justification {
                statement
                    .bind(4, &finalized_block_justification[..])
                    .unwrap();
            } else {
                statement.bind(4, ()).unwrap();
            }
            statement.next().unwrap();
        }

        {
            let mut statement = self
                .database
                .prepare("INSERT INTO blocks_body(hash, idx, extrinsic) VALUES(?, ?, ?)")
                .unwrap();
            for (index, item) in finalized_block_body.enumerate() {
                statement.bind(1, &finalized_block_hash[..]).unwrap();
                statement.bind(2, i64::try_from(index).unwrap()).unwrap();
                statement.bind(3, item).unwrap();
                statement.next().unwrap();
                statement.reset().unwrap();
            }
        }

        super::meta_set_blob(&self.database, "best", &finalized_block_hash[..]).unwrap();
        super::meta_set_number(
            &self.database,
            "finalized",
            chain_information.finalized_block_header.number,
        )
        .unwrap();

        match &chain_information.finality {
            chain_information::ChainInformationFinalityRef::Outsourced => {}
            chain_information::ChainInformationFinalityRef::Grandpa {
                finalized_triggered_authorities,
                after_finalized_block_authorities_set_id,
                finalized_scheduled_change,
            } => {
                super::meta_set_number(
                    &self.database,
                    "grandpa_authorities_set_id",
                    *after_finalized_block_authorities_set_id,
                )
                .unwrap();

                let mut statement = self
                    .database
                    .prepare("INSERT INTO grandpa_triggered_authorities(idx, public_key, weight) VALUES(?, ?, ?)")
                    .unwrap();
                for (index, item) in finalized_triggered_authorities.iter().enumerate() {
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

                if let Some((height, list)) = finalized_scheduled_change {
                    super::meta_set_number(&self.database, "grandpa_scheduled_target", *height)
                        .unwrap();

                    let mut statement = self
                        .database
                        .prepare("INSERT INTO grandpa_scheduled_authorities(idx, public_key, weight) VALUES(?, ?, ?)")
                        .unwrap();
                    for (index, item) in list.iter().enumerate() {
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
                }
            }
        }

        match &chain_information.consensus {
            chain_information::ChainInformationConsensusRef::AllAuthorized => {}
            chain_information::ChainInformationConsensusRef::Aura {
                finalized_authorities_list,
                slot_duration,
            } => {
                super::meta_set_number(&self.database, "aura_slot_duration", slot_duration.get())
                    .unwrap();

                let mut statement = self
                    .database
                    .prepare("INSERT INTO aura_finalized_authorities(idx, public_key) VALUES(?, ?)")
                    .unwrap();
                for (index, item) in finalized_authorities_list.clone().enumerate() {
                    statement
                        .bind(1, i64::from_ne_bytes(index.to_ne_bytes()))
                        .unwrap();
                    statement.bind(2, &item.public_key[..]).unwrap();
                    statement.next().unwrap();
                    statement.reset().unwrap();
                }
            }
            chain_information::ChainInformationConsensusRef::Babe {
                slots_per_epoch,
                finalized_next_epoch_transition,
                finalized_block_epoch_information,
            } => {
                super::meta_set_number(
                    &self.database,
                    "babe_slots_per_epoch",
                    slots_per_epoch.get(),
                )
                .unwrap();
                super::meta_set_blob(
                    &self.database,
                    "babe_finalized_next_epoch",
                    &encode_babe_epoch_information(finalized_next_epoch_transition.clone())[..],
                )
                .unwrap();

                if let Some(finalized_block_epoch_information) = finalized_block_epoch_information {
                    super::meta_set_blob(&self.database, "babe_finalized_epoch", &encode_babe_epoch_information(
            finalized_block_epoch_information.clone(),
        )[..]).unwrap();
                }
            }
        }

        super::flush(&self.database)?;

        Ok(SqliteFullDatabase {
            database: parking_lot::Mutex::new(self.database),
        })
    }
}
