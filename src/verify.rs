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

//! Methods that verify whether a block is correct.
//!
//! # Description
//!
//! When a block is received from the network (or any other untrusted source), this block should
//! first be verified.
//!
//! Two possibilities are offered:
//!
//! - Verifying the authenticity of the block, using only its header and the ancestor blocks'
//! headers.
//! - Verifying both the validity and authenticity of the block, using.the block's header and
//! body, the headers of the ancestors, and storage of its parent.
//!
//! > **Note**: The body of a block consists in the list of its extrinsics. An extrinsic is
//! >           either an intrinsic (added by the block's author) or a transaction.
//!
//! > **Note**: The option of verifying only the authenticity of a block, and not its validity,
//! >           is normally for situations where a block's body or the parent block's storage is
//! >           not known. If the parent storage and the block's body are known, it is encouraged
//! >           that the block's validity be checked as well.
//!
//! Verifying the authenticity of a block consists in verifying the consensus digest logs found
//! in its header in order to make sure that the author of the block was authorized to produce it.
//! Whether the block has been correctly crafted isn't and cannot be verified with only the
//! header.
//!
//! Verifying the block's validity consists, in addition to verifying its header, in *executing*
//! the block. This involves calling the `Core_execute_block` runtime function, passing as
//! parameter the header and body of the block, and providing access to the storage of the parent
//! block. The runtime verifies that the header and body are correct.
//!
//! # Trust
//!
//! Verifying only the authenticity of a block, and not its validity, means that the author of
//! the block is trusted to have generated a correct block.
//!
//! Situations where the block author had the right to generate a block but created an invalid
//! block would result in a loss of money for this author, and are therefore most likely the
//! consequence of a bug (either on the side of the block author or on the side of the local
//! verification code).
//!
//! However, if the blocks' validity isn't verified, it is possible for a block author to target
//! a weak implementation by directly connecting to it and announcing invalid blocks directly
//! to it. Consequently, if blocks are for example used to verify whether a payment has gone
//! through, it is preferable to wait for blocks to have been finalized.
//!
//! No matter the validity of a block, keep in mind that a reorg (a "reorganization") might
//! happen, and many valid blocks don't get finalized.
//!

mod execute_block;

pub mod babe;
pub mod header_body;
pub mod header_only;
