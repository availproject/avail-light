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
use core::convert::TryFrom;
use parity_scale_codec::{Decode, Encode, EncodeAsRef, EncodeLike, HasCompact, Input, Output};

/// Returns a hash of the SCALE-encoded header.
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
pub fn decode<'a>(mut scale_encoded: &'a [u8]) -> Result<DecodedHeader<'a>, Error> {
    if scale_encoded.len() < 32 + 8 + 32 + 32 {
        return Err(Error::TooShort);
    }

    let parent_hash: &[u8; 32] = TryFrom::try_from(&scale_encoded[0..32]).unwrap();
    scale_encoded = &scale_encoded[32..];

    let number: parity_scale_codec::Compact<u64> =
        parity_scale_codec::Decode::decode(&mut scale_encoded)
            .map_err(Error::BlockNumberDecodeError)?;

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

    Ok(DecodedHeader {
        parent_hash,
        number: number.0,
        state_root,
        extrinsics_root,
        digest: Digest {
            digest_logs_len: digest_logs_len.0,
            digest,
        },
    })
}

/// Potential error while decoding a header.
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
pub struct DecodedHeader<'a> {
    /// Hash of the parent block stored in the header.
    pub parent_hash: &'a [u8; 32],
    /// Block number stored in the header.
    pub number: u64,
    /// The state trie merkle root
    pub state_root: &'a [u8; 32],
    /// The merkle root of the extrinsics.
    pub extrinsics_root: &'a [u8; 32],
    /// List of auxiliary data appended to the block header.
    pub digest: Digest<'a>,
}

/// Generic header digest.
#[derive(Debug, Clone)]
pub struct Digest<'a> {
    /// Number of log items in the header.
    digest_logs_len: u64,
    /// Encoded digest.
    digest: &'a [u8],
}

impl<'a> Digest<'a> {
    /// Pops the last element of the [`Digest`].
    pub fn pop(&mut self) -> Option<DigestItem<'a>> {
        let digest_logs_len_minus_one = self.digest_logs_len.checked_sub(1)?;

        let mut iter = self.logs();
        for _ in 0..digest_logs_len_minus_one {
            let _item = iter.next();
            debug_assert!(_item.is_some());
        }

        self.digest_logs_len = digest_logs_len_minus_one;
        self.digest = &self.digest[..self.digest.len() - iter.pointer.len()];

        Some(iter.next().unwrap())
    }

    /// Returns an iterator to the log items in this digest.
    pub fn logs(&self) -> LogsIter<'a> {
        LogsIter {
            pointer: self.digest,
            remaining_len: self.digest_logs_len,
        }
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
    type Item = DigestItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining_len == 0 {
            return None;
        }

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

/// A 'referencing view' for digest item. Does not own its contents. Used by
/// final runtime implementations for encoding/decoding its log items.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum DigestItem<'a> {
    ChangesTrieRoot(&'a [u8; 32]),
    PreRuntime(&'a [u8; 4], &'a [u8]),
    Consensus(&'a [u8; 4], &'a [u8]),
    Seal(&'a [u8; 4], &'a [u8]),
    /// Digest item that contains signal from changes tries manager to the
    /// native code.
    ChangesTrieSignal(ChangesTrieSignal),
    /// Any 'non-system' digest item, opaque to the native code.
    Other(&'a [u8]),
}

/// Available changes trie signals.
#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Encode, Decode)]
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
fn decode_item<'a>(mut slice: &'a [u8]) -> Result<(DigestItem<'a>, &'a [u8]), Error> {
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
            Ok((DigestItem::Other(content), slice))
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
                4 => DigestItem::Consensus(engine_id, content),
                5 => DigestItem::Seal(engine_id, content),
                6 => DigestItem::PreRuntime(engine_id, content),
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
            Ok((DigestItem::ChangesTrieRoot(hash), slice))
        }
        7 => {
            let item = parity_scale_codec::Decode::decode(&mut slice)
                .map_err(Error::DigestItemDecodeError)?;
            Ok((DigestItem::ChangesTrieSignal(item), slice))
        }
        ty => Err(Error::UnknownDigestLogType(ty)),
    }
}
