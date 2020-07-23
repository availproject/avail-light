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
//! The standard format for block headers is the
//! [SCALE encoding](https://substrate.dev/docs/en/knowledgebase/advanced/codec).

use blake2::digest::{Input as _, VariableOutput as _};
use core::{convert::TryFrom, iter};

/// Returns a hash of the SCALE-encoded header.
///
/// Does not verify the validity of the header.
pub fn hash_from_scale_encoded_header(header: &[u8]) -> [u8; 32] {
    let mut out = [0; 32];

    let mut hasher = blake2::VarBlake2b::new_keyed(&[], 32);
    hasher.input(header);
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
        let mut out = [0; 32];

        let mut hasher = blake2::VarBlake2b::new_keyed(&[], 32);
        for buffer in self.scale_encoding() {
            hasher.input(buffer.as_ref());
        }
        hasher.variable_result(|result| {
            debug_assert_eq!(result.len(), 32);
            out.copy_from_slice(result)
        });

        out
    }
}

/// Generic header digest.
#[derive(Debug, Clone)]
pub struct DigestRef<'a> {
    /// Number of log items in the header.
    /// Must always match the actual number of items in `digest`. The validity must be verified
    /// before a [`DigestRef`] object is instantiated.
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
    PreRuntime(&'a [u8; 4], &'a [u8]),
    Consensus(&'a [u8; 4], &'a [u8]),
    Seal(&'a [u8; 4], &'a [u8]),
    ChangesTrieSignal(ChangesTrieSignal),
    Other(&'a [u8]),
}

impl<'a> DigestItemRef<'a> {
    /// Returns an iterator to list of buffers which, when concatenated, produces the SCALE
    /// encoding of that digest item.
    pub fn scale_encoding(
        &self,
    ) -> impl Iterator<Item = impl AsRef<[u8]> + Clone + 'a> + Clone + 'a {
        let index = One([match self {
            DigestItemRef::ChangesTrieRoot(_) => 2,
            DigestItemRef::PreRuntime(_, _) => 6,
            DigestItemRef::Consensus(_, _) => 4,
            DigestItemRef::Seal(_, _) => 5,
            DigestItemRef::ChangesTrieSignal(_) => 7,
            DigestItemRef::Other(_) => 0,
        }]);

        #[derive(Clone)]
        struct One([u8; 1]);
        impl AsRef<[u8]> for One {
            fn as_ref(&self) -> &[u8] {
                &self.0[..]
            }
        }

        // TODO: don't use Vecs?
        match *self {
            DigestItemRef::PreRuntime(engine_id, data)
            | DigestItemRef::Consensus(engine_id, data)
            | DigestItemRef::Seal(engine_id, data) => {
                let encoded_len = parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(
                    u64::try_from(data.len()).unwrap(),
                ));

                let iter = iter::once(either::Either::Left(index))
                    .chain(iter::once(either::Either::Right(either::Either::Right(
                        &engine_id[..],
                    ))))
                    .chain(iter::once(either::Either::Right(either::Either::Left(
                        encoded_len,
                    ))))
                    .chain(iter::once(either::Either::Right(either::Either::Right(
                        data,
                    ))));

                either::Either::Left(either::Either::Left(iter))
            }
            DigestItemRef::ChangesTrieSignal(ref changes) => {
                let encoded = parity_scale_codec::Encode::encode(changes);
                let iter = iter::once(either::Either::Left(index)).chain(iter::once(
                    either::Either::Right(either::Either::Left(encoded)),
                ));
                either::Either::Left(either::Either::Right(iter))
            }
            DigestItemRef::ChangesTrieRoot(data) => {
                let iter = iter::once(either::Either::Left(index)).chain(iter::once(
                    either::Either::Right(either::Either::Right(&data[..])),
                ));
                either::Either::Right(either::Either::Left(iter))
            }
            DigestItemRef::Other(data) => {
                let encoded_len = parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(
                    u64::try_from(data.len()).unwrap(),
                ));

                let iter = iter::once(either::Either::Left(index))
                    .chain(iter::once(either::Either::Right(either::Either::Left(
                        encoded_len,
                    ))))
                    .chain(iter::once(either::Either::Right(either::Either::Right(
                        data,
                    ))));
                either::Either::Right(either::Either::Right(iter))
            }
        }
    }
}

/// Available changes trie signals.
// TODO: review documentation
#[derive(Debug, PartialEq, Eq, Clone, parity_scale_codec::Encode, parity_scale_codec::Decode)]
pub enum ChangesTrieSignal {
    /// New changes trie configuration is enacted, starting from **next block**.
    ///
    /// The block that emits this signal will contain changes trie (CT) that covers
    /// blocks range [BEGIN; current block], where BEGIN is (order matters):
    /// - LAST_TOP_LEVEL_DIGEST_BLOCK+1 if top level digest CT has ever been created
    ///   using current configuration AND the last top level digest CT has been created
    ///   at block LAST_TOP_LEVEL_DIGEST_BLOCK;
    /// - LAST_CONFIGURATION_CHANGE_BLOCK+1 if there has been CT configuration change
    ///   before and the last configuration change happened at block
    ///   LAST_CONFIGURATION_CHANGE_BLOCK;
    /// - 1 otherwise.
    NewConfiguration(Option<ChangesTrieConfiguration>),
}

/// Substrate changes trie configuration.
// TODO: review documentation
#[derive(
    Debug, Clone, PartialEq, Eq, Default, parity_scale_codec::Encode, parity_scale_codec::Decode,
)]
pub struct ChangesTrieConfiguration {
    /// Interval (in blocks) at which level1-digests are created. Digests are not
    /// created when this is less or equal to 1.
    pub digest_interval: u32,

    /// Maximal number of digest levels in hierarchy. 0 means that digests are not
    /// created at all (even level1 digests). 1 means only level1-digests are created.
    /// 2 means that every digest_interval^2 there will be a level2-digest, and so on.
    /// Please ensure that maximum digest interval (i.e. digest_interval^digest_levels)
    /// is within `u32` limits. Otherwise you'll never see digests covering such intervals
    /// && maximal digests interval will be truncated to the last interval that fits
    /// `u32` limits.
    pub digest_levels: u32,
}

/// Decodes a single digest log item. On success, returns the item and the data that remains
/// after the item.
fn decode_item<'a>(mut slice: &'a [u8]) -> Result<(DigestItemRef<'a>, &'a [u8]), Error> {
    let index = *slice.get(0).ok_or(Error::TooShort)?;
    slice = &slice[1..];

    match index {
        0 => {
            let len: parity_scale_codec::Compact<u64> =
                parity_scale_codec::Decode::decode(&mut slice)
                    .map_err(Error::DigestItemLenDecodeError)?;

            let len = TryFrom::try_from(len.0).map_err(|_| Error::TooShort)?;

            if slice.len() < len {
                return Err(Error::TooShort);
            }

            let content = &slice[..len];
            slice = &slice[len..];
            Ok((DigestItemRef::Other(content), slice))
        }
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

            let item = match index {
                4 => DigestItemRef::Consensus(engine_id, content),
                5 => DigestItemRef::Seal(engine_id, content),
                6 => DigestItemRef::PreRuntime(engine_id, content),
                _ => unreachable!(),
            };

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
