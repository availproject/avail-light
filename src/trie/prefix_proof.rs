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

//! Scanning, through trie proofs, the list of all keys that share a certain prefix.
//!
//! This module is a helper whose objective is to find out the list of all keys that start with
//! a certain prefix by performing storage proofs.
//!
//! The total number of storage proofs required is equal to the maximum depth of the tree below
//! the requested prefix, plus one. For example, if a tree has the nodes `[1, 5]`, `[1, 5, 8, 9]`,
//! and `[1, 5, 8, 9, 2]`, then four queries are necessary to find all the keys whose prefix
//! is `[1]`.

// TODO: usage example

// TODO: this code is entirely untested; no idea if it works

use super::{nibble, proof_verify};

use alloc::{vec, vec::Vec};
use core::{fmt, iter};

/// Configuration to pass to [`prefix_scan`].
pub struct Config<'a> {
    /// Prefix that all the keys must share.
    pub prefix: &'a [u8],

    /// Merkle value (or node value) of the root node of the trie.
    ///
    /// > **Note**: The Merkle value and node value are always the same for the root node.
    pub trie_root_hash: [u8; 32],
}

/// Start a new scanning process.
pub fn prefix_scan(config: Config<'_>) -> PrefixScan {
    PrefixScan {
        trie_root_hash: config.trie_root_hash,
        next_queries: vec![nibble::bytes_to_nibbles(config.prefix.iter().copied()).collect()],
        final_result: Vec::with_capacity(32),
    }
}

/// Scan of a prefix in progress.
pub struct PrefixScan {
    trie_root_hash: [u8; 32],
    // TODO: we have lots of Vecs here; maybe find a way to optimize
    next_queries: Vec<Vec<nibble::Nibble>>,
    // TODO: we have lots of Vecs here; maybe find a way to optimize
    final_result: Vec<Vec<u8>>,
}

impl PrefixScan {
    /// Returns the list of keys whose storage proof must be queried.
    pub fn requested_keys(
        &'_ self,
    ) -> impl Iterator<Item = impl Iterator<Item = nibble::Nibble> + '_> + '_ {
        self.next_queries.iter().map(|l| l.iter().copied())
    }

    /// Injects the proof presumably containing the keys returned by [`PrefixScan::requested_keys`].
    ///
    /// Returns an error if the proof is invalid. In that case, `self` isn't modified.
    pub fn resume<'a>(
        mut self,
        proof: impl Iterator<Item = &'a [u8]> + Clone + 'a,
    ) -> Result<ResumeOutcome, (Self, proof_verify::Error)> {
        // The entire body is executed as long as verifying at least one proof succeeds.
        for is_first_iteration in iter::once(true).chain(iter::repeat(false)) {
            // Filled with the queries to perform at the next iteration.
            // Capacity assumes a maximum of 2 children per node on average. This value was chosen
            // completely arbitrarily.
            let mut next = Vec::with_capacity(self.next_queries.len() * 2);

            // True if any proof verification has succeeded during this iteration.
            // Controls whether we continue iterating.
            let mut any_successful_proof = false;

            for query in &self.next_queries {
                let info = match proof_verify::trie_node_info(proof_verify::TrieNodeInfoConfig {
                    requested_key: query.iter().cloned(),
                    trie_root_hash: &self.trie_root_hash,
                    proof: proof.clone(),
                }) {
                    Ok(info) => info,
                    Err(err) if is_first_iteration => return Err((self, err)),
                    Err(_) => continue,
                };

                any_successful_proof = true;

                if info.node_value.is_some() {
                    // Trie nodes with a value are always aligned to "bytes-keys". In other words, the
                    // number of nibbles is always even.
                    debug_assert_eq!(query.len() % 2, 0);
                    let key = query
                        .chunks(2)
                        .map(|n| (u8::from(n[0]) << 4) | u8::from(n[1]))
                        .collect::<Vec<_>>();

                    // Insert in final results, making sure we check for duplicates.
                    debug_assert!(!self.final_result.iter().any(|n| *n == key));
                    self.final_result.push(key);
                }

                for child_nibble in info.children.next_nibbles() {
                    let mut next_query = Vec::with_capacity(query.len() + 1);
                    next_query.extend_from_slice(&query);
                    next_query.push(child_nibble);
                    next.push(next_query);
                }
            }

            // Finished when nothing more to request.
            if next.is_empty() {
                return Ok(ResumeOutcome::Success {
                    keys: self.final_result,
                });
            }

            // Update `next_queries` for the next iteration.
            self.next_queries = next;

            if !any_successful_proof {
                break;
            }
        }

        Ok(ResumeOutcome::InProgress(self))
    }
}

impl fmt::Debug for PrefixScan {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("PrefixScan").finish()
    }
}

/// Outcome of calling [`PrefixScan::resume`].
#[derive(Debug)]
pub enum ResumeOutcome {
    /// Scan must continue with the next storage proof query.
    InProgress(PrefixScan),
    /// Scan has succeeded.
    Success {
        /// List of keys with the requested prefix.
        keys: Vec<Vec<u8>>,
    },
}
