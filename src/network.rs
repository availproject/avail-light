//! Connecting to the peer-to-peer network.
//!
//! The [`Network`] struct provided by this module allows you to connect to other nodes and
//! exchange messages with them.
//!
//! # Usage
//!
//! - Call [`builder::builder`] in order to create a [`builder::NetworkBuilder`].
//! - Configure it, then call [`builder::NetworkBuilder::build`] to obtain a [`Network`].
//! - Call methods such as [`Network::start_block_request`] to perform actions on the network.
//! - Repeatedly call [`Network::next_event`] in order to query for messages or events that happen
//! on the network.
//!
//! > **Note**: It is assumed that [`Network::next_event`] gets called as often as possible. If it
//! >           is called too infrequently, then back-pressure will be applied on the networking
//! >           stack, and the networking as a whole will slow down in order to adjust for the
//! >           processing speed.
//!
//! # Concepts
//!
//! Here are the major concepts that you should understand in order to use this module:
//!
//! - **Peer** vs **node**. A **node** is a machine that is part of the network. A **peer** is a
//! node which we are directly connected to. Be aware that this documentation doesn't necessarily
//! strictly enforce the distinction between the node.
//!
//! - A **peer ID**, or **node ID**, represented by the [`PeerId`] struct, is the identity of a
//! node on the network (not necessarily a peer). It is the encoding of a public key. Whenever
//! connection is established, an
//! [ECDH](https://en.wikipedia.org/wiki/Elliptic-curve_Diffie%E2%80%93Hellman) handshake is
//! performed and communications encrypted. It is therefore guaranteed that all communications
//! with a peer come from or arrive to the owner of the corresponding private key.
//!
//! - A **multiaddr* is an abstraction over a way to reach a node, such as an IP address and a
//! port. TODO: expand
//!
//! - A **bootstrap node** is a node whose address is hard-coded into the client on startup. You
//! are encouraged to pass the list of bootstrap nodes' peer IDs and their addresses as part of the
//! configuration of the network.
//!
//! - A **substream** is a subdivision of a connection. Each connection with a peer is subdivided
//! into multiple substreams multiplexed together. Communications are ordered within each
//! individual substreams, but not necessarily between substreams.
//!
//! - TODO: finish
//!

pub use behaviour::{
    BlockData, BlockHeader, BlocksRequestConfig, BlocksRequestConfigStart, BlocksRequestDirection,
    BlocksRequestFields,
};
pub use builder::builder;
pub use libp2p::{Multiaddr, PeerId};
pub use worker::{Event, Network};

#[doc(inline)]
pub use libp2p::multiaddr;

pub mod builder;

mod behaviour;
mod block_requests;
mod debug_info;
mod discovery;
mod generic_proto;
mod legacy_message;
mod schema;
mod transport;
mod worker;
