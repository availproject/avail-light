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

// TODO: document all this

use crate::finality::justification::decode::PrecommitRef;

use alloc::vec::Vec;
use core::{convert::TryFrom as _, iter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrandpaNotificationRef<'a> {
    Vote(VoteMessageRef<'a>),
    Commit(CommitMessageRef<'a>),
    Neighbor(NeighborPacket),
    CatchUpRequest(CatchUpRequest),
    CatchUp(CatchUpRef<'a>),
}

impl<'a> GrandpaNotificationRef<'a> {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(&self) -> impl Iterator<Item = impl AsRef<[u8]> + Clone> + Clone {
        match self {
            GrandpaNotificationRef::Neighbor(n) => {
                iter::once(either::Left(&[2u8])).chain(n.scale_encoding().map(either::Right))
            }
            _ => todo!(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteMessageRef<'a> {
    pub round_number: u64,
    pub set_id: u64,
    pub message: MessageRef<'a>,
    pub signature: &'a [u8; 64],
    pub authority_public_key: &'a [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRef<'a> {
    Prevote(UnsignedPrevoteRef<'a>),
    Precommit(UnsignedPrecommitRef<'a>),
    PrimaryPropose(PrimaryProposeRef<'a>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedPrevoteRef<'a> {
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedPrecommitRef<'a> {
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryProposeRef<'a> {
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMessageRef<'a> {
    pub round_number: u64,
    pub set_id: u64,
    pub message: CompactCommitRef<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactCommitRef<'a> {
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
    pub precommits: Vec<UnsignedPrecommitRef<'a>>,

    /// List of ed25519 signatures and public keys.
    pub auth_data: Vec<(&'a [u8; 64], &'a [u8; 32])>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeighborPacket {
    pub round_number: u64,
    pub set_id: u64,
    pub commit_finalized_height: u32,
}

impl NeighborPacket {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that object.
    pub fn scale_encoding(&self) -> impl Iterator<Item = impl AsRef<[u8]> + Clone> + Clone {
        iter::once(either::Right(either::Left([1u8])))
            .chain(iter::once(either::Left(self.round_number.to_le_bytes())))
            .chain(iter::once(either::Left(self.set_id.to_le_bytes())))
            .chain(iter::once(either::Right(either::Right(
                self.commit_finalized_height.to_le_bytes(),
            ))))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUpRequest {
    pub round_number: u64,
    pub set_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUpRef<'a> {
    pub set_id: u64,
    pub round_number: u64,
    pub prevotes: Vec<PrevoteRef<'a>>,
    pub precommits: Vec<PrecommitRef<'a>>,
    pub base_hash: &'a [u8; 32],
    pub base_number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
                    nom::multi::many_m_n(num_elems, num_elems, unsigned_precommit)
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
            |(set_id, round_number, prevotes, precommits, base_hash, base_number)| CatchUpRef {
                set_id,
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

#[cfg(test)]
mod tests {
    #[test]
    fn basic_decode_neighbor() {
        let actual = super::decode_grandpa_notification(&[
            2, 1, 87, 14, 0, 0, 0, 0, 0, 0, 162, 13, 0, 0, 0, 0, 0, 0, 49, 231, 77, 0,
        ])
        .unwrap();

        let expected = super::GrandpaNotificationRef::Neighbor(super::NeighborPacket {
            round_number: 3671,
            set_id: 3490,
            commit_finalized_height: 5105457,
        });

        assert_eq!(actual, expected);
    }

    #[test]
    fn basic_decode_commit() {
        let actual = super::decode_grandpa_notification(&[
            1, 85, 14, 0, 0, 0, 0, 0, 0, 162, 13, 0, 0, 0, 0, 0, 0, 182, 68, 115, 35, 15, 201, 152,
            195, 12, 181, 59, 244, 231, 124, 34, 248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118,
            151, 68, 101, 104, 187, 82, 49, 231, 77, 0, 28, 182, 68, 115, 35, 15, 201, 152, 195,
            12, 181, 59, 244, 231, 124, 34, 248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151,
            68, 101, 104, 187, 82, 49, 231, 77, 0, 182, 68, 115, 35, 15, 201, 152, 195, 12, 181,
            59, 244, 231, 124, 34, 248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101,
            104, 187, 82, 49, 231, 77, 0, 182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244,
            231, 124, 34, 248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104, 187,
            82, 49, 231, 77, 0, 182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124,
            34, 248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104, 187, 82, 49,
            231, 77, 0, 182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34, 248,
            98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104, 187, 82, 49, 231, 77, 0,
            182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34, 248, 98, 253, 4,
            180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104, 187, 82, 49, 231, 77, 0, 182, 68,
            115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34, 248, 98, 253, 4, 180, 158,
            70, 161, 84, 76, 118, 151, 68, 101, 104, 187, 82, 49, 231, 77, 0, 28, 189, 185, 216,
            33, 163, 12, 201, 104, 162, 255, 11, 241, 156, 90, 244, 205, 251, 44, 45, 139, 129,
            117, 178, 85, 129, 78, 58, 255, 76, 232, 199, 85, 236, 30, 227, 87, 50, 34, 22, 27,
            241, 6, 33, 137, 55, 5, 190, 36, 122, 61, 112, 51, 99, 34, 119, 46, 185, 156, 188, 133,
            140, 103, 33, 10, 45, 154, 173, 12, 30, 12, 25, 95, 195, 198, 235, 98, 29, 248, 44,
            121, 73, 203, 132, 51, 196, 138, 65, 42, 3, 49, 169, 182, 129, 146, 242, 193, 228, 217,
            26, 9, 233, 239, 30, 213, 103, 10, 33, 27, 44, 13, 178, 236, 216, 167, 190, 9, 123,
            151, 143, 1, 199, 58, 77, 121, 122, 215, 22, 19, 238, 190, 216, 8, 62, 6, 216, 37, 197,
            124, 141, 51, 196, 205, 205, 193, 24, 86, 246, 60, 16, 139, 66, 51, 93, 168, 159, 147,
            77, 90, 91, 8, 64, 14, 252, 119, 77, 211, 141, 23, 18, 115, 222, 3, 2, 22, 42, 105, 85,
            176, 71, 232, 230, 141, 12, 9, 124, 205, 194, 191, 90, 47, 202, 233, 218, 161, 80, 55,
            8, 134, 223, 202, 4, 137, 45, 10, 71, 90, 162, 252, 99, 19, 252, 17, 175, 5, 75, 208,
            81, 0, 96, 218, 5, 89, 250, 183, 161, 188, 227, 62, 107, 34, 63, 155, 28, 176, 141,
            174, 113, 162, 229, 148, 55, 39, 65, 36, 97, 159, 198, 238, 222, 34, 76, 187, 40, 19,
            109, 1, 67, 146, 40, 75, 194, 208, 80, 208, 221, 175, 151, 239, 239, 127, 65, 39, 237,
            145, 130, 36, 154, 135, 68, 105, 52, 102, 49, 62, 137, 34, 187, 159, 55, 157, 88, 195,
            49, 116, 72, 11, 37, 132, 176, 74, 69, 60, 157, 67, 36, 156, 165, 71, 164, 86, 220,
            240, 241, 13, 40, 125, 79, 147, 27, 56, 254, 198, 231, 108, 187, 214, 187, 98, 229,
            123, 116, 160, 126, 192, 98, 132, 247, 206, 70, 228, 175, 152, 217, 252, 4, 109, 98,
            24, 90, 117, 184, 11, 107, 32, 186, 217, 155, 44, 253, 198, 120, 175, 170, 229, 66,
            122, 141, 158, 75, 68, 108, 104, 182, 223, 91, 126, 210, 38, 84, 143, 10, 142, 225, 77,
            169, 12, 215, 222, 158, 85, 4, 111, 196, 47, 56, 147, 93, 1, 202, 247, 137, 115, 30,
            127, 94, 191, 31, 223, 162, 16, 73, 219, 118, 52, 40, 255, 191, 183, 70, 132, 115, 91,
            214, 191, 156, 189, 203, 208, 152, 165, 115, 64, 123, 209, 153, 80, 44, 134, 143, 188,
            140, 168, 162, 134, 178, 192, 122, 10, 137, 41, 133, 127, 72, 223, 16, 65, 170, 114,
            53, 173, 180, 59, 208, 190, 54, 96, 123, 199, 137, 214, 115, 240, 73, 87, 253, 137, 81,
            36, 66, 175, 76, 40, 52, 216, 110, 234, 219, 158, 208, 142, 85, 168, 43, 164, 19, 154,
            21, 125, 174, 153, 165, 45, 54, 100, 36, 196, 46, 95, 64, 192, 178, 156, 16, 112, 5,
            237, 207, 113, 132, 125, 148, 34, 132, 105, 148, 216, 148, 182, 33, 74, 215, 161, 252,
            44, 24, 67, 77, 87, 6, 94, 109, 38, 64, 10, 195, 28, 194, 169, 175, 7, 98, 210, 151, 4,
            221, 136, 161, 204, 171, 251, 101, 63, 21, 245, 84, 189, 77, 59, 75, 136, 44, 17, 217,
            119, 206, 191, 191, 137, 127, 81, 55, 208, 225, 33, 209, 59, 83, 121, 234, 160, 191,
            38, 82, 1, 102, 178, 140, 58, 20, 131, 206, 37, 148, 106, 135, 149, 74, 57, 27, 84,
            215, 0, 47, 68, 1, 8, 139, 183, 125, 169, 4, 165, 168, 86, 218, 178, 95, 157, 185, 64,
            45, 211, 221, 151, 205, 240, 69, 133, 200, 15, 213, 170, 162, 127, 93, 224, 36, 86,
            116, 44, 42, 22, 255, 144, 193, 35, 175, 145, 62, 184, 67, 143, 199, 253, 37, 115, 23,
            154, 213, 141, 122, 105,
        ])
        .unwrap();

        let expected = super::GrandpaNotificationRef::Commit(super::CommitMessageRef {
            round_number: 3669,
            set_id: 3490,
            message: super::CompactCommitRef {
                target_hash: &[
                    182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34, 248, 98,
                    253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104, 187, 82,
                ],
                target_number: 5105457,
                precommits: vec![
                    super::UnsignedPrecommitRef {
                        target_hash: &[
                            182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34,
                            248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104,
                            187, 82,
                        ],
                        target_number: 5105457,
                    },
                    super::UnsignedPrecommitRef {
                        target_hash: &[
                            182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34,
                            248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104,
                            187, 82,
                        ],
                        target_number: 5105457,
                    },
                    super::UnsignedPrecommitRef {
                        target_hash: &[
                            182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34,
                            248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104,
                            187, 82,
                        ],
                        target_number: 5105457,
                    },
                    super::UnsignedPrecommitRef {
                        target_hash: &[
                            182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34,
                            248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104,
                            187, 82,
                        ],
                        target_number: 5105457,
                    },
                    super::UnsignedPrecommitRef {
                        target_hash: &[
                            182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34,
                            248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104,
                            187, 82,
                        ],
                        target_number: 5105457,
                    },
                    super::UnsignedPrecommitRef {
                        target_hash: &[
                            182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34,
                            248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104,
                            187, 82,
                        ],
                        target_number: 5105457,
                    },
                    super::UnsignedPrecommitRef {
                        target_hash: &[
                            182, 68, 115, 35, 15, 201, 152, 195, 12, 181, 59, 244, 231, 124, 34,
                            248, 98, 253, 4, 180, 158, 70, 161, 84, 76, 118, 151, 68, 101, 104,
                            187, 82,
                        ],
                        target_number: 5105457,
                    },
                ],
                auth_data: vec![
                    (
                        &[
                            189, 185, 216, 33, 163, 12, 201, 104, 162, 255, 11, 241, 156, 90, 244,
                            205, 251, 44, 45, 139, 129, 117, 178, 85, 129, 78, 58, 255, 76, 232,
                            199, 85, 236, 30, 227, 87, 50, 34, 22, 27, 241, 6, 33, 137, 55, 5, 190,
                            36, 122, 61, 112, 51, 99, 34, 119, 46, 185, 156, 188, 133, 140, 103,
                            33, 10,
                        ],
                        &[
                            45, 154, 173, 12, 30, 12, 25, 95, 195, 198, 235, 98, 29, 248, 44, 121,
                            73, 203, 132, 51, 196, 138, 65, 42, 3, 49, 169, 182, 129, 146, 242,
                            193,
                        ],
                    ),
                    (
                        &[
                            228, 217, 26, 9, 233, 239, 30, 213, 103, 10, 33, 27, 44, 13, 178, 236,
                            216, 167, 190, 9, 123, 151, 143, 1, 199, 58, 77, 121, 122, 215, 22, 19,
                            238, 190, 216, 8, 62, 6, 216, 37, 197, 124, 141, 51, 196, 205, 205,
                            193, 24, 86, 246, 60, 16, 139, 66, 51, 93, 168, 159, 147, 77, 90, 91,
                            8,
                        ],
                        &[
                            64, 14, 252, 119, 77, 211, 141, 23, 18, 115, 222, 3, 2, 22, 42, 105,
                            85, 176, 71, 232, 230, 141, 12, 9, 124, 205, 194, 191, 90, 47, 202,
                            233,
                        ],
                    ),
                    (
                        &[
                            218, 161, 80, 55, 8, 134, 223, 202, 4, 137, 45, 10, 71, 90, 162, 252,
                            99, 19, 252, 17, 175, 5, 75, 208, 81, 0, 96, 218, 5, 89, 250, 183, 161,
                            188, 227, 62, 107, 34, 63, 155, 28, 176, 141, 174, 113, 162, 229, 148,
                            55, 39, 65, 36, 97, 159, 198, 238, 222, 34, 76, 187, 40, 19, 109, 1,
                        ],
                        &[
                            67, 146, 40, 75, 194, 208, 80, 208, 221, 175, 151, 239, 239, 127, 65,
                            39, 237, 145, 130, 36, 154, 135, 68, 105, 52, 102, 49, 62, 137, 34,
                            187, 159,
                        ],
                    ),
                    (
                        &[
                            55, 157, 88, 195, 49, 116, 72, 11, 37, 132, 176, 74, 69, 60, 157, 67,
                            36, 156, 165, 71, 164, 86, 220, 240, 241, 13, 40, 125, 79, 147, 27, 56,
                            254, 198, 231, 108, 187, 214, 187, 98, 229, 123, 116, 160, 126, 192,
                            98, 132, 247, 206, 70, 228, 175, 152, 217, 252, 4, 109, 98, 24, 90,
                            117, 184, 11,
                        ],
                        &[
                            107, 32, 186, 217, 155, 44, 253, 198, 120, 175, 170, 229, 66, 122, 141,
                            158, 75, 68, 108, 104, 182, 223, 91, 126, 210, 38, 84, 143, 10, 142,
                            225, 77,
                        ],
                    ),
                    (
                        &[
                            169, 12, 215, 222, 158, 85, 4, 111, 196, 47, 56, 147, 93, 1, 202, 247,
                            137, 115, 30, 127, 94, 191, 31, 223, 162, 16, 73, 219, 118, 52, 40,
                            255, 191, 183, 70, 132, 115, 91, 214, 191, 156, 189, 203, 208, 152,
                            165, 115, 64, 123, 209, 153, 80, 44, 134, 143, 188, 140, 168, 162, 134,
                            178, 192, 122, 10,
                        ],
                        &[
                            137, 41, 133, 127, 72, 223, 16, 65, 170, 114, 53, 173, 180, 59, 208,
                            190, 54, 96, 123, 199, 137, 214, 115, 240, 73, 87, 253, 137, 81, 36,
                            66, 175,
                        ],
                    ),
                    (
                        &[
                            76, 40, 52, 216, 110, 234, 219, 158, 208, 142, 85, 168, 43, 164, 19,
                            154, 21, 125, 174, 153, 165, 45, 54, 100, 36, 196, 46, 95, 64, 192,
                            178, 156, 16, 112, 5, 237, 207, 113, 132, 125, 148, 34, 132, 105, 148,
                            216, 148, 182, 33, 74, 215, 161, 252, 44, 24, 67, 77, 87, 6, 94, 109,
                            38, 64, 10,
                        ],
                        &[
                            195, 28, 194, 169, 175, 7, 98, 210, 151, 4, 221, 136, 161, 204, 171,
                            251, 101, 63, 21, 245, 84, 189, 77, 59, 75, 136, 44, 17, 217, 119, 206,
                            191,
                        ],
                    ),
                    (
                        &[
                            191, 137, 127, 81, 55, 208, 225, 33, 209, 59, 83, 121, 234, 160, 191,
                            38, 82, 1, 102, 178, 140, 58, 20, 131, 206, 37, 148, 106, 135, 149, 74,
                            57, 27, 84, 215, 0, 47, 68, 1, 8, 139, 183, 125, 169, 4, 165, 168, 86,
                            218, 178, 95, 157, 185, 64, 45, 211, 221, 151, 205, 240, 69, 133, 200,
                            15,
                        ],
                        &[
                            213, 170, 162, 127, 93, 224, 36, 86, 116, 44, 42, 22, 255, 144, 193,
                            35, 175, 145, 62, 184, 67, 143, 199, 253, 37, 115, 23, 154, 213, 141,
                            122, 105,
                        ],
                    ),
                ],
            },
        });

        assert_eq!(actual, expected);
    }
}
