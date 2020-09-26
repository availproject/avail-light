// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Calculation of the Merkle value of a node given the information about it.
//!
//! Use the [`calculate_merkle_root`] function to calculate the Merkle value. The [`Config`]
//! struct contains all the input required for the calculation.
//!
//! # Example
//!
//! ```
//! use std::convert::TryFrom as _;
//! use substrate_lite::trie::{Nibble, node_value};
//!
//! let merkle_value = {
//!     // The example node whose value we calculate has three children.
//!     let children = {
//!         let mut children = Vec::new();
//!         for _ in 0..2 {
//!             children.push(None);
//!         }
//!         children.push(Some(node_value::Output::from_bytes(b"foo")));
//!         for _ in 0..7 {
//!             children.push(None);
//!         }
//!         children.push(Some(node_value::Output::from_bytes(b"bar")));
//!         for _ in 0..5 {
//!             children.push(None);
//!         }
//!         children
//!     };
//!
//!     node_value::calculate_merkle_root(node_value::Config {
//!         is_root: false,
//!         children: children.iter().map(|opt| opt.as_ref()),
//!         partial_key: [
//!             Nibble::try_from(8).unwrap(),
//!             Nibble::try_from(12).unwrap(),
//!             Nibble::try_from(1).unwrap(),
//!         ]
//!         .iter()
//!         .cloned(),
//!         stored_value: Some(b"hello world"),
//!     })
//! };
//!
//! assert_eq!(
//!     merkle_value.as_ref(),
//!     &[
//!         195, 8, 193, 4, 4, 12, 102, 111, 111, 12, 98, 97, 114, 44, 104, 101, 108, 108, 111,
//!         32, 119, 111, 114, 108, 100
//!     ]
//! );
//! ```

use super::nibble::Nibble;

use arrayvec::ArrayVec;
use core::{convert::TryFrom as _, fmt};
use parity_scale_codec::Encode as _;

/// Information about a node whose Merkle value is to be calculated.
///
/// The documentation here assumes that you already know how the trie works.
pub struct Config<TChIter, TPKey, TVal> {
    /// True if this is the root node of the trie.
    pub is_root: bool,

    /// Iterator to the Merkle values of the 16 possible children of the node. `None` if there is
    /// no child at this index.
    pub children: TChIter,

    /// Partial key of the node, as an iterator of nibbles.
    ///
    /// If `is_root` is true, this is the entire key of the node.
    ///
    /// For reminder, the key of non-root nodes is made of three parts:
    ///
    /// - The parent's key.
    /// - The child index, one single nibble indicating which child we are relative to the parent.
    /// - The partial key.
    ///
    pub partial_key: TPKey,

    /// Value of the node in the storage.
    pub stored_value: Option<TVal>,
}

/// Calculates the Merkle value of a node given the information about this node.
///
/// # Panic
///
/// Panics if `config.children.len() != 16`.
///
pub fn calculate_merkle_root<'a, TChIter, TPKey, TVal>(
    mut config: Config<TChIter, TPKey, TVal>,
) -> Output
where
    TChIter: ExactSizeIterator<Item = Option<&'a Output>> + Clone,
    TPKey: ExactSizeIterator<Item = Nibble>,
    TVal: AsRef<[u8]>,
{
    assert_eq!(config.children.len(), 16);

    let has_children = config.children.clone().any(|c| c.is_some());

    // This value will be used as the sink for all the components of the merkle value.
    let mut merkle_value_sink = if config.is_root {
        HashOrInline::Hasher(blake2_rfc::blake2b::Blake2b::new(32))
    } else {
        HashOrInline::Inline(ArrayVec::new())
    };

    // Push the header of the node to `merkle_value_sink`.
    {
        // The first two most significant bits of the header contain the type of node.
        let two_msb: u8 = {
            let has_stored_value = config.stored_value.is_some();
            match (has_stored_value, has_children) {
                (false, false) => {
                    // This should only ever be reached if we compute the root node of an
                    // empty trie.
                    0b00
                }
                (true, false) => 0b01,
                (false, true) => 0b10,
                (true, true) => 0b11,
            }
        };

        // Another weird algorithm to encode the partial key length into the header.
        let mut pk_len = config.partial_key.len();
        if pk_len >= 63 {
            pk_len -= 63;
            merkle_value_sink.update(&[(two_msb << 6) + 63]);
            while pk_len > 255 {
                pk_len -= 255;
                merkle_value_sink.update(&[255]);
            }
            merkle_value_sink.update(&[u8::try_from(pk_len).unwrap()]);
        } else {
            merkle_value_sink.update(&[(two_msb << 6) + u8::try_from(pk_len).unwrap()]);
        }
    }

    // Turn the `config.partial_key` into bytes with a weird encoding and push it to
    // `merkle_value_sink`.
    if config.partial_key.len() % 2 != 0 {
        // next().unwrap() can't panic, otherwise `len() % 2` would have returned 0.
        merkle_value_sink.update(&[u8::from(config.partial_key.next().unwrap())]);
    }
    {
        let mut previous = None;
        for nibble in config.partial_key {
            if let Some(prev) = previous.take() {
                let val = (u8::from(prev) << 4) | u8::from(nibble);
                merkle_value_sink.update(&[val]);
            } else {
                previous = Some(nibble);
            }
        }
        assert!(previous.is_none());
    }

    // Compute the node subvalue and push it to `merkle_value_sink`.

    // If there isn't any children, the node subvalue only consists in the storage value.
    // We take a shortcut and end the calculation now.
    if !has_children {
        if let Some(stored_value) = config.stored_value {
            // Doing something like `merkle_value_sink.update(stored_value.encode());` would be
            // quite expensive because we would duplicate the storage value. Instead, we do the
            // encoding manually by pushing the length then the value.
            parity_scale_codec::Compact(u64::try_from(stored_value.as_ref().len()).unwrap())
                .encode_to(&mut merkle_value_sink);
            merkle_value_sink.update(stored_value.as_ref());
        }

        return merkle_value_sink.finalize();
    }

    // If there is any child, we a `u16` where each bit is `1` if there exists a child there.
    merkle_value_sink.update({
        let mut children_bitmap = 0u16;
        for (child_index, child) in config.children.clone().enumerate() {
            if child.is_some() {
                children_bitmap |= 1 << u32::try_from(child_index).unwrap();
            }
        }
        &children_bitmap.to_le_bytes()[..]
    });

    // Push the merkle values of all the children.
    for child in config.children.clone() {
        let child_merkle_value = match child {
            Some(v) => v,
            None => continue,
        };

        // Doing something like `merkle_value_sink.update(child_merkle_value.encode());` would be
        // expensive because we would duplicate the merkle value. Instead, we do the encoding
        // manually by pushing the length then the value.
        parity_scale_codec::Compact(u64::try_from(child_merkle_value.as_ref().len()).unwrap())
            .encode_to(&mut merkle_value_sink);
        merkle_value_sink.update(child_merkle_value.as_ref());
    }

    // Finally, add our own stored value.
    if let Some(stored_value) = config.stored_value {
        // Doing something like `merkle_value_sink.update(stored_value.encode());` would be
        // quite expensive because we would duplicate the storage value. Instead, we do the
        // encoding manually by pushing the length then the value.
        parity_scale_codec::Compact(u64::try_from(stored_value.as_ref().len()).unwrap())
            .encode_to(&mut merkle_value_sink);
        merkle_value_sink.update(stored_value.as_ref());
    }

    merkle_value_sink.finalize()
}

/// Output of the calculation.
#[derive(Clone)]
pub struct Output {
    inner: OutputInner,
}

#[derive(Clone)]
enum OutputInner {
    Inline(ArrayVec<[u8; 31]>),
    Hasher(blake2_rfc::blake2b::Blake2bResult),
    Bytes(ArrayVec<[u8; 32]>),
}

impl Output {
    /// Builds an [`Output`] from a slice of bytes.
    ///
    /// # Panic
    ///
    /// Panics if `bytes.len() > 32`.
    ///
    pub fn from_bytes(bytes: &[u8]) -> Output {
        assert!(bytes.len() <= 32);
        Output {
            inner: OutputInner::Bytes({
                let mut v = ArrayVec::new();
                v.try_extend_from_slice(bytes).unwrap();
                v
            }),
        }
    }
}

impl AsRef<[u8]> for Output {
    fn as_ref(&self) -> &[u8] {
        match &self.inner {
            OutputInner::Inline(a) => a.as_slice(),
            OutputInner::Hasher(a) => a.as_bytes(),
            OutputInner::Bytes(a) => a.as_slice(),
        }
    }
}

impl From<Output> for [u8; 32] {
    fn from(output: Output) -> Self {
        let mut out = [0; 32];
        out.copy_from_slice(output.as_ref());
        out
    }
}

impl fmt::Debug for Output {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self.as_ref(), f)
    }
}

/// The merkle value of a node is defined as either the hash of the node value, or the node value
/// itself if it is shorted than 32 bytes (or if we are the root).
///
/// This struct serves as a helper to handle these situations. Rather than putting intermediary
/// values in buffers then hashing the node value as a whole, we push the elements of the node
/// value to this struct which automatically switches to hashing if the value exceeds 32 bytes.
enum HashOrInline {
    Inline(ArrayVec<[u8; 31]>),
    Hasher(blake2_rfc::blake2b::Blake2b),
}

impl HashOrInline {
    /// Adds data to the node value. If this is a [`HashOrInline::Inline`] and the total size would
    /// go above 32 bytes, then we switch to a hasher.
    fn update(&mut self, data: &[u8]) {
        match self {
            HashOrInline::Inline(curr) => {
                if curr.try_extend_from_slice(data).is_err() {
                    let mut hasher = blake2_rfc::blake2b::Blake2b::new(32);
                    hasher.update(&curr);
                    hasher.update(data);
                    *self = HashOrInline::Hasher(hasher);
                }
            }
            HashOrInline::Hasher(hasher) => {
                hasher.update(data);
            }
        }
    }

    fn finalize(self) -> Output {
        Output {
            inner: match self {
                HashOrInline::Inline(b) => OutputInner::Inline(b),
                HashOrInline::Hasher(h) => OutputInner::Hasher(h.finalize()),
            },
        }
    }
}

impl parity_scale_codec::Output for HashOrInline {
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::Nibble;
    use core::{convert::TryFrom as _, iter};

    #[test]
    fn empty_root() {
        let obtained = super::calculate_merkle_root(super::Config {
            is_root: true,
            children: (0..16).map(|_| None),
            partial_key: iter::empty(),
            stored_value: None::<Vec<u8>>,
        });

        assert_eq!(
            obtained.as_ref(),
            &[
                3, 23, 10, 46, 117, 151, 183, 183, 227, 216, 76, 5, 57, 29, 19, 154, 98, 177, 87,
                231, 135, 134, 216, 192, 130, 242, 157, 207, 76, 17, 19, 20
            ]
        );
    }

    #[test]
    fn empty_node() {
        let obtained = super::calculate_merkle_root(super::Config {
            is_root: false,
            children: (0..16).map(|_| None),
            partial_key: iter::empty(),
            stored_value: None::<Vec<u8>>,
        });

        assert_eq!(obtained.as_ref(), &[0u8]);
    }

    #[test]
    fn basic_test() {
        let children = {
            let mut children = Vec::new();
            for _ in 0..2 {
                children.push(None);
            }
            children.push(Some(super::Output::from_bytes(b"foo")));
            for _ in 0..7 {
                children.push(None);
            }
            children.push(Some(super::Output::from_bytes(b"bar")));
            for _ in 0..5 {
                children.push(None);
            }
            children
        };

        let obtained = super::calculate_merkle_root(super::Config {
            is_root: false,
            children: children.iter().map(|opt| opt.as_ref()),
            partial_key: [
                Nibble::try_from(8).unwrap(),
                Nibble::try_from(12).unwrap(),
                Nibble::try_from(1).unwrap(),
            ]
            .iter()
            .cloned(),
            stored_value: Some(b"hello world"),
        });

        assert_eq!(
            obtained.as_ref(),
            &[
                195, 8, 193, 4, 4, 12, 102, 111, 111, 12, 98, 97, 114, 44, 104, 101, 108, 108, 111,
                32, 119, 111, 114, 108, 100
            ]
        );
    }

    #[test]
    #[should_panic]
    fn bad_children_len() {
        super::calculate_merkle_root(super::Config {
            is_root: false,
            children: iter::empty(),
            partial_key: iter::empty(),
            stored_value: None::<Vec<u8>>,
        });
    }
}
