//! State machine handling a single TCP or WebSocket libp2p connection.
//!
//! This state machine tries to negotiate and apply the noise and yamux protocols on top of the
//! connection.

use super::multiplexed_connection;
use libp2p::PeerId;

pub struct Connection {
    state: State,
}

enum State {
    NegotiatingEncryption,
    NegotiatingMultiplexing,
    Open,
}

impl Connection {
    /// Initializes a new state machine.
    ///
    /// Must pass [`Endpoint::Dialer`] if the connection has been opened by the local machine,
    /// and [`Endpoint::Listener`] if it has been opened by the remote.
    pub fn new(endpoint: Endpoint) {}

    /// Parse the content of `data`. Returns the number of bytes that have been consumed from
    /// `data` and an object representing an event that happened on the connection.
    pub fn inject_data<'c, 'd>(
        &'c mut self,
        data: impl Iterator<Item = &'d [u8]>,
    ) -> (usize, InjectDataOutcome<'c, 'd>) {
        todo!()
    }

    pub fn write_ready(&mut self) -> impl Iterator<Item = impl AsRef<[u8]>> {
        core::iter::empty::<Vec<u8>>() // TODO:
    }

    pub fn advance_write_cursor(&mut self, size: usize) {
        todo!()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Dialer,
    Listener,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InjectDataOutcome<'c, 'd> {
    ReceivedIdentity(PeerId),
    /// Received a ping request from the remote and answered it.
    Ping,
    /// Received a pong from the remote.
    Pong,
    RequestIn {
        protocol_name: &'c str,
        request: &'d [u8],
    },
}
