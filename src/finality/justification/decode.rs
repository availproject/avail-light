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

use alloc::vec::Vec;
use core::{convert::TryFrom, fmt};

/// Attempt to decode the given SCALE-encoded justification.
// TODO: shouldn't this method be specific to Grandpa?
pub fn decode(scale_encoded: &[u8]) -> Result<JustificationRef, Error> {
    match nom::combinator::all_consuming(justification)(scale_encoded) {
        Ok((_, justification)) => Ok(justification),
        Err(nom::Err::Error(err)) | Err(nom::Err::Failure(err)) => Err(Error(err.code)),
        Err(_) => unreachable!(),
    }
}

/// Attempt to decode the given SCALE-encoded justification.
///
/// Contrary to [`decode`], doesn't return an error if the slice is too long but returns the
/// remainder.
pub fn decode_partial(scale_encoded: &[u8]) -> Result<(JustificationRef, &[u8]), Error> {
    match justification(scale_encoded) {
        Ok((remainder, justification)) => Ok((justification, remainder)),
        Err(nom::Err::Error(err)) | Err(nom::Err::Failure(err)) => Err(Error(err.code)),
        Err(_) => unreachable!(),
    }
}

const PRECOMMIT_ENCODED_LEN: usize = 32 + 4 + 64 + 32;

/// Decoded justification.
// TODO: document and explain
#[derive(Debug)]
pub struct JustificationRef<'a> {
    pub round: u64,
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
    pub precommits: PrecommitsRef<'a>,
    pub votes_ancestries: VotesAncestriesIter<'a>,
}

/// Decoded justification.
// TODO: document and explain
#[derive(Debug)]
pub struct Justification {
    pub round: u64,
    pub target_hash: [u8; 32],
    pub target_number: u32,
    pub precommits: Vec<Precommit>,
    // TODO: pub votes_ancestries: Vec<Header>,
}

impl<'a> From<&'a Justification> for JustificationRef<'a> {
    fn from(j: &'a Justification) -> JustificationRef<'a> {
        JustificationRef {
            round: j.round,
            target_hash: &j.target_hash,
            target_number: j.target_number,
            precommits: PrecommitsRef {
                inner: PrecommitsRefInner::Decoded(&j.precommits),
            },
            // TODO:
            votes_ancestries: VotesAncestriesIter { slice: &[], num: 0 },
        }
    }
}

impl<'a> From<JustificationRef<'a>> for Justification {
    fn from(j: JustificationRef<'a>) -> Justification {
        Justification {
            round: j.round,
            target_hash: *j.target_hash,
            target_number: j.target_number,
            precommits: j.precommits.iter().map(Into::into).collect(),
        }
    }
}

#[derive(Copy, Clone)]
pub struct PrecommitsRef<'a> {
    inner: PrecommitsRefInner<'a>,
}

impl<'a> fmt::Debug for PrecommitsRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[derive(Copy, Clone)]
enum PrecommitsRefInner<'a> {
    Undecoded(&'a [u8]),
    Decoded(&'a [Precommit]),
}

impl<'a> PrecommitsRef<'a> {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = PrecommitRef<'a>> + 'a {
        match self.inner {
            PrecommitsRefInner::Undecoded(slice) => {
                debug_assert_eq!(slice.len() % PRECOMMIT_ENCODED_LEN, 0);
                PrecommitsRefIter {
                    inner: PrecommitsRefIterInner::Undecoded {
                        remaining_len: slice.len() / PRECOMMIT_ENCODED_LEN,
                        pointer: slice,
                    },
                }
            }
            PrecommitsRefInner::Decoded(slice) => PrecommitsRefIter {
                inner: PrecommitsRefIterInner::Decoded(slice.iter()),
            },
        }
    }
}

pub struct PrecommitsRefIter<'a> {
    inner: PrecommitsRefIterInner<'a>,
}

enum PrecommitsRefIterInner<'a> {
    Decoded(core::slice::Iter<'a, Precommit>),
    Undecoded {
        remaining_len: usize,
        pointer: &'a [u8],
    },
}

impl<'a> Iterator for PrecommitsRefIter<'a> {
    type Item = PrecommitRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            PrecommitsRefIterInner::Decoded(iter) => iter.next().map(Into::into),
            PrecommitsRefIterInner::Undecoded {
                pointer,
                remaining_len,
            } => {
                if *remaining_len == 0 {
                    return None;
                }

                let (new_pointer, precommit) = precommit(pointer).unwrap();
                *pointer = new_pointer;
                *remaining_len -= 1;

                Some(precommit)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.inner {
            PrecommitsRefIterInner::Decoded(iter) => iter.size_hint(),
            PrecommitsRefIterInner::Undecoded { remaining_len, .. } => {
                (*remaining_len, Some(*remaining_len))
            }
        }
    }
}

impl<'a> ExactSizeIterator for PrecommitsRefIter<'a> {}

#[derive(Debug, Clone)]
pub struct PrecommitRef<'a> {
    /// Hash of the block concerned by the pre-commit.
    pub target_hash: &'a [u8; 32],
    /// Height of the block concerned by the pre-commit.
    pub target_number: u32,

    /// Ed25519 signature made with [`PrecommitRef::authority_public_key`].
    // TODO: document what is being signed
    pub signature: &'a [u8; 64],

    /// Authority that signed the precommit. Must be part of the authority set for the
    /// justification to be valid.
    pub authority_public_key: &'a [u8; 32],
}

impl<'a> PrecommitRef<'a> {
    /// Decodes a SCALE-encoded precommit.
    ///
    /// Returns the rest of the data alongside with the decoded struct.
    pub fn decode_partial(scale_encoded: &[u8]) -> Result<(PrecommitRef, &[u8]), Error> {
        match precommit(scale_encoded) {
            Ok((remainder, precommit)) => Ok((precommit, remainder)),
            Err(nom::Err::Error(err)) | Err(nom::Err::Failure(err)) => Err(Error(err.code)),
            Err(_) => unreachable!(),
        }
    }
}

pub struct Precommit {
    /// Hash of the block concerned by the pre-commit.
    pub target_hash: [u8; 32],
    /// Height of the block concerned by the pre-commit.
    pub target_number: u32,

    /// Ed25519 signature made with [`PrecommitRef::authority_public_key`].
    // TODO: document what is being signed
    pub signature: [u8; 64],

    /// Authority that signed the precommit. Must be part of the authority set for the
    /// justification to be valid.
    pub authority_public_key: [u8; 32],
}

impl<'a> From<&'a Precommit> for PrecommitRef<'a> {
    fn from(pc: &'a Precommit) -> PrecommitRef<'a> {
        PrecommitRef {
            target_hash: &pc.target_hash,
            target_number: pc.target_number,
            signature: &pc.signature,
            authority_public_key: &pc.authority_public_key,
        }
    }
}

impl<'a> From<PrecommitRef<'a>> for Precommit {
    fn from(pc: PrecommitRef<'a>) -> Precommit {
        Precommit {
            target_hash: *pc.target_hash,
            target_number: pc.target_number,
            signature: *pc.signature,
            authority_public_key: *pc.authority_public_key,
        }
    }
}

impl fmt::Debug for Precommit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Precommit")
            .field("target_hash", &self.target_hash)
            .field("target_number", &self.target_number)
            .field("signature", &&self.signature[..])
            .field("authority_public_key", &self.authority_public_key)
            .finish()
    }
}

/// Iterator towards the headers of the vote ancestries.
#[derive(Debug, Clone)]
pub struct VotesAncestriesIter<'a> {
    /// Encoded headers.
    slice: &'a [u8],
    /// Number of headers items remaining.
    num: usize,
}

impl<'a> Iterator for VotesAncestriesIter<'a> {
    type Item = header::HeaderRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.num == 0 {
            return None;
        }

        // Validity is guaranteed when the `VotesAncestriesIter` is constructed.
        let (item, new_slice) = header::decode_partial(self.slice).unwrap();
        self.slice = new_slice;
        self.num -= 1;

        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.num, Some(self.num))
    }
}

impl<'a> ExactSizeIterator for VotesAncestriesIter<'a> {}

/// Potential error when decoding a justification.
#[derive(Debug, derive_more::Display)]
#[display(fmt = "Justification parsing error: {:?}", _0)]
pub struct Error(nom::error::ErrorKind);

/// Nom combinator that parses a justification.
fn justification(bytes: &[u8]) -> nom::IResult<&[u8], JustificationRef> {
    nom::error::context(
        "justification",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::number::complete::le_u64,
                nom::bytes::complete::take(32u32),
                nom::number::complete::le_u32,
                precommits,
                votes_ancestries,
            )),
            |(round, target_hash, target_number, precommits, votes_ancestries)| JustificationRef {
                round,
                target_hash: TryFrom::try_from(target_hash).unwrap(),
                target_number,
                precommits,
                votes_ancestries,
            },
        ),
    )(bytes)
}

/// Nom combinator that parses a list of precommits.
fn precommits(bytes: &[u8]) -> nom::IResult<&[u8], PrecommitsRef> {
    nom::combinator::map(
        nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |num_elems| {
            nom::combinator::recognize(nom::multi::fold_many_m_n(
                num_elems,
                num_elems,
                precommit,
                (),
                |(), _| (),
            ))
        }),
        |inner| PrecommitsRef {
            inner: PrecommitsRefInner::Undecoded(inner),
        },
    )(bytes)
}

/// Nom combinator that parses a single precommit.
fn precommit(bytes: &[u8]) -> nom::IResult<&[u8], PrecommitRef> {
    nom::error::context(
        "precommit",
        nom::combinator::map(
            nom::sequence::tuple((
                nom::bytes::complete::take(32u32),
                nom::number::complete::le_u32,
                nom::bytes::complete::take(64u32),
                nom::bytes::complete::take(32u32),
            )),
            |(target_hash, target_number, signature, authority_public_key)| PrecommitRef {
                target_hash: TryFrom::try_from(target_hash).unwrap(),
                target_number,
                signature: TryFrom::try_from(signature).unwrap(),
                authority_public_key: TryFrom::try_from(authority_public_key).unwrap(),
            },
        ),
    )(bytes)
}

/// Nom combinator that parses a list of headers.
fn votes_ancestries(bytes: &[u8]) -> nom::IResult<&[u8], VotesAncestriesIter> {
    nom::error::context(
        "votes ancestries",
        nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |num_elems| {
            nom::combinator::map(
                nom::combinator::recognize(nom::multi::fold_many_m_n(
                    num_elems,
                    num_elems,
                    |s| {
                        header::decode_partial(s).map(|(a, b)| (b, a)).map_err(|_| {
                            nom::Err::Failure(nom::error::make_error(
                                s,
                                nom::error::ErrorKind::Verify,
                            ))
                        })
                    },
                    (),
                    |(), _| (),
                )),
                move |slice| VotesAncestriesIter {
                    slice,
                    num: num_elems,
                },
            )
        }),
    )(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn decode() {
        super::decode(&[
            7, 181, 6, 0, 0, 0, 0, 0, 41, 241, 171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160,
            115, 76, 8, 195, 253, 109, 240, 108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210,
            0, 158, 4, 0, 20, 41, 241, 171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115,
            76, 8, 195, 253, 109, 240, 108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0,
            158, 4, 0, 13, 247, 129, 120, 204, 170, 120, 173, 41, 241, 213, 234, 121, 111, 20, 38,
            193, 94, 99, 139, 57, 30, 71, 209, 236, 222, 165, 123, 70, 139, 71, 65, 36, 142, 39,
            13, 94, 240, 44, 174, 150, 85, 149, 223, 166, 82, 210, 103, 40, 129, 102, 26, 212, 116,
            231, 209, 163, 107, 49, 82, 229, 197, 82, 8, 28, 21, 28, 17, 203, 114, 51, 77, 38, 215,
            7, 105, 227, 175, 123, 191, 243, 128, 26, 78, 45, 202, 43, 9, 183, 204, 224, 175, 141,
            216, 19, 7, 41, 241, 171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115, 76, 8,
            195, 253, 109, 240, 108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0, 158, 4,
            0, 62, 37, 145, 44, 21, 192, 120, 229, 236, 113, 122, 56, 193, 247, 45, 210, 184, 12,
            62, 220, 253, 147, 70, 133, 85, 18, 90, 167, 201, 118, 23, 107, 184, 187, 3, 104, 170,
            132, 17, 18, 89, 77, 156, 145, 242, 8, 185, 88, 74, 87, 21, 52, 247, 101, 57, 154, 163,
            5, 130, 20, 15, 230, 8, 3, 104, 13, 39, 130, 19, 249, 8, 101, 138, 73, 161, 2, 90, 127,
            70, 108, 25, 126, 143, 182, 250, 187, 94, 98, 34, 10, 123, 215, 95, 134, 12, 171, 41,
            241, 171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115, 76, 8, 195, 253, 109,
            240, 108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0, 158, 4, 0, 125, 172, 79,
            71, 1, 38, 137, 128, 232, 95, 70, 104, 217, 95, 7, 58, 28, 114, 182, 216, 171, 56, 231,
            218, 199, 244, 220, 122, 6, 225, 5, 175, 172, 47, 198, 61, 84, 42, 75, 66, 62, 90, 243,
            18, 58, 36, 108, 235, 132, 103, 136, 38, 164, 164, 237, 164, 41, 225, 152, 157, 146,
            237, 24, 11, 142, 89, 54, 135, 0, 234, 137, 226, 191, 137, 34, 204, 158, 75, 134, 214,
            101, 29, 28, 104, 154, 13, 87, 129, 63, 151, 104, 219, 170, 222, 207, 113, 41, 241,
            171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115, 76, 8, 195, 253, 109, 240,
            108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0, 158, 4, 0, 68, 192, 211, 142,
            239, 33, 55, 222, 165, 127, 203, 155, 217, 170, 61, 95, 206, 74, 74, 19, 123, 60, 67,
            142, 80, 18, 175, 40, 136, 156, 151, 224, 191, 157, 91, 187, 39, 185, 249, 212, 158,
            73, 197, 90, 54, 222, 13, 76, 181, 134, 69, 3, 165, 248, 94, 196, 68, 186, 80, 218, 87,
            162, 17, 11, 222, 166, 244, 167, 39, 211, 178, 57, 146, 117, 214, 238, 136, 23, 136,
            31, 16, 89, 116, 113, 220, 29, 39, 241, 68, 41, 90, 214, 251, 147, 60, 122, 41, 241,
            171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115, 76, 8, 195, 253, 109, 240,
            108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0, 158, 4, 0, 58, 187, 123, 135,
            2, 157, 81, 197, 40, 200, 218, 52, 253, 193, 119, 104, 190, 246, 221, 225, 175, 195,
            177, 218, 209, 175, 83, 119, 98, 175, 196, 48, 67, 76, 59, 223, 13, 202, 48, 1, 10, 99,
            200, 201, 123, 29, 89, 131, 120, 70, 162, 235, 11, 191, 96, 57, 83, 51, 217, 199, 35,
            50, 174, 2, 247, 45, 175, 46, 86, 14, 79, 15, 34, 251, 92, 187, 4, 173, 29, 127, 238,
            133, 10, 171, 35, 143, 208, 20, 193, 120, 118, 158, 126, 58, 155, 132, 0,
        ])
        .unwrap();
    }
}
