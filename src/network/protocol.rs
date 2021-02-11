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

//! Substrate/Polkadot-specific protocols on top of libp2p.
//!
//! Libp2p provides a framework on top of which higher-level, domain-specific protocols can be
//! used. This module contains the domain-specific protocols specific to Substrate and Polkadot.
//!
//! This module only provides the tools to encode/decode messages.

// TODO: expand docs

// Implementation note: each protocol goes into a different sub-module whose content is
// re-exported here.

mod block_announces;
mod block_request;
mod call_proof;
mod grandpa;
mod grandpa_warp_sync;
mod identify;
mod storage_proof;

pub use self::block_announces::*;
pub use self::block_request::*;
pub use self::call_proof::*;
pub use self::grandpa::*;
pub use self::grandpa_warp_sync::*;
pub use self::identify::*;
pub use self::storage_proof::*;

// Protobuf schemas are gathered here.
mod schema {
    include!(concat!(env!("OUT_DIR"), "/api.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/api.v1.light.rs"));
    include!(concat!(env!("OUT_DIR"), "/structs.rs"));
}

/// Error while decoding the protobuf encoding.
#[derive(Debug, derive_more::Display)]
#[display(fmt = "{}", _0)]
pub struct ProtobufDecodeError(prost::DecodeError);
