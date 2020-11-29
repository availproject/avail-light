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

//! Substrate/Polkadot-specific protocols on top of libp2p.
//!
//! Libp2p provides a framework on top of which higher-level, domain-specific protocols can be
//! used. This module contains the domain-specific protocols specific to Substrate and Polkadot.

// TODO: expand docs

use alloc::vec::Vec;
use core::{
    convert::TryFrom,
    iter,
    num::{NonZeroU32, NonZeroU64},
};
use prost::Message as _;

mod schema {
    include!(concat!(env!("OUT_DIR"), "/api.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/api.v1.light.rs"));
}

/// Description of a block request that can be sent to a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlocksRequestConfig {
    /// First block that the remote must return.
    pub start: BlocksRequestConfigStart,
    /// Number of blocks to request. The remote is free to return fewer blocks than requested.
    pub desired_count: NonZeroU32,
    /// Whether the first block should be the one with the highest number, of the one with the
    /// lowest number.
    pub direction: BlocksRequestDirection,
    /// Which fields should be present in the response.
    pub fields: BlocksRequestFields,
}

/// Whether the first block should be the one with the highest number, of the one with the lowest
/// number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlocksRequestDirection {
    /// Blocks should be returned in ascending number, starting from the requested one.
    Ascending,
    /// Blocks should be returned in descending number, starting from the requested one.
    Descending,
}

/// Which fields should be present in the response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlocksRequestFields {
    pub header: bool,
    pub body: bool,
    pub justification: bool,
}

/// Which block the remote must return first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlocksRequestConfigStart {
    /// Hash of the block.
    Hash([u8; 32]),
    /// Number of the block, where 0 would be the genesis block.
    Number(NonZeroU64),
}

/// Builds the bytes corresponding to a block request.
pub fn build_block_request(config: BlocksRequestConfig) -> impl Iterator<Item = impl AsRef<[u8]>> {
    let request = {
        let mut fields = 0u32;
        if config.fields.header {
            fields |= 0b00000001;
        }
        if config.fields.body {
            fields |= 0b00000010;
        }
        if config.fields.justification {
            fields |= 0b00010000;
        }

        schema::BlockRequest {
            // TODO: make this cleaner; don't use swap_bytes
            fields: fields.swap_bytes(),
            from_block: match config.start {
                BlocksRequestConfigStart::Hash(h) => {
                    Some(schema::block_request::FromBlock::Hash(h.to_vec()))
                }
                BlocksRequestConfigStart::Number(n) => Some(
                    schema::block_request::FromBlock::Number(n.get().to_le_bytes().to_vec()),
                ),
            },
            to_block: Vec::new(),
            direction: match config.direction {
                BlocksRequestDirection::Ascending => schema::Direction::Ascending as i32,
                BlocksRequestDirection::Descending => schema::Direction::Descending as i32,
            },
            max_blocks: config.desired_count.get(),
        }
    };

    let request_bytes = {
        let mut buf = Vec::with_capacity(request.encoded_len());
        request.encode(&mut buf).unwrap();
        buf
    };

    iter::once(request_bytes)
}

/// Decodes a response to a block request.
// TODO: should have a more zero-cost API, but we're limited by the protobuf library for that
pub fn decode_block_response(
    response_bytes: &[u8],
) -> Result<Vec<BlockData>, DecodeBlockResponseError> {
    let response = schema::BlockResponse::decode(&response_bytes[..])
        .map_err(ProtobufDecodeError)
        .map_err(DecodeBlockResponseError::ProtobufDecode)?;

    let mut blocks = Vec::with_capacity(response.blocks.len());
    for block in response.blocks {
        if block.hash.len() != 32 {
            return Err(DecodeBlockResponseError::InvalidHashLength);
        }

        let mut body = Vec::with_capacity(block.body.len());
        for extrinsic in block.body {
            // TODO: this encoding really is a bit stupid
            let ext =
                match <Vec<u8> as parity_scale_codec::DecodeAll>::decode_all(extrinsic.as_ref()) {
                    Ok(e) => e,
                    Err(_) => {
                        return Err(DecodeBlockResponseError::BodyDecodeError);
                    }
                };

            body.push(ext);
        }

        blocks.push(BlockData {
            hash: <[u8; 32]>::try_from(&block.hash[..]).unwrap(),
            header: if !block.header.is_empty() {
                Some(block.header)
            } else {
                None
            },
            // TODO: no; we might not have asked for the body
            body: Some(body),
            justification: if !block.justification.is_empty() {
                Some(block.justification)
            } else if block.is_empty_justification {
                Some(Vec::new())
            } else {
                None
            },
        });
    }

    Ok(blocks)
}

/// Block sent in a block response.
///
/// > **Note**: Assuming that this response comes from the network, the information in this struct
/// >           can be erroneous and shouldn't be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockData {
    /// Block hash.
    ///
    /// > **Note**: This should contain the hash of the header, but, like the rest of this
    /// >           structure, this cannot be trusted.
    pub hash: [u8; 32],

    /// SCALE-encoded block header, if requested.
    pub header: Option<Vec<u8>>,

    /// Block body, if requested.
    pub body: Option<Vec<Vec<u8>>>,

    /// Justification, if requested and available.
    pub justification: Option<Vec<u8>>,
}

/// Error potentially returned by [`decode_block_response`].
#[derive(Debug, derive_more::Display)]
pub enum DecodeBlockResponseError {
    /// Error while decoding the protobuf encoding.
    ProtobufDecode(ProtobufDecodeError),
    /// Hash length isn't of the correct length.
    InvalidHashLength,
    BodyDecodeError,
}

/// Description of a storate proof request that can be sent to a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageProofRequestConfig<TKeysIter> {
    /// Hash of the block to request the storage of.
    pub block_hash: [u8; 32],
    /// List of storage keys to query.
    pub keys: TKeysIter,
}

/// Builds the bytes corresponding to a storage proof request.
pub fn build_storage_proof_request(
    config: StorageProofRequestConfig<impl Iterator<Item = impl AsRef<[u8]>>>,
) -> impl Iterator<Item = impl AsRef<[u8]>> {
    let request = schema::Request {
        request: Some(schema::request::Request::RemoteReadRequest(
            schema::RemoteReadRequest {
                block: config.block_hash.to_vec(),
                keys: config.keys.map(|k| k.as_ref().to_vec()).collect(),
            },
        )),
    };

    let request_bytes = {
        let mut buf = Vec::with_capacity(request.encoded_len());
        request.encode(&mut buf).unwrap();
        buf
    };

    iter::once(request_bytes)
}

/// Decodes a response to a storage proof request.
// TODO: should have a more zero-cost API, but we're limited by the protobuf library for that
pub fn decode_storage_proof_response(
    response_bytes: &[u8],
) -> Result<Vec<Vec<u8>>, DecodeStorageProofResponseError> {
    let response = schema::Response::decode(&response_bytes[..])
        .map_err(ProtobufDecodeError)
        .map_err(DecodeStorageProofResponseError::ProtobufDecode)?;

    let proof = match response.response {
        Some(schema::response::Response::RemoteReadResponse(rsp)) => rsp.proof,
        _ => return Err(DecodeStorageProofResponseError::BadResponseTy),
    };

    // The proof itself is a SCALE-encoded `Vec<Vec<u8>>`.
    // Each inner `Vec<u8>` is a node value in the storage trie.
    let (_, decoded) = nom::combinator::all_consuming(nom::combinator::flat_map(
        crate::util::nom_scale_compact_usize,
        |num_elems| {
            nom::multi::many_m_n(
                num_elems,
                num_elems,
                nom::combinator::map(
                    nom::multi::length_data(crate::util::nom_scale_compact_usize),
                    |b| b.to_vec(),
                ),
            )
        },
    ))(&proof)
    .map_err(|_: nom::Err<nom::error::Error<&[u8]>>| {
        DecodeStorageProofResponseError::ProofDecodeError
    })?;

    Ok(decoded)
}

/// Error potentially returned by [`decode_storage_proof_response`].
#[derive(Debug, derive_more::Display)]
pub enum DecodeStorageProofResponseError {
    /// Error while decoding the protobuf encoding.
    ProtobufDecode(ProtobufDecodeError),
    /// Response isn't a response to a storage proof request.
    BadResponseTy,
    /// Failed to decode response as a storage proof.
    ProofDecodeError,
}

/// Error while decoding the protobuf encoding.
#[derive(Debug, derive_more::Display)]
#[display(fmt = "{}", _0)]
pub struct ProtobufDecodeError(prost::DecodeError);

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

/// Turns a block announces handshake into its SCALE-encoding ready to be sent over the wire.
///
/// This function returns an iterator of buffers. The encoded message consists in the
/// concatenation of the buffers.
pub fn encode_block_announces_handshake<'a>(
    handshake: BlockAnnouncesHandshakeRef<'a>,
) -> impl Iterator<Item = impl AsRef<[u8]> + 'a> + 'a {
    let mut header = [0; 5];
    header[0] = match handshake.role {
        Role::Full => 0b1,
        Role::Light => 0b10,
        Role::Authority => 0b100,
    };

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
) -> Result<BlockAnnouncesHandshakeRef, BlockAnnouncesDecodeError> {
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
    .map_err(BlockAnnouncesDecodeError)
}

/// Error potentially returned by [`decode_block_announces_handshake`].
#[derive(Debug, derive_more::Display)]
pub struct BlockAnnouncesDecodeError<'a>(nom::Err<nom::error::Error<&'a [u8]>>);
