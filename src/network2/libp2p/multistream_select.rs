use core::str;
use nom::branch::alt;
use nom::bytes::streaming::{tag, take_till};
use nom::combinator::{map, map_res};
use nom::sequence::terminated;
use nom::IResult;

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
