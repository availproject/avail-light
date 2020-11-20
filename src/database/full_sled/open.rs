// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
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

use super::{
    encode_aura_authorities_list, encode_babe_epoch_information, encode_grandpa_authorities_list,
    AccessError, SledError, SledFullDatabase,
};
use crate::{chain::chain_information, header, util};

use sled::Transactional as _;
use std::path::Path;

/// Opens the database using the given [`Config`].
///
/// Note that this doesn't return a [`SledFullDatabase`], but rather a [`DatabaseOpen`].
pub fn open(config: Config) -> Result<DatabaseOpen, SledError> {
    let database = sled::Config::default()
        // We put a `/v1/` behind the path in case we change the schema.
        .path(config.path.join("v1"))
        .use_compression(true)
        .open()
        .map_err(SledError)?;

    let meta_tree = database.open_tree(b"meta").map_err(SledError)?;
    let block_hashes_by_number_tree = database
        .open_tree(b"block_hashes_by_number")
        .map_err(SledError)?;
    let block_headers_tree = database.open_tree(b"block_headers").map_err(SledError)?;
    let block_bodies_tree = database.open_tree(b"block_bodies").map_err(SledError)?;
    let block_justifications_tree = database
        .open_tree(b"block_justifications")
        .map_err(SledError)?;
    let finalized_storage_top_trie_tree = database
        .open_tree(b"finalized_storage_top_trie")
        .map_err(SledError)?;
    let non_finalized_changes_keys_tree = database
        .open_tree(b"non_finalized_changes_keys")
        .map_err(SledError)?;
    let non_finalized_changes_tree = database
        .open_tree(b"non_finalized_changes")
        .map_err(SledError)?;

    Ok(if meta_tree.get(b"best").map_err(SledError)?.is_some() {
        DatabaseOpen::Open(SledFullDatabase {
            block_hashes_by_number_tree,
            meta_tree,
            block_headers_tree,
            block_bodies_tree,
            block_justifications_tree,
            finalized_storage_top_trie_tree,
            non_finalized_changes_keys_tree,
            non_finalized_changes_tree,
        })
    } else {
        DatabaseOpen::Empty(DatabaseEmpty {
            block_hashes_by_number_tree,
            meta_tree,
            block_headers_tree,
            block_bodies_tree,
            block_justifications_tree,
            finalized_storage_top_trie_tree,
            non_finalized_changes_keys_tree,
            non_finalized_changes_tree,
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
    Open(SledFullDatabase),

    /// Either a database has just been created, or there existed a database but it is empty.
    ///
    /// > **Note**: The situation where a database existed but is empty can happen if you have
    /// >           previously called [`open`] then dropped the [`DatabaseOpen`] object without
    /// >           filling the newly-created database with data.
    Empty(DatabaseEmpty),
}

/// An open database. Holds file descriptors.
pub struct DatabaseEmpty {
    /// See the similar field in [`SledFullDatabase`].
    meta_tree: sled::Tree,

    /// See the similar field in [`SledFullDatabase`].
    block_hashes_by_number_tree: sled::Tree,

    /// See the similar field in [`SledFullDatabase`].
    block_headers_tree: sled::Tree,

    /// See the similar field in [`SledFullDatabase`].
    block_bodies_tree: sled::Tree,

    /// See the similar field in [`SledFullDatabase`].
    block_justifications_tree: sled::Tree,

    /// See the similar field in [`SledFullDatabase`].
    finalized_storage_top_trie_tree: sled::Tree,

    /// See the similar field in [`SledFullDatabase`].
    non_finalized_changes_keys_tree: sled::Tree,

    /// See the similar field in [`SledFullDatabase`].
    non_finalized_changes_tree: sled::Tree,
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
    ) -> Result<SledFullDatabase, AccessError> {
        let chain_information = chain_information.into();

        // Because the closure below might potentially be run multiple times, we compute some
        // information ahead of time.
        let scale_encoded_finalized_block_body = {
            let mut val = Vec::new();
            val.extend_from_slice(
                util::encode_scale_compact_usize(finalized_block_body.len()).as_ref(),
            );
            for item in finalized_block_body {
                val.extend_from_slice(util::encode_scale_compact_usize(item.len()).as_ref());
                val.extend_from_slice(item);
            }
            val
        };

        let finalized_block_hash = chain_information.finalized_block_header.hash();

        let scale_encoded_finalized_block_header = chain_information
            .finalized_block_header
            .scale_encoding()
            .fold(Vec::new(), |mut a, b| {
                a.extend_from_slice(b.as_ref());
                a
            });

        // Try to apply changes. This is done atomically through a transaction.
        let result = (
            &self.block_hashes_by_number_tree,
            &self.block_headers_tree,
            &self.block_bodies_tree,
            &self.block_justifications_tree,
            &self.finalized_storage_top_trie_tree,
            &self.meta_tree,
        )
            .transaction(
                move |(
                    block_hashes_by_number,
                    block_headers,
                    block_bodies,
                    block_justifications,
                    finalized_storage_top_trie_tree,
                    meta,
                )| {
                    for (key, value) in finalized_block_storage_top_trie_entries.clone() {
                        finalized_storage_top_trie_tree.insert(key, value)?;
                    }

                    block_hashes_by_number.insert(
                        &chain_information
                            .finalized_block_header
                            .number
                            .to_be_bytes()[..],
                        &finalized_block_hash[..],
                    )?;

                    block_headers.insert(
                        &finalized_block_hash[..],
                        &scale_encoded_finalized_block_header[..],
                    )?;
                    block_bodies.insert(
                        &finalized_block_hash[..],
                        &scale_encoded_finalized_block_body[..],
                    )?;
                    if let Some(finalized_block_justification) = &finalized_block_justification {
                        block_justifications.insert(
                            &finalized_block_hash[..],
                            &finalized_block_justification[..],
                        )?;
                    }
                    meta.insert(b"best", &finalized_block_hash[..])?;
                    meta.insert(
                        b"finalized",
                        &chain_information
                            .finalized_block_header
                            .number
                            .to_be_bytes()[..],
                    )?;

                    match &chain_information.finality {
                        chain_information::ChainInformationFinalityRef::Grandpa {
                            finalized_triggered_authorities,
                            after_finalized_block_authorities_set_id,
                            finalized_scheduled_change,
                        } => {
                            meta.insert(
                                b"grandpa_authorities_set_id",
                                &after_finalized_block_authorities_set_id.to_be_bytes()[..],
                            )?;
                            meta.insert(
                                b"grandpa_triggered_authorities",
                                encode_grandpa_authorities_list(
                                    header::GrandpaAuthoritiesIter::new(
                                        finalized_triggered_authorities,
                                    ),
                                ),
                            )?;

                            if let Some((height, list)) = finalized_scheduled_change {
                                meta.insert(
                                    b"grandpa_scheduled_target",
                                    &height.to_be_bytes()[..],
                                )?;
                                meta.insert(
                                    b"grandpa_scheduled_authorities",
                                    encode_grandpa_authorities_list(
                                        header::GrandpaAuthoritiesIter::new(list),
                                    ),
                                )?;
                            }
                        }
                    }

                    match &chain_information.consensus {
                        chain_information::ChainInformationConsensusRef::Aura {
                            finalized_authorities_list,
                            slot_duration,
                        } => {
                            meta.insert(
                                b"aura_slot_duration",
                                &slot_duration.get().to_be_bytes()[..],
                            )?;
                            meta.insert(
                                b"aura_finalized_authorities",
                                encode_aura_authorities_list(finalized_authorities_list.clone()),
                            )?;
                        }
                        chain_information::ChainInformationConsensusRef::Babe {
                            slots_per_epoch,
                            finalized_next_epoch_transition,
                            finalized_block_epoch_information,
                        } => {
                            meta.insert(
                                b"babe_slots_per_epoch",
                                &slots_per_epoch.get().to_be_bytes()[..],
                            )?;
                            meta.insert(
                                b"babe_finalized_next_epoch",
                                encode_babe_epoch_information(
                                    finalized_next_epoch_transition.clone(),
                                ),
                            )?;
                            if let Some(finalized_block_epoch_information) =
                                finalized_block_epoch_information
                            {
                                meta.insert(
                                    b"babe_finalized_epoch",
                                    encode_babe_epoch_information(
                                        finalized_block_epoch_information.clone(),
                                    ),
                                )?;
                            }
                        }
                    }

                    Ok(())
                },
            );

        match result {
            Ok(()) => {}
            Err(sled::transaction::TransactionError::Abort(())) => unreachable!(),
            Err(sled::transaction::TransactionError::Storage(err)) => {
                return Err(AccessError::Database(SledError(err)))
            }
        };

        Ok(SledFullDatabase {
            block_hashes_by_number_tree: self.block_hashes_by_number_tree,
            meta_tree: self.meta_tree,
            block_headers_tree: self.block_headers_tree,
            block_bodies_tree: self.block_bodies_tree,
            block_justifications_tree: self.block_justifications_tree,
            finalized_storage_top_trie_tree: self.finalized_storage_top_trie_tree,
            non_finalized_changes_keys_tree: self.non_finalized_changes_keys_tree,
            non_finalized_changes_tree: self.non_finalized_changes_tree,
        })
    }
}
