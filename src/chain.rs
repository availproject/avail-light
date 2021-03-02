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

//! Data structures describing a chain of blocks.
//!
//! A chain of blocks is composed of two parts:
//!
//! - A list of finalized blocks. Finalized blocks are blocks that are forever part of the chain
//! and can never be reverted.
//! - A tree of non-finalized blocks built on top of the last finalized block.
//!
//! When a block first appears it is verified and, on success, added to the tree of
//! non-finalized blocks. Later, this block might get finalized. When a block is finalized, all
//! the blocks that are not one of its ancestors or descendants is entirely discarded.
//!
//! Example chain:
//!
//! ```ignore
//!                             +-> #5
//!                             |
//!                      +-> #4 +-> #5
//!                      |
//! #0 +> #1 +> #2 +> #3 +-> #4 +-> #5 +> #6
//! ```
//!
//! In this example, #3 is the latest finalized block. Before and including #3, the chain is
//! always a simple list. After #3, the chain becomes a tree.
//!
//! > **Note**: This example is exaggerated, and in most situations the non-finalized blocks also
//! >           form a simple list with a few extra individual leaves.
//!
//! Amongst the non-finalized blocks, one block is chosen as the *best* block. When authoring
//! blocks, the *best block* is the one upon new blocks will be built upon. When finalizing
//! blocks, the *best block* is the one that will be voted for finalization. If there isn't any
//! non-finalized block, the latest finalized block is also the best block.

pub mod blocks_tree;
pub mod chain_information;
pub mod fork_tree;
