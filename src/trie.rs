//! Radix-16 Merkle-Patricia trie.
//!
//! This Substrate/Polkadot-specific radix-16 Merkle-Patricia trie is a data structure that
//! associates keys with values, and that allows efficient verification of the integrity of the
//! data.
//!
//! This data structure is a tree composed of nodes, each node being identified by a key. A key
//! consists in a sequence of 4-bits values called *nibbles*. Example key: `[3, 12, 7, 0]`.
//!
//! Some of these nodes contain a value. These values are inserted by calling [`Trie::insert`].
//!
//! A node A is an *ancestor* of another node B if the key of A is a prefix of the key of B. For
//! example, the node whose key is `[3, 12]` is an ancestor of the node whose key is
//! `[3, 12, 8, 9]`. B is a *descendant* of A.
//!
//! Nodes exist only either if they contain a value, or if their key is the longest shared prefix
//! of two or more nodes that contain a value. For example, if nodes `[7, 2, 9, 11]` and
//! `[7, 2, 14, 8]` contain a value, then node `[7, 2]` also exist, because it is the longest
//! prefix shared between the two.
//!
//! The *Merkle value* of a node is composed, amongst other things, of its associated value and of
//! the Merkle value of its descendants. As such, modifying a node modifies the Merkle value of
//! all its ancestors. Note, however, that modifying a node modifies the Merkle value of *only*
//! its ancestors. As such, the time spent calculating the Merkle value of the root node of a trie
//! mostly depends on the number of modifications that are performed on it, and only a bit on the
//! size of the trie.

use alloc::collections::BTreeMap;

pub mod calculate_root;

/// Radix-16 Merkle-Patricia trie.
// TODO: probably useless, remove
pub struct Trie {
    /// The entries in the tree.
    ///
    /// Since this is a binary tree, the elements are ordered lexicographically.
    /// Example order: "a", "ab", "ac", "b".
    ///
    /// This list only contains the nodes that have an entry in the storage, and not the nodes
    /// that are branches and don't have a storage entry.
    ///
    /// All the keys have an even number of nibbles.
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl Trie {
    /// Builds a new empty [`Trie`].
    pub fn new() -> Trie {
        Trie {
            entries: BTreeMap::new(),
        }
    }

    /// Inserts a new entry in the trie.
    pub fn insert(&mut self, key: &[u8], value: impl Into<Vec<u8>>) {
        self.entries.insert(key.into(), value.into());
    }

    /// Removes an entry from the trie.
    pub fn remove(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        self.entries.remove(key)
    }

    /// Returns true if the `Trie` is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Removes all the elements from the trie.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Calculates the Merkle value of the root node.
    ///
    /// Passes an optional cache.
    pub fn root_merkle_value(
        &self,
        cache: Option<&mut calculate_root::CalculationCache>,
    ) -> [u8; 32] {
        calculate_root::root_merkle_value(calculate_root::Config {
            get_value: &|key: &[u8]| self.entries.get(key).map(|v| &v[..]),
            prefix_keys: &|prefix: &[u8]| {
                self.entries
                    .range(prefix.to_vec()..) // TODO: this to_vec() is annoying
                    .take_while(|(k, _)| k.starts_with(prefix))
                    .map(|(k, _)| From::from(&k[..]))
                    .collect()
            },
            cache,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Trie;

    #[test]
    fn trie_root_one_node() {
        let mut trie = Trie::new();
        trie.insert(b"abcd", b"hello world".to_vec());

        let expected = [
            122, 177, 134, 89, 211, 178, 120, 158, 242, 64, 13, 16, 113, 4, 199, 212, 251, 147,
            208, 109, 154, 182, 168, 182, 65, 165, 222, 124, 63, 236, 200, 81,
        ];

        assert_eq!(trie.root_merkle_value(None), &expected[..]);
    }

    #[test]
    fn trie_root_empty() {
        let trie = Trie::new();
        let expected = blake2_rfc::blake2b::blake2b(32, &[], &[0x0]);
        assert_eq!(trie.root_merkle_value(None), expected.as_bytes());
    }

    #[test]
    fn trie_root_single_tuple() {
        let mut trie = Trie::new();
        trie.insert(&[0xaa], [0xbb].to_vec());

        fn to_compact(n: u8) -> u8 {
            use parity_scale_codec::Encode as _;
            parity_scale_codec::Compact(n).encode()[0]
        }

        let expected = blake2_rfc::blake2b::blake2b(
            32,
            &[],
            &[
                0x42,          // leaf 0x40 (2^6) with (+) key of 2 nibbles (0x02)
                0xaa,          // key data
                to_compact(1), // length of value in bytes as Compact
                0xbb,          // value data
            ],
        );

        assert_eq!(trie.root_merkle_value(None), expected.as_bytes());
    }

    #[test]
    fn trie_root() {
        let mut trie = Trie::new();
        trie.insert(&[0x48, 0x19], [0xfe].to_vec());
        trie.insert(&[0x13, 0x14], [0xff].to_vec());

        fn to_compact(n: u8) -> u8 {
            use parity_scale_codec::Encode as _;
            parity_scale_codec::Compact(n).encode()[0]
        }

        let mut ex = Vec::<u8>::new();
        ex.push(0x80); // branch, no value (0b_10..) no nibble
        ex.push(0x12); // slots 1 & 4 are taken from 0-7
        ex.push(0x00); // no slots from 8-15
        ex.push(to_compact(0x05)); // first slot: LEAF, 5 bytes long.
        ex.push(0x43); // leaf 0x40 with 3 nibbles
        ex.push(0x03); // first nibble
        ex.push(0x14); // second & third nibble
        ex.push(to_compact(0x01)); // 1 byte data
        ex.push(0xff); // value data
        ex.push(to_compact(0x05)); // second slot: LEAF, 5 bytes long.
        ex.push(0x43); // leaf with 3 nibbles
        ex.push(0x08); // first nibble
        ex.push(0x19); // second & third nibble
        ex.push(to_compact(0x01)); // 1 byte data
        ex.push(0xfe); // value data

        let expected = blake2_rfc::blake2b::blake2b(32, &[], &ex);
        assert_eq!(trie.root_merkle_value(None), expected.as_bytes());
    }
}
