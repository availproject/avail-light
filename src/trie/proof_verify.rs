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

//! Verification of a trie proof.
//!
//! A trie proof is a proof that a certain key in the trie has a certain storage value (or lacks
//! a storage value). The proof can be verified by knowing only the Merkle value of the root node.
//!
//! # Details
//!
//! > **Note**: For reminder, the Merkle value of a node is the hash of its node value, or the
//! >           node value directly if its length is smaller than 32 bytes.
//!
//! A trie proof consists in a list of node values of nodes in the trie. For the proof to be valid,
//! the hash of one of these node values must match the expected trie root node value. Since a
//! node value contains the Merkle values of the children of the node, it is possible to iterate
//! down the hierarchy of nodes until the one closest to the desired key is found.
//!
//! # Multiple proofs merged into one
//!
//! Considering that a trie proof consists in a list of node values, it is possible to reduce the
//! space occupied by multiple trie proofs built from the same trie by merging them into a single
//! list and removing duplicate elements.
//!
//! In order to support this use case, the [`verify_proof`] function intentionally doesn't return
//! an error if some elements in the proof are unused, as it might be that these elements are part
//! of a different proof that has been merged with the one that is relevant.
//!
//! > **Note**: The main use case for merging multiple proofs into one is when a machine that has
//! >           access to the storage of a block sends to a machine that doesn't all the proofs
//! >           corresponding to the storage entries necessary for a certain runtime call.
//!

use super::nibble;

use alloc::vec::Vec;
use core::{convert::TryFrom as _, iter};

/// Configuration to pass to [`verify_proof`].
pub struct VerifyProofConfig<'a, I> {
    /// Key whose storage value needs to be found.
    pub requested_key: &'a [u8],

    /// Merkle value (or node value) of the root node of the trie.
    ///
    /// > **Note**: The Merkle value and node value are always the same for the root node.
    pub trie_root_hash: &'a [u8; 32],

    /// List of node values of nodes found in the trie. No specific order is required. All the
    /// values between the root node and the node closest to the requested key have to be included
    /// in the list in order for the verification to be able to succeed.
    pub proof: I,
}

/// Find the storage value of the requested key (as designated by
/// [`VerifyProofConfig::requested_key`]).
///
/// Returns an error if the proof couldn't be verified.
/// If the proof could be verified and the key has an associated storage value, `Ok(Some(_))` is
/// returned, containining that storage value.
/// If the proof could be verified but the key does not have an associated storage value,
/// `Ok(None)` is returned.
///
/// > **Note**: This does not fully verify the correctness of the node values provided by `proof`.
/// >           Only the minimum amount of information required is fetched from `proof`, and an
/// >           error is returned if a problem happens during this process.
pub fn verify_proof<'a, 'b>(
    config: VerifyProofConfig<'a, impl Iterator<Item = &'b [u8]> + Clone>,
) -> Result<Option<&'b [u8]>, Error> {
    Ok(trie_node_info(TrieNodeInfoConfig {
        requested_key: nibble::bytes_to_nibbles(config.requested_key.iter().cloned()),
        trie_root_hash: config.trie_root_hash,
        proof: config.proof,
    })?
    .node_value)
}

/// Configuration to pass to [`trie_node_info`].
pub struct TrieNodeInfoConfig<'a, K, I> {
    /// Key whose storage value needs to be found.
    pub requested_key: K,

    /// Merkle value (or node value) of the root node of the trie.
    ///
    /// > **Note**: The Merkle value and node value are always the same for the root node.
    pub trie_root_hash: &'a [u8; 32],

    /// List of node values of nodes found in the trie. No specific order is required. All the
    /// values between the root node and the node closest to the requested key have to be included
    /// in the list in order for the verification to be able to succeed.
    pub proof: I,
}

/// Find information about the node whose key is requested by
/// [`TrieNodeInfoConfig::requested_key`].
///
/// The node in question doesn't necessarily have to exist. Nodes that don't exist still return
/// `Ok` but have no storage value and no children.
///
/// Returns an error if the proof couldn't be verified.
///
/// > **Note**: This does not fully verify the correctness of the node values provided by `proof`.
/// >           Only the minimum amount of information required is fetched from `proof`, and an
/// >           error is returned if a problem happens during this process.
pub fn trie_node_info<'a, 'b>(
    config: TrieNodeInfoConfig<
        'a,
        impl Iterator<Item = nibble::Nibble>,
        impl Iterator<Item = &'b [u8]> + Clone,
    >,
) -> Result<TrieNodeInfo<'b>, Error> {
    // The proof contains node values, while Merkle values will be needed. Create a list of
    // Merkle values, one per entry in `config.proof`.
    let merkle_values = config
        .proof
        .clone()
        .map(|proof_entry| -> arrayvec::ArrayVec<[u8; 32]> {
            if proof_entry.len() >= 32 {
                blake2_rfc::blake2b::blake2b(32, &[], proof_entry)
                    .as_bytes()
                    .iter()
                    .cloned()
                    .collect()
            } else {
                proof_entry.iter().cloned().collect()
            }
        })
        .collect::<Vec<_>>();

    // Find the expected trie root in the proof and put it in `node_value`. This is the start
    // point of the verification.
    // `node_value` is updated as the decoding progresses.
    let mut node_value = {
        let proof_iter = merkle_values
            .iter()
            .position(|v| v[..] == config.trie_root_hash[..])
            .ok_or(Error::TrieRootNotFound)?;
        config.proof.clone().nth(proof_iter).unwrap()
    };

    // The verification consists in iterating using `expected_nibbles_iter` and `node_value`.
    let mut expected_nibbles_iter = config.requested_key;
    loop {
        if node_value.is_empty() {
            return Err(Error::InvalidNodeValue);
        }

        let has_children = (node_value[0] & 0x80) != 0;
        let has_storage_value = (node_value[0] & 0x40) != 0;

        // Iterator to the partial key found in the node value of `proof_iter`.
        let mut partial_key = {
            // Length of the partial key, in nibbles.
            let pk_len = {
                let mut accumulator = usize::from(node_value[0] & 0x3f);
                node_value = &node_value[1..];
                let mut continue_iter = accumulator == 63;
                while continue_iter {
                    if node_value.is_empty() {
                        return Err(Error::InvalidNodeValue);
                    }
                    continue_iter = node_value[0] == 255;
                    accumulator = accumulator
                        .checked_add(usize::from(node_value[0]))
                        .ok_or(Error::InvalidNodeValue)?;
                    node_value = &node_value[1..];
                }
                accumulator
            };

            // Length of the partial key, in bytes.
            let pk_len_bytes = if pk_len == 0 {
                0
            } else {
                1 + ((pk_len - 1) / 2)
            };
            if node_value.len() < pk_len_bytes {
                return Err(Error::InvalidNodeValue);
            }

            let pk_nibbles_iter = node_value
                .iter()
                .take(pk_len_bytes)
                .flat_map(|byte| nibble::bytes_to_nibbles(iter::once(*byte)))
                .skip(pk_len % 2);
            node_value = &node_value[pk_len_bytes..];
            pk_nibbles_iter
        };

        // Iterating over this partial key, checking if it matches `expected_nibbles_iter`.
        while let Some(nibble) = partial_key.next() {
            match expected_nibbles_iter.next() {
                None => {
                    return Ok(TrieNodeInfo {
                        node_value: None,
                        children: Children::One(nibble),
                    });
                }
                Some(n) if n != nibble => {
                    return Ok(TrieNodeInfo {
                        node_value: None,
                        children: Children::None,
                    });
                }
                Some(_) => {}
            }
        }

        // After the partial key, the node value optionally contains a bitfield of child nodes.
        let children_bitmap = if has_children {
            if node_value.len() < 2 {
                return Err(Error::InvalidNodeValue);
            }
            let val = u16::from_le_bytes(<[u8; 2]>::try_from(&node_value[..2]).unwrap());
            node_value = &node_value[2..];
            val
        } else {
            0
        };

        if let Some(expected_nibble) = expected_nibbles_iter.next() {
            // The iteration needs to continue with another node.
            // Update `proof_iter` to the point to the child whose index matches next nibble that
            // was just pulled from `expected_nibbles_iter`.

            // No child with the requested index exists.
            if children_bitmap & (1 << u8::from(expected_nibble)) == 0 {
                return Ok(TrieNodeInfo {
                    node_value: None,
                    children: Children::None,
                });
            }

            for n in 0.. {
                if children_bitmap & (1 << n) == 0 {
                    continue;
                }

                // Find the Merkle value of that child in `node_value`.
                let (node_value_update, len) = crate::util::nom_scale_compact_usize(node_value)
                    .map_err(|_: nom::Err<nom::error::Error<&[u8]>>| Error::InvalidNodeValue)?;
                node_value = node_value_update;
                if node_value.len() < len {
                    return Err(Error::InvalidNodeValue);
                }

                // The Merkle value that was just found is the one that interests us.
                if n == u8::from(expected_nibble) {
                    if len < 32 {
                        // If the node value is less than 32 bytes, it means it's unhashed. In that
                        // case, the child isn't part of `proof`.
                        node_value = &node_value[..len];
                    } else {
                        // Find the entry in `proof` matching this Merkle value and update
                        // `proof_iter`.
                        let proof_iter = merkle_values
                            .iter()
                            .position(|v| v[..] == node_value[..len])
                            .ok_or(Error::MissingProofEntry)?;
                        node_value = config.proof.clone().nth(proof_iter).unwrap();
                    }

                    // Break out of the children iteration, to jump to the next node.
                    break;
                }

                node_value = &node_value[len..];
            }
        } else if has_storage_value {
            // The current node (as per `proof_iter`) exactly matches the requested key, and
            // a storage value exists.

            // Skip over the Merkle values of the children.
            for _ in 0..children_bitmap.count_ones() {
                let (node_value_update, len) = crate::util::nom_scale_compact_usize(node_value)
                    .map_err(|_: nom::Err<nom::error::Error<&[u8]>>| Error::InvalidNodeValue)?;
                node_value = node_value_update;
                if node_value.len() < len {
                    return Err(Error::InvalidNodeValue);
                }
                node_value = &node_value[len..];
            }

            // Now at the value that interests us.
            let (node_value_update, len) = crate::util::nom_scale_compact_usize(node_value)
                .map_err(|_: nom::Err<nom::error::Error<&[u8]>>| Error::InvalidNodeValue)?;
            node_value = node_value_update;
            if node_value.len() != len {
                return Err(Error::InvalidNodeValue);
            }
            return Ok(TrieNodeInfo {
                node_value: Some(node_value),
                children: Children::Multiple { children_bitmap },
            });
        } else {
            // The current node (as per `proof_iter`) exactly matches the requested key, but no
            // storage value exists.
            return Ok(TrieNodeInfo {
                node_value: None,
                children: Children::Multiple { children_bitmap },
            });
        }
    }
}

/// Information about a node of the trie.
pub struct TrieNodeInfo<'a> {
    /// Storage value of the node, if any.
    pub node_value: Option<&'a [u8]>,
    /// Which children the node has.
    pub children: Children,
}

/// See [`TrieNodeInfo::children`].
#[derive(Debug, Copy, Clone)]
pub enum Children {
    /// Node doesn't have any child.
    None,
    /// Node has one child. The key of that child starts with the key of the parent, followed with
    /// the nibble contained here, followed with 0 or more extra nibbles unknown here.
    One(nibble::Nibble),
    /// Node has zero or more children.
    Multiple {
        /// If `(children_bitmap & (1 << n)) == 1` (where `n is in 0..16`), then this node has a
        /// child whose key starts with the key of the parent, followed with
        /// `Nibble::try_from(n).unwrap()`, followed with 0 or more extra nibbles unknown here.
        children_bitmap: u16,
    },
}

impl Children {
    /// Iterates over all the children of the node. For each child, contains the nibble that must
    /// be appended to the key of the node in order to find the child.
    pub fn next_nibbles(&self) -> impl Iterator<Item = nibble::Nibble> {
        match *self {
            Children::None => either::Left(None.into_iter()),
            Children::One(nibble) => either::Left(Some(nibble).into_iter()),
            Children::Multiple { children_bitmap } => either::Right(
                nibble::all_nibbles().filter(move |n| (children_bitmap & (1 << u8::from(*n)) != 0)),
            ),
        }
    }
}

/// Possible error returned by [`verify_proof`]
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Trie root wasn't found in the proof.
    TrieRootNotFound,
    /// One of the node values in the proof has an invalid format.
    InvalidNodeValue,
    /// Missing an entry in the proof.
    MissingProofEntry,
}

#[cfg(test)]
mod tests {
    use core::convert::TryFrom as _;

    #[test]
    fn basic_works() {
        // Key/value taken from the Polkadot genesis block.

        let proof = vec![
            hex::decode("7d01542596adb05d6140c170ac479edf7cfd5aa35357590acfe5d11a804d944e500d1456fdda7b8ec7f9e5c794cd83194f0593e4ea").unwrap(),
            hex::decode("803f93804e4c6c4222b747e507008ef1def063bb0d2deeadf17ef4b10e71624d3a0cf81c80241f2c06f22ec58968fb68d432319e25e6c8faa3ad2c5ca9ee48f2e8ed158e2480ad8a68234932269846bc40240a47cfd8d8857b1d81e167bfb24c947a4cdad9e680c84590e39f8b79a2694ad2bf7e7258af686b472f38b064bbce7d08404931a430805c72f25b1b6304d16667e2766fa1a906cb081788eb4502787df7c3597412b17b806e21c5f1a24a196615b4e5b36d21280cdcc80098c1e2bce8eeaf301e9951767480424f1acd80ba074a2ce8d180bf3488a5ca91cb81fba96c8c3c1d33eacbb18160805e849d5c148ca361a55a2c9b384e17ce919e936ccb8011a4f72504e9f93db8cd80edd005a1495c70250d77f81c24c15a9919f034f7983df8e505e53a5af7b402138012a0dd90497b65312bda67ea15996578eeb3891bca8666951a326612418e3143").unwrap(),
            hex::decode("80555d8043fb497c1b2a7b9e4feb59f410c1a29e28b2a628ff9c6003e080f6b9fadd95f9806e8d911b6818038eb7c8534af8e78e9920a1ab8d939c36d3e69b0a1e5928110b80ba4d3f543957f422b40c8e74af9de00acbeba8154afca57a7f80fbbcfebb1e4a803d1b8f5cf1788b294537b8fd2d34acec4646a7627c6cd3d2039af64ff5d1976d80e7620f21cf13964f29d34ba708c3b44ea45ea11c58fbbedda29d13470bc80ca080f98aae4f83d81bf15d88019e5c303d7c19d0524e84c714e05f61517cde0b138280d518faf566fdc4d045094abe372bb3bbecd4753f76db8c41ba9fc015558bf23a80908f991126d12ce7acd55508ff1e7dffa56f742401e1814fc1469658a78c7a7f8001b0a08da0c83253d5c0cb877286c062da2f530ae424fe2545377941fd016913").unwrap(),
            hex::decode("80b3a780a29fac7f7dfae21d05d9506e7da6515b7fa1ad970ff876de35f1bec2599ec002805b6772dc6a4e7604c8d0652479f95b343607c2d9138c59eeb799d85bf43b6bbf803d12becb6a4b9919ddc7c5973d04eed7696c834f90c779fc1fcf7350ccc28d6b805f33ebcf191fddcf3b3f346ec336c105c74b40a4d35dfda0c592f2bea00084e980f764c733d6e35771a9b26a1fa86b9bec59742b046f698be6c140af1073897d3d80cd3bc8c3ce3cf8359f7371a13316f02fd22b02a3d327684a2b61f4a47e0022b880da752afaeb925d5300e45b851052c5f8a9c5aae884f15d64764edf961b8b22c880bf1fa9c7e4c94340dbafd75cbe016c980d0e5d5b4e76823fa11e61629014c34b804f54a15e5d51d02b84e8cae94c9833ae81e56b8f0b684d257f6f722ee66cadf98094833fb2dce8c78d443cd6786e0c01d8974a4b779c178ef5e66b49e021dd7f1a").unwrap(),
            hex::decode("9f0c5d795d0297be56027a4b2464e33397609280f332ff556abf5daf0d34523df7c8cd1369bcb6adbb23a48093bf070a9711bf3480382934134aa919b59c16ff8de8d97a7fdcc2448ea327b26f44005d756d1785878081d634140b36ce031c4b6c6266e2a7c19d9a88e38fdd8ad23abd3db20e714f6980fde17041f22f09609d79dbe38dcccefcaac139c7a10fb23bd284c1c492b004fd80d287ad1d0ade65e64d3969f4ab85a37076816031438cea0bf8c33b7b2bc6c330").unwrap(),
            hex::decode("9f03e6d3c1fb15805edfd024172ea4817dffff80152833e34a852e9751cfc0f954aeb835e1f843936ba9979853a40e439937255f806a36e0ad23fb3224fff6e6db62048463a7f27ccb92f65b4e348acd5a7aa3a0688027b6e099c11581fb2e8acf3b6b94eaed442277b9a74ce7f922f6e3bf2959867b80fd0cc2c846db6a9ed19a715d6c3cd46a48b7f409883c70b2d4c978b306de379e80ab008a78c340f5cc75d99cdb905951936686445c834719be21f7620b950dcd5c806d86af54d5dfb1c06f3fefdd5a430861c0d19e25fad4bad07c6e70d4a679f0b880f35edc5400b6661fb1e6fba7c599c8ba891458d14400030fa506999a1972369f80746cdaa0b7da2e9c3864971f50f12d9b4281f804d5a2dba6ebe06959b2a9fb47802ecfde11456423c87fed8068f414a5ba44ebe3ae91b06d14cc231a78d4aba68e80f655291833a49cf23d057bb15c42d377c55d50f5885329060b0aaab22283cbb1808c95fb2b62baf30718b8330ef68a527c97c1bc9960304353224d8a8ae88a79d58045c1b6d9904ae171d573bdcebaa05142d81648bdbeb16ceeddc54a0ed15d3e2b80a8ea193282fe85b6481707091c77c9218ea19de914e75950925fe86400fb0cb080c222ceab5355eaa41da807146f2e2df7ff648c3e8bbb6d8ee23274ba724551b18008f142dc3c59bf1151c829ecefea35919e80453db5e9669f5a73899aaa5166ee804f1d21fbdc0180c4de886bf40f91dfc2202b3eb6d42548d476908041dd617bb8").unwrap(),
        ];

        let requested_key = hex::decode("9c5d795d0297be56027a4b2464e3339763e6d3c1fb15805edfd024172ea4817d7081542596adb05d6140c170ac479edf7cfd5aa35357590acfe5d11a804d944e").unwrap();

        let trie_root = {
            let bytes =
                hex::decode(&"29d0d972cd27cbc511e9589fcb7a4506d5eb6a9e8df205f00472e5ab354a4e17")
                    .unwrap();
            <[u8; 32]>::try_from(&bytes[..]).unwrap()
        };

        let obtained = super::verify_proof(super::VerifyProofConfig {
            requested_key: &requested_key[..],
            trie_root_hash: &trie_root,
            proof: proof.iter().map(|p| &p[..]),
        })
        .unwrap();

        assert_eq!(
            obtained,
            Some(&hex::decode("0d1456fdda7b8ec7f9e5c794cd83194f0593e4ea").unwrap()[..])
        );
    }

    #[test]
    fn node_values_smaller_than_32bytes() {
        let proof = vec![
            vec![
                158, 195, 101, 195, 207, 89, 214, 113, 235, 114, 218, 14, 122, 65, 19, 196, 0, 3,
                88, 95, 7, 141, 67, 77, 97, 37, 180, 4, 67, 254, 17, 253, 41, 45, 19, 164, 16, 2,
                0, 0, 0, 104, 95, 15, 31, 5, 21, 244, 98, 205, 207, 132, 224, 241, 214, 4, 93, 252,
                187, 32, 80, 82, 127, 41, 119, 1, 0, 0,
            ],
            vec![
                128, 175, 188, 128, 15, 126, 137, 9, 189, 204, 29, 117, 244, 124, 194, 9, 181, 214,
                119, 106, 91, 55, 85, 146, 101, 112, 37, 46, 31, 42, 133, 72, 101, 38, 60, 66, 128,
                28, 186, 118, 76, 106, 111, 232, 204, 106, 88, 52, 218, 113, 2, 76, 119, 132, 172,
                202, 215, 130, 198, 184, 230, 206, 134, 44, 171, 25, 86, 243, 121, 128, 233, 10,
                145, 50, 95, 100, 17, 213, 147, 28, 9, 142, 56, 95, 33, 40, 56, 9, 39, 3, 193, 79,
                169, 207, 115, 80, 61, 217, 4, 106, 172, 152, 128, 12, 255, 241, 157, 249, 219,
                101, 33, 139, 178, 174, 121, 165, 33, 175, 0, 232, 230, 129, 23, 89, 219, 21, 35,
                23, 48, 18, 153, 124, 96, 81, 66, 128, 30, 174, 194, 227, 100, 149, 97, 237, 23,
                238, 114, 178, 106, 158, 238, 48, 166, 82, 19, 210, 129, 122, 70, 165, 94, 186, 31,
                28, 80, 29, 73, 252, 128, 16, 56, 19, 158, 188, 178, 192, 234, 12, 251, 221, 107,
                119, 243, 74, 155, 111, 53, 36, 107, 183, 204, 174, 253, 183, 67, 77, 199, 47, 121,
                185, 162, 128, 17, 217, 226, 195, 240, 113, 144, 201, 129, 184, 240, 237, 204, 79,
                68, 191, 165, 29, 219, 170, 152, 134, 160, 153, 245, 38, 181, 131, 83, 209, 245,
                194, 128, 137, 217, 3, 84, 1, 224, 52, 199, 112, 213, 150, 42, 51, 214, 103, 194,
                225, 224, 210, 84, 84, 53, 31, 159, 82, 201, 3, 104, 118, 212, 110, 7, 128, 240,
                251, 81, 190, 126, 80, 60, 139, 88, 152, 39, 153, 231, 178, 31, 184, 56, 44, 133,
                31, 47, 98, 234, 107, 15, 248, 64, 78, 36, 89, 9, 149, 128, 233, 75, 238, 120, 212,
                149, 223, 135, 48, 174, 211, 219, 223, 217, 20, 172, 212, 172, 3, 234, 54, 130, 55,
                225, 63, 17, 255, 217, 150, 252, 93, 15, 128, 89, 54, 254, 99, 202, 80, 50, 27, 92,
                48, 57, 174, 8, 211, 44, 58, 108, 207, 129, 245, 129, 80, 170, 57, 130, 80, 166,
                250, 214, 40, 156, 181,
            ],
            vec![
                128, 65, 0, 128, 182, 204, 71, 61, 83, 76, 85, 166, 19, 22, 212, 242, 236, 229, 51,
                88, 16, 191, 227, 125, 217, 54, 7, 31, 36, 176, 211, 111, 72, 220, 181, 241, 128,
                149, 2, 12, 26, 95, 9, 193, 115, 207, 253, 90, 218, 0, 41, 140, 119, 189, 166, 101,
                244, 74, 171, 53, 248, 82, 113, 79, 110, 25, 72, 62, 65,
            ],
        ];

        let requested_key =
            hex::decode("f0c365c3cf59d671eb72da0e7a4113c49f1f0515f462cdcf84e0f1d6045dfcbb")
                .unwrap();

        let trie_root = [
            43, 100, 198, 174, 1, 66, 26, 95, 93, 119, 43, 242, 5, 176, 153, 134, 193, 74, 159,
            215, 134, 15, 252, 135, 67, 129, 21, 16, 20, 211, 97, 217,
        ];

        let obtained = super::verify_proof(super::VerifyProofConfig {
            requested_key: &requested_key[..],
            trie_root_hash: &trie_root,
            proof: proof.iter().map(|p| &p[..]),
        })
        .unwrap();

        assert_eq!(obtained, Some(&[80, 82, 127, 41, 119, 1, 0, 0][..]));
    }
}
