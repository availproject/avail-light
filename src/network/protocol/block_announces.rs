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

use crate::header;

use core::{convert::TryFrom, iter};

/// Decoded handshake sent or received when opening a block announces notifications substream.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BlockAnnouncesHandshakeRef<'a> {
    /// Role a node reports playing on the network.
    pub role: Role,

    /// Height of the best block according to this node.
    pub best_number: u64,

    /// Hash of the best block according to this node.
    pub best_hash: &'a [u8; 32],

    /// Hash of the genesis block according to this node.
    ///
    /// > **Note**: This should be compared to the locally known genesis block hash, to make sure
    /// >           that both nodes are on the same chain.
    pub genesis_hash: &'a [u8; 32],
}

/// Role a node reports playing on the network.
// TODO: document why this is here and what this entails
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Role {
    Full,
    Light,
    Authority,
}

impl Role {
    /// Returns the SCALE encoding of this enum. Always guaranteed to be one byte.
    pub fn scale_encoding(&self) -> [u8; 1] {
        match *self {
            Role::Full => [0b1],
            Role::Light => [0b10],
            Role::Authority => [0b100],
        }
    }
}

/// Decoded block announcement notification.
#[derive(Debug)]
pub struct BlockAnnounceRef<'a> {
    /// Header of the announced block.
    pub header: header::HeaderRef<'a>,
    /// True if the block is the new best block of the announcer.
    pub is_best: bool,
    // TODO: missing a `Vec<u8>` field that SCALE-decodes into this type: https://github.com/paritytech/polkadot/blob/fff4635925c12c80717a524367687fcc304bcb13/node%2Fprimitives%2Fsrc%2Flib.rs#L87
}

/// Turns a block announcement into its SCALE-encoding ready to be sent over the wire.
///
/// This function returns an iterator of buffers. The encoded message consists in the
/// concatenation of the buffers.
pub fn encode_block_announce<'a>(
    announce: BlockAnnounceRef<'a>,
) -> impl Iterator<Item = impl AsRef<[u8]> + 'a> + 'a {
    let is_best = if announce.is_best { [1u8] } else { [0u8] };
    announce
        .header
        .scale_encoding()
        .map(either::Left)
        .chain(iter::once(either::Right(is_best)))
}

/// Decodes a block announcement.
pub fn decode_block_announce(bytes: &[u8]) -> Result<BlockAnnounceRef, DecodeBlockAnnounceError> {
    nom::combinator::all_consuming(nom::combinator::map(
        nom::sequence::tuple((
            |s| {
                header::decode_partial(s).map(|(a, b)| (b, a)).map_err(|_| {
                    nom::Err::Failure(nom::error::make_error(s, nom::error::ErrorKind::Verify))
                })
            },
            nom::branch::alt((
                nom::combinator::map(nom::bytes::complete::tag(&[0]), |_| false),
                nom::combinator::map(nom::bytes::complete::tag(&[1]), |_| true),
            )),
            crate::util::nom_bytes_decode,
        )),
        |(header, is_best, _)| BlockAnnounceRef { header, is_best },
    ))(&bytes)
    .map(|(_, ann)| ann)
    .map_err(DecodeBlockAnnounceError)
}

/// Error potentially returned by [`decode_block_announces_handshake`].
#[derive(Debug, derive_more::Display)]
pub struct DecodeBlockAnnounceError<'a>(nom::Err<nom::error::Error<&'a [u8]>>);

/// Turns a block announces handshake into its SCALE-encoding ready to be sent over the wire.
///
/// This function returns an iterator of buffers. The encoded message consists in the
/// concatenation of the buffers.
pub fn encode_block_announces_handshake<'a>(
    handshake: BlockAnnouncesHandshakeRef<'a>,
) -> impl Iterator<Item = impl AsRef<[u8]> + 'a> + 'a {
    let mut header = [0; 5];
    header[0] = handshake.role.scale_encoding()[0];

    // TODO: the message actually contains a u32, and doesn't use compact encoding; as such, any block height superior to 2^32 cannot be encoded
    assert!(handshake.best_number < u64::from(u32::max_value()));
    header[1..].copy_from_slice(&handshake.best_number.to_le_bytes()[..4]);

    iter::once(either::Left(header))
        .chain(iter::once(either::Right(handshake.best_hash)))
        .chain(iter::once(either::Right(handshake.genesis_hash)))
}

/// Decodes a SCALE-encoded block announces handshake.
pub fn decode_block_announces_handshake(
    handshake: &[u8],
) -> Result<BlockAnnouncesHandshakeRef, BlockAnnouncesHandshakeDecodeError> {
    nom::combinator::all_consuming(nom::combinator::map(
        nom::sequence::tuple((
            nom::branch::alt((
                nom::combinator::map(nom::bytes::complete::tag(&[0b1]), |_| Role::Full),
                nom::combinator::map(nom::bytes::complete::tag(&[0b10]), |_| Role::Light),
                nom::combinator::map(nom::bytes::complete::tag(&[0b100]), |_| Role::Authority),
            )),
            nom::number::complete::le_u32,
            nom::bytes::complete::take(32u32),
            nom::bytes::complete::take(32u32),
        )),
        |(role, best_number, best_hash, genesis_hash)| BlockAnnouncesHandshakeRef {
            role,
            best_number: u64::from(best_number),
            best_hash: TryFrom::try_from(best_hash).unwrap(),
            genesis_hash: TryFrom::try_from(genesis_hash).unwrap(),
        },
    ))(handshake)
    .map(|(_, hs)| hs)
    .map_err(BlockAnnouncesHandshakeDecodeError)
}

/// Error potentially returned by [`decode_block_announces_handshake`].
#[derive(Debug, derive_more::Display)]
pub struct BlockAnnouncesHandshakeDecodeError<'a>(nom::Err<nom::error::Error<&'a [u8]>>);
