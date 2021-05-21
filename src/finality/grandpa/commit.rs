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

//! GrandPa commits contain a proof of the finality of a block.
//!
//! In order to finalize a block, GrandPa authorities must emit votes, also called pre-commits.
//! We consider a block finalized when more than two thirds of the authorities have voted for that
//! block (or one of its descendants) to be finalized.
//!
//! A commit contains all the votes that have been used to prove that a certain block has been
//! finalized.
//!
//! Contrary to justifications, commits don't include the block headers of the forks that have
//! been voted on. It is expected for the node to be aware of these headers through a separate
//! mechanism.
//!
//! When a commit is received from a third party, it must first be verified. See the [`verify`]
//! module.

pub mod decode;
pub mod verify;
