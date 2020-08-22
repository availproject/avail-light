// Copyright 2017-2020 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

//! Parsing SCALE-encoded header.
//!
//! Each block of a chain is composed of two parts: its header, and its body.
//!
//! The header of a block consists in a list of hardcoded fields such as the parent block's hash
//! or the block number, and a variable-sized list of log items.
//!
//! The standard format of a block header is the
//! [SCALE encoding](https://substrate.dev/docs/en/knowledgebase/advanced/codec). It is typically
//! under this encoding that block headers are for example transferred over the network or stored
//! in the database. Use the [`decode`] function in order to decompose a SCALE-encoded header
//! into a usable [`HeaderRef`].

use blake2::digest::{Input as _, VariableOutput as _};
use core::{convert::TryFrom, fmt, iter};
use parity_scale_codec::{Decode as _, DecodeAll as _};

mod babe;
mod grandpa;

pub use babe::*;
pub use grandpa::*;

/// Returns a hash of a SCALE-encoded header.
///
/// Does not verify the validity of the header.
pub fn hash_from_scale_encoded_header(header: impl AsRef<[u8]>) -> [u8; 32] {
    hash_from_scale_encoded_header_vectored(iter::once(header))
}

/// Returns a hash of a SCALE-encoded header.
///
/// Must be passed a list of buffers, which, when concatenated, form the SCALE-encoded header.
///
/// Does not verify the validity of the header.
pub fn hash_from_scale_encoded_header_vectored(
    header: impl Iterator<Item = impl AsRef<[u8]>>,
) -> [u8; 32] {
    let mut out = [0; 32];

    let mut hasher = blake2::VarBlake2b::new_keyed(&[], 32);
    for buf in header {
        hasher.input(buf.as_ref());
    }
    hasher.variable_result(|result| {
        debug_assert_eq!(result.len(), 32);
        out.copy_from_slice(result)
    });

    out
}

/// Attempt to decode the given SCALE-encoded header.
pub fn decode<'a>(mut scale_encoded: &'a [u8]) -> Result<HeaderRef<'a>, Error> {
    if scale_encoded.len() < 32 + 1 {
        return Err(Error::TooShort);
    }

    let parent_hash: &[u8; 32] = TryFrom::try_from(&scale_encoded[0..32]).unwrap();
    scale_encoded = &scale_encoded[32..];

    let number: parity_scale_codec::Compact<u64> =
        parity_scale_codec::Decode::decode(&mut scale_encoded)
            .map_err(Error::BlockNumberDecodeError)?;

    if scale_encoded.len() < 32 + 32 + 1 {
        return Err(Error::TooShort);
    }

    let state_root: &[u8; 32] = TryFrom::try_from(&scale_encoded[0..32]).unwrap();
    scale_encoded = &scale_encoded[32..];
    let extrinsics_root: &[u8; 32] = TryFrom::try_from(&scale_encoded[0..32]).unwrap();
    scale_encoded = &scale_encoded[32..];

    let digest_logs_len: parity_scale_codec::Compact<u64> =
        parity_scale_codec::Decode::decode(&mut scale_encoded)
            .map_err(Error::DigestLenDecodeError)?;
    let digest = scale_encoded;

    // Iterate through the log items to see if anything is wrong.
    {
        let mut digest = digest;
        for _ in 0..digest_logs_len.0 {
            let (_, next) = decode_item(digest)?;
            digest = next;
        }

        if !digest.is_empty() {
            return Err(Error::TooLong);
        }
    }

    Ok(HeaderRef {
        parent_hash,
        number: number.0,
        state_root,
        extrinsics_root,
        digest: DigestRef {
            digest_logs_len: digest_logs_len.0,
            digest,
        },
    })
}

/// Potential error when decoding a header.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Header is not long enough.
    TooShort,
    /// Header is too long.
    TooLong,
    /// Error while decoding the block number.
    BlockNumberDecodeError(parity_scale_codec::Error),
    /// Error while decoding the digest length.
    DigestLenDecodeError(parity_scale_codec::Error),
    /// Error while decoding a digest log item length.
    DigestItemLenDecodeError(parity_scale_codec::Error),
    /// Error while decoding a digest item.
    DigestItemDecodeError(parity_scale_codec::Error),
    /// Digest log item with an unrecognized type.
    UnknownDigestLogType(u8),
    /// Bad length of a BABE seal.
    BadBabeSealLength,
    BadBabePreDigestRefType,
    BadBabeConsensusRefType,
    BadGrandpaConsensusRefType,
    /// Unknown consensus engine specified in a digest log.
    #[display(fmt = "Unknown consensus engine specified in a digest log: {:?}", _0)]
    UnknownConsensusEngine([u8; 4]),
}

/// Header of a block, after decoding.
///
/// Note that the information in there are not guaranteed to be exact. The exactness of the
/// information depends on the context.
#[derive(Debug, Clone)]
pub struct HeaderRef<'a> {
    /// Hash of the parent block stored in the header.
    pub parent_hash: &'a [u8; 32],
    /// Block number stored in the header.
    pub number: u64,
    /// The state trie merkle root
    pub state_root: &'a [u8; 32],
    /// The merkle root of the extrinsics.
    pub extrinsics_root: &'a [u8; 32],
    /// List of auxiliary data appended to the block header.
    pub digest: DigestRef<'a>,
}

impl<'a> HeaderRef<'a> {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of the header.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't allocate?
        let encoded_number =
            parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(self.number));

        iter::once(either::Either::Left(either::Either::Left(
            &self.parent_hash[..],
        )))
        .chain(iter::once(either::Either::Left(either::Either::Right(
            encoded_number,
        ))))
        .chain(iter::once(either::Either::Left(either::Either::Left(
            &self.state_root[..],
        ))))
        .chain(iter::once(either::Either::Left(either::Either::Left(
            &self.extrinsics_root[..],
        ))))
        .chain(self.digest.scale_encoding().map(either::Either::Right))
    }

    /// Builds the hash of the header.
    pub fn hash(&self) -> [u8; 32] {
        hash_from_scale_encoded_header_vectored(self.scale_encoding())
    }
}

/// Generic header digest.
#[derive(Clone)]
pub struct DigestRef<'a> {
    /// Number of log items in the header.
    /// Must always match the actual number of items in [`DigestRef::digest`]. The validity must
    /// be verified before a [`DigestRef`] object is instantiated.
    digest_logs_len: u64,
    /// Encoded digest. Its validity must be verified before a [`DigestRef`] object is instantiated.
    digest: &'a [u8],
}

impl<'a> DigestRef<'a> {
    /// Returns a digest with empty logs.
    pub const fn empty() -> DigestRef<'a> {
        DigestRef {
            digest_logs_len: 0,
            digest: &[],
        }
    }

    /// Pops the last element of the [`DigestRef`].
    pub fn pop(&mut self) -> Option<DigestItemRef<'a>> {
        let digest_logs_len_minus_one = self.digest_logs_len.checked_sub(1)?;

        let mut iter = self.logs();
        for _ in 0..digest_logs_len_minus_one {
            let _item = iter.next();
            debug_assert!(_item.is_some());
        }

        self.digest_logs_len = digest_logs_len_minus_one;
        self.digest = &self.digest[..self.digest.len() - iter.pointer.len()];

        debug_assert_eq!(iter.remaining_len, 1);
        Some(iter.next().unwrap())
    }

    /// Returns an iterator to the log items in this digest.
    pub fn logs(&self) -> LogsIter<'a> {
        LogsIter {
            pointer: self.digest,
            remaining_len: self.digest_logs_len,
        }
    }

    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of the digest items.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't allocate?
        let encoded_len =
            parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(self.digest_logs_len));
        iter::once(either::Either::Left(encoded_len)).chain(
            self.logs()
                .flat_map(|v| v.scale_encoding().map(either::Either::Right)),
        )
    }
}

impl<'a> fmt::Debug for DigestRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.logs()).finish()
    }
}

/// Iterator towards the digest log items.
#[derive(Debug, Clone)]
pub struct LogsIter<'a> {
    /// Encoded digest.
    pointer: &'a [u8],
    /// Number of log items remaining.
    remaining_len: u64,
}

impl<'a> Iterator for LogsIter<'a> {
    type Item = DigestItemRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining_len == 0 {
            return None;
        }

        // Validity is guaranteed when the `DigestRef` is constructed.
        let (item, new_pointer) = decode_item(self.pointer).unwrap();
        self.pointer = new_pointer;
        self.remaining_len -= 1;

        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let num = usize::try_from(self.remaining_len).unwrap();
        (num, Some(num))
    }
}

impl<'a> ExactSizeIterator for LogsIter<'a> {}

// TODO: document
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum DigestItemRef<'a> {
    ChangesTrieRoot(&'a [u8; 32]),
    BabePreDigest(BabePreDigestRef<'a>),
    BabeConsensus(BabeConsensusLogRef<'a>),
    GrandpaConsensus(GrandpaConsensusLogRef<'a>),

    /// Block signature made using the BABE consensus engine.
    ///
    /// Guaranteed to be 64 bytes long.
    // TODO: we don't use a &[u8; 64] because traits aren't defined on this type; need to fix after Rust gets proper support or use a newtype
    BabeSeal(&'a [u8]),
    ChangesTrieSignal(ChangesTrieSignal),
}

impl<'a> DigestItemRef<'a> {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that digest item.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        // TODO: don't use Vecs?
        match *self {
            DigestItemRef::BabePreDigest(ref babe_pre_digest) => {
                let encoded = babe_pre_digest
                    .scale_encoding()
                    .fold(Vec::new(), |mut a, b| {
                        a.extend_from_slice(b.as_ref());
                        a
                    });

                let mut ret = vec![6];
                ret.extend_from_slice(b"BABE");
                ret.extend_from_slice(&parity_scale_codec::Encode::encode(
                    &parity_scale_codec::Compact(u64::try_from(encoded.len()).unwrap()),
                ));
                ret.extend_from_slice(&encoded);
                iter::once(ret)
            }
            DigestItemRef::BabeConsensus(ref babe_consensus) => {
                let encoded = babe_consensus
                    .scale_encoding()
                    .fold(Vec::new(), |mut a, b| {
                        a.extend_from_slice(b.as_ref());
                        a
                    });

                let mut ret = vec![4];
                ret.extend_from_slice(b"BABE");
                ret.extend_from_slice(&parity_scale_codec::Encode::encode(
                    &parity_scale_codec::Compact(u64::try_from(encoded.len()).unwrap()),
                ));
                ret.extend_from_slice(&encoded);
                iter::once(ret)
            }
            DigestItemRef::GrandpaConsensus(ref gp_consensus) => {
                let encoded = gp_consensus.scale_encoding().fold(Vec::new(), |mut a, b| {
                    a.extend_from_slice(b.as_ref());
                    a
                });

                let mut ret = vec![4];
                ret.extend_from_slice(b"FRNK");
                ret.extend_from_slice(&parity_scale_codec::Encode::encode(
                    &parity_scale_codec::Compact(u64::try_from(encoded.len()).unwrap()),
                ));
                ret.extend_from_slice(&encoded);
                iter::once(ret)
            }
            DigestItemRef::BabeSeal(seal) => {
                assert_eq!(seal.len(), 64);

                let mut ret = vec![5];
                ret.extend_from_slice(b"BABE");
                ret.extend_from_slice(&parity_scale_codec::Encode::encode(
                    &parity_scale_codec::Compact(64u32),
                ));
                ret.extend_from_slice(&seal);
                iter::once(ret)
            }
            DigestItemRef::ChangesTrieSignal(ref changes) => {
                let mut ret = vec![7];
                ret.extend_from_slice(&parity_scale_codec::Encode::encode(changes));
                iter::once(ret)
            }
            DigestItemRef::ChangesTrieRoot(data) => {
                let mut ret = vec![2];
                ret.extend_from_slice(data);
                iter::once(ret)
            }
        }
    }
}

/// Decodes a single digest log item. On success, returns the item and the data that remains
/// after the item.
fn decode_item<'a>(mut slice: &'a [u8]) -> Result<(DigestItemRef<'a>, &'a [u8]), Error> {
    let index = *slice.get(0).ok_or(Error::TooShort)?;
    slice = &slice[1..];

    match index {
        4 | 5 | 6 => {
            if slice.len() < 4 {
                return Err(Error::TooShort);
            }

            let engine_id: &[u8; 4] = TryFrom::try_from(&slice[..4]).unwrap();
            slice = &slice[4..];

            let len: parity_scale_codec::Compact<u64> =
                parity_scale_codec::Decode::decode(&mut slice)
                    .map_err(Error::DigestItemLenDecodeError)?;

            let len = TryFrom::try_from(len.0).map_err(|_| Error::TooShort)?;

            if slice.len() < len {
                return Err(Error::TooShort);
            }

            let content = &slice[..len];
            slice = &slice[len..];

            let item = decode_item_from_parts(index, engine_id, content)?;
            Ok((item, slice))
        }
        2 => {
            if slice.len() < 32 {
                return Err(Error::TooShort);
            }

            let hash: &[u8; 32] = TryFrom::try_from(&slice[0..32]).unwrap();
            slice = &slice[32..];
            Ok((DigestItemRef::ChangesTrieRoot(hash), slice))
        }
        7 => {
            let item = parity_scale_codec::Decode::decode(&mut slice)
                .map_err(Error::DigestItemDecodeError)?;
            Ok((DigestItemRef::ChangesTrieSignal(item), slice))
        }
        ty => Err(Error::UnknownDigestLogType(ty)),
    }
}

/// When we know the index, engine id, and content of an item, we can finish decoding.
fn decode_item_from_parts<'a>(
    index: u8,
    engine_id: &'a [u8; 4],
    content: &'a [u8],
) -> Result<DigestItemRef<'a>, Error> {
    Ok(match (index, engine_id) {
        (4, b"BABE") => DigestItemRef::BabeConsensus(BabeConsensusLogRef::from_slice(content)?),
        (4, b"FRNK") => {
            DigestItemRef::GrandpaConsensus(GrandpaConsensusLogRef::from_slice(content)?)
        }
        (4, e) => return Err(Error::UnknownConsensusEngine(*e)),
        (5, b"BABE") => DigestItemRef::BabeSeal({
            if content.len() != 64 {
                return Err(Error::BadBabeSealLength);
            }
            content
        }),
        (5, e) => return Err(Error::UnknownConsensusEngine(*e)),
        (6, b"BABE") => DigestItemRef::BabePreDigest(BabePreDigestRef::from_slice(content)?),
        (6, e) => return Err(Error::UnknownConsensusEngine(*e)),
        _ => unreachable!(),
    })
}
