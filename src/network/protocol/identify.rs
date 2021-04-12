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

//! The identify protocol is a request-response protocol.
//!
//! The request's body is empty. Contrary to other request-response protocols, it doesn't even
//! contain a message length. As soon as the protocol has been negotiated, the other side should
//! send back the response.
//!
//! The response's body consists in various useful general-purpose information about the node.
//! See [`IdentifyResponse`] for details.
//!
//! The two most important fields are [`IdentifyResponse::listen_addrs`] and
//! [`IdentifyResponse::observed_addr`]. They are necessary in order for nodes to discover their
//! public address, and in order to insert peers in the Kademlia k-buckets.
//!
//! See also [the official specifications](https://github.com/libp2p/specs/tree/69e57d59dc5d59d3979d79842b577ec2c483f7fa/identify).

use super::schema;
use crate::libp2p::{peer_id::PublicKey, Multiaddr};

use alloc::{borrow::ToOwned as _, vec::Vec};
use core::iter;
use prost::Message as _;

/// Description of a response to an identify request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifyResponse<'a, TLaIter, TProtoIter> {
    pub protocol_version: &'a str,
    pub agent_version: &'a str,
    /// Ed25519 public key of the local node.
    pub ed25519_public_key: &'a [u8; 32],
    /// List of addresses the local node is listening on. This should include first and foremost
    /// addresses that are publicly-reachable.
    pub listen_addrs: TLaIter,
    /// Address of the sender of the identify request, as seen from the receiver.
    pub observed_addr: &'a Multiaddr,
    /// Names of the protocols supported by the local node.
    pub protocols: TProtoIter,
}

/// Builds the bytes corresponding to a block request.
pub fn build_identify_response<'a>(
    config: IdentifyResponse<
        'a,
        impl Iterator<Item = &'a Multiaddr>,
        impl Iterator<Item = &'a str>,
    >,
) -> impl Iterator<Item = impl AsRef<[u8]>> {
    // Note: while the API of this function allows for a zero-cost implementation, the protobuf
    // library doesn't permit to avoid allocations.

    let protobuf = schema::Identify {
        protocol_version: Some(config.protocol_version.to_owned()),
        agent_version: Some(config.agent_version.to_owned()),
        public_key: Some(PublicKey::Ed25519(*config.ed25519_public_key).to_protobuf_encoding()),
        listen_addrs: config.listen_addrs.map(|addr| addr.to_vec()).collect(),
        observed_addr: Some(config.observed_addr.to_vec()),
        protocols: config.protocols.map(|p| p.to_owned()).collect(),
    };

    let request_bytes = {
        let mut buf = Vec::with_capacity(protobuf.encoded_len());
        protobuf.encode(&mut buf).unwrap();
        buf
    };

    iter::once(request_bytes)
}
