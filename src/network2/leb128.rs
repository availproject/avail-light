//! Little Endian Base 128
//!
//! The LEB128 encoding is used throughout the networking code.
//!
//! See https://en.wikipedia.org/wiki/LEB128

// TODO: better doc

use core::{convert::TryFrom as _, iter};

/// Returns an LEB128-encoded integer as a list of bytes.
// TODO: could be an ExactSizeIterator
pub fn encode(value: impl Into<u64>) -> impl Iterator<Item = u8> {
    let mut value = value.into();
    let mut finished = false;

    iter::from_fn(move || {
        if finished {
            return None;
        }

        if value < (1 << 7) {
            finished = true;
            return Some(u8::try_from(value).unwrap());
        }

        let ret = (1 << 7) | u8::try_from(value & 0b1111111).unwrap();
        value >>= 7;
        Some(ret)
    })
}

/// Returns an LEB128-encoded `usize` as a list of bytes.
pub fn encode_usize(value: usize) -> impl Iterator<Item = u8> {
    encode(u64::try_from(value).unwrap())
}

// TODO: tests
