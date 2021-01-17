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

//! Mechanisms related to discovering nodes that are part of a certain overlay network.
//!
//! # Synopsis
//!
//! Internet is a network of computers. The list of machines that are running a Substrate/Polkadot
//! client is a subset of the Internet. This module provides the mechanisms that help finding out
//! which machines (more precisely, which IP address and port) run a Substrate/Polkadot client.
//!
//! # Details
//!
//! Substrate-compatible chains use two discovery mechanisms:
//!
//! - **Kademlia**. All nodes that belong to a certain chain are encouraged to participate in the
//! Kademlia [DHT](https://en.wikipedia.org/wiki/Distributed_hash_table) of that chain, making it
//! possible to ask a node for the nodes it has learned about in the past.
//! - **mDNS**. By broadcasting UDP packets over the local network, one can find other nodes
//! running the same chain. // TODO: not implemented yet
//!
//! The main discovery mechanism is the DHT. In order to bootstrap this mechanism, a list of nodes
//! known to always be part of the chain is hardcoded in the chain specifications. These nodes are
//! called **boostrap nodes**.
//!

pub mod kademlia;
// TODO: pub mod mdns;
