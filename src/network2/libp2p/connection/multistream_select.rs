use core::{cmp, str};
use nom::branch::alt;
use nom::bytes::streaming::{tag, take_till};
use nom::combinator::{map, map_res};
use nom::sequence::terminated;
use nom::IResult;

pub struct Negotiation {
    has_received_handshake: bool,
    buffer: Vec<u8>,

    /// Number of bytes of [`HANDSHAKE`] that have been written out. If superior or equal to the
    /// length of the handshake, then the handshake has already finished being written out.
    handshake_written_bytes: usize,
}

enum NegotiationState {
    Handshake,
}

impl Negotiation {
    /// State of a protocol negotiation.
    pub fn new() -> Self {
        Negotiation {
            has_received_handshake: false,
            buffer: Vec::new(),
            handshake_written_bytes: 0,
        }
    }

    /// Feeds more data to the negotiation.
    pub fn inject_data(&mut self, data: &[u8]) -> usize {
        self.buffer.extend_from_slice(data);

        if !self.has_received_handshake {
            if self.buffer.len() >= HANDSHAKE.len() {
                if self.buffer.starts_with(HANDSHAKE) {
                    self.has_received_handshake = true;
                } else {
                }
            }
        }

        todo!()
    }

    /// Write to the given buffer the bytes that are ready to be sent out. Returns the number of
    /// bytes written to `destination`.
    pub fn write_out(&mut self, destination: &mut [u8]) -> usize {
        let handshake_to_write = cmp::min(
            HANDSHAKE.len() - self.handshake_written_bytes,
            destination.len(),
        );
        destination.copy_from_slice(
            &HANDSHAKE
                [self.handshake_written_bytes..(self.handshake_written_bytes + handshake_to_write)],
        );
        self.handshake_written_bytes += handshake_to_write;

        handshake_to_write
    }
}

const HANDSHAKE: &'static [u8] = b"/multistream/1.0.0\n";

/// Nom combinator that parses the multistream-select handshake.
fn handshake(i: &[u8]) -> IResult<&[u8], ()> {
    map(tag(b"/multistream/1.0.0\n"), |_| ())(i)
}

/// Nom combinator that parses a command sent from the dialer towards the
/// receiver.
fn command(i: &[u8]) -> IResult<&[u8], Command> {
    alt((
        map(tag(b"ls\n"), |_| Command::Ls),
        map_res(terminated(take_till(|c| c != b'\n'), tag(b"\n")), |p| {
            str::from_utf8(p).map(Command::Protocol)
        }),
    ))(i)
}

enum Command<'a> {
    Ls,
    Protocol(&'a str),
}

/// Nom combinator that parses a response to a [`Command::Ls`].
fn response_ls(i: &[u8]) -> IResult<&[u8], ()> {
    todo!()
}

/// Nom combinator that parses a response to a [`Command::Protocol`].
///
/// Returns true if the protocol has been accepted by the remote.
fn response_protocol<'a>(i: &'a [u8], protocol_name: &str) -> IResult<&'a [u8], bool> {
    alt((
        map(
            terminated(tag(protocol_name.as_bytes()), tag(b"\n")),
            |_| true,
        ),
        map(tag(b"na\n"), |_| false),
    ))(i)
}
