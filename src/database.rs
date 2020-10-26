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

//! Persistent data storage.
//!
//! This module contains sub-modules that provide different means of storing data in a
//! persistent way.

pub mod local_storage_light;

// TODO: when implementing an actual database for the full node, here is some inspiration:
// - https://github.com/paritytech/substrate-lite/blob/e53148ca8af7450e9995e960279b6c8925a37663/src/database/sled.rs
// - https://github.com/paritytech/substrate-lite/blob/e53148ca8af7450e9995e960279b6c8925a37663/bin/full-node.rs#L717-L758
// - https://github.com/paritytech/substrate-lite/blob/8f41c567d8dc644e2166f06f0bd5e5dfca4fddd3/src/lib.rs#L171-L208
