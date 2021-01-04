// Substrate-lite
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

//! Module containing everything related to the processing of a single libp2p connection.
//!
//! # Overview
//!
//! Libp2p connections use TCP sockets provided by the operating system.
//!
//! > **Note**: Explaining the TCP/IP protocol is out of scope of this documentation. See for
//! >           example [the Wikipedia page](https://en.wikipedia.org/wiki/Transmission_Control_Protocol)
//! >           for more information.
//!
//! Once a TCP connection has been established, the *handshake* phase starts. This handshake
//! is described in details in the documentation [`handshake`] module.
//!
//! The handshake consists in negotiating with the remote an encryption layer, thanks to which all
//! communications are encrypted, and a multiplexing layer, thanks to which the stream of data can
//! be split into a multitude of **substreams**.
//!
//! After the handshake is over, the connection is now in the *established* phase. See the
//! documentation of the [`established`] module for more details.
//!
//! Once in the established phase, substreams can be opened either by the local endpoint or by the
//! remote endpoint. When a substream is opened, a protocol is first negotiated in order to
//! determine the purpose of this substream. Two kinds of protocols, and therefore two kinds of
//! substreams, are supported:
//!
//! - So-called "request-response" substreams. After the protocol is negotiated, the opening side
//! sends a single message containing a request, and the other endpoint sends back a single
//! message containing a response. The substream is then closed.
//! - So-called "notifications" substreams. After the protocol is negotiated, the opening side
//! sends a single handshake message. The other endpoint must then either immediately close the
//! substream to reject the handshake, or send back a handshake in order to accept it. After the
//! handshake is accepted, the opening side can send an unbounded number of individual messages.
//! Only the opening side is allowed to send messages on any given notifications substream.
//!
//! These two kinds of substreams serve as a base framework. The exact nature of these
//! request-responses and of these notifications is out of scope of this module.
//!
//! # Usage
//!
//! To connect to a libp2p node, start by establishing a TCP connection with the target. This can
//! be done either through an outgoing connection or connection incoming from a TCP listening
//! socket.
//!
//! > **Note**: This module only contains *no_std*-friendly code, and creating TCP connections
//! >           isn't handled by it.
//!
//! After a TCP connection is established, use [`handshake::HealthyHandshake::new`] to initialize
//! the state machine that needs to be maintained in parallel of the connection. The data send and
//! received over the socket must respectively be obtained or injected using
//! [`handshake::HealthyHandshake::read_write`]. See the [`handshake`] module documentation for
//! more details.
//!
//! Keep in mind that the [`handshake`] module doesn't provide any timeout. Users are strongly
//! encouraged to add one, in order to detect situations where the remote is, intentionally or
//! not, unresponsive.
//!
//! After the handshake is successful, use [`established::ConnectionPrototype::into_connection`]
//! to obtain an [`established::Established`]. Similar to the handshake, use
//! [`established::Established::read_write`] to update the state machine.
//!

pub use noise::{NoiseKey, UnsignedNoiseKey};

pub mod established;
pub mod handshake;
pub mod multistream_select;
pub mod noise;
pub mod yamux;
