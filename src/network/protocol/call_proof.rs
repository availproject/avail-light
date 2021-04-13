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

/// Description of a call proof request that can be sent to a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallProofRequestConfig<'a, I> {
    /// Hash of the block to request the storage of.
    pub block_hash: [u8; 32],
    /// Name of the runtime function to call.
    pub method: &'a str,
    /// Iterator to buffers of bytes to be concatenated then passed as input to the call. The
    /// semantics of these bytes depend on which method is being called.
    pub parameter_vectored: I,
}

/// Builds the bytes corresponding to a call proof request.
pub fn build_call_proof_request(
    config: CallProofRequestConfig<impl Iterator<Item = impl AsRef<[u8]>>>,
) -> impl Iterator<Item = impl AsRef<[u8]>> {
    // Note: while the API of this function allows for a zero-cost implementation, the protobuf
    // library doesn't permit to avoid allocations.

    let request = schema::Request {
        request: Some(schema::request::Request::RemoteCallRequest(
            schema::RemoteCallRequest {
                block: config.block_hash.to_vec(),
                method: config.method.into(),
                data: config.parameter_vectored.fold(Vec::new(), |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                }),
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

/// Decodes a response to a call proof request.
// TODO: should have a more zero-cost API, but we're limited by the protobuf library for that
pub fn decode_call_proof_response(
    response_bytes: &[u8],
) -> Result<Vec<Vec<u8>>, DecodeCallProofResponseError> {
    let response = schema::Response::decode(&response_bytes[..])
        .map_err(ProtobufDecodeError)
        .map_err(DecodeCallProofResponseError::ProtobufDecode)?;

    let proof = match response.response {
        Some(schema::response::Response::RemoteCallResponse(rsp)) => rsp.proof,
        _ => return Err(DecodeCallProofResponseError::BadResponseTy),
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
        DecodeCallProofResponseError::ProofDecodeError
    })?;

    Ok(decoded)
}

/// Error potentially returned by [`decode_call_proof_response`].
#[derive(Debug, derive_more::Display)]
pub enum DecodeCallProofResponseError {
    /// Error while decoding the protobuf encoding.
    ProtobufDecode(ProtobufDecodeError),
    /// Response isn't a response to a call proof request.
    BadResponseTy,
    /// Failed to decode response as a call proof.
    ProofDecodeError,
}
