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

//! Client for Polkadot and Substrate-compatible chains.
//!
//! # Overview of a blockchain
//!
//! A blockchain is, in its essence, a distributed and decentralized key-value database. The
//! principle of a blockchain is to make it possible for any participant to perform modifications
//! to this database, and for all participants to eventually agree on the current state of said
//! database.
//!
//! In Polkadot and Substrate-compatible chains, the state of this database is referred to as
//! "the storage". The storage can be seen more or less as a very large `HashMap`.
//!
//! A blockchain therefore consists in three main things:
//!
//! - The initial state of the storage at the moment when the blockchain starts.
//! - A list of blocks, where each block represents a group of modifications performed to the
//! storage.
//! - A peer-to-peer network of clients connected to each other and exchanging information such
//! as newly-produced blocks.
//!
//! Blocks are built on top of each other, forming a sequential list of modifications to the
//! storage on top of its initial state.
//!
//! ## Blocks
//!
//! A block primarily consists in three properties:
//!
//! - A parent block, referred to by its hash.
//! - An ordered list of **extrinsics**, representing a state change about the storage. An
//! extrinsic can be either a **transaction** or an **intrisic**.
//! - A list of **digest log items**, which include information necessary to verify the
//! authenticity of the block, such as a cryptographic signature of the block made by its author.
//!
//! In order to make abstractions easier, there also exists what is called the **genesis block**,
//! or block number 0. It doesn't have any parent, extrinsic, or digest item.
//!
//! From these three block properties, the following other properties can be derived:
//!
//! - The **hash** of the block. This is a unique 32 bytes identifier obtained by hashing all the
//! block's information together in a specific way.
//! - The **block number**. It is equal to the parent's block number plus one, or equal to zero
//! for the genesis block
//! - The state of the storage. It consists in the state of the storage of the parent block, on
//! top of which the block's extrinsics have been applied. The state of the storage of the genesis
//! block is the initial state.
//!
//! > **Note**: Not all these properties are held in memory or even on disk. For instance, the
//! >           state of the storage is quite large and holding a full version of the storage of
//! >           every single block in the chain would be unrealistic.
//!
//! ## Trie
//!
//! The **trie** is a data structure that plays an important role in the way a blockchain
//! functions.
//!
//! It consists in a tree of nodes associated with a key, some of these nodes containing a value.
//! Each node is associated with a hash called the **Merkle value** that encompasses the Merkle
//! values of the node's children. The Merkle value of the root node of the tree is designated
//! as "the Merkle trie root" or "the trie root".
//!
//! See the [`trie`] module for more details.
//!
//! ## Block headers
//!
//! In practical terms, when a block needs to be stored or transferred between machines, it
//! is split in two parts: a **header** and a **body**. The body of a block simply consists in its
//! list of extrinsics.
//!
//! > **Note**: The body of a block and the list of extrinsics of a block are the exact same
//! >           thing, and these two designations can be used interchangeably.
//!
//! A block's header contains the following:
//!
//! - The hash of the parent of the block.
//! - The block number.
//! - The **state trie root**, which consists in the trie root of all the keys and values of the
//! storage of this block.
//! - The **extrinsics trie root**, which consists in the Merkle root of a trie containing the
//! extrinsics of the block.
//! - The list of digest log items.
//!
//! The hash of a block consists in the blake2 hash of its header.
//!
//! See the [`header`] module for more information.
//!
//! ## Runtime
//!
//! The state of the storage of each block is mostly opaque from the client's perspective. There
//! exists, however, a few hardcoded keys, the most important one being
//! `[0x3a, 0x63, 0x6f, 0x64, 0x65]` (which is the ASCII encoding of the string `:code`). The
//! value associated with this key must always be a [WebAssembly](https://webassembly.org/) binary
//! code called the **runtime**.
//!
//! This WebAssembly binary code must obey a certain ABI and is responsible for the following
//! actions:
//!
//! - Verifying whether a block has been correctly created by its author.
//! - Providing the list of modifications to the storage performed by a block, given its header
//! and body.
//! - Generating the header of newly-created blocks.
//! - Validating transactions received from third-parties, in order to later be potentially
//! included in a block.
//! - Providing the tools necessary to create transactions. See the [`metadata`] module.
//!
//! In other words, the runtime is responsible for all the actual *logic* behind the chain being
//! run. For instance, when performing a transfer of tokens between one account and another, it is
//! the runtime WebAssembly code that verifies that the balances are correct and updates them.
//!
//! Since applying an extrinsic modifies the state of the storage, it is possible for an
//! extrinsic to modify this WebAssembly binary code. This is called a **runtime upgrade**. In
//! other words, any block can potentially modify the logic of the chain.
//!
//! See also the [`executor`] module for more information.
//!
//! ## Forks and finalization
//!
//! While blocks are built on top of each other, they don't necessarily form one single chain. It
//! is possible for multiple different blocks to have the same parent, thereby forming multiple
//! chains. This is called a **fork**.
//!
//! When multiple chains fly around in the network, each node select one chain they consider as
//! the "best". However, because of latency, disconnects, or similar reasons, it is possible for
//! a node to not be immediately aware of the existence of a chain that it would consider as a
//! better chain than its current best. Later, when this node learns about this better chain, it
//! will need to perform a **reorg** (short for *reorganization*), where blocks that were part of
//! the best chain no longer are.
//!
//! In order to avoid troublesome situations arising from reorgs, Substrate/Polkadot provides the
//! concept of **finality**. Once a block has been finalized, it is guaranteed to always be part
//! of the best chain. By extension, the parent of a finalized block is always finalized as well.
//! The genesis block of a chain is by definition is always finalized.
//!
//! As such, a chain of blocks is composed of two parts: one **finalized** part (usually the
//! longest), and one non-finalized part. While the finalized part is a single linear list of
//! blocks, the non-finalized part consists in a *directed rooted tree*.
//!
//! In order to finalize a block, Substrate/Polkadot nodes use the **GrandPa** algorithm. Nodes
//! that are authorized to do so by the runtime emit votes on the peer-to-peer network. When two
//! thirds or more of the authorized nodes have voted for a specific block to be finalized, it
//! effectively becomes finalized.
//!
//! After these votes have been collected, they are collected in what is called a
//! **justification**. This justification can later be requested by nodes who might not have
//! received all the votes, or for example if they were offline.
//!
//! See the [`chain`] and [`finality`] modules for more information.
//!
//! # Usage
//!
//! This library intentionally doesn't provide any ready-to-use blockchain client. Instead, it
//! provides tools that can be combined together in order to create a client.
//!
//! Components that are most certainly needed are:
//!
//! - Connectivity to a peer-to-peer network. See the [`network`] module.
//! - A persistent storage for the blocks. See the [`database`] module.
//! - A state machine that holds information about the state of the chain and verifies the
//! authenticity and/or correctness of blocks received from the network. See the [`sync`]
//! module.
//!
//! Optionally:
//!
//! - The capacity to author new blocks. This isn't implemented as of the writing of this
//! documentation.
//! - A JSON-RPC client, in order to put a convenient-to-use UI on top of the client. See the
//! [`json_rpc`] module.
//! - TODO: telemetry
//!

// The library part of `smoldot` should as pure as possible and shouldn't rely on any environment
// such as a file system, environment variables, time, randomness, etc.
#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![deny(broken_intra_doc_links)]
#![deny(unused_crate_dependencies)]

extern crate alloc;

pub mod author;
pub mod chain;
pub mod chain_spec;
pub mod database;
pub mod executor;
pub mod finality;
pub mod header;
pub mod informant;
pub mod json_rpc;
pub mod libp2p;
pub mod metadata;
pub mod network;
pub mod sync;
pub mod trie;
pub mod verify;

mod util;

/// Builds the header of the genesis block, from the values in storage.
///
/// # Example
///
/// ```no_run
/// # let chain_spec_json: &[u8] = b"";
/// let chain_spec = smoldot::chain_spec::ChainSpec::from_json_bytes(chain_spec_json)
///     .unwrap();
/// let genesis_block_header =
///     smoldot::calculate_genesis_block_header(chain_spec.genesis_storage());
/// println!("{:?}", genesis_block_header);
/// ```
pub fn calculate_genesis_block_header<'a>(
    genesis_storage: impl Iterator<Item = (&'a [u8], &'a [u8])> + Clone,
) -> header::Header {
    let state_root = {
        let mut calculation = trie::calculate_root::root_merkle_value(None);

        loop {
            match calculation {
                trie::calculate_root::RootMerkleValueCalculation::Finished { hash, .. } => {
                    break hash
                }
                trie::calculate_root::RootMerkleValueCalculation::AllKeys(keys) => {
                    calculation =
                        keys.inject(genesis_storage.clone().map(|(k, _)| k.iter().cloned()));
                }
                trie::calculate_root::RootMerkleValueCalculation::StorageValue(val) => {
                    let value = genesis_storage
                        .clone()
                        .find(|(k, _)| itertools::equal(k.iter().copied(), val.key()))
                        .map(|(_, v)| v);
                    calculation = val.inject(value);
                }
            }
        }
    };

    header::Header {
        parent_hash: [0; 32],
        number: 0,
        state_root,
        extrinsics_root: trie::empty_trie_merkle_value(),
        digest: header::DigestRef::empty().into(),
    }
}
