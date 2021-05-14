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

use alloc::{borrow::Cow, vec::Vec};

mod tests;

/// A runtime blob beginning with this prefix should first be decompressed with zstandard
/// compression.
///
/// This differs from the Wasm magic bytes, so real Wasm blobs will not have this prefix.
pub(super) const ZSTD_PREFIX: [u8; 8] = [82, 188, 83, 118, 70, 219, 142, 5];

/// If the given blob starts with [`ZSTD_PREFIX`], decompresses it. Otherwise, passes it through.
///
/// The output data shall not be larger than `max_allowed`, to avoid potential zip bombs.
pub(super) fn zstd_decode_if_necessary(
    data: &[u8],
    max_allowed: usize,
) -> Result<Cow<[u8]>, Error> {
    if data.starts_with(&ZSTD_PREFIX) {
        Ok(Cow::Owned(zstd_decode(
            &data[ZSTD_PREFIX.len()..],
            max_allowed,
        )?))
    } else if data.len() > max_allowed {
        Err(Error::TooLarge)
    } else {
        Ok(Cow::Borrowed(data))
    }
}

/// Decompresses the given blob of zstd-compressed data.
///
/// The output data shall not be larger than `max_allowed`, to avoid potential zip bombs.
fn zstd_decode(mut data: &[u8], max_allowed: usize) -> Result<Vec<u8>, Error> {
    let mut decoder = ruzstd::frame_decoder::FrameDecoder::new();
    decoder.init(&mut data).map_err(|_| Error::InvalidZstd)?;

    match decoder.decode_blocks(
        &mut data,
        ruzstd::frame_decoder::BlockDecodingStrategy::UptoBytes(max_allowed),
    ) {
        Ok(true) => {}
        Ok(false) => return Err(Error::TooLarge),
        Err(_) => return Err(Error::InvalidZstd),
    }
    debug_assert!(decoder.is_finished());

    // When the decoding is finished, `Some` is always guaranteed to be returned.
    let out_buf = decoder.collect().unwrap();
    debug_assert!(out_buf.len() <= max_allowed);
    Ok(out_buf)
}

/// Error possibly returned when decoding a zstd-compressed Wasm blob.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// The data is zstandard-compressed, but the data is in an invalid format.
    InvalidZstd,
    /// The size of the code exceeds the maximum allowed length.
    TooLarge,
}
