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

use super::{schema, ProtobufDecodeError};

use alloc::vec::Vec;
use core::iter;
use prost::Message as _;

/// Description of a storage proof request that can be sent to a peer.
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
    // Note: while the API of this function allows for a zero-cost implementation, the protobuf
    // library doesn't permit to avoid allocations.

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
                nom::combinator::map(crate::util::nom_bytes_decode, |b| b.to_vec()),
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
