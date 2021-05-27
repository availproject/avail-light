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

//! Transactions handling.
//!
//! This module contains everything related to the handling of transactions.
//!
//! # Overview
//!
//! From a thousand miles perspective, the process of including a transaction in the chain is as
//! follows:
//!
//! - The transaction gets built, in other words the bytes that encode the transaction are
//! generated. This can be done for example through a UI, through an offchain worker, or other. A
//! transaction can be either signed (i.e. have a signature attached to it) or unsigned, depending
//! on the action to be performed. A balance transfer, for example, generally always requires a
//! signature.
//!
//! - The transaction is then processed by a node, generally the node that belongs to the author
//! of the transaction, where it is *validated* by passing it as parameter to a runtime entry
//! point. See the [`validate`] module for more info.
//!
//! - If the validation process indicates that the transaction can be propagated, it is then sent
//! over the peer-to-peer network to other peers. Each node that receives the transaction
//! similarly validates it and relays it to its own peers.
//!
//! - When a block is authored, the node that authors it picks from its pool of validated
//! transactions the ones to include in the block. The logic under which transactions are picked
//! and their ordering depends on the output of the validation. The *body* of the newly-authored
//! block is made of (but not exclusively) the transactions that have been included in said block.
//!
//! - When a node receives a new block, the transactions in the pool are re-validated against
//! this block. The validation function found in the runtime is expected to return an error if
//! the transaction in question is already present in the chain and should not be included again.
//!
//! ## About duplicate transactions
//!
//! In practice, the vast majority of the time, a given transaction can only ever be inserted
//! once in a chain, as the vast majority of transactions include some sort of nonce in their
//! encoding. In other words, once a transaction has been included in a block, trying to
//! re-validate that same transaction against that same block or any of its descendants will
//! return an error.
//!
//! Once a transaction has been included in a block, it becomes known to everyone. If the runtime
//! *always* considered a certain transaction as valid, then this transaction could be repeatedly
//! included in the chain over and over again (and by anyone, since the transaction is public).
//! On a higher level, the logic of such a transaction couldn't possibly make sense, as the reason
//! why transactions exist in the first place is to modify the state of the chain. In other words,
//! a runtime that always considers a certain transaction to be valid doesn't make sense on a
//! higher level.
//!
//! With that in mind, a Substrate/Polkadot client implementation is allowed to make two
//! assumptions:
//!
//! - After a transaction has been included in a block, trying to re-validate it against that
//! same block or any of its descendants always yields an error.
//! - The same transaction can't possibly be included twice in the same block.
//!
//! > **Note**: The official Substrate client, in particular, tracks transactions by their hash,
//! >           and removes from the pool any transaction from the pool after it has been included
//! >           in the best chain, without even attempting to re-validate it (but re-inserts
//! >           transactions in the pool if a block gets reverted).
//!
//! However, this is in theory not the correct behaviour. A legitimate (but in practice very
//! uncommon) use-case for runtimes is to consider a transaction as invalid right after it has
//! been included, but then valid again some time in the future.
//!
//! The specific use case is considered to be uncommon enough to not explicitly be taken into
//! account. The client considers that once a transaction has been considered invalid against a
//! certain block B, it will forever remain considered as invalid on any descendant of B, but a
//! client also attempts to not cache that information for *too long* through heuristics.
//!

pub mod validate;
