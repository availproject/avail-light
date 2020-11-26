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

/*********************************************************
**   Prototype for a rewrite of the networking code     **
**   Not ready yet                                      **
*********************************************************/

pub mod connection;
pub mod discovery;
pub mod leb128;
pub mod libp2p;
#[doc(inline)]
pub use parity_multiaddr as multiaddr;
pub mod peer_id;
pub mod peerset;
pub mod protocol;
pub mod service;
pub mod with_buffers;

pub use parity_multiaddr::Multiaddr;
pub use peer_id::PeerId;
