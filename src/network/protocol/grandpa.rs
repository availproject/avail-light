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

// TODO: document all this

use crate::finality::justification::decode::PrecommitRef;

use alloc::vec::Vec;
use core::convert::TryFrom as _;

#[derive(Debug, Clone)]
pub enum GrandpaNotificationRef<'a> {
    Vote(VoteMessageRef<'a>),
    Commit(CommitMessageRef<'a>),
    Neighbor(NeighborPacket),
    CatchUpRequest(CatchUpRequest),
    CatchUp(CatchUpRef<'a>),
}

#[derive(Debug, Clone)]
pub struct VoteMessageRef<'a> {
    pub round_number: u64,
    pub set_id: u64,
    pub message: MessageRef<'a>,
    pub signature: &'a [u8; 64],
    pub authority_public_key: &'a [u8; 32],
}

#[derive(Debug, Clone)]
pub enum MessageRef<'a> {
    Prevote(UnsignedPrevoteRef<'a>),
    Precommit(UnsignedPrecommitRef<'a>),
    PrimaryPropose(PrimaryProposeRef<'a>),
}

#[derive(Debug, Clone)]
pub struct UnsignedPrevoteRef<'a> {
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
}

#[derive(Debug, Clone)]
pub struct UnsignedPrecommitRef<'a> {
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
}

#[derive(Debug, Clone)]
pub struct PrimaryProposeRef<'a> {
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
}

#[derive(Debug, Clone)]
pub struct CommitMessageRef<'a> {
    pub round_number: u64,
    pub set_id: u64,
    pub message: CompactCommitRef<'a>,
}

#[derive(Debug, Clone)]
pub struct CompactCommitRef<'a> {
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
    pub precommits: Vec<PrecommitRef<'a>>,

    /// List of ed25519 signatures and public keys.
    pub auth_data: Vec<(&'a [u8; 64], &'a [u8; 32])>,
}

#[derive(Debug, Clone)]
pub struct NeighborPacket {
    pub round_number: u64,
    pub set_id: u64,
    pub commit_finalized_height: u32,
}

#[derive(Debug, Clone)]
pub struct CatchUpRequest {
    pub round_number: u64,
    pub set_id: u64,
}

#[derive(Debug, Clone)]
pub struct CatchUpRef<'a> {
    pub round_number: u64,
    pub prevotes: Vec<PrevoteRef<'a>>,
    pub precommits: Vec<PrecommitRef<'a>>,
    pub base_hash: &'a [u8; 32],
    pub base_number: u32,
}

#[derive(Debug, Clone)]
pub struct PrevoteRef<'a> {
    /// Hash of the block concerned by the pre-vote.
    pub target_hash: &'a [u8; 32],
    /// Height of the block concerned by the pre-vote.
    pub target_number: u32,

    /// Ed25519 signature made with [`PrevoteRef::authority_public_key`].
    pub signature: &'a [u8; 64],

    /// Authority that signed the pre-vote. Must be part of the authority set for the
    /// justification to be valid.
    pub authority_public_key: &'a [u8; 32],
}

/// Attempt to decode the given SCALE-encoded Grandpa notification.
pub fn decode_grandpa_notification(
    scale_encoded: &[u8],
) -> Result<GrandpaNotificationRef, DecodeGrandpaNotificationError> {
    match nom::combinator::all_consuming(grandpa_notification)(scale_encoded) {
        Ok((_, notif)) => Ok(notif),
        Err(err) => Err(DecodeGrandpaNotificationError(err)),
    }
}

/// Error potentially returned by [`decode_grandpa_notification`].
#[derive(Debug, derive_more::Display)]
pub struct DecodeGrandpaNotificationError<'a>(nom::Err<nom::error::Error<&'a [u8]>>);

// Nom combinators below.

fn grandpa_notification(bytes: &[u8]) -> nom::IResult<&[u8], GrandpaNotificationRef> {
    nom::error::context(
        "grandpa_notification",
        nom::branch::alt((
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[0]), vote_message),
                GrandpaNotificationRef::Vote,
            ),
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[1]), commit_message),
                GrandpaNotificationRef::Commit,
            ),
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[2]), neighbor_packet),
                GrandpaNotificationRef::Neighbor,
            ),
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[3]), catch_up_request),
                GrandpaNotificationRef::CatchUpRequest,
            ),
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[4]), catch_up),
                GrandpaNotificationRef::CatchUp,
            ),
        )),
    )(bytes)
}

fn vote_message(bytes: &[u8]) -> nom::IResult<&[u8], VoteMessageRef> {
    nom::error::context(
        "vote_message",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::number::complete::le_u64,
                nom::number::complete::le_u64,
                message,
                nom::bytes::complete::take(64u32),
                nom::bytes::complete::take(32u32),
            )),
            |(round_number, set_id, message, signature, authority_public_key)| VoteMessageRef {
                round_number,
                set_id,
                message,
                signature: <&[u8; 64]>::try_from(signature).unwrap(),
                authority_public_key: <&[u8; 32]>::try_from(authority_public_key).unwrap(),
            },
        ),
    )(bytes)
}

fn message(bytes: &[u8]) -> nom::IResult<&[u8], MessageRef> {
    nom::error::context(
        "message",
        nom::branch::alt((
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[0]), unsigned_prevote),
                MessageRef::Prevote,
            ),
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[1]), unsigned_precommit),
                MessageRef::Precommit,
            ),
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[2]), primary_propose),
                MessageRef::PrimaryPropose,
            ),
        )),
    )(bytes)
}

fn unsigned_prevote(bytes: &[u8]) -> nom::IResult<&[u8], UnsignedPrevoteRef> {
    nom::error::context(
        "unsigned_prevote",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::bytes::complete::take(32u32),
                nom::number::complete::le_u32,
            )),
            |(target_hash, target_number)| UnsignedPrevoteRef {
                target_hash: <&[u8; 32]>::try_from(target_hash).unwrap(),
                target_number,
            },
        ),
    )(bytes)
}

fn unsigned_precommit(bytes: &[u8]) -> nom::IResult<&[u8], UnsignedPrecommitRef> {
    nom::error::context(
        "unsigned_precommit",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::bytes::complete::take(32u32),
                nom::number::complete::le_u32,
            )),
            |(target_hash, target_number)| UnsignedPrecommitRef {
                target_hash: <&[u8; 32]>::try_from(target_hash).unwrap(),
                target_number,
            },
        ),
    )(bytes)
}

fn primary_propose(bytes: &[u8]) -> nom::IResult<&[u8], PrimaryProposeRef> {
    nom::error::context(
        "primary_propose",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::bytes::complete::take(32u32),
                nom::number::complete::le_u32,
            )),
            |(target_hash, target_number)| PrimaryProposeRef {
                target_hash: <&[u8; 32]>::try_from(target_hash).unwrap(),
                target_number,
            },
        ),
    )(bytes)
}

fn commit_message(bytes: &[u8]) -> nom::IResult<&[u8], CommitMessageRef> {
    nom::error::context(
        "commit_message",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::number::complete::le_u64,
                nom::number::complete::le_u64,
                compact_commit,
            )),
            |(round_number, set_id, message)| CommitMessageRef {
                round_number,
                set_id,
                message,
            },
        ),
    )(bytes)
}

fn compact_commit(bytes: &[u8]) -> nom::IResult<&[u8], CompactCommitRef> {
    nom::error::context(
        "compact_commit",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::bytes::complete::take(32u32),
                nom::number::complete::le_u32,
                nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |num_elems| {
                    nom::multi::many_m_n(num_elems, num_elems, |s| {
                        crate::finality::justification::decode::PrecommitRef::decode_partial(s)
                            .map(|(a, b)| (b, a))
                            .map_err(|_| {
                                nom::Err::Failure(nom::error::make_error(
                                    s,
                                    nom::error::ErrorKind::Verify,
                                ))
                            })
                    })
                }),
                nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |num_elems| {
                    nom::multi::many_m_n(
                        num_elems,
                        num_elems,
                        nom::combinator::map(
                            nom::sequence::tuple((
                                nom::bytes::complete::take(64u32),
                                nom::bytes::complete::take(32u32),
                            )),
                            |(sig, pubkey)| {
                                (
                                    <&[u8; 64]>::try_from(sig).unwrap(),
                                    <&[u8; 32]>::try_from(pubkey).unwrap(),
                                )
                            },
                        ),
                    )
                }),
            )),
            |(target_hash, target_number, precommits, auth_data)| CompactCommitRef {
                target_hash: <&[u8; 32]>::try_from(target_hash).unwrap(),
                target_number,
                precommits,
                auth_data,
            },
        ),
    )(bytes)
}

fn neighbor_packet(bytes: &[u8]) -> nom::IResult<&[u8], NeighborPacket> {
    nom::error::context(
        "neighbor_packet",
        nom::combinator::map(
            nom::sequence::preceded(
                nom::bytes::complete::tag(&[1]),
                nom::sequence::tuple((
                    nom::number::complete::le_u64,
                    nom::number::complete::le_u64,
                    nom::number::complete::le_u32,
                )),
            ),
            |(round_number, set_id, commit_finalized_height)| NeighborPacket {
                round_number,
                set_id,
                commit_finalized_height,
            },
        ),
    )(bytes)
}

fn catch_up_request(bytes: &[u8]) -> nom::IResult<&[u8], CatchUpRequest> {
    nom::error::context(
        "catch_up_request",
        nom::combinator::map(
            nom::sequence::tuple((nom::number::complete::le_u64, nom::number::complete::le_u64)),
            |(round_number, set_id)| CatchUpRequest {
                round_number,
                set_id,
            },
        ),
    )(bytes)
}

fn catch_up(bytes: &[u8]) -> nom::IResult<&[u8], CatchUpRef> {
    nom::error::context(
        "catch_up",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::number::complete::le_u64,
                nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |num_elems| {
                    nom::multi::many_m_n(num_elems, num_elems, prevote)
                }),
                nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |num_elems| {
                    nom::multi::many_m_n(num_elems, num_elems, |s| {
                        crate::finality::justification::decode::PrecommitRef::decode_partial(s)
                            .map(|(a, b)| (b, a))
                            .map_err(|_| {
                                nom::Err::Failure(nom::error::make_error(
                                    s,
                                    nom::error::ErrorKind::Verify,
                                ))
                            })
                    })
                }),
                nom::bytes::complete::take(32u32),
                nom::number::complete::le_u32,
            )),
            |(round_number, prevotes, precommits, base_hash, base_number)| CatchUpRef {
                round_number,
                prevotes,
                precommits,
                base_hash: <&[u8; 32]>::try_from(base_hash).unwrap(),
                base_number,
            },
        ),
    )(bytes)
}

fn prevote(bytes: &[u8]) -> nom::IResult<&[u8], PrevoteRef> {
    nom::error::context(
        "prevote",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::bytes::complete::take(32u32),
                nom::number::complete::le_u32,
                nom::bytes::complete::take(64u32),
                nom::bytes::complete::take(32u32),
            )),
            |(target_hash, target_number, signature, authority_public_key)| PrevoteRef {
                target_hash: <&[u8; 32]>::try_from(target_hash).unwrap(),
                target_number,
                signature: <&[u8; 64]>::try_from(signature).unwrap(),
                authority_public_key: <&[u8; 32]>::try_from(authority_public_key).unwrap(),
            },
        ),
    )(bytes)
}
