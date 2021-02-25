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

use alloc::vec::Vec;

#[derive(Debug)]
pub struct GrandpaWarpSyncResponse {
    pub fragments: Vec<GrandpaWarpSyncResponseFragment>,
    // TODO: remove this `Option` when a polkadot version that serves
    // `is_finished` is released. The `Option` is only here to allow for
    // backwards compatibility.
    pub is_finished: Option<bool>,
}

#[derive(Debug)]
pub struct GrandpaWarpSyncResponseFragment {
    pub header: crate::header::Header,
    pub justification: crate::finality::justification::decode::Justification,
}

/// Error returned by [`decode_grandpa_warp_sync_response`].
#[derive(Debug, derive_more::Display)]
pub enum DecodeGrandpaWarpSyncResponseError {
    BadResponse,
}

// TODO: make this a zero-cost API
pub fn decode_grandpa_warp_sync_response(
    bytes: &[u8],
) -> Result<GrandpaWarpSyncResponse, DecodeGrandpaWarpSyncResponseError> {
    nom::combinator::map(
        nom::sequence::tuple((
            decode_fragments,
            // TODO: remove this `opt` when a polkadot version that serves
            // `is_finished` is released.
            nom::combinator::opt(nom::number::complete::le_u8),
        )),
        |(fragments, is_finished)| GrandpaWarpSyncResponse {
            fragments,
            is_finished: is_finished.map(|byte| byte != 0),
        },
    )(bytes)
    .map(|(_, parse_result)| parse_result)
    .map_err(|_| DecodeGrandpaWarpSyncResponseError::BadResponse)
}

fn decode_fragments(bytes: &[u8]) -> nom::IResult<&[u8], Vec<GrandpaWarpSyncResponseFragment>> {
    nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |num_elems| {
        nom::multi::many_m_n(
            num_elems,
            num_elems,
            nom::combinator::map(
                nom::sequence::tuple((
                    |s| {
                        crate::header::decode_partial(s)
                            .map(|(a, b)| (b, a))
                            .map_err(|_| {
                                nom::Err::Failure(nom::error::make_error(
                                    s,
                                    nom::error::ErrorKind::Verify,
                                ))
                            })
                    },
                    |s| {
                        crate::finality::justification::decode::decode_partial(s)
                            .map(|(a, b)| (b, a))
                            .map_err(|_| {
                                nom::Err::Failure(nom::error::make_error(
                                    s,
                                    nom::error::ErrorKind::Verify,
                                ))
                            })
                    },
                )),
                move |(header, justification)| GrandpaWarpSyncResponseFragment {
                    header: header.into(),
                    justification: justification.into(),
                },
            ),
        )
    })(bytes)
}
